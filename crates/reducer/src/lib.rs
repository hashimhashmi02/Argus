//! The pure status state machine.
//!
//! ```text
//! reduce(state, screen_match, hook_signal, now, policy) -> state
//! ```
//!
//! # Why this is a crate of its own
//!
//! Deciding whether a session is working, blocked, idle or finished is the
//! judgement at the heart of Argus, and it is the part most likely to be wrong
//! in ways that are hard to see. It is also, on its own, completely free of I/O.
//! Separating it means the judgement can be exercised exhaustively without
//! spawning a process, opening a PTY, or starting a runtime.
//!
//! So: no async, no I/O, no allocation of consequence, and above all **no
//! clock**. Time arrives as a [`Timestamp`] parameter. Every debounce path is
//! therefore testable by writing down a sequence of numbers, and the test suite
//! runs in microseconds instead of sleeping through the delays it is checking.
//!
//! # The three judgements it owns
//!
//! - **Debounce.** One quiet frame must not flip a working session to idle.
//!   Agents pause constantly -- between a tool call and its result, while a
//!   model streams its first token, during a network round trip -- and every one
//!   of those looks exactly like idleness for a few hundred milliseconds.
//! - **Blocker arbitration.** The screen and an agent's hooks are independent
//!   evidence about the same question and can disagree. The more urgent claim
//!   wins, because missing a blocked session is the failure Argus exists to
//!   prevent, while showing one wrongly costs a glance.
//! - **Subagent isolation.** A subagent finishing says nothing about the parent,
//!   which is still working. Modelled as its own [`HookSignal`] variant that
//!   implies no status at all, so the isolation is enforced by the type rather
//!   than remembered.
//!
//! # Example
//!
//! ```
//! use argus_reducer::{HookSignal, Policy, ScreenMatch, SessionState, Status, Timestamp, reduce};
//!
//! let policy = Policy::default();
//! let state = SessionState::new(Timestamp::ZERO);
//! assert_eq!(state.status(), Status::Working);
//!
//! // One idle frame is not idleness.
//! let state = reduce(
//!     &state,
//!     &ScreenMatch::new(Status::Idle, "prompt"),
//!     None,
//!     Timestamp::from_millis(100),
//!     &policy,
//! );
//! assert_eq!(state.status(), Status::Working);
//! assert_eq!(state.pending(), Some(Status::Idle));
//!
//! // Sustained past the debounce, it is.
//! let state = reduce(
//!     &state,
//!     &ScreenMatch::new(Status::Idle, "prompt"),
//!     None,
//!     Timestamp::from_millis(1_000),
//!     &policy,
//! );
//! assert_eq!(state.status(), Status::Idle);
//!
//! // A blocker is not debounced at all.
//! let state = reduce(
//!     &state,
//!     &ScreenMatch::new(Status::NeedsYou, "permission-prompt"),
//!     Some(HookSignal::TurnStarted),
//!     Timestamp::from_millis(1_010),
//!     &policy,
//! );
//! assert_eq!(state.status(), Status::NeedsYou);
//! ```

mod state;
mod status;
mod time;

pub use state::{Policy, SessionState, reduce};
pub use status::{HookSignal, ScreenMatch, Status};
pub use time::Timestamp;
