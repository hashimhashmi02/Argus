//! The listening side of the pipe.

use std::future::Future;
use std::io;
use std::sync::Arc;

use argus_protocol::{
    CodecError, Envelope, ErrorCode, FrameDecoder, PROTOCOL_VERSION, Reply, Request, Response,
    RpcError, WatchEvent, encode,
};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;

/// Whatever answers requests. The engine implements this; tests implement a
/// stub.
///
/// A trait rather than a hard dependency on the engine so this crate can be
/// tested on its own: the interesting failures here are about pipes and
/// framing, and they should not need a real agent process to reproduce.
///
/// Spelled with `impl Future + Send` rather than `async fn` because each
/// connection runs on its own spawned task, which requires the future to be
/// `Send`. An `async fn` in a trait does not promise that, and the error when it
/// turns out not to be is a long way from the cause.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> impl Future<Output = Response> + Send;

    /// Start streaming a session's terminal, for [`Request::Watch`].
    ///
    /// The events go out on the connection in order until the channel closes,
    /// which a handler does after sending [`WatchEvent::Ended`]. Dropping the
    /// receiver -- when the client hangs up -- is how a handler learns to stop.
    ///
    /// Defaults to refusing, so a handler that has nothing to stream, like the
    /// tests' stubs, does not have to say so.
    fn watch(
        &self,
        id: String,
    ) -> impl Future<Output = Result<mpsc::Receiver<WatchEvent>, RpcError>> + Send {
        async move {
            Err(RpcError::new(
                ErrorCode::BadRequest,
                format!("this server cannot watch `{id}`"),
            ))
        }
    }
}

/// A bound pipe, ready to accept clients.
///
/// Binding and running are separate steps so that by the time `bind` returns,
/// the pipe exists and a client can connect -- no race between "the server
/// started" and "the server is actually listening".
pub struct Server {
    name: String,
    /// The instance waiting for the next client.
    next: NamedPipeServer,
}

impl Server {
    /// Create the pipe, failing if anything already owns that name.
    pub fn bind(name: impl Into<String>) -> io::Result<Self> {
        let name = name.into();
        let next = ServerOptions::new()
            // Fail rather than quietly becoming one more instance of a pipe that
            // some other process already created under our name. A process that
            // squats on this name receives our clients' requests -- including
            // `spawn`, which runs a program as the user. That cannot be fully
            // prevented from the server side, but it can be made loud: the
            // daemon refuses to start, instead of half-working alongside an
            // impostor.
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        Ok(Self { name, next })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Accept and serve clients until `shutdown` completes.
    pub async fn run<H: Handler>(
        mut self,
        handler: Arc<H>,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        tokio::pin!(shutdown);

        loop {
            // `connect` is asynchronous -- tokio drives ConnectNamedPipe with
            // overlapped I/O -- so waiting for a client parks this task, not a
            // runtime worker. That is what satisfies the "accept() must not block
            // the async pool" rule from DESIGN.md. A synchronous
            // ConnectNamedPipe here would hold a worker for as long as nobody
            // connected, which on a daemon is most of its life.
            tokio::select! {
                connected = self.next.connect() => connected?,
                () = &mut shutdown => return Ok(()),
            }

            // Create the next instance *before* handing this one off. Between a
            // client connecting and a new instance existing, the pipe has no
            // listening instance at all, and any client arriving in that gap gets
            // a "pipe not found" error even though the daemon is running fine.
            // Making the replacement first closes the gap.
            let replacement = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)?;
            let client = std::mem::replace(&mut self.next, replacement);

            tokio::spawn(serve_connection(client, Arc::clone(&handler)));
        }
    }
}

/// Serve one client until it disconnects.
async fn serve_connection<H: Handler>(mut pipe: NamedPipeServer, handler: Arc<H>) {
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 8 * 1024];

    loop {
        // Answer every complete request already buffered before reading again,
        // because one read can carry several frames.
        loop {
            let response = match decoder.next_frame::<Envelope>() {
                Ok(Some(envelope)) => match watch_target(&envelope) {
                    // A watch takes the connection over for good, if it starts.
                    Some(id) => match handler.watch(id).await {
                        Ok(events) => {
                            if write_frame(&mut pipe, &Response::Ok(Reply::Watching))
                                .await
                                .is_ok()
                            {
                                stream_events(&mut pipe, events).await;
                            }
                            return;
                        }
                        Err(error) => Response::Err(error),
                    },
                    None => dispatch(&*handler, envelope).await,
                },
                Ok(None) => break,
                Err(error) => {
                    let fatal = error.is_fatal();
                    let response = codec_error_response(&error);
                    // Best effort: tell the client why before hanging up on it.
                    let _ = write_frame(&mut pipe, &response).await;
                    if fatal {
                        return;
                    }
                    continue;
                }
            };

            if write_frame(&mut pipe, &response).await.is_err() {
                return;
            }
        }

        match pipe.read(&mut buf).await {
            // Zero bytes is a clean disconnect. An error is a dirty one. Either
            // way this connection is over, and only this connection: a client
            // that crashes mid-request must not take the daemon with it.
            Ok(0) | Err(_) => return,
            Ok(n) => decoder.push(&buf[..n]),
        }
    }
}

async fn dispatch<H: Handler>(handler: &H, envelope: Envelope) -> Response {
    if envelope.version != PROTOCOL_VERSION {
        return Response::Err(RpcError::new(
            ErrorCode::VersionMismatch,
            format!(
                "client speaks protocol v{}, this engine speaks v{PROTOCOL_VERSION}; \
                 rebuild or upgrade one side",
                envelope.version
            ),
        ));
    }
    handler.handle(envelope.request).await
}

/// The session a request asks to watch, if it is a watch this server should
/// stream. One written against another protocol version goes through
/// `dispatch` like anything else, to be refused there with the usual message.
fn watch_target(envelope: &Envelope) -> Option<String> {
    match &envelope.request {
        Request::Watch { id } if envelope.version == PROTOCOL_VERSION => Some(id.clone()),
        _ => None,
    }
}

/// Forward watch events to the client until either side is finished.
///
/// The client sends nothing on a watching connection, so the pipe is read only
/// to notice it hanging up. That matters for a quiet session: without the
/// read, a client that left would only be discovered at the next write, which
/// for an idle agent might be hours away -- and until then the engine would
/// keep a subscription open for nobody.
async fn stream_events(pipe: &mut NamedPipeServer, mut events: mpsc::Receiver<WatchEvent>) {
    enum Next {
        Event(Option<WatchEvent>),
        Readable(io::Result<()>),
    }

    let mut scratch = [0u8; 256];
    loop {
        // Decided first and acted on after, so the two futures borrowing the
        // pipe are gone before it is written to.
        let next = tokio::select! {
            event = events.recv() => Next::Event(event),
            ready = pipe.readable() => Next::Readable(ready),
        };
        match next {
            Next::Event(Some(event)) => {
                if write_frame(pipe, &event).await.is_err() {
                    return;
                }
            }
            // The handler is done: the session ended, and `Ended` has gone out.
            Next::Event(None) => return,
            Next::Readable(Err(_)) => return,
            Next::Readable(Ok(())) => match pipe.try_read(&mut scratch) {
                // Zero bytes is the client hanging up. Anything it does send is
                // ignored: there is nothing it can ask on this connection.
                Ok(0) => return,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            },
        }
    }
}

fn codec_error_response(error: &CodecError) -> Response {
    Response::Err(RpcError::new(ErrorCode::BadRequest, error.to_string()))
}

async fn write_frame(pipe: &mut NamedPipeServer, message: &impl Serialize) -> io::Result<()> {
    let frame = encode(message).map_err(io::Error::other)?;
    pipe.write_all(&frame).await
}
