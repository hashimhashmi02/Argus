//! The real `argus-cli` binary against a real engine.
//!
//! The engine runs in-process on a private pipe; the CLI runs as a separate
//! process, exactly as a person would run it. Arguments go straight to the
//! binary through `Command`, with no shell in between to reinterpret them.

use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Server, unique_pipe_name};
use argus_engine::Engine;
use argus_manifest::Agent;
use argus_protocol::Reply;
use tokio::sync::oneshot;

const LIMIT: Duration = Duration::from_secs(20);

fn cmd_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot should be set on Windows");
    PathBuf::from(root).join("System32").join("cmd.exe")
}

fn shell_agent() -> Agent {
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
    Agent::from_json(&json, "test-shell.json").unwrap()
}

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

/// Run `argus-cli --pipe <pipe> <args...>`.
async fn cli(pipe: &str, args: &[&str]) -> Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_argus-cli"))
        .arg("--pipe")
        .arg(pipe)
        .args(args)
        .output()
        .await
        .expect("argus-cli should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Re-run a command until its stdout satisfies `predicate`.
async fn poll(pipe: &str, args: &[&str], what: &str, predicate: impl Fn(&str) -> bool) -> String {
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        let output = cli(pipe, args).await;
        let text = stdout(&output);
        if output.status.success() && predicate(&text) {
            return text;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}\n--- stdout ---\n{text}\n--- stderr ---\n{}",
                stderr(&output)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `spawn` prints `<id>  <agent>  <status> ...`; the id is the first word.
async fn spawn(pipe: &str) -> String {
    let output = cli(pipe, &["spawn", "test-shell"]).await;
    assert!(output.status.success(), "spawn failed: {}", stderr(&output));
    stdout(&output)
        .split_whitespace()
        .next()
        .expect("spawn should print an id")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_prints_json_events_until_the_session_ends() {
    let engine = start("cli-watch");
    let id = spawn(&engine.pipe).await;
    poll(&engine.pipe, &["screen", &id], "the prompt", |text| {
        text.contains('>')
    })
    .await;

    let watch = tokio::process::Command::new(env!("CARGO_BIN_EXE_argus-cli"))
        .args(["--pipe", &engine.pipe, "--json", "watch", &id])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("argus-cli should run");

    // Let it attach, then end the session from another process. If it attaches
    // late, it is shown the finished screen instead, and must still end.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let kill = cli(&engine.pipe, &["kill", &id]).await;
    assert!(kill.status.success(), "kill failed: {}", stderr(&kill));

    let output = tokio::time::timeout(LIMIT, watch.wait_with_output())
        .await
        .expect("watch should exit once the session ends")
        .unwrap();
    assert!(output.status.success(), "watch failed: {}", stderr(&output));

    let text = stdout(&output);
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is one JSON event"))
        .collect();
    assert_eq!(events.first().unwrap()["event"], "repaint", "{text}");
    assert_eq!(events.last().unwrap()["event"], "ended", "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_screen_send_list_and_kill() {
    let engine = start("cli-lifecycle");
    let pipe = &engine.pipe;

    let empty = cli(pipe, &["list"]).await;
    assert!(empty.status.success());
    assert!(stdout(&empty).contains("no sessions"));

    let id = spawn(pipe).await;
    assert_eq!(id.len(), 8, "expected an 8-character id, got {id:?}");

    // The prompt shows up on the emulated screen.
    poll(pipe, &["screen", &id], "the prompt", |text| {
        text.contains('>')
    })
    .await;

    // Words after the id are joined and followed by Enter.
    let sent = cli(pipe, &["send", &id, "echo", "hello-from-cli"]).await;
    assert!(sent.status.success(), "send failed: {}", stderr(&sent));
    poll(pipe, &["screen", &id], "the echoed text", |text| {
        text.lines().any(|line| line == "hello-from-cli")
    })
    .await;

    // The list shows the session idle, under the rule that decided it.
    let listed = poll(pipe, &["list"], "an idle row", |text| text.contains("idle")).await;
    let row = listed.lines().find(|line| line.starts_with(&id)).unwrap();
    assert!(
        row.contains("test-shell") && row.contains("cmd-prompt"),
        "row: {row}"
    );

    // A prefix is enough.
    let killed = cli(pipe, &["kill", &id[..3]]).await;
    assert!(killed.status.success(), "kill failed: {}", stderr(&killed));
    poll(pipe, &["list"], "the session to finish", |text| {
        text.contains("done")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_enter_and_key_compose_into_one_command() {
    // Type without pressing Enter, then press it as a separate named key. Proves
    // `--no-enter` really withholds the keystroke and `--key enter` really sends
    // the same byte a manifest's "enter" would.
    let engine = start("cli-keys");
    let pipe = &engine.pipe;
    let id = spawn(pipe).await;
    poll(pipe, &["screen", &id], "the prompt", |text| {
        text.contains('>')
    })
    .await;

    let typed = cli(
        pipe,
        &["send", &id, "--no-enter", "echo", "split-keystroke"],
    )
    .await;
    assert!(typed.status.success(), "{}", stderr(&typed));
    // Typed but not run: the text is on the prompt line, no output line yet.
    poll(pipe, &["screen", &id], "the typed text", |text| {
        text.contains("echo split-keystroke")
    })
    .await;

    let entered = cli(pipe, &["send", &id, "--key", "enter"]).await;
    assert!(entered.status.success(), "{}", stderr(&entered));
    poll(pipe, &["screen", &id], "the command to run", |text| {
        text.lines().any(|line| line == "split-keystroke")
    })
    .await;

    cli(pipe, &["kill", &id]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn json_output_is_the_engines_reply_verbatim() {
    let engine = start("cli-json");
    let pipe = &engine.pipe;
    let id = spawn(pipe).await;

    let output = cli(pipe, &["--json", "list"]).await;
    assert!(output.status.success());
    let reply: Reply = serde_json::from_str(&stdout(&output)).expect("--json should be valid JSON");
    match reply {
        Reply::Sessions { sessions } => assert_eq!(sessions[0].id, id),
        other => panic!("unexpected reply {other:?}"),
    }

    cli(pipe, &["kill", &id]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_errors_exit_1_with_the_code_named() {
    let engine = start("cli-errors");
    let pipe = &engine.pipe;

    let missing = cli(pipe, &["screen", "deadbeef"]).await;
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("UnknownSession"),
        "{}",
        stderr(&missing)
    );

    // An unknown agent lists the ones that do exist -- the most useful thing to
    // tell someone who just mistyped one.
    let bad_agent = cli(pipe, &["spawn", "claude-cod"]).await;
    assert_eq!(bad_agent.status.code(), Some(1));
    let message = stderr(&bad_agent);
    assert!(
        message.contains("UnknownAgent") && message.contains("test-shell"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_refuses_an_agent_that_cannot_and_takes_a_size() {
    let engine = start("cli-resume");
    let pipe = &engine.pipe;
    let id = spawn(pipe).await;
    cli(pipe, &["kill", &id]).await;
    poll(pipe, &["list"], "the session to finish", |out| {
        out.contains("done")
    })
    .await;

    // The test shell's manifest has no `resume`: refused, exit 1, with why.
    let refused = cli(pipe, &["resume", &id[..4], "--size", "100x30"]).await;
    assert_eq!(refused.status.code(), Some(1));
    let message = stderr(&refused);
    assert!(
        message.contains("no `resume`") && message.contains("BadRequest"),
        "{message}"
    );
    assert!(!stdout(&cli(pipe, &["list"]).await).contains('*'));

    let bad_size = cli(pipe, &["resume", &id, "--size", "wide"]).await;
    assert_eq!(bad_size.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_engine_exits_3_with_a_hint() {
    // Distinct from exit 1, so a script can tell "the engine said no" from
    // "there is no engine to ask".
    let output = cli(&unique_pipe_name("cli-nobody"), &["list"]).await;

    assert_eq!(output.status.code(), Some(3));
    let message = stderr(&output);
    assert!(
        message.contains("not running") && message.contains("argus-engine"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_pipe_name_is_accepted() {
    // The pipe name without its `\\.\pipe\` prefix -- what a person types,
    // because the prefix does not survive every shell intact.
    let engine = start("cli-bare");
    let bare = engine.pipe.trim_start_matches(r"\\.\pipe\");
    assert_ne!(bare, engine.pipe, "the helper should produce a full path");

    let output = cli(bare, &["list"]).await;
    assert!(
        output.status.success(),
        "bare name failed: {}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_size_is_a_usage_error() {
    // Rejected by argument parsing before any pipe is opened, with clap's
    // usage exit code.
    let output = cli(
        &unique_pipe_name("cli-usage"),
        &["spawn", "x", "--size", "0x5"],
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_approve_and_deny() {
    // The test manifest declares no keys, so the defaults apply: approve is
    // Enter, deny is Escape. The CLI names neither -- the engine looks them up.
    let engine = start("cli-respond");
    let pipe = &engine.pipe;

    let agents = cli(pipe, &["agents"]).await;
    assert!(agents.status.success());
    assert!(
        stdout(&agents).contains("test-shell"),
        "{}",
        stdout(&agents)
    );

    let id = spawn(pipe).await;
    poll(pipe, &["screen", &id], "the prompt", |text| {
        text.contains('>')
    })
    .await;

    cli(
        pipe,
        &["send", &id, "--no-enter", "echo", "approved-from-cli"],
    )
    .await;
    let approved = cli(pipe, &["approve", &id]).await;
    assert!(approved.status.success(), "{}", stderr(&approved));
    poll(pipe, &["screen", &id], "the approved command", |text| {
        text.lines().any(|line| line == "approved-from-cli")
    })
    .await;

    let missing = cli(pipe, &["deny", "deadbeef"]).await;
    assert_eq!(missing.status.code(), Some(1));

    cli(pipe, &["kill", &id]).await;
}
