//! The connecting side of the pipe.

use std::io;
use std::time::Duration;

use argus_protocol::{
    CodecError, Envelope, ErrorCode, FrameDecoder, Reply, Request, Response, RpcError, WatchEvent,
    encode,
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

/// The pause between typing a line and pressing Enter. See [`Client::send_line`].
///
/// Only needs to be long enough for the two writes to reach the child as
/// separate reads; 100ms is far more than that and too short for a person to
/// notice.
pub const ENTER_GAP: Duration = Duration::from_millis(100);

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

        match self.next_frame::<Response>().await? {
            Some(response) => Result::<Reply, RpcError>::from(response).map_err(ClientError::Rpc),
            None => Err(ClientError::Closed),
        }
    }

    /// Type a line into a session and press Enter, the way a person would.
    ///
    /// Two requests, not one. Text and Enter written together are read by most
    /// modern TUIs as a *paste*, and inside a paste Enter is just a newline: the
    /// line is typed and never submitted. Found by driving a real Claude Code
    /// session, where a prompt sat unsubmitted for a full minute. A person
    /// presses Enter as a separate keystroke, so this does too.
    ///
    /// Lives in the client rather than in each caller so the CLI and the UI
    /// cannot drift apart on it -- the lesson was expensive enough to learn once.
    pub async fn send_line(&mut self, id: &str, text: &str) -> Result<(), ClientError> {
        self.call(Request::SendText {
            id: id.to_string(),
            text: text.to_string(),
        })
        .await?;
        tokio::time::sleep(ENTER_GAP).await;
        // `\r`, not `\n`: Enter on a real terminal sends a carriage return, and
        // a program reading raw input may ignore a line feed entirely.
        self.call(Request::SendText {
            id: id.to_string(),
            text: "\r".to_string(),
        })
        .await?;
        Ok(())
    }

    /// Start following a session's terminal.
    ///
    /// Takes the client by value: once the engine starts streaming, this
    /// connection carries nothing else. Open another `Client` to type into the
    /// session.
    pub async fn watch(mut self, id: &str) -> Result<Watcher, ClientError> {
        match self.call(Request::Watch { id: id.to_string() }).await? {
            Reply::Watching => Ok(Watcher { client: self }),
            other => Err(ClientError::Rpc(RpcError::new(
                ErrorCode::Internal,
                format!("the engine answered `watch` with {other:?}"),
            ))),
        }
    }

    /// The next complete frame, reading as much as it takes. `None` when the
    /// engine closes the connection between frames.
    async fn next_frame<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, ClientError> {
        loop {
            if let Some(frame) = self.decoder.next_frame::<T>()? {
                return Ok(Some(frame));
            }
            let n = self.pipe.read(&mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.decoder.push(&self.buf[..n]);
        }
    }
}

/// A connection that is following a session. See [`Client::watch`].
///
/// Dropping it hangs up, which is how the engine learns to stop sending.
pub struct Watcher {
    client: Client,
}

impl Watcher {
    /// The next event, waiting as long as the session stays quiet.
    ///
    /// `None` once the engine has closed the stream, which it does straight
    /// after [`WatchEvent::Ended`].
    pub async fn next(&mut self) -> Result<Option<WatchEvent>, ClientError> {
        self.client.next_frame().await
    }
}
