//! Sessions in git worktrees, driven through the pipe against real git.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_control_server::{Client, Server, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::{ErrorCode, Reply, Request, SessionInfo};
use tokio::sync::oneshot;

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

fn scratch(label: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argus-wt-{label}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Argus Test",
            "-c",
            "user.email=test@argus.invalid",
        ])
        .args(args)
        .output()
        .expect("git should be installed to run these tests");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with one commit on `main`.
fn repo() -> PathBuf {
    let dir = scratch("repo").join("myrepo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    dir
}

struct Running {
    pipe: String,
    _stop: oneshot::Sender<()>,
}

fn start(root: &Path) -> Running {
    let pipe = unique_pipe_name("worktree");
    let engine = Arc::new(
        Engine::new([shell_agent()])
            .unwrap()
            .with_worktree_root(root),
    );
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

async fn spawn(
    pipe: &str,
    cwd: &Path,
    branch: &str,
) -> Result<SessionInfo, argus_protocol::RpcError> {
    let mut client = Client::connect(pipe).await.unwrap();
    let reply = client
        .call(Request::Spawn {
            agent: "test-shell".into(),
            cwd: Some(cwd.display().to_string()),
            cols: None,
            rows: None,
            worktree: Some(branch.into()),
        })
        .await;
    match reply {
        Ok(Reply::Session { session }) => Ok(session),
        Ok(other) => panic!("unexpected reply {other:?}"),
        Err(argus_control_server::ClientError::Rpc(error)) => Err(error),
        Err(other) => panic!("call failed: {other}"),
    }
}

async fn kill(pipe: &str, id: &str) {
    let mut client = Client::connect(pipe).await.unwrap();
    client.call(Request::Kill { id: id.into() }).await.unwrap();
}

#[tokio::test]
async fn a_session_gets_a_worktree_on_a_new_branch_and_a_second_one_shares_it() {
    let repo = repo();
    let root = scratch("root");
    let engine = start(&root);

    let first = spawn(&engine.pipe, &repo, "feature/login").await.unwrap();
    assert_eq!(first.branch.as_deref(), Some("feature/login"));
    let folder = PathBuf::from(first.cwd.clone().unwrap());
    assert_eq!(folder, root.join("myrepo").join("feature-login"));
    assert!(
        folder.join("README.md").is_file(),
        "the worktree is checked out"
    );
    assert_eq!(
        git(&folder, &["branch", "--show-current"]).trim(),
        "feature/login"
    );
    assert_eq!(
        git(&repo, &["branch", "--show-current"]).trim(),
        "main",
        "the main checkout is untouched"
    );

    let second = spawn(&engine.pipe, &repo, "feature/login").await.unwrap();
    assert_eq!(second.cwd, first.cwd, "same branch, same folder");

    kill(&engine.pipe, &first.id).await;
    kill(&engine.pipe, &second.id).await;
    assert!(folder.is_dir(), "killing a session keeps its worktree");
}

#[tokio::test]
async fn an_existing_branch_is_checked_out_rather_than_recreated() {
    let repo = repo();
    git(&repo, &["branch", "fix/typo"]);
    let root = scratch("root");
    let engine = start(&root);

    let session = spawn(&engine.pipe, &repo, "fix/typo").await.unwrap();
    let folder = PathBuf::from(session.cwd.clone().unwrap());
    assert_eq!(
        git(&folder, &["branch", "--show-current"]).trim(),
        "fix/typo"
    );
    kill(&engine.pipe, &session.id).await;
}

#[tokio::test]
async fn the_branch_the_main_checkout_is_on_runs_in_the_main_checkout() {
    let repo = repo();
    let root = scratch("root");
    let engine = start(&root);

    let session = spawn(&engine.pipe, &repo, "main").await.unwrap();
    let folder = PathBuf::from(session.cwd.clone().unwrap());
    assert_eq!(
        std::fs::canonicalize(folder).unwrap(),
        std::fs::canonicalize(&repo).unwrap()
    );
    kill(&engine.pipe, &session.id).await;
}

#[tokio::test]
async fn a_folder_outside_any_repository_is_refused_with_a_reason() {
    let plain = scratch("plain");
    let root = scratch("root");
    let engine = start(&root);

    let error = spawn(&engine.pipe, &plain, "feature/x").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error.message.contains("not in a git repository"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_bad_branch_name_is_refused_before_anything_is_made() {
    let repo = repo();
    let root = scratch("root");
    let engine = start(&root);

    let error = spawn(&engine.pipe, &repo, "has space..and dots")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error.message.contains("not a valid branch name"),
        "{}",
        error.message
    );
    assert!(!root.join("myrepo").exists());
}
