//! The commands the webview can call.
//!
//! The webview never talks to the engine. It calls these, and these talk to the
//! engine over the named pipe through `argus-control-server`'s `Client` -- the
//! same client the CLI uses. Two reasons it is built this way round:
//!
//! - The pipe is the engine's whole API surface, and it can spawn programs as
//!   the user. Exposing it to JavaScript directly would put that power one XSS
//!   away from anything the webview ever renders. Here, the webview can only do
//!   what one of these functions allows.
//! - Lessons already learned live in the Rust client -- framing, version checks,
//!   pressing Enter as its own keystroke -- and the UI inherits them instead of
//!   re-learning them in TypeScript.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_control_server::{Client, ClientError};
use argus_protocol::{AgentInfo, ErrorCode, Reply, Request, ScreenView, SessionInfo};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::State;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;

use crate::engine::EngineLauncher;

/// Where the engine is listening.
pub struct Pipe(pub String);

/// The watches this window has running, by the number handed to the webview.
#[derive(Default)]
pub struct Watches {
    next: AtomicU32,
    running: Mutex<HashMap<u32, JoinHandle<()>>>,
}

/// An error the UI can act on, not just display.
///
/// `kind` lets the frontend tell "the engine is not running" -- show the start
/// button -- apart from "the engine said no" -- show the message. Everything
/// arrives in the webview as JSON, so this is the shape it sees.
#[derive(Debug, Serialize)]
pub struct UiError {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<ErrorCode>,
    message: String,
}

impl From<ClientError> for UiError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::NotRunning(_) => UiError {
                kind: "not_running",
                code: None,
                message: "The Argus engine is not running.".into(),
            },
            ClientError::Rpc(rpc) => UiError {
                kind: "rpc",
                code: Some(rpc.code),
                message: rpc.message,
            },
            other => UiError {
                kind: "other",
                code: None,
                message: other.to_string(),
            },
        }
    }
}

type Result<T> = std::result::Result<T, UiError>;

/// Open a connection for one call.
///
/// A fresh connection per call rather than one held open. The UI polls a few
/// times a second, and a named-pipe connect on the same machine costs
/// microseconds -- while a held connection would need reconnect logic for every
/// engine restart, which is exactly the moment the UI most needs to just work.
async fn connect(pipe: &Pipe) -> Result<Client> {
    Ok(Client::connect(&pipe.0).await?)
}

/// How long any one command may take before the UI gives up on it.
///
/// Every engine call is bounded. Found the hard way: without a bound, one
/// request that never got a reply left its command waiting forever, and the
/// poller behind it -- which never starts a request while one is in flight --
/// stopped for good. The session list froze on "no sessions" while the CLI
/// listed four. A slow engine should produce an error the user can see, never a
/// UI that silently stops updating.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bound `work` by [`CALL_TIMEOUT`].
async fn bounded<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(CALL_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| {
            Err(UiError {
                kind: "other",
                code: None,
                message: format!("the engine did not answer within {CALL_TIMEOUT:?}"),
            })
        })
}

async fn call(pipe: &Pipe, request: Request) -> Result<Reply> {
    bounded(async { Ok(connect(pipe).await?.call(request).await?) }).await
}

/// A reply of the wrong kind means the engine and UI disagree about the
/// protocol in a way the version check did not catch. Say so plainly.
fn unexpected(reply: Reply) -> UiError {
    UiError {
        kind: "other",
        code: None,
        message: format!("unexpected reply from the engine: {reply:?}"),
    }
}

#[tauri::command]
pub async fn list_sessions(pipe: State<'_, Pipe>) -> Result<Vec<SessionInfo>> {
    match call(&pipe, Request::List).await? {
        Reply::Sessions { sessions } => Ok(sessions),
        other => Err(unexpected(other)),
    }
}

#[tauri::command]
pub async fn list_agents(pipe: State<'_, Pipe>) -> Result<Vec<AgentInfo>> {
    match call(&pipe, Request::Agents).await? {
        Reply::Agents { agents } => Ok(agents),
        other => Err(unexpected(other)),
    }
}

#[tauri::command]
pub async fn read_screen(pipe: State<'_, Pipe>, id: String) -> Result<ScreenView> {
    match call(&pipe, Request::ReadScreen { id }).await? {
        Reply::Screen { screen } => Ok(screen),
        other => Err(unexpected(other)),
    }
}

#[tauri::command]
pub async fn spawn_session(
    pipe: State<'_, Pipe>,
    agent: String,
    cwd: Option<String>,
) -> Result<SessionInfo> {
    // An empty working-directory box means "the default", not "the empty path".
    let cwd = cwd.filter(|dir| !dir.trim().is_empty());
    let request = Request::Spawn {
        agent,
        cwd,
        cols: None,
        rows: None,
    };
    match call(&pipe, request).await? {
        Reply::Session { session } => Ok(session),
        other => Err(unexpected(other)),
    }
}

/// Type a line and press Enter -- as two keystrokes, via `Client::send_line`.
#[tauri::command]
pub async fn send_line(pipe: State<'_, Pipe>, id: String, text: String) -> Result<()> {
    bounded(async { Ok(connect(&pipe).await?.send_line(&id, &text).await?) }).await
}

/// Type exactly this, as the interactive terminal's keyboard produces it:
/// characters, escape sequences for keys, control characters.
#[tauri::command]
pub async fn send_text(pipe: State<'_, Pipe>, id: String, text: String) -> Result<()> {
    call(&pipe, Request::SendText { id, text }).await?;
    Ok(())
}

/// Change a session's terminal size, to fit the window showing it.
#[tauri::command]
pub async fn resize_session(pipe: State<'_, Pipe>, id: String, cols: u16, rows: u16) -> Result<()> {
    call(&pipe, Request::Resize { id, cols, rows }).await?;
    Ok(())
}

/// Follow a session's terminal, delivering the engine's watch events to
/// `on_event` as they arrive. Returns a number for [`unwatch_session`].
///
/// Each event goes to the webview as the engine's own JSON. One more is added
/// here: `{"event": "disconnected"}` when the stream stops without the session
/// having ended -- the engine went away, or the pipe broke -- so the webview
/// can tell "finished" from "lost contact" and try again.
#[tauri::command]
pub async fn watch_session(
    pipe: State<'_, Pipe>,
    watches: State<'_, Watches>,
    id: String,
    on_event: Channel<Value>,
) -> Result<u32> {
    // Only the handshake is bounded: once streaming, a quiet session can go
    // hours between events, and that is not a failure.
    let mut watcher = bounded(async { Ok(connect(&pipe).await?.watch(&id).await?) }).await?;

    let task = tauri::async_runtime::spawn(async move {
        let disconnected = loop {
            match watcher.next().await {
                Ok(Some(event)) => {
                    let ended = matches!(event, argus_protocol::WatchEvent::Ended);
                    let Ok(value) = serde_json::to_value(&event) else {
                        break true;
                    };
                    // Fails once the webview that asked has gone: reloaded or
                    // closed. Nobody is left to tell.
                    if on_event.send(value).is_err() {
                        return;
                    }
                    if ended {
                        break false;
                    }
                }
                Ok(None) | Err(_) => break true,
            }
        };
        if disconnected {
            let _ = on_event.send(json!({ "event": "disconnected" }));
        }
    });

    let key = watches.next.fetch_add(1, Ordering::Relaxed);
    watches
        .running
        .lock()
        .expect("watch table poisoned")
        .insert(key, task);
    Ok(key)
}

/// Stop a watch. Dropping the connection is what tells the engine to stop
/// sending. Unknown numbers are ignored: the watch may already have ended.
#[tauri::command]
pub fn unwatch_session(watches: State<'_, Watches>, watch: u32) {
    let task = watches
        .running
        .lock()
        .expect("watch table poisoned")
        .remove(&watch);
    if let Some(task) = task {
        task.abort();
    }
}

/// Press a named key: `enter`, `escape`, `ctrl-c`, `up`, and the rest of the
/// names a manifest uses.
#[tauri::command]
pub async fn send_key(pipe: State<'_, Pipe>, id: String, key: String) -> Result<()> {
    let bytes = argus_manifest::key_bytes(&key);
    let text = String::from_utf8(bytes).map_err(|_| UiError {
        kind: "other",
        code: None,
        message: format!("key `{key}` is not valid text"),
    })?;
    call(&pipe, Request::SendText { id, text }).await?;
    Ok(())
}

/// Approve or deny what a session is asking, with that agent's own keys.
#[tauri::command]
pub async fn respond(pipe: State<'_, Pipe>, id: String, approve: bool) -> Result<()> {
    call(&pipe, Request::Respond { id, approve }).await?;
    Ok(())
}

#[tauri::command]
pub async fn kill_session(pipe: State<'_, Pipe>, id: String) -> Result<()> {
    call(&pipe, Request::Kill { id }).await?;
    Ok(())
}

/// Start the engine as a detached background process.
#[tauri::command]
pub async fn start_engine(
    pipe: State<'_, Pipe>,
    launcher: State<'_, EngineLauncher>,
) -> Result<()> {
    launcher.start(&pipe.0).map_err(|e| UiError {
        kind: "other",
        code: None,
        message: format!("{e:#}"),
    })
}
