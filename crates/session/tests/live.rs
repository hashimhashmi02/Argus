//! End-to-end tests against a real ConPTY child.
//!
//! Unlike the layers below, this crate cannot be tested from byte fixtures: its
//! whole job is the composition, and the interesting failures are in the
//! sequencing of real events — a handshake that has not finished, a child that
//! has exited but whose console is still open. So these spawn an actual
//! `cmd.exe` and drive it.
//!
//! `cmd.exe` rather than PowerShell because it starts in milliseconds and its
//! prompt is trivially matchable. The manifest is built here with an absolute
//! program path rather than loaded from `manifests/`, so the tests do not depend
//! on how `PATH` happens to be formatted by whichever shell invoked `cargo`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argus_manifest::Agent;
use argus_reducer::{HookSignal, Status};
use argus_session::{Session, SessionConfig, SessionHandle, SessionSnapshot, StreamItem, Watch};
use argus_terminal_emu::TerminalEmulator;
use tokio::sync::broadcast::error::RecvError;

/// Generous: CI machines are slow and a false failure here is worse than a slow
/// test.
const LIMIT: Duration = Duration::from_secs(20);

fn cmd_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    PathBuf::from(root).join("System32").join("cmd.exe")
}

/// A manifest for `cmd.exe` whose only rule recognises its prompt as idle.
fn shell_agent() -> Arc<Agent> {
    // JSON needs each regex backslash doubled, so a literal `\` in the pattern
    // is `\\\\` here.
    let json = format!(
        r#"{{
          "id": "test-shell",
          "display_name": "Test Shell",
          "launch": {{ "program": {program} }},
          "rules": [
            {{
              "name": "cmd-prompt",
              "status": "idle",
              "pattern": "(?m)^[A-Za-z]:\\\\[^\\n]*>$",
              "region": {{ "last_lines": 3 }}
            }}
          ]
        }}"#,
        program = serde_json::to_string(&cmd_exe()).unwrap(),
    );

    Arc::new(Agent::from_json(&json, "test-shell.json").expect("test manifest should be valid"))
}

/// Wait until `predicate` holds, or fail with what the screen looked like.
async fn wait_for(
    handle: &mut SessionHandle,
    what: &str,
    mut predicate: impl FnMut(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let wait = async {
        loop {
            let snapshot = handle.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            if !handle.changed().await {
                panic!(
                    "session ended while waiting for {what}\n--- last screen ---\n{}",
                    handle.snapshot().screen.text()
                );
            }
        }
    };

    match tokio::time::timeout(LIMIT, wait).await {
        Ok(snapshot) => snapshot,
        Err(_) => panic!(
            "timed out waiting for {what}\n--- screen ---\n{}",
            handle.snapshot().screen.text()
        ),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_session_reaches_idle_runs_a_command_and_exits() {
    // The whole pipeline in one test: ConPTY bytes become an emulated screen,
    // the manifest classifies it, the reducer debounces it into a status, input
    // goes back the other way, and the exit is sequenced correctly.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    let idle = wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;
    assert_eq!(idle.rule.as_deref(), Some("cmd-prompt"));
    assert!(idle.pid.is_some(), "a live child should report a pid");

    assert!(handle.send_text("echo hello-from-session\r").await);
    wait_for(&mut handle, "the command output", |s| {
        s.screen.text().contains("hello-from-session")
    })
    .await;

    assert!(handle.send_text("exit\r").await);
    let done = wait_for(&mut handle, "the child to exit", |s| {
        s.status == Status::Done
    })
    .await;
    assert_eq!(done.exit_code, Some(0));

    // Proves the Windows shutdown ordering: the task noticed the exit, closed
    // the pseudo-console so the reader saw EOF, drained what was left, and
    // finished. If any of that were wrong this would hang rather than fail.
    tokio::time::timeout(LIMIT, session.wait())
        .await
        .expect("session should finish once the child is gone")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn input_sent_before_the_child_is_ready_is_not_lost() {
    // The ConPTY handshake hazard from step 1, now handled by the session rather
    // than by every caller. Bytes written before the handshake completes are
    // silently discarded -- no error, they simply never arrive. A human never
    // notices because a human waits for a prompt; a program does not.
    //
    // So this sends immediately, before anything could possibly be ready, and
    // asserts the command still ran.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    assert!(handle.send_text("echo queued-before-ready\r").await);

    wait_for(&mut handle, "the queued command to run", |s| {
        s.screen.text().contains("queued-before-ready")
    })
    .await;

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_is_debounced_rather_than_flipping_on_the_first_quiet_frame() {
    // A session starts Working and must not be called idle the instant the
    // screen stops changing. The default policy holds idle for 750ms; the tick
    // is 100ms, so there are several evaluations in between where the screen
    // already looks idle and the status must not have moved yet.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    // As soon as the prompt is on screen, idle is being debounced but not yet
    // believed.
    wait_for(&mut handle, "the prompt to appear on screen", |s| {
        s.screen.text().contains('>')
    })
    .await;

    let snapshot = handle.snapshot();
    if snapshot.status != Status::Idle {
        assert_eq!(
            snapshot.pending,
            Some(Status::Idle),
            "the prompt is visible, so idle should at least be pending"
        );
    }

    // And it does commit, given time.
    wait_for(&mut handle, "idle to commit", |s| s.status == Status::Idle).await;

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn killing_a_session_finishes_it() {
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;
    assert!(handle.kill().await);

    let done = wait_for(&mut handle, "the kill to take effect", |s| {
        s.status == Status::Done
    })
    .await;
    assert!(done.exit_code.is_some());

    tokio::time::timeout(LIMIT, session.wait())
        .await
        .expect("a killed session should still finish cleanly")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resize_is_applied_to_the_emulated_screen() {
    let session = Session::spawn(shell_agent(), SessionConfig::default().size(80, 24)).unwrap();
    let mut handle = session.handle();

    let idle = wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;
    assert_eq!(idle.screen.size(), (80, 24));

    assert!(handle.resize(100, 30).await);
    wait_for(&mut handle, "the new size", |s| {
        s.screen.size() == (100, 30)
    })
    .await;

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_signal_reaches_the_reducer() {
    // The screen says idle -- the prompt is up. A hook says a turn has started.
    // The two are independent evidence, and activity outranks absence, so the
    // session should move to working on the hook alone.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    assert!(handle.hook(HookSignal::TurnStarted).await);
    let working = wait_for(&mut handle, "the hook to take effect", |s| {
        s.status == Status::Working
    })
    .await;
    // The rule is not attributed to the screen, because the screen did not win.
    assert_eq!(working.rule, None);

    // A subagent finishing is not evidence about the parent at all.
    assert!(handle.hook(HookSignal::SubagentEnded).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_ne!(
        handle.snapshot().pending,
        Some(Status::Done),
        "a subagent exit must not look like the session ending"
    );

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_session_keeps_running() {
    // The daemon/client split as a property of the type. Dropping the `Session`
    // is the in-process equivalent of closing the UI window, and it must not
    // take the child with it.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    drop(session);

    assert!(
        handle.is_alive(),
        "the task should outlive the Session value"
    );
    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_age_measures_time_in_the_current_status_not_since_spawn() {
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();

    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    // Idle is entered some time after spawn, so time-in-status must be strictly
    // less than total age. If they were equal, the two would be measuring from
    // different instants -- or the same wrong one.
    let first = handle.status_age();
    assert!(
        first < handle.age(),
        "status age {first:?} should be under session age"
    );

    // And it keeps counting while nothing changes, even though no snapshot is
    // published in that time. A stored value would be frozen at the last change.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = handle.status_age();
    assert!(
        second >= first + Duration::from_millis(250),
        "status age should advance on its own: {first:?} -> {second:?}"
    );

    assert!(handle.kill().await);
}

/// The next stream item, or a failure naming what was expected.
async fn next_item(
    stream: &mut tokio::sync::broadcast::Receiver<StreamItem>,
    what: &str,
) -> Result<StreamItem, RecvError> {
    tokio::time::timeout(LIMIT, stream.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_rebuilds_the_exact_screen_from_its_repaint_and_stream() {
    // The contract `watch` exists for: a terminal that starts from the repaint
    // and is fed every streamed byte ends up showing exactly what the session
    // shows. Checked against a fresh emulator, which is what a client is.
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();
    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    let Watch {
        repaint,
        mut stream,
    } = handle.watch().await.expect("a live session can be watched");
    let mut copy = TerminalEmulator::new(repaint.cols, repaint.rows);
    let _ = copy.advance(repaint.data.as_bytes());
    assert_eq!(copy.screen(), handle.snapshot().screen, "the repaint alone");

    // `%OS%` expands, so the output differs from the typed command and can only
    // have come from the shell, not from its echo of what was typed.
    assert!(handle.send_text("echo watched-%OS%\r").await);
    while !copy.screen().text().contains("watched-Windows_NT") {
        match next_item(&mut stream, "the command's output").await {
            Ok(StreamItem::Output(text)) => {
                let _ = copy.advance(text.as_bytes());
            }
            other => panic!("expected output, got {other:?}"),
        }
    }

    // Once the shell is quiet again, the copy must have caught up completely.
    wait_for(&mut handle, "the prompt after the command", |s| {
        s.status == Status::Idle && s.screen.text().contains("watched-Windows_NT")
    })
    .await;
    while let Ok(Ok(StreamItem::Output(text))) =
        tokio::time::timeout(Duration::from_millis(300), stream.recv()).await
    {
        let _ = copy.advance(text.as_bytes());
    }
    assert_eq!(copy.screen(), handle.snapshot().screen, "after the stream");

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resize_sends_watchers_a_fresh_repaint() {
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();
    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    let mut stream = handle.watch().await.unwrap().stream;
    assert!(handle.resize(100, 20).await);
    loop {
        match next_item(&mut stream, "the repaint").await {
            Ok(StreamItem::Repaint(repaint)) => {
                assert_eq!((repaint.cols, repaint.rows), (100, 20));
                break;
            }
            // The shell may redraw before or after; either is fine.
            Ok(StreamItem::Output(_)) => continue,
            Err(error) => panic!("stream failed: {error}"),
        }
    }

    assert!(handle.kill().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_session_closes_its_stream_and_cannot_be_watched() {
    let session = Session::spawn(shell_agent(), SessionConfig::default()).unwrap();
    let mut handle = session.handle();
    wait_for(&mut handle, "the first prompt", |s| {
        s.status == Status::Idle
    })
    .await;

    let mut stream = handle.watch().await.unwrap().stream;
    assert!(handle.kill().await);
    loop {
        match next_item(&mut stream, "the stream to close").await {
            Ok(_) => continue,
            Err(RecvError::Closed) => break,
            Err(error) => panic!("unexpected stream error: {error}"),
        }
    }

    tokio::time::timeout(LIMIT, session.wait())
        .await
        .expect("session should finish")
        .unwrap();
    assert!(handle.watch().await.is_none(), "nothing left to watch");
}
