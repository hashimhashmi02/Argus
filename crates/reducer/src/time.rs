//! Injected time.

use std::time::Duration;

/// A monotonic instant, supplied by the caller.
///
/// This crate never reads the clock. Every function that cares about time takes
/// a `Timestamp` argument instead, and that single constraint is what makes the
/// debounce logic testable: a test advances time by constructing a number, not
/// by sleeping. Timing tests that sleep are slow *and* flaky, because they
/// quietly assume something about scheduler latency that stops being true on a
/// loaded machine.
///
/// The value is a duration since an epoch the caller chooses -- in practice,
/// when the session was spawned. Nothing here depends on that choice, only on
/// the differences between timestamps, which is why `Instant` (whose values
/// cannot be constructed from thin air) is deliberately not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(Duration);

impl Timestamp {
    /// The caller's epoch.
    pub const ZERO: Timestamp = Timestamp(Duration::ZERO);

    /// Build a timestamp from milliseconds since the epoch. Mostly for tests.
    pub const fn from_millis(millis: u64) -> Self {
        Timestamp(Duration::from_millis(millis))
    }

    /// Build a timestamp from a duration since the epoch.
    ///
    /// Production callers use `Timestamp::from_elapsed(session_start.elapsed())`,
    /// which keeps the one real clock read at the edge of the system where it
    /// belongs.
    pub const fn from_elapsed(elapsed: Duration) -> Self {
        Timestamp(elapsed)
    }

    /// How long since `earlier`.
    ///
    /// Saturating rather than panicking on a backwards timestamp. This crate has
    /// no way to enforce that callers pass monotonically increasing values, and
    /// a status reducer that panics because two threads disagreed about the
    /// order of two events by a microsecond would be a bad trade.
    pub fn saturating_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}
