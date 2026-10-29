//! `shutdown`: the sessions stop first, then the engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Client, Server, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{Reply, Request, Status};

fn shell_agent() -> Agent {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    let cmd = PathBuf::from(root).join("System32").join("cmd.exe");
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
        program = serde_json::to_string(&cmd).unwrap(),
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
