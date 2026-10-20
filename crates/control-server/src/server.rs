//! The listening side of the pipe.

use std::future::Future;
use std::io;
use std::sync::Arc;

use argus_protocol::{
    CodecError, Envelope, ErrorCode, FrameDecoder, PROTOCOL_VERSION, Request, Response, RpcError,
    encode,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

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
                Ok(Some(envelope)) => dispatch(&*handler, envelope).await,
                Ok(None) => break,
                Err(error) => {
                    let fatal = error.is_fatal();
                    let response = codec_error_response(&error);
                    // Best effort: tell the client why before hanging up on it.
                    let _ = write_response(&mut pipe, &response).await;
                    if fatal {
                        return;
                    }
                    continue;
                }
            };

            if write_response(&mut pipe, &response).await.is_err() {
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

fn codec_error_response(error: &CodecError) -> Response {
    Response::Err(RpcError::new(ErrorCode::BadRequest, error.to_string()))
}

async fn write_response(pipe: &mut NamedPipeServer, response: &Response) -> io::Result<()> {
    let frame = encode(response).map_err(io::Error::other)?;
    pipe.write_all(&frame).await
}
