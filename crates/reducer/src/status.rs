//! The status vocabulary, and the two kinds of evidence for it.

/// What a session is doing, as far as Argus can tell.
///
/// Four states, because four is what a person glancing at a list of sessions can
/// act on. The ordering of the variants is not cosmetic -- see
/// [`Status::urgency`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Status {
    /// Nothing is happening and nothing is waiting. A shell at a prompt.
    Idle,
    /// The agent is doing something. Leave it alone.
    Working,
    /// The agent is blocked on the user: a permission prompt, a question, a
    /// choice. This is the only status that is a call to action.
    NeedsYou,
    /// The process is gone.
    Done,
}

impl Status {
    /// How strongly this status should win an argument.
    ///
    /// When the screen and a hook disagree within a single observation, the more
    /// urgent one wins. The reasoning is asymmetric on purpose:
    ///
    /// - `NeedsYou` outranks everything because a blocked session is the only
    ///   status the user must act on. Missing it means an agent sits waiting
    ///   while the user stares at a different window -- the exact failure Argus
    ///   exists to prevent. Showing it wrongly costs a glance.
    /// - `Working` outranks `Idle` because evidence of activity is stronger than
    ///   the absence of it. "I saw nothing happening" is a much weaker claim
    ///   than "I saw something happening".
    ///
    /// `Done` is not part of this ordering in practice: it is decided by process
    /// exit, not by argument. It sits at the top so that a `Done` session cannot
    /// be argued back to life by a stale screen.
    fn urgency(self) -> u8 {
        match self {
            Status::Idle => 0,
            Status::Working => 1,
            Status::NeedsYou => 2,
            Status::Done => 3,
        }
    }

    /// Whichever of the two is more urgent.
    pub fn most_urgent(self, other: Status) -> Status {
        if other.urgency() > self.urgency() {
            other
        } else {
            self
        }
    }
}

/// What the manifest's screen-match rules made of the current screen.
///
/// `status` is `None` when no rule matched, which means "no information" rather
/// than "idle" -- an important distinction, and one the reducer honours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenMatch {
    pub status: Option<Status>,
    /// Name of the rule that matched, carried purely so a surprising status can
    /// be traced back to the line of JSON that caused it.
    pub rule: Option<String>,
}

impl ScreenMatch {
    /// No rule matched.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn new(status: Status, rule: impl Into<String>) -> Self {
        Self {
            status: Some(status),
            rule: Some(rule.into()),
        }
    }
}

/// An out-of-band signal from the agent itself.
///
/// Some agents can tell Argus what they are doing directly -- Claude Code, for
/// instance, can run a hook on turn boundaries. That is far more reliable than
/// inferring it from pixels, so when a hook speaks it is treated as evidence on
/// equal footing with the screen rather than as a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSignal {
    /// The agent began a turn.
    TurnStarted,
    /// The agent finished a turn and is waiting for the user.
    TurnEnded,
    /// A *subagent* finished.
    ///
    /// Deliberately a separate variant from `TurnEnded`, and deliberately not
    /// evidence of anything about the session's status. A parent that spawned
    /// three subagents gets three of these while it is still very much working,
    /// and treating them as "finished" would flash the session to idle three
    /// times mid-task. Modelling it as its own variant means the isolation is
    /// enforced by the type rather than remembered by the next person to touch
    /// the code.
    SubagentEnded,
    /// The process exited.
    Exited,
}

impl HookSignal {
    /// The status this signal implies, if any.
    pub fn implied_status(self) -> Option<Status> {
        match self {
            HookSignal::TurnStarted => Some(Status::Working),
            HookSignal::TurnEnded => Some(Status::Idle),
            HookSignal::SubagentEnded => None,
            HookSignal::Exited => Some(Status::Done),
        }
    }
}
