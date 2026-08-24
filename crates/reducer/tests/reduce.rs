//! Every test here advances time by constructing a number.
//!
//! Nothing sleeps, nothing spawns, nothing is async. The whole file runs in
//! microseconds, which is the payoff for `now` being a parameter rather than
//! something the reducer reads from the clock.

use std::time::Duration;

use argus_reducer::{HookSignal, Policy, ScreenMatch, SessionState, Status, Timestamp, reduce};

/// Shorthand: fold one observation in at time `ms`.
fn step(
    state: &SessionState,
    screen: ScreenMatch,
    hook: Option<HookSignal>,
    ms: u64,
) -> SessionState {
    reduce(
        state,
        &screen,
        hook,
        Timestamp::from_millis(ms),
        &Policy::default(),
    )
}

fn idle_screen() -> ScreenMatch {
    ScreenMatch::new(Status::Idle, "shell-prompt")
}

fn working_screen() -> ScreenMatch {
    ScreenMatch::new(Status::Working, "spinner")
}

fn blocked_screen() -> ScreenMatch {
    ScreenMatch::new(Status::NeedsYou, "permission-prompt")
}

#[test]
fn a_new_session_is_working() {
    // A process launched a moment ago is starting up. Announcing "nothing is
    // happening" for the first frames of every spawn would be wrong and jumpy.
    let state = SessionState::new(Timestamp::ZERO);
    assert_eq!(state.status(), Status::Working);
    assert_eq!(state.pending(), None);
}

#[test]
fn one_idle_frame_does_not_flip_a_working_session() {
    // The core anti-flicker case. Agents go quiet between a tool call and its
    // result; that is not idleness.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);

    assert_eq!(state.status(), Status::Working);
    assert_eq!(state.pending(), Some(Status::Idle));
}

#[test]
fn sustained_idle_commits_once_the_debounce_elapses() {
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);
    let state = step(&state, idle_screen(), None, 500);
    assert_eq!(state.status(), Status::Working, "749ms is not yet enough");

    let state = step(&state, idle_screen(), None, 850);
    assert_eq!(state.status(), Status::Idle);
    assert_eq!(state.pending(), None);
    assert_eq!(state.rule(), Some("shell-prompt"));
}

#[test]
fn the_debounce_runs_from_first_sighting_not_the_latest() {
    // If the timer restarted on every repeat sighting, a status observed
    // steadily would never commit at all -- the bug this test exists to catch.
    let state = SessionState::new(Timestamp::ZERO);
    let mut state = step(&state, idle_screen(), None, 0);

    for ms in [100, 200, 300, 400, 500, 600, 700] {
        state = step(&state, idle_screen(), None, ms);
        assert_eq!(
            state.status(),
            Status::Working,
            "still inside the debounce at {ms}ms"
        );
    }

    state = step(&state, idle_screen(), None, 760);
    assert_eq!(state.status(), Status::Idle);
}

#[test]
fn activity_during_the_debounce_cancels_it() {
    // The flicker went away. This is the actual anti-flicker mechanism: the
    // candidate is discarded, so the next idle frame starts its wait over.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);
    assert_eq!(state.pending(), Some(Status::Idle));

    let state = step(&state, working_screen(), None, 200);
    assert_eq!(state.status(), Status::Working);
    assert_eq!(state.pending(), None);

    // A fresh idle frame much later must still serve its full debounce.
    let state = step(&state, idle_screen(), None, 900);
    assert_eq!(state.status(), Status::Working);
}

#[test]
fn a_blocker_is_not_debounced() {
    // Latency is the expensive direction here: an agent waiting on a permission
    // prompt is the one thing the user has to act on.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, blocked_screen(), None, 10);

    assert_eq!(state.status(), Status::NeedsYou);
    assert_eq!(state.rule(), Some("permission-prompt"));
}

#[test]
fn a_blocker_on_screen_beats_a_hook_saying_working() {
    // Not a contradiction -- both are true. The turn really is still running,
    // and it is running a prompt that is waiting for the user. The more urgent
    // reading wins.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, blocked_screen(), Some(HookSignal::TurnStarted), 10);

    assert_eq!(state.status(), Status::NeedsYou);
}

#[test]
fn a_blocker_on_screen_beats_a_hook_saying_the_turn_ended() {
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, blocked_screen(), Some(HookSignal::TurnEnded), 10);

    assert_eq!(state.status(), Status::NeedsYou);
}

#[test]
fn working_beats_idle_because_evidence_beats_absence() {
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);
    let state = step(&state, idle_screen(), None, 900);
    assert_eq!(state.status(), Status::Idle);

    // Screen sees nothing conclusive, hook says a turn began.
    let state = step(
        &state,
        ScreenMatch::none(),
        Some(HookSignal::TurnStarted),
        950,
    );
    assert_eq!(state.status(), Status::Working);
}

#[test]
fn a_subagent_finishing_does_not_move_the_parent() {
    // A parent that spawned three subagents gets three of these while it is
    // still very much working. Treating them as "finished" would flash the
    // session to idle three times mid-task.
    let state = SessionState::new(Timestamp::ZERO);

    let mut state = state;
    for ms in [100, 900, 1_800, 2_700] {
        state = step(
            &state,
            ScreenMatch::none(),
            Some(HookSignal::SubagentEnded),
            ms,
        );
        assert_eq!(
            state.status(),
            Status::Working,
            "subagent exit at {ms}ms must not idle us"
        );
        assert_eq!(state.pending(), None, "and must not even start a debounce");
    }
}

#[test]
fn the_parents_own_turn_ending_does_move_it() {
    // The contrast with the previous test: same shape of signal, different
    // variant, different meaning.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(
        &state,
        ScreenMatch::none(),
        Some(HookSignal::TurnEnded),
        100,
    );
    assert_eq!(
        state.status(),
        Status::Working,
        "still debounced like any idle claim"
    );

    let state = step(
        &state,
        ScreenMatch::none(),
        Some(HookSignal::TurnEnded),
        900,
    );
    assert_eq!(state.status(), Status::Idle);
}

#[test]
fn exit_is_immediate() {
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, ScreenMatch::none(), Some(HookSignal::Exited), 5);

    assert_eq!(state.status(), Status::Done);
}

#[test]
fn done_is_absorbing() {
    // The dead session's final screen is still sitting in the emulator's grid,
    // still matching whatever rule it matched a moment before the exit. Without
    // the absorbing guard, a finished agent would flip back to working forever.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, ScreenMatch::none(), Some(HookSignal::Exited), 5);
    assert_eq!(state.status(), Status::Done);

    let state = step(&state, working_screen(), Some(HookSignal::TurnStarted), 10);
    assert_eq!(state.status(), Status::Done);

    let state = step(&state, blocked_screen(), None, 5_000);
    assert_eq!(state.status(), Status::Done);
}

#[test]
fn no_evidence_changes_nothing_and_preserves_a_running_debounce() {
    // Absence of evidence is not evidence of absence. A frame where no rule
    // matched must not reset a timer, or a status alternating between "idle" and
    // "nothing matched" would never commit.
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);
    assert_eq!(state.pending(), Some(Status::Idle));

    let state = step(&state, ScreenMatch::none(), None, 400);
    assert_eq!(state.status(), Status::Working);
    assert_eq!(
        state.pending(),
        Some(Status::Idle),
        "the debounce is still running"
    );

    // Committing at 900ms proves the timer still measures from 100ms.
    let state = step(&state, idle_screen(), None, 900);
    assert_eq!(state.status(), Status::Idle);
}

#[test]
fn since_marks_when_the_status_was_entered() {
    let state = SessionState::new(Timestamp::ZERO);
    let state = step(&state, idle_screen(), None, 100);
    let state = step(&state, idle_screen(), None, 900);

    assert_eq!(state.status(), Status::Idle);
    assert_eq!(state.since(), Timestamp::from_millis(900));
}

#[test]
fn a_custom_policy_is_honoured() {
    // A manifest may want a slower or faster hand for a particular agent.
    let policy = Policy {
        idle_debounce: Duration::from_millis(100),
        ..Policy::default()
    };
    let state = SessionState::new(Timestamp::ZERO);

    let state = reduce(
        &state,
        &idle_screen(),
        None,
        Timestamp::from_millis(50),
        &policy,
    );
    assert_eq!(state.status(), Status::Working);

    let state = reduce(
        &state,
        &idle_screen(),
        None,
        Timestamp::from_millis(160),
        &policy,
    );
    assert_eq!(state.status(), Status::Idle);
}

#[test]
fn a_backwards_timestamp_does_not_panic() {
    // Two threads can disagree about the order of two events by a microsecond.
    // A status reducer is not worth crashing the daemon over.
    let state = SessionState::new(Timestamp::from_millis(1_000));
    let state = step(&state, idle_screen(), None, 10);

    assert_eq!(state.status(), Status::Working);
}
