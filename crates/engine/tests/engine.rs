//! The engine, driven only through its pipe.
//!
//! Everything here goes through `Client`, exactly as the CLI in step 6 and the
//! UI after it will. If a behaviour is not reachable this way, no user can reach
//! it either.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Client, ClientError, Server, Watcher, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{ErrorCode, Reply, Request, ScreenView, SessionInfo, Status, WatchEvent};
use argus_terminal_emu::TerminalEmulator;
use tokio::sync::oneshot;

const LIMIT: Duration = Duration::from_secs(20);

fn cmd_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    PathBuf::from(root).join("System32").join("cmd.exe")
}

/// `cmd.exe` by absolute path, so the test does not depend on how `PATH` is
/// formatted by whichever shell ran `cargo`.
fn shell_agent() -> Agent {
    let json = format!(
        r#"{{
          "id": "test-shell",
          "display_name": "Test Shell",
          "launch": {{ "program": {program} }},
          "keys": {{ "approve": "enter", "deny": "ctrl-c" }},
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
    Agent::from_json(&json, "test-shell.json").unwrap()
}

/// A running engine on a private pipe, stopped when the guard drops.
struct Running {
    pipe: String,
    _stop: oneshot::Sender<()>,
}

fn start(label: &str) -> Running {
    let pipe = unique_pipe_name(label);
    let engine = Arc::new(Engine::new([shell_agent()]).unwrap());
    let server = Server::bind(&pipe).unwrap();
    let (stop, stopped) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let _ = server
            .run(engine, async {
                let _ = stopped.await;
            })
            .await;
    });

    Running { pipe, _stop: stop }
}

async fn spawn_shell(client: &mut Client) -> SessionInfo {
    match client
        .call(Request::Spawn {
            agent: "test-shell".into(),
            cwd: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
    {
        Reply::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    }
}

async fn screen(client: &mut Client, id: &str) -> ScreenView {
    match client
        .call(Request::ReadScreen { id: id.into() })
        .await
        .unwrap()
    {
        Reply::Screen { screen } => screen,
        other => panic!("expected a screen, got {other:?}"),
    }
}

/// Poll the screen until `predicate` holds. Polling, not subscribing, because
/// that is all the protocol offers -- and all a CLI would do.
async fn poll_until(
    client: &mut Client,
    id: &str,
    what: &str,
    mut predicate: impl FnMut(&ScreenView) -> bool,
) -> ScreenView {
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        let view = screen(client, id).await;
        if predicate(&view) {
            return view;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}\n--- screen ---\n{}",
                view.lines.join("\n")
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_type_read_and_kill_entirely_over_the_pipe() {
    let engine = start("lifecycle");
    let mut client = Client::connect(&engine.pipe).await.unwrap();

    // Nothing yet.
    match client.call(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => assert!(sessions.is_empty()),
        other => panic!("unexpected {other:?}"),
    }

    let session = spawn_shell(&mut client).await;
    assert_eq!(session.agent, "test-shell");
    assert_eq!(session.id.len(), 8);

    let idle = poll_until(&mut client, &session.id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;
    assert_eq!(idle.session.rule.as_deref(), Some("cmd-prompt"));

    // Input goes in over the pipe; the result comes back as an emulated screen.
    client
        .call(Request::SendText {
            id: session.id.clone(),
            text: "echo via-the-pipe\r".into(),
        })
        .await
        .unwrap();
    poll_until(&mut client, &session.id, "the echoed text", |v| {
        v.lines.iter().any(|line| line == "via-the-pipe")
    })
    .await;

    // A prefix is enough to name it.
    let prefix = &session.id[..4];
    client
        .call(Request::Kill { id: prefix.into() })
        .await
        .unwrap();

    let done = poll_until(&mut client, &session.id, "the session to end", |v| {
        v.session.status == Status::Done
    })
    .await;
    assert!(done.session.exit_code.is_some());

    // Still listed after it ended.
    match client.call(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].status, Status::Done);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_outlive_the_client_that_spawned_them() {
    // The daemon/client split, end to end. A client spawns a session and goes
    // away -- the in-process stand-in for closing the UI. A different client
    // comes along later and finds the session still running.
    let engine = start("outlive");

    let id = {
        let mut first = Client::connect(&engine.pipe).await.unwrap();
        spawn_shell(&mut first).await.id
    };

    let mut second = Client::connect(&engine.pipe).await.unwrap();
    let view = poll_until(&mut second, &id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;
    assert!(view.session.pid.is_some());

    second.call(Request::Kill { id }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn several_sessions_run_side_by_side() {
    let engine = start("parallel");
    let mut client = Client::connect(&engine.pipe).await.unwrap();

    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(spawn_shell(&mut client).await.id);
    }

    for (i, id) in ids.iter().enumerate() {
        poll_until(&mut client, id, "the prompt", |v| {
            v.session.status == Status::Idle
        })
        .await;
        client
            .call(Request::SendText {
                id: id.clone(),
                text: format!("echo session-{i}\r"),
            })
            .await
            .unwrap();
    }

    // Each session saw only its own input.
    for (i, id) in ids.iter().enumerate() {
        let mine = format!("session-{i}");
        let view = poll_until(&mut client, id, "its own output", |v| {
            v.lines.iter().any(|line| line == &mine)
        })
        .await;
        for j in (0..ids.len()).filter(|&j| j != i) {
            let theirs = format!("session-{j}");
            assert!(
                !view.lines.iter().any(|line| line == &theirs),
                "session {i} shows output meant for session {j}"
            );
        }
    }

    for id in ids {
        client.call(Request::Kill { id }).await.unwrap();
    }
}

async fn expect_error(client: &mut Client, request: Request) -> ErrorCode {
    match client.call(request).await {
        Err(ClientError::Rpc(error)) => error.code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_are_specific_enough_to_act_on() {
    let engine = start("errors");
    let mut client = Client::connect(&engine.pipe).await.unwrap();

    let unknown_agent = Request::Spawn {
        agent: "nope".into(),
        cwd: None,
        cols: None,
        rows: None,
    };
    assert_eq!(
        expect_error(&mut client, unknown_agent).await,
        ErrorCode::UnknownAgent
    );

    let read_missing = Request::ReadScreen {
        id: "deadbeef".into(),
    };
    assert_eq!(
        expect_error(&mut client, read_missing).await,
        ErrorCode::UnknownSession
    );

    let bad_cwd = Request::Spawn {
        agent: "test-shell".into(),
        cwd: Some(r"C:\definitely\not\a\real\directory".into()),
        cols: None,
        rows: None,
    };
    assert_eq!(
        expect_error(&mut client, bad_cwd).await,
        ErrorCode::BadRequest
    );

    // The connection survives every one of those.
    assert!(matches!(
        client.call(Request::List).await,
        Ok(Reply::Sessions { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_into_a_finished_session_is_reported_but_killing_it_again_is_fine() {
    let engine = start("finished");
    let mut client = Client::connect(&engine.pipe).await.unwrap();

    let id = spawn_shell(&mut client).await.id;
    poll_until(&mut client, &id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;
    client
        .call(Request::SendText {
            id: id.clone(),
            text: "exit\r".into(),
        })
        .await
        .unwrap();
    poll_until(&mut client, &id, "the exit", |v| {
        v.session.status == Status::Done
    })
    .await;

    // The session task finishes shortly after Done; give it a moment so the
    // command channel is genuinely closed rather than racing the drain.
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        match client
            .call(Request::SendText {
                id: id.clone(),
                text: "dir\r".into(),
            })
            .await
        {
            Err(ClientError::Rpc(e)) if e.code == ErrorCode::SessionFinished => break,
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => panic!("expected SessionFinished, got {other:?}"),
        }
    }

    // Kill is idempotent: the caller wanted it stopped, and it is.
    assert!(matches!(
        client.call(Request::Kill { id }).await,
        Ok(Reply::Done)
    ));
}

#[test]
fn duplicate_agent_ids_are_refused_at_startup() {
    // Otherwise `spawn` would silently launch whichever was loaded last.
    assert!(Engine::new([shell_agent(), shell_agent()]).is_err());
}

#[test]
fn the_shipped_manifests_load_into_an_engine() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../manifests");
    let engine = Engine::load_manifest_dir(&dir).unwrap();
    let ids: Vec<&str> = engine.agent_ids().collect();
    assert!(ids.contains(&"claude-code"), "got {ids:?}");
    assert!(ids.contains(&"shell"), "got {ids:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_lists_what_can_be_spawned() {
    let engine = start("agents");
    let mut client = Client::connect(&engine.pipe).await.unwrap();

    match client.call(Request::Agents).await.unwrap() {
        Reply::Agents { agents } => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, "test-shell");
            assert_eq!(agents[0].display_name, "Test Shell");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn respond_sends_the_keys_from_the_agents_own_manifest() {
    // The test manifest maps approve to Enter and deny to Ctrl+C. Type a command
    // without pressing Enter, then let `respond` decide its fate: approving must
    // run it, denying must cancel it. Neither key appears anywhere in the
    // request -- the engine looked them up.
    let engine = start("respond");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let id = spawn_shell(&mut client).await.id;
    poll_until(&mut client, &id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;

    client
        .call(Request::SendText {
            id: id.clone(),
            text: "echo approved-path".into(),
        })
        .await
        .unwrap();
    client
        .call(Request::Respond {
            id: id.clone(),
            approve: true,
        })
        .await
        .unwrap();
    poll_until(&mut client, &id, "the approved command to run", |v| {
        v.lines.iter().any(|line| line == "approved-path")
    })
    .await;

    client
        .call(Request::SendText {
            id: id.clone(),
            text: "echo denied-path".into(),
        })
        .await
        .unwrap();
    poll_until(&mut client, &id, "the typed text", |v| {
        v.lines
            .iter()
            .any(|line| line.ends_with("echo denied-path"))
    })
    .await;
    client
        .call(Request::Respond {
            id: id.clone(),
            approve: false,
        })
        .await
        .unwrap();

    // Ctrl+C at a cmd prompt abandons the typed line and draws a fresh, bare
    // prompt. Waiting for that bare prompt is what proves a key arrived at all:
    // "the command never ran" alone would also be true if deny sent nothing.
    let view = poll_until(&mut client, &id, "a fresh prompt after Ctrl+C", |v| {
        v.lines
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|last| last.ends_with('>'))
    })
    .await;
    assert!(
        !view.lines.iter().any(|line| line == "denied-path"),
        "deny should have cancelled the command, not run it:\n{}",
        view.lines.join("\n")
    );

    client.call(Request::Kill { id }).await.unwrap();
}

/// The next watch event, failing the test rather than hanging.
async fn next_event(watcher: &mut Watcher, what: &str) -> Option<WatchEvent> {
    tokio::time::timeout(LIMIT, watcher.next())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_sees_the_terminal_and_typing_from_another_connection() {
    // The shape the UI uses: one connection watching, others typing.
    let engine = start("watch-live");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let session = spawn_shell(&mut client).await;
    poll_until(&mut client, &session.id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;

    let mut watcher = Client::connect(&engine.pipe)
        .await
        .unwrap()
        .watch(&session.id)
        .await
        .unwrap();
    let mut terminal = match next_event(&mut watcher, "the repaint").await {
        Some(WatchEvent::Repaint { cols, rows, data }) => {
            let mut terminal = TerminalEmulator::new(cols, rows);
            let _ = terminal.advance(data.as_bytes());
            terminal
        }
        other => panic!("a watch must start with a repaint, got {other:?}"),
    };
    assert!(
        terminal.screen().text().contains('>'),
        "the prompt is drawn"
    );

    client
        .call(Request::SendText {
            id: session.id.clone(),
            text: "echo watched-%OS%\r".into(),
        })
        .await
        .unwrap();
    while !terminal.screen().text().contains("watched-Windows_NT") {
        match next_event(&mut watcher, "the command's output").await {
            Some(WatchEvent::Output { data }) => {
                let _ = terminal.advance(data.as_bytes());
            }
            other => panic!("expected output, got {other:?}"),
        }
    }

    client
        .call(Request::Kill {
            id: session.id.clone(),
        })
        .await
        .unwrap();
    loop {
        match next_event(&mut watcher, "the end of the stream").await {
            Some(WatchEvent::Output { .. } | WatchEvent::Repaint { .. }) => continue,
            Some(WatchEvent::Ended) => break,
            None => panic!("the stream closed without saying the session ended"),
        }
    }
    assert_eq!(next_event(&mut watcher, "the hang-up").await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_a_finished_session_shows_its_last_screen_and_ends() {
    let engine = start("watch-finished");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let session = spawn_shell(&mut client).await;
    poll_until(&mut client, &session.id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;
    client
        .call(Request::SendText {
            id: session.id.clone(),
            text: "echo last-words&exit\r".into(),
        })
        .await
        .unwrap();
    poll_until(&mut client, &session.id, "the session to end", |v| {
        v.session.status == Status::Done
    })
    .await;

    let mut watcher = Client::connect(&engine.pipe)
        .await
        .unwrap()
        .watch(&session.id)
        .await
        .unwrap();
    match next_event(&mut watcher, "the last screen").await {
        Some(WatchEvent::Repaint { data, .. }) => assert!(data.contains("last-words")),
        other => panic!("expected the last screen, got {other:?}"),
    }
    assert_eq!(
        next_event(&mut watcher, "the end").await,
        Some(WatchEvent::Ended)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_an_unknown_session_is_an_unknown_session_error() {
    let engine = start("watch-missing");
    let error = Client::connect(&engine.pipe)
        .await
        .unwrap()
        .watch("deadbeef")
        .await
        .err()
        .expect("nothing to watch");
    match error {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, ErrorCode::UnknownSession),
        other => panic!("expected an rpc error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn respond_to_an_unknown_session_is_an_unknown_session_error() {
    let engine = start("respond-missing");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let request = Request::Respond {
        id: "deadbeef".into(),
        approve: true,
    };
    assert_eq!(
        expect_error(&mut client, request).await,
        ErrorCode::UnknownSession
    );
}
