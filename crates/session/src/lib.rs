//! One live agent process: PTY, emulator, manifest and reducer, tied together.
//!
//! # What this crate is
//!
//! Steps 1 to 3 each solved one problem in isolation and were deliberately kept
//! ignorant of each other: the PTY layer knows nothing about agents, the
//! emulator knows nothing about PTYs, the manifest knows nothing about screens
//! as a type, and the reducer knows nothing about anything. This crate is where
//! they meet, and it is the only place that knows the shape of the whole
//! pipeline:
//!
//! ```text
//! ConPTY bytes -> emulator -> screen -> manifest rules -> reducer -> status
//!                     |                                      ^
//!                     +---- write-backs to the PTY           |
//!                                            hook signals ---+
//! ```
//!
//! # The loop
//!
//! A [`Session`] is one `tokio` task owning one child process. It wakes on
//! three things: output from the child, a command from a client, or a timer
//! tick — and re-evaluates its status on every one of them.
//!
//! The tick is not a detail. A debounce is a claim about *elapsed time* ("idle
//! must hold for 750ms before I believe it"), and elapsed time passes whether or
//! not the child says anything. A loop that only woke on bytes would pin a
//! session at "working" from the moment it genuinely went quiet — exactly the
//! case the debounce exists to resolve.
//!
//! # Where the clock lives
//!
//! Here, and nowhere below. `argus-reducer` takes time as a parameter and never
//! reads it, which is what lets its whole test suite run without sleeping.
//! [`SessionTask`](task::SessionTask) holds the one `Instant` and converts it
//! into a [`Timestamp`](argus_reducer::Timestamp) on each pass. Pushing the
//! clock read to the edge is the point of the design, and this is the edge.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use argus_manifest::Agent;
//! use argus_session::{Session, SessionConfig};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let agent = Arc::new(Agent::from_path("manifests/shell.json")?);
//! let session = Session::spawn(agent, SessionConfig::default())?;
//! let mut handle = session.handle();
//!
//! while handle.changed().await {
//!     let snapshot = handle.snapshot();
//!     println!("{:?}: {}", snapshot.status, snapshot.screen.line(0));
//! }
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use argus_manifest::{Agent, Start};
use argus_pty::{ConPtyProcess, SpawnConfig};
use tokio::sync::{mpsc, watch};

mod config;
mod handle;
mod reader;
mod snapshot;
mod stream;
mod task;

pub use config::SessionConfig;
pub use handle::SessionHandle;
pub use snapshot::SessionSnapshot;
pub use stream::{Repaint, STREAM_DEPTH, StreamItem, Watch};

/// How many commands may be queued before a caller is made to wait.
///
/// Small on purpose. Commands are keystrokes and resizes; a client that is
/// thousands of them ahead of the session has lost track of what it is doing,
/// and making it wait is a better answer than growing a queue of input that will
/// arrive long after the screen it was meant for.
const COMMAND_DEPTH: usize = 32;

/// A running agent session.
///
/// Dropping a `Session` does **not** kill the child: the task holds everything
/// it needs and keeps running. That is the daemon/client split showing up in a
/// type — a session has to survive the thing that was watching it going away.
/// Use [`SessionHandle::kill`] to actually stop it.
pub struct Session {
    handle: SessionHandle,
    task: tokio::task::JoinHandle<()>,
    agent_session: Option<String>,
}

impl Session {
    /// Launch the agent described by `agent` and start driving it.
    ///
    /// Must be called from inside a `tokio` runtime.
    pub fn spawn(agent: Arc<Agent>, config: SessionConfig) -> Result<Self> {
        let manifest = agent.manifest();

        let agent_session = match (&config.agent_session, config.start) {
            (Some(id), _) => Some(id.clone()),
            (None, _) if !agent.wants_session_uuid() => None,
            (None, Start::New) => Some(uuid::Uuid::new_v4().to_string()),
            // Making one up here would ask the agent for a conversation that
            // never existed.
            (None, Start::Resume) => bail!(
                "agent `{}` resumes a conversation by its id, and none was given",
                manifest.id
            ),
        };
        let args = agent
            .args(config.start, agent_session.as_deref())
            .with_context(|| format!("agent `{}` has no `resume` in its manifest", manifest.id))?;

        let mut spawn_config = SpawnConfig::new(&manifest.launch.program)
            .args(args)
            .size(config.cols, config.rows);
        spawn_config.scrub_env_prefixes = manifest.env_scrub_prefixes.clone();
        if let Some(cwd) = &config.cwd {
            spawn_config = spawn_config.cwd(cwd);
        }

        let spawned = ConPtyProcess::spawn(&spawn_config)
            .with_context(|| format!("failed to start agent `{}`", manifest.id))?;

        let emulator = argus_terminal_emu::TerminalEmulator::new(config.cols, config.rows);
        let pid = spawned.process.process_id();
        let (snapshot_tx, snapshot_rx) =
            watch::channel(SessionSnapshot::initial(emulator.screen(), pid));

        // The reader thread is detached rather than joined. Its lifetime is
        // already tied to the pseudo-console's: the session task closes the
        // console on exit, the read returns EOF, and the thread ends. Joining it
        // would mean a blocking `join` somewhere on the async side -- exactly the
        // thing this design exists to avoid -- in exchange for nothing.
        let (bytes_rx, _reader_thread) = reader::spawn_reader(spawned.reader);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_DEPTH);

        // Read once and shared, so the task's `since` and the handle's `age` are
        // measured from the same instant. See `SessionHandle::status_age`.
        let started = Instant::now();

        let session_task =
            task::SessionTask::new(agent, config, spawned.process, snapshot_tx, started);
        let task = tokio::spawn(session_task.run(bytes_rx, command_rx));

        Ok(Self {
            handle: SessionHandle::new(command_tx, snapshot_rx, started),
            task,
            agent_session,
        })
    }

    /// The agent's own conversation id, if its manifest uses one.
    ///
    /// Made up at a fresh start, so whoever means to resume this session
    /// later has to keep it.
    pub fn agent_session(&self) -> Option<&str> {
        self.agent_session.as_deref()
    }

    /// A cloneable handle for reading status and sending input.
    pub fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    /// Wait for the child to exit and the session to finish draining.
    pub async fn wait(self) -> Result<()> {
        self.task.await.context("session task panicked")
    }
}
