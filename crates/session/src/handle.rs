//! The client-facing side of a session.

use std::time::{Duration, Instant};

use argus_reducer::{HookSignal, Status};
use tokio::sync::{mpsc, oneshot, watch};

use crate::snapshot::SessionSnapshot;
use crate::stream::Watch;

/// Something for the session task to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Bytes to type at the child, as if from a keyboard.
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// An out-of-band signal from the agent itself.
    Hook(HookSignal),
    Kill,
    /// Start watching: reply with a repaint and a stream that begins where it
    /// ends.
    Watch(oneshot::Sender<Watch>),
}

/// A cheap, cloneable handle to a running session.
///
/// Reads and writes travel by different routes on purpose.
///
/// Writes go through an `mpsc` channel, because they must be ordered and must
/// be applied by the one task that owns the PTY.
///
/// Reads come from a `watch` channel, which always holds the latest snapshot
/// and never blocks a writer. The alternative -- asking the task for its status
/// over the command channel and awaiting a reply -- would queue every read
/// behind every pending write, and status is read far more often than anything
/// is written: a UI listing a dozen sessions reads all of them on every repaint.
/// A read that cannot be starved by a busy session is worth the extra channel.
#[derive(Clone)]
pub struct SessionHandle {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<SessionSnapshot>,
    /// The same instant the session task measures `since` from.
    started: Instant,
}

impl SessionHandle {
    pub(crate) fn new(
        commands: mpsc::Sender<Command>,
        snapshot: watch::Receiver<SessionSnapshot>,
        started: Instant,
    ) -> Self {
        Self {
            commands,
            snapshot,
            started,
        }
    }

    /// How long since the session was spawned.
    pub fn age(&self) -> Duration {
        self.started.elapsed()
    }

    /// How long the session has held its current status.
    ///
    /// The number a person actually wants -- "needs you *for four minutes*" is
    /// what says which window to look at first. Computed on demand rather than
    /// stored in the snapshot, because a snapshot is only republished when
    /// something changes, and a session that has been idle for an hour has, by
    /// definition, not changed in an hour.
    pub fn status_age(&self) -> Duration {
        let since = self
            .snapshot
            .borrow()
            .since
            .saturating_since(argus_reducer::Timestamp::ZERO);
        self.age().saturating_sub(since)
    }

    /// The most recent snapshot. Never blocks, never fails, never waits on the
    /// session task -- even if that task is busy or has already finished.
    pub fn snapshot(&self) -> SessionSnapshot {
        self.snapshot.borrow().clone()
    }

    pub fn status(&self) -> Status {
        self.snapshot.borrow().status
    }

    /// Wait until the snapshot changes.
    ///
    /// For a client that wants to react to a session becoming blocked rather
    /// than poll for it. Returns `false` once the session has finished and no
    /// further changes can arrive.
    pub async fn changed(&mut self) -> bool {
        self.snapshot.changed().await.is_ok()
    }

    /// Type text at the child.
    ///
    /// Safe to call before the child is ready: input that arrives during
    /// ConPTY's startup handshake is held and flushed once the child speaks,
    /// rather than being silently discarded. A caller should not have to know
    /// that the handshake exists.
    pub async fn send_text(&self, text: &str) -> bool {
        self.send_bytes(text.as_bytes().to_vec()).await
    }

    /// Send raw bytes -- a keystroke, a control character, a manifest's
    /// approve key.
    pub async fn send_bytes(&self, bytes: Vec<u8>) -> bool {
        self.commands.send(Command::Input(bytes)).await.is_ok()
    }

    /// Tell the session its window changed size.
    ///
    /// Applied to the PTY and the emulator together. They must never disagree:
    /// a TUI lays itself out against the size it was given, so a mismatch makes
    /// the emulated screen wrap where the child never intended and matching
    /// starts failing on lines that look fine in a real terminal.
    pub async fn resize(&self, cols: u16, rows: u16) -> bool {
        self.commands
            .send(Command::Resize { cols, rows })
            .await
            .is_ok()
    }

    /// Deliver an out-of-band signal from the agent.
    ///
    /// Nothing produces these yet -- the transport is a hook endpoint on the
    /// control server in step 5. The plumbing exists now so that the reducer's
    /// hook path is reachable and exercised rather than dead code waiting for a
    /// caller.
    pub async fn hook(&self, signal: HookSignal) -> bool {
        self.commands.send(Command::Hook(signal)).await.is_ok()
    }

    /// Kill the child.
    pub async fn kill(&self) -> bool {
        self.commands.send(Command::Kill).await.is_ok()
    }

    /// Follow the terminal itself: a repaint of the screen as it is now, then
    /// every byte of output after it.
    ///
    /// For a client that shows the agent's real terminal rather than its
    /// status. `None` once the session has finished; its last screen is still
    /// in [`snapshot`](Self::snapshot), as text.
    ///
    /// Asked of the session task rather than read from the snapshot, because
    /// the two halves have to be taken at the same instant -- see [`Watch`].
    pub async fn watch(&self) -> Option<Watch> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::Watch(reply)).await.ok()?;
        answer.await.ok()
    }

    /// Whether the session task is still running.
    pub fn is_alive(&self) -> bool {
        !self.commands.is_closed()
    }
}
