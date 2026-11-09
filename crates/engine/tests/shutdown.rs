//! `shutdown`: the sessions stop first, then the engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Client, Server, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{Reply, Request, Status};

/// A shell that exists here, the arguments that make it print a prompt, and
/// the prompt to look for.
///
/// These tests need idle to be detectable, which means a shell that shows
/// something recognisable while it waits. `cmd.exe` always does. `sh` only
/// does when it is interactive, hence `-i`; its prompt is `$ ` as a user and
/// `# ` as root, and macOS answers `sh-3.2$ `, so the pattern matches the
/// ending rather than the whole line.
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

#[tokio::test]
async fn shutdown_finishes_every_session_then_stops_the_engine() {
    let pipe = unique_pipe_name("shutdown");
    let engine = Arc::new(Engine::new([shell_agent()]).unwrap());
    let server = Server::bind(&pipe).unwrap();
    let serving = tokio::spawn(server.run(Arc::clone(&engine), engine.stopped()));

    let mut client = Client::connect(&pipe).await.unwrap();
    for _ in 0..2 {
        client
            .call(Request::Spawn {
                agent: "test-shell".into(),
                cwd: None,
                cols: None,
                rows: None,
                worktree: None,
            })
            .await
            .unwrap();
    }

    assert_eq!(client.call(Request::Shutdown).await.unwrap(), Reply::Done);

    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("the server stops after a shutdown")
        .unwrap()
        .unwrap();

    let Reply::Sessions { sessions } = engine_list(&engine).await else {
        unreachable!()
    };
    assert_eq!(sessions.len(), 2);
    for session in sessions {
        assert_eq!(session.status, Status::Done, "{session:?}");
        assert!(!session.lost, "stopped on purpose, not lost: {session:?}");
    }
}

/// The engine's own answer to `list`, now that its pipe is closed.
async fn engine_list(engine: &Arc<Engine>) -> Reply {
    use argus_control_server::Handler;
    match engine.handle(Request::List).await {
        argus_protocol::Response::Ok(reply) => reply,
        argus_protocol::Response::Err(error) => panic!("{error}"),
    }
}
