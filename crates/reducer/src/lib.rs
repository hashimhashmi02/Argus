//! The pure status state machine. No I/O, no async, no system clock.
//!
//! ```text
//! fn reduce(state, screen_match, hook_signal, now) -> state
//! ```
//!
//! `now` is an injected parameter rather than something this crate reads from
//! the clock itself. That single constraint is what makes every debounce and
//! anti-flicker path testable by advancing a fake timestamp instead of
//! sleeping, so the whole test suite stays instant and deterministic.
//!
//! Responsibilities: debounce (one stray idle frame must not flip a working
//! session to idle), blocker arbitration, and subagent isolation (a subagent
//! finishing must not drag the parent session to idle).
//!
//! Implemented in step 3. See DESIGN.md.
