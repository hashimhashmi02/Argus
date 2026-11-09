//! The engine, driven only through its pipe.
//!
//! Everything here goes through `Client`, exactly as the CLI in step 6 and the
//! UI after it will. If a behaviour is not reachable this way, no user can reach
//! it either.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Capability, Client, ClientError, Server, Watcher, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{ErrorCode, Reply, Request, ScreenView, SessionInfo, Status, WatchEvent};
use argus_terminal_emu::TerminalEmulator;
use tokio::sync::oneshot;

const LIMIT: Duration = Duration::from_secs(20);

/// A shell that exists here, the arguments that make it print a prompt, and
/// the prompt to look for.
///
/// These tests need idle to be detectable, which means a shell that shows
/// something recognisable while it waits. `cmd.exe` always does. `sh` only
/// does when it is interactive, hence `-i`; its prompt is `$ ` as a user and
/// `# ` as root, and macOS answers `sh-3.2$ `, so the pattern matches the
/// ending rather than the whole line.
/// Keys to type at a session that is no longer running. Nothing runs them --
/// the point is the refusal -- so they need not name a command.
const SOME_KEYS: &str = "anything\r";

/// A directory that cannot be there, so spawning in it has to fail.
///
/// Spelt for the platform: a Unix kernel would take the Windows form as one
/// very oddly named entry in the current directory, which fails for the right
/// reason by accident rather than because the path is absolute and absent.
#[cfg(windows)]
fn a_directory_that_is_not_there() -> &'static str {
    r"C:\definitely\not\a\real\directory"
}

#[cfg(unix)]
fn a_directory_that_is_not_there() -> &'static str {
    "/definitely/not/a/real/directory"
}

/// Whether `line` ends with a shell prompt.
///
/// `cmd.exe` ends its prompt with `>`, `sh` with `$` as a user and `#` as
/// root. Testing the last character keeps this clear of the rest of the
/// prompt, which carries the working directory on Windows, the shell's own
/// name on macOS, and nothing at all on Linux.
fn ends_with_a_prompt(line: &str) -> bool {
    #[cfg(windows)]
    {
        line.ends_with('>')
    }
    #[cfg(unix)]
    {
        line.ends_with('$') || line.ends_with('#')
    }
}

/// Keys that make the shell print something, and the text it should print.
///
/// The shell has to expand the expression, so what the test looks for never
/// appears in the typed line itself -- otherwise the echo of the keys alone
/// would satisfy it, and the test would pass without the shell running
/// anything. `%OS%` is cmd's; arithmetic expansion is POSIX sh's.
#[cfg(windows)]
fn echo_an_expansion() -> (&'static str, &'static str) {
    ("echo watched-%OS%\r", "watched-Windows_NT")
}

#[cfg(unix)]
fn echo_an_expansion() -> (&'static str, &'static str) {
    ("echo watched-$((6 * 7))\r", "watched-42")
}

#[cfg(windows)]
fn test_shell() -> (PathBuf, Vec<String>, &'static str) {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    (
        PathBuf::from(root).join("System32").join("cmd.exe"),
        Vec::new(),
        r"(?m)^[A-Za-z]:\\[^\n]*>$",
    )
}

#[cfg(unix)]
fn test_shell() -> (PathBuf, Vec<String>, &'static str) {
    (
        PathBuf::from("/bin/sh"),
        vec!["-i".to_string()],
        r"(?m)[$#] ?$",
    )
}

/// `cmd.exe` by absolute path, so the test does not depend on how `PATH` is
/// formatted by whichever shell ran `cargo`.
fn shell_agent() -> Agent {
    let shell = test_shell();
    let json = format!(
        r#"{{
          "id": "test-shell",
          "display_name": "Test Shell",
          "launch": {{ "program": {program}, "args": {args} }},
          "keys": {{ "approve": "enter", "deny": "ctrl-c" }},
          "rules": [
            {{
              "name": "cmd-prompt",
              "status": "idle",
              "pattern": {pattern},
              "region": {{ "last_lines": 3 }}
            }}
          ]
        }}"#,
        program = serde_json::to_string(&shell.0).unwrap(),
        args = serde_json::to_string(&shell.1).unwrap(),
        pattern = serde_json::to_string(shell.2).unwrap(),
    );
    Agent::from_json(&json, "test-shell.json").unwrap()
}

/// A running engine on a private pipe, stopped when the guard drops.
struct Running {
    pipe: String,
    /// Kept so a test can put a second, restricted endpoint on the same engine.
    engine: Arc<Engine>,
    _stop: oneshot::Sender<()>,
    /// One per endpoint made by [`Running::invited`], so they stop with it.
    _invited: Vec<oneshot::Sender<()>>,
}

impl Running {
    /// A second endpoint onto the same engine, permitted to watch `id` and
    /// nothing else.
    ///
    /// That is exactly what redeeming an invite grants, so a watch arriving
    /// here is a remote one as far as the engine is concerned -- without the
    /// test having to stand up QUIC, which is a transport question and is
    /// tested as one in `argus-control-server`.
    fn invited(&mut self, id: &str) -> String {
        let pipe = unique_pipe_name("invited");
        let server = Server::bind(&pipe)
            .unwrap()
            .with_capability(Capability::WatchSession { id: id.to_string() });
        let (stop, stopped) = oneshot::channel::<()>();
        let engine = Arc::clone(&self.engine);
        tokio::spawn(async move {
            let _ = server
                .run(engine, async {
                    let _ = stopped.await;
                })
                .await;
        });
        self._invited.push(stop);
        pipe
    }
}

fn start(label: &str) -> Running {
    let pipe = unique_pipe_name(label);
    let engine = Arc::new(Engine::new([shell_agent()]).unwrap());
    let server = Server::bind(&pipe).unwrap();
    let (stop, stopped) = oneshot::channel::<()>();

    let serving = Arc::clone(&engine);
    tokio::spawn(async move {
        let _ = server
            .run(serving, async {
                let _ = stopped.await;
            })
            .await;
    });

    Running {
        pipe,
        engine,
        _stop: stop,
        _invited: Vec::new(),
    }
}

async fn spawn_shell(client: &mut Client) -> SessionInfo {
    match client
        .call(Request::Spawn {
            agent: "test-shell".into(),
            cwd: None,
            cols: None,
            rows: None,
            worktree: None,
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
        worktree: None,
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
        cwd: Some(a_directory_that_is_not_there().into()),
        cols: None,
        rows: None,
        worktree: None,
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
                text: SOME_KEYS.into(),
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

    // Ctrl+C at a prompt abandons the typed line and draws a fresh, bare
    // prompt. Waiting for that bare prompt is what proves a key arrived at all:
    // "the command never ran" alone would also be true if deny sent nothing.
    let view = poll_until(&mut client, &id, "a fresh prompt after Ctrl+C", |v| {
        v.lines
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|last| ends_with_a_prompt(last))
    })
    .await;
    assert!(
        !view.lines.iter().any(|line| line == "denied-path"),
        "deny should have cancelled the command, not run it:\n{}",
        view.lines.join("\n")
    );

    client.call(Request::Kill { id }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_says_how_many_people_are_watching_it_from_elsewhere() {
    let mut engine = start("watch-count");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let session = spawn_shell(&mut client).await;
    poll_until(&mut client, &session.id, "the prompt", |v| {
        v.session.status == Status::Idle
    })
    .await;

    // The window watches the session it is showing. That must not count: the
    // number answers "is anyone *else* looking at this", and one that was
    // always at least one would answer nothing.
    let mut mine = Client::connect(&engine.pipe)
        .await
        .unwrap()
        .watch(&session.id)
        .await
        .unwrap();
    next_event(&mut mine, "my own repaint").await;
    assert_eq!(
        screen(&mut client, &session.id)
            .await
            .session
            .remote_watchers,
        0,
        "watching from this machine is not someone else watching"
    );

    let invited = engine.invited(&session.id);
    let mut theirs = Client::connect(&invited)
        .await
        .unwrap()
        .watch(&session.id)
        .await
        .unwrap();
    next_event(&mut theirs, "their repaint").await;
    poll_until(
        &mut client,
        &session.id,
        "one watcher from elsewhere",
        |v| v.session.remote_watchers == 1,
    )
    .await;

    // An invite is for one person, but the engine counts rather than latches,
    // so two of them read as two.
    let mut also = Client::connect(&invited)
        .await
        .unwrap()
        .watch(&session.id)
        .await
        .unwrap();
    next_event(&mut also, "the second repaint").await;
    poll_until(&mut client, &session.id, "two watchers", |v| {
        v.session.remote_watchers == 2
    })
    .await;

    // And it comes back down when they go, which is the half that a counter
    // incremented in the easy place gets wrong.
    drop(theirs);
    drop(also);
    poll_until(&mut client, &session.id, "nobody watching", |v| {
        v.session.remote_watchers == 0
    })
    .await;

    client.call(Request::Kill { id: session.id }).await.unwrap();
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
    let (keys, printed) = echo_an_expansion();
    assert!(
        terminal
            .screen()
            .text()
            .lines()
            .any(|line| ends_with_a_prompt(line.trim_end())),
        "the prompt is drawn:\n{}",
        terminal.screen().text()
    );

    client
        .call(Request::SendText {
            id: session.id.clone(),
            text: keys.into(),
        })
        .await
        .unwrap();
    while !terminal.screen().text().contains(printed) {
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

#[tokio::test]
async fn an_invite_is_refused_when_remote_watching_is_off() {
    // Off is the default. Asking for an invite then has to say so plainly,
    // rather than failing somewhere further in with a confusing reason.
    let engine = start("invite-off");
    let mut client = Client::connect(&engine.pipe).await.unwrap();
    let session = spawn_shell(&mut client).await;

    // The message has to name the flag that turns it on, so whoever hit this
    // learns what to do rather than only that it did not work.
    let message = match client.call(Request::Invite { id: session.id }).await {
        Err(ClientError::Rpc(error)) => {
            assert_eq!(error.code, ErrorCode::BadRequest);
            error.message
        }
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(message.contains("--quic"), "{message}");
}
