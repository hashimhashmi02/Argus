//! A real named pipe, a stub handler.
//!
//! No sessions or agents here: the failures this crate can have are about the
//! transport -- concurrency, framing, misbehaving clients -- and they are easier
//! to see without a child process in the way. Each test gets its own pipe name,
//! so they run in parallel without colliding.

use std::sync::Arc;
use std::time::Duration;

use argus_control_server::{Client, ClientError, Handler, Server, unique_pipe_name};
use argus_protocol::{
    ErrorCode, PROTOCOL_VERSION, Reply, Request, Response, RpcError, SessionInfo, Status,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::oneshot;

/// Answers `List` with one fake session, echoes ids back for `ReadScreen`, and
/// reports every id starting with `missing` as unknown.
struct Stub;

impl Handler for Stub {
    async fn handle(&self, request: Request) -> Response {
        match request {
            Request::List => Response::Ok(Reply::Sessions {
                sessions: vec![fake("s1")],
            }),
            Request::Kill { id } if id.starts_with("missing") => Response::Err(RpcError::new(
                ErrorCode::UnknownSession,
                format!("no session {id}"),
            )),
            Request::Kill { .. } => Response::Ok(Reply::Done),
            Request::ReadScreen { id } => Response::Ok(Reply::Session { session: fake(&id) }),
            _ => Response::Ok(Reply::Done),
        }
    }
}

fn fake(id: &str) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        agent: "stub".into(),
        status: Status::Idle,
        since_ms: 0,
        status_for_ms: 0,
        rule: None,
        pid: None,
        exit_code: None,
        lost: false,
    }
}

/// Bind a server on a fresh pipe and run it in the background.
fn start(label: &str) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let name = unique_pipe_name(label);
    let server = Server::bind(&name).expect("bind should succeed on a fresh name");
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        server
            .run(Arc::new(Stub), async {
                let _ = stopped.await;
            })
            .await
            .expect("server should run cleanly");
    });
    (name, stop, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_gets_its_reply() {
    let (name, _stop, _task) = start("roundtrip");
    let mut client = Client::connect(&name).await.unwrap();

    match client.call(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => assert_eq!(sessions[0].id, "s1"),
        other => panic!("unexpected reply {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_connection_carries_many_requests_in_order() {
    let (name, _stop, _task) = start("sequential");
    let mut client = Client::connect(&name).await.unwrap();

    // If replies could ever be paired with the wrong request, this would show
    // it: each one names the id it was asked about.
    for i in 0..50 {
        let id = format!("s{i}");
        match client
            .call(Request::ReadScreen { id: id.clone() })
            .await
            .unwrap()
        {
            Reply::Session { session } => assert_eq!(session.id, id),
            other => panic!("unexpected reply {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn many_clients_are_served_at_once() {
    // Exercises the "create the next instance before handing this one off"
    // ordering. If there were a gap with no listening instance, some of these
    // connects would fail with "pipe not found".
    let (name, _stop, _task) = start("concurrent");

    let clients = (0..16).map(|i| {
        let name = name.clone();
        tokio::spawn(async move {
            let mut client = Client::connect(&name).await.unwrap();
            for j in 0..10 {
                let id = format!("c{i}-{j}");
                match client
                    .call(Request::ReadScreen { id: id.clone() })
                    .await
                    .unwrap()
                {
                    Reply::Session { session } => assert_eq!(session.id, id),
                    other => panic!("unexpected reply {other:?}"),
                }
            }
        })
    });

    for client in clients {
        client.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_error_arrives_as_a_typed_rpc_error() {
    let (name, _stop, _task) = start("rpc-error");
    let mut client = Client::connect(&name).await.unwrap();

    match client
        .call(Request::Kill {
            id: "missing-1".into(),
        })
        .await
    {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, ErrorCode::UnknownSession),
        other => panic!("expected an UnknownSession error, got {other:?}"),
    }

    // And the connection is still good afterwards.
    assert!(matches!(
        client.call(Request::List).await,
        Ok(Reply::Sessions { .. })
    ));
}

/// Write one raw frame and read one raw response, bypassing `Client`, so tests
/// can send things a well-behaved client never would.
async fn raw_exchange(
    pipe: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    payload: &[u8],
) -> Response {
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(payload);
    pipe.write_all(&frame).await.unwrap();

    let mut header = [0u8; 4];
    pipe.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
    pipe.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_mismatch_is_refused_clearly() {
    // Two separately built binaries can disagree about the protocol even though
    // they share a crate. The engine must say so, not guess.
    let (name, _stop, _task) = start("version");
    let mut pipe = ClientOptions::new().open(&name).unwrap();

    let future_version = PROTOCOL_VERSION + 1;
    let request = format!(r#"{{"version":{future_version},"method":"list"}}"#);

    match raw_exchange(&mut pipe, request.as_bytes()).await {
        Response::Err(error) => {
            assert_eq!(error.code, ErrorCode::VersionMismatch);
            assert!(error.message.contains(&format!("v{future_version}")));
        }
        other => panic!("expected a version error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_request_gets_an_error_and_the_connection_survives() {
    let (name, _stop, _task) = start("malformed");
    let mut pipe = ClientOptions::new().open(&name).unwrap();

    match raw_exchange(&mut pipe, b"this is not json").await {
        Response::Err(error) => assert_eq!(error.code, ErrorCode::BadRequest),
        other => panic!("expected BadRequest, got {other:?}"),
    }

    // Same connection, valid request: still answered.
    let valid = format!(r#"{{"version":{PROTOCOL_VERSION},"method":"list"}}"#);
    assert!(matches!(
        raw_exchange(&mut pipe, valid.as_bytes()).await,
        Response::Ok(Reply::Sessions { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_frame_drops_that_client_but_not_the_server() {
    let (name, _stop, _task) = start("oversized");

    // Announce a 4 GiB frame. The server must not try to buffer it.
    let mut hostile = ClientOptions::new().open(&name).unwrap();
    hostile.write_all(&u32::MAX.to_be_bytes()).await.unwrap();

    // The server explains, then hangs up.
    let mut header = [0u8; 4];
    hostile.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
    hostile.read_exact(&mut body).await.unwrap();
    let response: Response = serde_json::from_slice(&body).unwrap();
    assert!(matches!(response, Response::Err(ref e) if e.code == ErrorCode::BadRequest));

    let mut rest = [0u8; 1];
    let after = tokio::time::timeout(Duration::from_secs(5), hostile.read(&mut rest)).await;
    assert!(
        matches!(after, Ok(Ok(0)) | Ok(Err(_))),
        "the hostile connection should be closed, got {after:?}"
    );

    // Everyone else is unaffected.
    let mut client = Client::connect(&name).await.unwrap();
    assert!(matches!(
        client.call(Request::List).await,
        Ok(Reply::Sessions { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_vanishing_mid_request_does_not_hurt_the_server() {
    let (name, _stop, _task) = start("vanish");

    {
        // Half a header, then gone.
        let mut pipe = ClientOptions::new().open(&name).unwrap();
        pipe.write_all(&[0, 0]).await.unwrap();
    }

    let mut client = Client::connect(&name).await.unwrap();
    assert!(matches!(
        client.call(Request::List).await,
        Ok(Reply::Sessions { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_with_no_engine_says_so() {
    let name = unique_pipe_name("nobody-home");

    match Client::connect(&name).await {
        Err(ClientError::NotRunning(pipe)) => assert_eq!(pipe, name),
        Err(other) => panic!("expected NotRunning, got {other:?}"),
        Ok(_) => panic!("expected NotRunning, but connected"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_server_cannot_claim_a_name_already_in_use() {
    // `first_pipe_instance`: rather than silently becoming another instance of
    // someone else's pipe, binding fails loudly.
    let (name, _stop, _task) = start("squatter");

    assert!(
        Server::bind(&name).is_err(),
        "binding an existing pipe name must fail"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_the_accept_loop() {
    let (_name, stop, task) = start("shutdown");

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("server should stop promptly")
        .unwrap();
}

/// Records every request it receives, in order.
#[derive(Default)]
struct Recorder {
    seen: std::sync::Mutex<Vec<(Request, std::time::Instant)>>,
}

impl Handler for Recorder {
    async fn handle(&self, request: Request) -> Response {
        self.seen
            .lock()
            .unwrap()
            .push((request, std::time::Instant::now()));
        Response::Ok(Reply::Done)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_line_types_the_text_then_presses_enter_separately() {
    // Not one write of "text\r" -- most TUIs read that as a paste, and a pasted
    // Enter is just a newline. Two requests, the Enter after a pause.
    let name = unique_pipe_name("send-line");
    let server = Server::bind(&name).unwrap();
    let recorder = Arc::new(Recorder::default());
    let (_stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(server.run(Arc::clone(&recorder), async {
        let _ = stopped.await;
    }));

    let mut client = Client::connect(&name).await.unwrap();
    client.send_line("a1", "echo hello").await.unwrap();

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(
        seen[0].0,
        Request::SendText {
            id: "a1".into(),
            text: "echo hello".into()
        }
    );
    assert_eq!(
        seen[1].0,
        Request::SendText {
            id: "a1".into(),
            text: "\r".into()
        }
    );
    let gap = seen[1].1 - seen[0].1;
    assert!(
        gap >= argus_control_server::ENTER_GAP - Duration::from_millis(10),
        "Enter followed the text after only {gap:?}"
    );
}
