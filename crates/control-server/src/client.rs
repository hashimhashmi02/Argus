//! The connecting side of the pipe.

use std::io;
use std::time::Duration;

use argus_protocol::{
    CodecError, Envelope, FrameDecoder, Reply, Request, Response, RpcError, encode,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

/// `ERROR_FILE_NOT_FOUND`: no pipe by that name exists.
///
/// Spelled as a number rather than pulled in from `windows-sys`, because two
/// constants are not worth a dependency on a crate this size.
const ERROR_FILE_NOT_FOUND: i32 = 2;

/// `ERROR_PIPE_BUSY`: the pipe exists, but every instance is taken.
///
/// Normal and transient. The server makes a new instance as soon as it accepts
/// each client, so this only happens in the brief window while it does.
const ERROR_PIPE_BUSY: i32 = 231;

/// How long to keep retrying a busy pipe before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing is listening on the pipe: the engine is not running.
    ///
    /// Its own variant because it is by far the most common failure, and the
    /// right message for it ("start the engine") is completely different from
    /// the right message for anything else.
    #[error("the Argus engine is not running (no pipe at {0})")]
    NotRunning(String),

    #[error("pipe I/O failed")]
    Io(#[from] io::Error),

    #[error("could not frame the message")]
    Codec(#[from] CodecError),

    /// The engine answered, and the answer was an error.
    #[error(transparent)]
    Rpc(#[from] RpcError),

    /// The engine closed the connection without replying.
    #[error("the engine closed the connection")]
    Closed,
}

/// A connection to the engine.
pub struct Client {
    pipe: NamedPipeClient,
    decoder: FrameDecoder,
    buf: Vec<u8>,
}

impl Client {
    /// Connect, retrying briefly if the pipe is momentarily busy.
    pub async fn connect(name: &str) -> Result<Self, ClientError> {
        let deadline = tokio::time::Instant::now() + BUSY_TIMEOUT;

        let pipe = loop {
            match ClientOptions::new().open(name) {
                Ok(pipe) => break pipe,
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                    return Err(ClientError::NotRunning(name.to_string()));
                }
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e.into()),
            }
        };

        Ok(Self {
            pipe,
            decoder: FrameDecoder::new(),
            buf: vec![0u8; 8 * 1024],
        })
    }

    /// Send one request and wait for its reply.
    ///
    /// An engine-side error comes back as `Err(ClientError::Rpc(..))`, so a
    /// caller can use `?` throughout and branch on the error code only where it
    /// matters.
    pub async fn call(&mut self, request: Request) -> Result<Reply, ClientError> {
        let frame = encode(&Envelope::new(request))?;
        self.pipe.write_all(&frame).await?;

        loop {
            if let Some(response) = self.decoder.next_frame::<Response>()? {
                return Result::<Reply, RpcError>::from(response).map_err(ClientError::Rpc);
            }
            let n = self.pipe.read(&mut self.buf).await?;
            if n == 0 {
                return Err(ClientError::Closed);
            }
            self.decoder.push(&self.buf[..n]);
        }
    }
}
