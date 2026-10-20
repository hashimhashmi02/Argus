//! What a client sees when it asks about a session.

use argus_reducer::{Status, Timestamp};
use argus_terminal_emu::Screen;

/// A consistent view of one session at one moment.
///
/// A snapshot rather than a set of getters on live state, because the fields
/// have to agree with each other. "Status is `NeedsYou`, and here is the screen
/// that is *not* the one that caused it" is worse than useless when the point of
/// the screen is to show the user what they are being asked. Publishing them
/// together means a reader can never catch the session halfway through an
/// update.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub status: Status,

    /// When the session entered this status, measured from spawn.
    pub since: Timestamp,

    /// Which manifest rule produced the status, if it came from the screen.
    ///
    /// The thread back from a surprising classification to the line of JSON
    /// responsible for it.
    pub rule: Option<String>,

    /// A status being debounced but not yet committed.
    ///
    /// Means "why is this still showing as working" has a real answer: idle is
    /// four hundred milliseconds into a wait.
    pub pending: Option<Status>,

    /// The emulated screen, as the user would see it.
    pub screen: Screen,

    /// OS process id, while the child is alive.
    pub pid: Option<u32>,

    /// Exit code, once the child is gone.
    pub exit_code: Option<u32>,
}

impl SessionSnapshot {
    pub(crate) fn initial(screen: Screen, pid: Option<u32>) -> Self {
        Self {
            status: Status::Working,
            since: Timestamp::ZERO,
            rule: None,
            pending: None,
            screen,
            pid,
            exit_code: None,
        }
    }
}
