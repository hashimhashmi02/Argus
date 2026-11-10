//! A restricted connection, driven over a real pipe by a real client.
//!
//! The unit tests in `capability.rs` prove the policy. These prove it is
//! actually consulted: that a refusal happens in the server, before the
//! handler, and that a `watch` the policy denies never starts streaming.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use argus_control_server::{
    Capability, Client, ClientError, Handler, Server, Viewer, unique_pipe_name,
};
use argus_protocol::{
    ErrorCode, Reply, Request, Response, RpcError, SessionInfo, Status, WatchEvent,
};
use tokio::sync::{mpsc, oneshot};

/// Answers anything, and counts what reached it.
#[derive(Default)]
struct Counting {
    handled: AtomicUsize,
    watched: AtomicUsize,
    /// Where each watch that started said it came from, in order.
    viewers: Mutex<Vec<Viewer>>,
}

impl Handler for Counting {
    async fn handle(&self, request: Request) -> Response {
        self.handled.fetch_add(1, Ordering::Relaxed);
        match request {
            Request::List => Response::Ok(Reply::Sessions {
                sessions: vec![SessionInfo {
                    id: "9b0e1172".into(),
                    agent: "shell".into(),
                    status: Status::Idle,
                    since_ms: 0,
                    status_for_ms: 0,
                    rule: None,
                    pid: None,
                    exit_code: None,
                    lost: false,
                    resumable: false,
                    cwd: None,
                    branch: None,
                    remote_watchers: 0,
                }],
            }),
            _ => Response::Ok(Reply::Done),
        }
    }

    async fn watch(
        &self,
        _id: String,
        from: Viewer,
    ) -> Result<mpsc::Receiver<WatchEvent>, RpcError> {
        self.watched.fetch_add(1, Ordering::Relaxed);
        self.viewers
            .lock()
            .expect("no test panics holding this")
            .push(from);
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx.send(WatchEvent::Ended).await;
        });
        Ok(rx)
    }
}

struct Running {
    pipe: String,
    handler: Arc<Counting>,
    _stop: oneshot::Sender<()>,
}

fn start(label: &str, capability: Capability) -> Running {
    let pipe = unique_pipe_name(label);
    let handler = Arc::new(Counting::default());
    let server = Server::bind(&pipe).unwrap().with_capability(capability);
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = Arc::clone(&handler);
    tokio::spawn(async move {
        let _ = server
            .run(serving, async {
                let _ = stopped.await;
            })
            .await;
    });
    Running {
        pipe,
        handler,
        _stop: stop,
    }
}

async fn call(pipe: &str, request: Request) -> Result<Reply, ClientError> {
    Client::connect(pipe).await.unwrap().call(request).await
}

fn forbidden(result: Result<Reply, ClientError>) -> RpcError {
    match result {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_session_token_is_refused_everything_but_its_own_watch() {
    let server = start(
        "cap-deny",
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        },
    );

    let denied = [
        Request::List,
        Request::Agents,
        Request::Shutdown,
        Request::Spawn {
            agent: "shell".into(),
            cwd: None,
            cols: None,
            rows: None,
            worktree: None,
        },
        Request::SendText {
            id: "3fa1c2d4".into(),
            text: "whoami".into(),
        },
        Request::Kill {
            id: "3fa1c2d4".into(),
        },
        // Minting an invite is the right to hand out access, so a watcher must
        // never reach it -- even for the session it is already watching.
        Request::Invite {
            id: "3fa1c2d4".into(),
        },
        Request::ReadScreen {
            id: "3fa1c2d4".into(),
        },
        // Its own session, but a different call.
        Request::Resize {
            id: "3fa1c2d4".into(),
            cols: 80,
            rows: 24,
        },
        // A watch, but of somebody else's session.
        Request::Watch {
            id: "9b0e1172".into(),
        },
    ];

    for request in denied {
        let error = forbidden(call(&server.pipe, request.clone()).await);
        assert_eq!(error.code, ErrorCode::Forbidden, "{request:?}");
    }

    // The point of checking in the server: none of that reached the engine.
    assert_eq!(server.handler.handled.load(Ordering::Relaxed), 0);
    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_session_token_may_watch_the_session_it_names() {
    let server = start(
        "cap-allow",
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        },
    );

    let client = Client::connect(&server.pipe).await.unwrap();
    let mut watcher = client.watch("3fa1c2d4").await.unwrap();
    assert_eq!(watcher.next().await.unwrap(), Some(WatchEvent::Ended));

    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn a_refused_watch_never_starts_streaming() {
    let server = start(
        "cap-no-stream",
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        },
    );

    // `watch` consumes the client, so the refusal arrives as its error rather
    // than as a reply the caller could mistake for a stream.
    let client = Client::connect(&server.pipe).await.unwrap();
    let error = match client.watch("9b0e1172").await {
        Err(ClientError::Rpc(error)) => error,
        other => panic!(
            "expected a refusal, got {}",
            other.map(|_| "a watcher").unwrap_or("?")
        ),
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_watch_says_whether_it_came_from_this_machine() {
    // The number the window shows is "is anyone *else* watching", so the
    // handler has to be able to tell its own watch from an invited one. It
    // learns that from the capability, which means neither the invite nor the
    // local socket has to carry anything extra to say so.
    let remote = start(
        "cap-from-away",
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        },
    );
    let client = Client::connect(&remote.pipe).await.unwrap();
    let mut watcher = client.watch("3fa1c2d4").await.unwrap();
    assert_eq!(watcher.next().await.unwrap(), Some(WatchEvent::Ended));
    assert_eq!(
        *remote.handler.viewers.lock().unwrap(),
        vec![Viewer::Remote],
        "a connection that redeemed an invite is watching from elsewhere"
    );

    let local = start("cap-from-here", Capability::Full);
    let client = Client::connect(&local.pipe).await.unwrap();
    let mut watcher = client.watch("3fa1c2d4").await.unwrap();
    assert_eq!(watcher.next().await.unwrap(), Some(WatchEvent::Ended));
    assert_eq!(
        *local.handler.viewers.lock().unwrap(),
        vec![Viewer::Local],
        "the engine's own pipe is this machine"
    );
}

#[tokio::test]
async fn a_refused_watch_is_never_counted_as_a_viewer() {
    // The refusal happens in the server, so `watch` is never called and
    // nothing can have been counted. Worth asserting rather than assuming:
    // counting a watch that was refused would show a session as watched by
    // someone who was turned away.
    let server = start(
        "cap-refused-viewer",
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        },
    );

    let client = Client::connect(&server.pipe).await.unwrap();
    assert!(client.watch("99999999").await.is_err());
    assert!(server.handler.viewers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_local_connection_is_unaffected() {
    let server = start("cap-full", Capability::Full);

    let reply = call(&server.pipe, Request::List).await.unwrap();
    assert!(matches!(reply, Reply::Sessions { .. }));
    assert_eq!(server.handler.handled.load(Ordering::Relaxed), 1);
}
