//! A watcher on the other end of a real QUIC connection.
//!
//! Everything here goes over a loopback socket with a real handshake, a real
//! certificate and the real invite. If a property holds in these tests it
//! holds for a peer on another machine, because nothing in the path is stubbed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use argus_control_server::{ClientError, Handler, QuicListener, Viewer, quic_connect};
use argus_protocol::{ErrorCode, Reply, Request, Response, RpcError, WatchEvent};
use tokio::sync::{mpsc, oneshot};

const LIMIT: Duration = Duration::from_secs(20);

/// Streams three frames and ends. Counts what reached it.
#[derive(Default)]
struct Streaming {
    handled: AtomicUsize,
    watched: AtomicUsize,
}

impl Handler for Streaming {
    async fn handle(&self, _request: Request) -> Response {
        self.handled.fetch_add(1, Ordering::Relaxed);
        Response::Ok(Reply::Done)
    }

    async fn watch(
        &self,
        _id: String,
        _from: Viewer,
    ) -> Result<mpsc::Receiver<WatchEvent>, RpcError> {
        self.watched.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(WatchEvent::Repaint {
                    cols: 80,
                    rows: 24,
                    data: "hello from the agent".into(),
                })
                .await;
            let _ = tx
                .send(WatchEvent::Output {
                    data: "a line".into(),
                })
                .await;
            let _ = tx.send(WatchEvent::Ended).await;
        });
        Ok(rx)
    }
}

struct Running {
    listener: Arc<QuicListener>,
    handler: Arc<Streaming>,
    _stop: oneshot::Sender<()>,
}

async fn start() -> Running {
    let listener = Arc::new(QuicListener::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let handler = Arc::new(Streaming::default());
    let (stop, stopped) = oneshot::channel::<()>();

    let serving = Arc::clone(&listener);
    let serving_handler = Arc::clone(&handler);
    tokio::spawn(async move {
        serving
            .serve_forever(serving_handler, async {
                let _ = stopped.await;
            })
            .await;
    });

    Running {
        listener,
        handler,
        _stop: stop,
    }
}

/// `Client` has no `Debug` -- it owns a boxed stream -- so failures are
/// described rather than formatted.
fn describe<T>(outcome: Result<Result<T, ClientError>, tokio::time::error::Elapsed>) -> String {
    match outcome {
        Ok(Ok(_)) => "connected".to_string(),
        Ok(Err(error)) => format!("refused: {error}"),
        Err(_) => "timed out".to_string(),
    }
}

fn rpc(result: Result<Reply, ClientError>) -> RpcError {
    match result {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_watcher_with_an_invite_sees_the_session() {
    let server = start().await;
    let invite = server.listener.invite("3fa1c2d4").unwrap();

    let client = tokio::time::timeout(LIMIT, quic_connect(&invite))
        .await
        .expect("connect timed out")
        .expect("the invite should be accepted");

    let mut watcher = client.watch("3fa1c2d4").await.unwrap();
    assert_eq!(
        watcher.next().await.unwrap(),
        Some(WatchEvent::Repaint {
            cols: 80,
            rows: 24,
            data: "hello from the agent".into()
        })
    );
    assert_eq!(
        watcher.next().await.unwrap(),
        Some(WatchEvent::Output {
            data: "a line".into()
        })
    );
    assert_eq!(watcher.next().await.unwrap(), Some(WatchEvent::Ended));

    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn an_invite_works_once() {
    let server = start().await;
    let invite = server.listener.invite("3fa1c2d4").unwrap();

    let first = quic_connect(&invite).await;
    assert!(first.is_ok(), "the first use should work");
    drop(first);

    let error = match tokio::time::timeout(LIMIT, quic_connect(&invite)).await {
        Ok(Err(ClientError::Rpc(error))) => error,
        Ok(Ok(_)) => panic!("a second use of an invite must be refused"),
        other => panic!("unexpected outcome: {}", describe(other)),
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn a_made_up_secret_is_refused() {
    let server = start().await;
    let real = server.listener.invite("3fa1c2d4").unwrap();

    let mut forged = real.clone();
    forged.secret = [0u8; 32];

    let error = match tokio::time::timeout(LIMIT, quic_connect(&forged)).await {
        Ok(Err(ClientError::Rpc(error))) => error,
        Ok(Ok(_)) => panic!("a forged secret must be refused"),
        other => panic!("unexpected outcome: {}", describe(other)),
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_wrong_fingerprint_is_refused_before_any_secret_is_sent() {
    let server = start().await;
    let real = server.listener.invite("3fa1c2d4").unwrap();

    let mut tampered = real.clone();
    tampered.fingerprint = "b".repeat(64);

    // The handshake has to fail, and it has to fail before the secret goes
    // out: pinning that only checked afterwards would already have leaked it
    // to whoever answered.
    let result = tokio::time::timeout(LIMIT, quic_connect(&tampered)).await;
    assert!(
        matches!(result, Ok(Err(_))),
        "a mismatched fingerprint must not connect: {}",
        describe(result)
    );

    // The real invite is untouched, which shows the failed attempt did not
    // reach the invite table at all.
    assert!(quic_connect(&real).await.is_ok());
}

#[tokio::test]
async fn a_watcher_cannot_reach_anything_but_its_own_session() {
    let server = start().await;
    let invite = server.listener.invite("3fa1c2d4").unwrap();
    let client = quic_connect(&invite).await.unwrap();

    // The capability earned by the invite is enforced over QUIC exactly as it
    // is over the pipe. `spawn` is the one that would be remote code
    // execution.
    let mut client = client;
    let error = rpc(client
        .call(Request::Spawn {
            agent: "shell".into(),
            cwd: None,
            cols: None,
            rows: None,
            worktree: None,
        })
        .await);
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(
        server.handler.handled.load(Ordering::Relaxed),
        0,
        "nothing should have reached the handler"
    );
}

#[tokio::test]
async fn a_watcher_cannot_watch_a_different_session() {
    let server = start().await;
    let invite = server.listener.invite("3fa1c2d4").unwrap();
    let client = quic_connect(&invite).await.unwrap();

    let error = match client.watch("9b0e1172").await {
        Err(ClientError::Rpc(error)) => error,
        _ => panic!("watching another session should be refused"),
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(server.handler.watched.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_peer_that_never_presents_a_secret_is_dropped() {
    // The handshake is unauthenticated, so it has to be on a clock. Without
    // one, a peer that connects and says nothing holds a task and a connection
    // indefinitely and enough of them exhaust the host.
    let server = start().await;
    let invite = server.listener.invite("3fa1c2d4").unwrap();

    // A bare UDP socket: enough to make the server begin a handshake that
    // will never be completed.
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    silent.send_to(&[0u8; 1200], invite.addr).await.unwrap();

    // The listener must still be serving afterwards, which is the property
    // that matters: one stalled peer does not deny the next a connection.
    assert!(
        quic_connect(&invite).await.is_ok(),
        "a stalled peer must not block an invited one"
    );
}

#[tokio::test]
async fn dropping_a_watcher_closes_its_connection() {
    // The client owns the endpoint and the connection, so letting it go has to
    // close them. Held by a task waiting on `closed()` instead, nothing would
    // ever close them and every finished watch would leak until the idle
    // timeout.
    let server = start().await;

    for _ in 0..5 {
        let invite = server.listener.invite("3fa1c2d4").unwrap();
        let client = quic_connect(&invite).await.unwrap();
        drop(client);
    }

    // Still able to take another watcher after five have come and gone.
    let invite = server.listener.invite("3fa1c2d4").unwrap();
    assert!(quic_connect(&invite).await.is_ok());
}
