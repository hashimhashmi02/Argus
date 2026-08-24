//! The state machine itself.

use std::time::Duration;

use crate::status::{HookSignal, ScreenMatch, Status};
use crate::time::Timestamp;

/// How long a new status must be observed before Argus commits to it.
///
/// Debouncing exists because the two failure modes are not symmetric. Flipping a
/// working session to idle on one quiet frame produces a list that flickers, and
/// a flickering list is one nobody trusts. Taking an extra half second to notice
/// genuine idleness costs nothing, because nobody is waiting on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// How long an idle observation must persist before the session is called
    /// idle.
    ///
    /// This is the one that matters. Agents go quiet mid-thought constantly --
    /// between a tool call and its result, while a model streams its first
    /// token, during a network round trip. Every one of those looks exactly like
    /// idleness for a few hundred milliseconds.
    pub idle_debounce: Duration,

    /// How long a needs-you observation must persist. Zero by default.
    ///
    /// Deliberately not debounced. A blocked agent is the one thing the user
    /// must act on, so latency here is the expensive direction, and being wrong
    /// is cheap: the next frame corrects it.
    pub blocker_debounce: Duration,

    /// How long a working observation must persist. Zero by default -- evidence
    /// of activity is unambiguous.
    pub working_debounce: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            idle_debounce: Duration::from_millis(750),
            blocker_debounce: Duration::ZERO,
            working_debounce: Duration::ZERO,
        }
    }
}

impl Policy {
    fn debounce_for(&self, status: Status) -> Duration {
        match status {
            Status::Idle => self.idle_debounce,
            Status::NeedsYou => self.blocker_debounce,
            Status::Working => self.working_debounce,
            // A process either has exited or it has not. There is nothing to
            // debounce and nothing that could contradict it later.
            Status::Done => Duration::ZERO,
        }
    }
}

/// A status that has been observed but not yet committed to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    status: Status,
    /// When this status was *first* seen.
    ///
    /// First, not most recent: the debounce measures how long the new status has
    /// held continuously. Resetting this on every repeat sighting would mean a
    /// status that is observed steadily never commits at all.
    first_seen: Timestamp,
    rule: Option<String>,
}

/// Everything the reducer knows about one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    status: Status,
    since: Timestamp,
    rule: Option<String>,
    candidate: Option<Candidate>,
}

impl SessionState {
    /// A session that has just been spawned.
    ///
    /// Starts `Working` rather than `Idle` because a process that was launched a
    /// moment ago is starting up, and announcing "nothing is happening" for the
    /// first few hundred milliseconds of every spawn would be both wrong and
    /// visibly jumpy.
    pub fn new(now: Timestamp) -> Self {
        Self {
            status: Status::Working,
            since: now,
            rule: None,
            candidate: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// When the current status was entered.
    pub fn since(&self) -> Timestamp {
        self.since
    }

    /// Which manifest rule produced the current status, if it came from the
    /// screen. For debugging a surprising classification.
    pub fn rule(&self) -> Option<&str> {
        self.rule.as_deref()
    }

    /// Whether a different status is currently being debounced.
    ///
    /// Exposed for tests and diagnostics: "why is this still showing as working"
    /// has a much better answer when you can see that idle is two hundred
    /// milliseconds into a debounce.
    pub fn pending(&self) -> Option<Status> {
        self.candidate.as_ref().map(|c| c.status)
    }
}

/// Fold one observation into the session's status.
///
/// Pure: no I/O, no async, no clock, no allocation beyond cloning a rule name.
/// The same inputs always produce the same output, which is what makes the whole
/// state machine testable by writing down a sequence of numbers.
///
/// `screen` and `hook` are two independent sources of evidence about the same
/// question, and either may be absent on any given tick.
pub fn reduce(
    state: &SessionState,
    screen: &ScreenMatch,
    hook: Option<HookSignal>,
    now: Timestamp,
    policy: &Policy,
) -> SessionState {
    // Done is absorbing. Once the process is gone, nothing can argue it back to
    // life -- and something would try: the dead session's final screen is still
    // sitting in the emulator's grid, still matching whatever rule it matched a
    // moment before the exit. Without this guard a finished agent would flip
    // back to "working" forever.
    if state.status == Status::Done {
        return state.clone();
    }

    let observed = arbitrate(screen, hook);

    let Some((observed, rule)) = observed else {
        // No rule matched and no hook fired. That is an absence of evidence, not
        // evidence of absence, so nothing changes -- including any debounce
        // already in progress. A quiet frame must not reset a timer, or a status
        // that alternates between "idle" and "nothing matched" would never
        // commit.
        return state.clone();
    };

    if observed == state.status {
        // The current status was reconfirmed. Any candidate was a flicker;
        // discard it, which is the actual anti-flicker mechanism.
        return SessionState {
            status: state.status,
            since: state.since,
            rule: rule.or_else(|| state.rule.clone()),
            candidate: None,
        };
    }

    // A different status. Either continue debouncing it or start doing so.
    let first_seen = match &state.candidate {
        Some(candidate) if candidate.status == observed => candidate.first_seen,
        _ => now,
    };

    let held_for = now.saturating_since(first_seen);
    if held_for >= policy.debounce_for(observed) {
        SessionState {
            status: observed,
            since: now,
            rule,
            candidate: None,
        }
    } else {
        SessionState {
            status: state.status,
            since: state.since,
            rule: state.rule.clone(),
            candidate: Some(Candidate {
                status: observed,
                first_seen,
                rule,
            }),
        }
    }
}

/// Combine the screen and the hook into a single claim about the status.
///
/// Both are evidence about the same question and they can disagree -- a hook
/// says the turn is still running while the screen shows a permission prompt,
/// which is not a contradiction at all but two true statements about different
/// things. The more urgent one wins; see [`Status::urgency`].
fn arbitrate(screen: &ScreenMatch, hook: Option<HookSignal>) -> Option<(Status, Option<String>)> {
    let from_hook = hook.and_then(HookSignal::implied_status);

    match (screen.status, from_hook) {
        (None, None) => None,
        (Some(status), None) => Some((status, screen.rule.clone())),
        (None, Some(status)) => Some((status, None)),
        (Some(from_screen), Some(from_hook)) => {
            let winner = from_screen.most_urgent(from_hook);
            // Only attribute a rule if the screen actually won; otherwise the
            // rule name would point at a rule that lost the argument.
            let rule = (winner == from_screen)
                .then(|| screen.rule.clone())
                .flatten();
            Some((winner, rule))
        }
    }
}
