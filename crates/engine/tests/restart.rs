//! Restarts, with the real `argus-engine` binary.
//!
//! These run the actual executable as a child process, hard-kill it the way a
//! crash or a closed terminal would, and start it again against the same state
//! file. An in-process test could not show that: dropping an `Engine` value is
//! not what happens when a process dies.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use argus_control_server::{Client, ClientError, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{Reply, Request, SessionInfo, Status};
use serde_json::Value;

const LIMIT: Duration = Duration::from_secs(20);

fn cmd_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    PathBuf::from(root).join("System32").join("cmd.exe")
}

fn manifest_json() -> String {
    format!(
        r#"{{
          "id": "test-shell",
          "display_name": "Test Shell",
          "launch": {{ "program": {program} }},
          "rules": [
            {{
              "name": "cmd-prompt",
              "status": "idle",
              "pattern": "(?m)^[A-Za-z]:\\\\[^\\n]*>$",
              "region": {{ "last_lines": 1 }}
            }}
          ]
        }}"#,
        program = serde_json::to_string(&cmd_exe()).unwrap(),
    )
}

/// A `cmd.exe` that can be resumed, and says on screen which way it started
/// and with which conversation id.
fn resumable_manifest_json() -> String {
    format!(
        r#"{{
          "id": "test-resumable",
          "display_name": "Test Resumable",
          "launch": {{
            "program": {program},
            "new_session_args": ["/k", "echo", "new", "{{session_uuid}}"]
          }},
          "resume": {{ "args": ["/k", "echo", "resumed", "{{session_uuid}}"] }},
          "rules": [
            {{
              "name": "cmd-prompt",
              "status": "idle",
              "pattern": "(?m)^[A-Za-z]:\\\\[^\\n]*>$",
              "region": {{ "last_lines": 1 }}
            }}
          ]
        }}"#,
        program = serde_json::to_string(&cmd_exe()).unwrap(),
    )
}

/// A scratch directory holding a manifest directory and a state file path.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "argus-restart-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("manifests")).unwrap();
        fs::write(
            dir.join("manifests").join("test-shell.json"),
            manifest_json(),
        )
        .unwrap();
        fs::write(
            dir.join("manifests").join("test-resumable.json"),
            resumable_manifest_json(),
        )
        .unwrap();
        Self { dir }
    }

    fn manifests(&self) -> PathBuf {
        self.dir.join("manifests")
    }

    fn state(&self) -> PathBuf {
        self.dir.join("state").join("state.json")
    }
}

/// A running `argus-engine` child process, killed if the test fails early.
struct EngineProcess {
    child: Child,
    pipe: String,
}

impl EngineProcess {
    fn start(scratch: &Scratch, pipe: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_argus-engine"))
            .arg("--manifests")
            .arg(scratch.manifests())
            .arg("--pipe")
            .arg(pipe)
            .arg("--state")
            .arg(scratch.state())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("argus-engine should start");
        Self {
            child,
            pipe: pipe.to_string(),
        }
    }

    /// Wait until the pipe accepts connections.
    async fn connect(&mut self) -> Client {
        let deadline = tokio::time::Instant::now() + LIMIT;
        loop {
            match Client::connect(&self.pipe).await {
                Ok(client) => return client,
                Err(ClientError::NotRunning(_)) if tokio::time::Instant::now() < deadline => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        panic!("engine exited early ({status}):\n{}", self.stderr());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("could not connect to the engine: {e}"),
            }
        }
    }

    /// Kill the process the way a crash would: no chance to clean up.
    fn crash(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Everything the engine wrote to stderr. Only meaningful once it exited.
    fn stderr(&mut self) -> String {
        let mut text = String::new();
        if let Some(stderr) = self.child.stderr.as_mut() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn list(client: &mut Client) -> Vec<SessionInfo> {
    match client.call(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => sessions,
        other => panic!("unexpected reply {other:?}"),
    }
}

async fn spawn(client: &mut Client) -> String {
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
        Reply::Session { session } => session.id,
        other => panic!("unexpected reply {other:?}"),
    }
}

async fn wait_status(client: &mut Client, id: &str, want: Status) -> SessionInfo {
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        if let Some(info) = list(client).await.into_iter().find(|s| s.id == id)
            && info.status == want
        {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{id} never reached {want:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll the state file until `predicate` holds for its parsed contents.
async fn wait_file(path: &Path, what: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<Value>(&text)
            && predicate(&value)
        {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "state file never showed {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn session_in_file<'a>(state: &'a Value, id: &str) -> Option<&'a Value> {
    state["sessions"].as_array()?.iter().find(|s| s["id"] == id)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_list_survives_a_crash() {
    let scratch = Scratch::new("crash");
    let pipe = unique_pipe_name("restart-crash");

    // ---- first life ---------------------------------------------------------
    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;

    // One session that finishes cleanly before the crash...
    let finished = spawn(&mut client).await;
    wait_status(&mut client, &finished, Status::Idle).await;
    client
        .call(Request::SendText {
            id: finished.clone(),
            text: "exit\r".into(),
        })
        .await
        .unwrap();
    wait_status(&mut client, &finished, Status::Done).await;

    // ...and one still running when it happens.
    let running = spawn(&mut client).await;
    let running_pid = wait_status(&mut client, &running, Status::Idle)
        .await
        .pid
        .unwrap();

    // Wait until the persister has written both as they are now.
    wait_file(&scratch.state(), "both sessions", |state| {
        session_in_file(state, &finished).is_some_and(|s| s["status"] == "done")
            && session_in_file(state, &running).is_some_and(|s| s["status"] == "idle")
    })
    .await;

    drop(client);
    engine.crash();

    // ---- what the crash did -------------------------------------------------
    // The running session's process died with the engine. This is the fact that
    // makes "lost" the honest word rather than a pessimistic one.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still_alive = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {running_pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&running_pid.to_string()))
        .unwrap_or(false);
    assert!(!still_alive, "pid {running_pid} outlived the engine");

    // ---- second life --------------------------------------------------------
    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;
    let sessions = list(&mut client).await;

    let before = sessions
        .iter()
        .find(|s| s.id == finished)
        .expect("finished session restored");
    assert_eq!(before.status, Status::Done);
    assert_eq!(before.exit_code, Some(0));
    assert!(!before.lost, "a session that exited cleanly was not lost");

    let lost = sessions
        .iter()
        .find(|s| s.id == running)
        .expect("running session restored");
    assert_eq!(lost.status, Status::Done);
    assert!(
        lost.lost,
        "a session running at the crash should come back as lost"
    );
    assert_eq!(lost.exit_code, None);
    assert_eq!(lost.pid, None);

    // Same ids, so a prefix typed before the crash still works after it.
    match client
        .call(Request::ReadScreen {
            id: running[..4].into(),
        })
        .await
        .unwrap()
    {
        Reply::Screen { screen } => assert!(screen.session.lost),
        other => panic!("unexpected reply {other:?}"),
    }

    // A restored session cannot be typed into, and the error says it was lost
    // rather than claiming it finished. It can be "killed" harmlessly.
    match client
        .call(Request::SendText {
            id: running.clone(),
            text: "dir\r".into(),
        })
        .await
    {
        Err(ClientError::Rpc(error)) => {
            assert!(error.message.contains("lost"), "{}", error.message);
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(matches!(
        client
            .call(Request::Kill {
                id: running.clone()
            })
            .await,
        Ok(Reply::Done)
    ));

    // New sessions join the restored ones, and all of them are written down.
    let fresh = spawn(&mut client).await;
    assert_eq!(list(&mut client).await.len(), 3);
    wait_file(&scratch.state(), "all three sessions", |state| {
        session_in_file(state, &fresh).is_some()
            && session_in_file(state, &running).is_some_and(|s| s["lost"] == true)
    })
    .await;

    drop(client);
    engine.crash();
}

async fn spawn_in(client: &mut Client, agent: &str, cwd: Option<&Path>) -> String {
    match client
        .call(Request::Spawn {
            agent: agent.into(),
            cwd: cwd.map(|dir| dir.display().to_string()),
            cols: None,
            rows: None,
            worktree: None,
        })
        .await
        .unwrap()
    {
        Reply::Session { session } => session.id,
        other => panic!("unexpected reply {other:?}"),
    }
}

async fn screen_text(client: &mut Client, id: &str) -> String {
    match client
        .call(Request::ReadScreen { id: id.into() })
        .await
        .unwrap()
    {
        Reply::Screen { screen } => screen.lines.join("\n"),
        other => panic!("unexpected reply {other:?}"),
    }
}

async fn resume(client: &mut Client, id: &str) -> Result<SessionInfo, ClientError> {
    client
        .call(Request::Resume {
            id: id.into(),
            cols: None,
            rows: None,
        })
        .await
        .map(|reply| match reply {
            Reply::Session { session } => session,
            other => panic!("unexpected reply {other:?}"),
        })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_session_is_resumed_under_its_own_id_in_its_own_folder() {
    let scratch = Scratch::new("resume");
    let pipe = unique_pipe_name("restart-resume");
    let workdir = scratch.dir.join("the project");
    fs::create_dir_all(&workdir).unwrap();

    // ---- first life ---------------------------------------------------------
    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;

    let resumable = spawn_in(&mut client, "test-resumable", Some(&workdir)).await;
    // No folder asked for: the engine records its own, so a later resume does
    // not depend on where the next engine happens to start.
    let shell = spawn_in(&mut client, "test-shell", None).await;
    wait_status(&mut client, &resumable, Status::Idle).await;
    wait_status(&mut client, &shell, Status::Idle).await;

    let state = wait_file(&scratch.state(), "a conversation id", |state| {
        session_in_file(state, &resumable).is_some_and(|s| s["agent_session"].is_string())
            && session_in_file(state, &shell).is_some()
    })
    .await;
    let uuid = session_in_file(&state, &resumable).unwrap()["agent_session"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        screen_text(&mut client, &resumable)
            .await
            .contains(&format!("new {uuid}"))
    );
    let shell_record = session_in_file(&state, &shell).unwrap();
    assert!(
        shell_record.get("agent_session").is_none(),
        "the shell's manifest has no use for one"
    );
    assert!(Path::new(shell_record["cwd"].as_str().expect("the engine's folder")).is_absolute());

    drop(client);
    engine.crash();

    // ---- second life --------------------------------------------------------
    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;
    let sessions = list(&mut client).await;
    let find = |id: &str| sessions.iter().find(|s| s.id == id).unwrap().clone();
    assert!(find(&resumable).lost && find(&resumable).resumable);
    assert!(find(&shell).lost && !find(&shell).resumable);

    // Typing at it says how to bring it back.
    match client
        .call(Request::SendText {
            id: resumable.clone(),
            text: "dir\r".into(),
        })
        .await
    {
        Err(ClientError::Rpc(error)) => {
            assert!(error.message.contains("resume"), "{}", error.message);
        }
        other => panic!("expected an error, got {other:?}"),
    }

    // An agent with no `resume` is refused, and says why.
    match resume(&mut client, &shell).await {
        Err(ClientError::Rpc(error)) => {
            assert!(error.message.contains("no `resume`"), "{}", error.message);
        }
        other => panic!("expected an error, got {other:?}"),
    }

    // By prefix, like every other call.
    let back = resume(&mut client, &resumable[..5]).await.unwrap();
    assert_eq!(back.id, resumable, "the same session, not a new one");
    assert!(!back.lost && !back.resumable);
    let pid = back.pid.expect("a live process");

    wait_status(&mut client, &resumable, Status::Idle).await;
    let screen = screen_text(&mut client, &resumable).await;
    assert!(
        screen.contains(&format!("resumed {uuid}")),
        "asked for the conversation it started with:\n{screen}"
    );
    assert!(!screen.contains("new "), "{screen}");
    assert!(
        screen.contains("the project>"),
        "running in the folder it ran in before:\n{screen}"
    );

    // Resuming something already running changes nothing.
    assert_eq!(
        resume(&mut client, &resumable).await.unwrap().pid,
        Some(pid)
    );
    assert_eq!(list(&mut client).await.len(), 2);

    // And it is written down as live, with the same conversation id.
    wait_file(&scratch.state(), "the resumed session", |state| {
        session_in_file(state, &resumable).is_some_and(|s| {
            s["status"] == "idle" && s.get("lost").is_none() && s["agent_session"] == uuid.as_str()
        })
    })
    .await;

    drop(client);
    engine.crash();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_state_file_is_quarantined_and_the_engine_still_starts() {
    let scratch = Scratch::new("corrupt");
    let pipe = unique_pipe_name("restart-corrupt");
    fs::create_dir_all(scratch.state().parent().unwrap()).unwrap();
    fs::write(
        scratch.state(),
        "{ this was mangled by a crash halfway through",
    )
    .unwrap();

    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;
    assert!(list(&mut client).await.is_empty());

    drop(client);
    engine.crash();
    let stderr = engine.stderr();
    assert!(
        stderr.contains("moved to"),
        "the engine should say where the file went:\n{stderr}"
    );

    // The original contents are preserved beside it, untouched.
    let quarantined: Vec<PathBuf> = fs::read_dir(scratch.state().parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".corrupt-")
        })
        .collect();
    assert_eq!(quarantined.len(), 1, "{quarantined:?}");
    assert_eq!(
        fs::read_to_string(&quarantined[0]).unwrap(),
        "{ this was mangled by a crash halfway through"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_engine_cannot_take_over_a_state_file_in_use() {
    // Different pipes, same state file: the pipe cannot stop this one. If the
    // second engine were allowed to load, it would read the first engine's live
    // session, decide it was lost in a crash, and write that down.
    let scratch = Scratch::new("lock");

    let mut first = EngineProcess::start(&scratch, &unique_pipe_name("restart-lock-a"));
    let mut client = first.connect().await;
    let live = spawn(&mut client).await;
    wait_status(&mut client, &live, Status::Idle).await;
    wait_file(&scratch.state(), "the live session", |state| {
        session_in_file(state, &live).is_some_and(|s| s["status"] == "idle")
    })
    .await;

    let mut second = EngineProcess::start(&scratch, &unique_pipe_name("restart-lock-b"));
    let status = tokio::time::timeout(LIMIT, async {
        loop {
            if let Ok(Some(status)) = second.child.try_wait() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the second engine should refuse to start, not run");
    assert!(!status.success());
    let stderr = second.stderr();
    assert!(stderr.contains("another engine"), "{stderr}");

    // And the first engine's session was not marked lost behind its back.
    let state: Value = serde_json::from_str(&fs::read_to_string(scratch.state()).unwrap()).unwrap();
    let entry = session_in_file(&state, &live).unwrap();
    assert_eq!(entry["status"], "idle");
    assert!(entry.get("lost").is_none(), "{entry}");

    drop(client);
    first.crash();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_registry_is_not_rewritten() {
    // The persister wakes every second. If the record for a steady session
    // differed by a millisecond on each pass, an idle engine would rewrite the
    // file every second forever. The status start time must be stable.
    let scratch = Scratch::new("stable");
    let agent = Agent::from_json(&manifest_json(), "test-shell.json").unwrap();
    let (engine, _) = Engine::new([agent])
        .unwrap()
        .with_state_file(&scratch.state())
        .unwrap();
    let engine = Arc::new(engine);

    assert!(
        !engine.save().await.unwrap(),
        "nothing has changed since startup"
    );

    let server = argus_control_server::Server::bind(unique_pipe_name("restart-stable")).unwrap();
    let pipe = server.name().to_string();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(Arc::clone(&engine), async {
        let _ = stopped.await;
    }));

    let mut client = Client::connect(&pipe).await.unwrap();
    let id = spawn(&mut client).await;
    wait_status(&mut client, &id, Status::Idle).await;

    assert!(
        engine.save().await.unwrap(),
        "a new idle session is a change"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !engine.save().await.unwrap(),
        "still idle, so nothing to write"
    );

    let _ = client.call(Request::Kill { id }).await;
    let _ = stop.send(());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_exited_can_be_resumed_and_racing_resumes_start_it_once() {
    let scratch = Scratch::new("resume-race");
    let pipe = unique_pipe_name("restart-resume-race");
    let mut engine = EngineProcess::start(&scratch, &pipe);
    let mut client = engine.connect().await;

    let id = spawn_in(&mut client, "test-resumable", Some(&scratch.dir)).await;
    wait_status(&mut client, &id, Status::Idle).await;
    client
        .call(Request::SendText {
            id: id.clone(),
            text: "exit\r".into(),
        })
        .await
        .unwrap();
    let exited = wait_status(&mut client, &id, Status::Done).await;
    assert!(!exited.lost, "it exited; nothing was lost");
    assert!(exited.resumable, "but it can still be brought back");

    // Two clients, at once.
    let mut other = engine.connect().await;
    let (first, second) = tokio::join!(resume(&mut client, &id), resume(&mut other, &id));

    // Whichever lost the race was either refused while the other was starting
    // it, or answered with the session the other had started. Never a second
    // process.
    let pids: Vec<u32> = [first, second]
        .into_iter()
        .filter_map(|result| match result {
            Ok(session) => Some(session.pid.expect("live")),
            Err(ClientError::Rpc(error)) => {
                assert!(
                    error.message.contains("already being resumed"),
                    "{}",
                    error.message
                );
                None
            }
            Err(other) => panic!("unexpected error {other}"),
        })
        .collect();
    assert!(!pids.is_empty());
    assert!(pids.iter().all(|&pid| pid == pids[0]), "{pids:?}");
    let live = wait_status(&mut client, &id, Status::Idle).await;
    assert_eq!(live.pid, Some(pids[0]));

    drop((client, other));
    engine.crash();
}
