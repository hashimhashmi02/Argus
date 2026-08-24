//! Spawning and driving a child process attached to a real pseudo-terminal.
//!
//! # Why a PTY at all
//!
//! Argus could pipe a child's stdout and read it. That would be simpler and it
//! would also be useless. A program connected to a pipe knows it is not talking
//! to a terminal, and every interactive CLI we care about changes its behaviour
//! accordingly: colour is disabled, spinners and progress bars are suppressed,
//! prompts that expect a keypress are skipped or fail outright, and the process
//! never receives a window size. A pseudo-terminal makes the child believe it
//! is attached to a real console, so it behaves exactly as it does for a human.
//! That fidelity is the entire premise of Argus -- we watch what the user would
//! have seen.
//!
//! On Windows the mechanism is ConPTY (Windows 10 1809+), reached here through
//! the `portable-pty` crate rather than raw Win32 FFI. Getting the whole system
//! working first and dropping to raw `CreatePseudoConsole` later is a deliberate
//! ordering choice, not an accident. See DESIGN.md.

use std::ffi::OsString;
use std::path::PathBuf;

mod conpty;
mod shell;

pub use conpty::{ConPtyProcess, SpawnedPty};
pub use shell::{Shell, resolve_shell};

/// How a child process ended.
///
/// This is our own type rather than a re-export of `portable_pty::ExitStatus`
/// on purpose: `PtyProcess` is meant to be the seam that a future raw-ConPTY
/// implementation slots into, and a trait whose signatures name another crate's
/// types is not really an abstraction over that crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: u32,
    pub success: bool,
}

/// Everything needed to launch a child on a fresh PTY.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Working directory. Later this is the agent's git worktree.
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    /// Environment variables whose names start with any of these prefixes are
    /// removed from the child's environment.
    ///
    /// A spawned agent inherits our environment by default, which means it
    /// would inherit Argus's own identity and credentials. Those belong to the
    /// orchestrator, not to the thing being orchestrated. Scrubbing by prefix
    /// (rather than by an exact allowlist) keeps the rule stable as Argus grows
    /// new variables. The real per-agent rules will come from the manifest
    /// crate in step 3; this field is the plumbing they will flow through.
    pub scrub_env_prefixes: Vec<String>,
}

impl SpawnConfig {
    /// A config for `program` with Argus's default 120x30 grid and default
    /// scrubbing.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            cols: 120,
            rows: 30,
            scrub_env_prefixes: vec!["ARGUS_".to_string()],
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }
}

/// The control surface for one live PTY child.
///
/// Note what is *absent*: there is no `read`. `portable-pty` hands back a
/// blocking `Read`, and a blocking read reachable from this trait would sooner
/// or later be called from inside an async task, where it parks a worker thread
/// and can starve every other session on a low-core machine. Keeping the reader
/// a separate owned value forces the caller to make a conscious decision about
/// which thread it blocks -- see `SpawnedPty`.
///
/// Errors are `anyhow` for now because `portable-pty` reports them that way and
/// nothing yet needs to branch on a specific failure. When the RPC layer has to
/// tell a client "that session is already dead" apart from "the pipe broke",
/// this becomes a typed error.
pub trait PtyProcess: Send {
    /// Send bytes to the child as if typed at the keyboard.
    fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()>;

    /// Tell the child its window changed size.
    ///
    /// This is not cosmetic. A TUI lays itself out against the size it was
    /// given, so a stale size means our emulated screen and the child's idea of
    /// the screen disagree, and status detection starts matching against text
    /// that wrapped somewhere the child never intended.
    fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()>;

    /// Terminate the child.
    fn kill(&mut self) -> anyhow::Result<()>;

    /// Poll for exit without blocking. `Ok(None)` means still running.
    fn try_wait(&mut self) -> anyhow::Result<Option<ExitStatus>>;
}
