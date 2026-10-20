//! How to start a session.

use std::path::PathBuf;
use std::time::Duration;

/// Terminal size and timing for one session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Working directory for the child. In the finished system this is the
    /// agent's git worktree.
    pub cwd: Option<PathBuf>,

    pub cols: u16,
    pub rows: u16,

    /// How often the session re-evaluates its status even when the child has
    /// said nothing.
    ///
    /// This is the single most important knob in the crate, and the reason the
    /// loop is built around `select!` with a timer rather than a blocking read.
    /// Debounce is a statement about *elapsed time* -- "idle must hold for 750ms
    /// before I believe it" -- and elapsed time keeps passing whether or not the
    /// child produces output. A loop that only wakes on bytes would hold a
    /// session at "working" forever the moment it genuinely went quiet, which is
    /// precisely the case the debounce exists to resolve.
    pub tick: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cwd: None,
            cols: 120,
            rows: 30,
            // Comfortably finer than the 750ms default idle debounce, so the
            // commit lands within a tick of being due rather than up to a
            // debounce late.
            tick: Duration::from_millis(100),
        }
    }
}

impl SessionConfig {
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    pub fn tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }
}
