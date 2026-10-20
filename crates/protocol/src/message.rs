//! What goes over the wire.

use argus_reducer::Status;
use serde::{Deserialize, Serialize};

/// The protocol revision this build speaks.
///
/// Bump on any change an older peer could misread. Sharing this crate stops the
/// engine and a client from drifting apart *at compile time* -- but only when
/// both were built from the same source. An `argus-cli` from last month talking
/// to this week's daemon was not, and nothing in the type system can see that.
/// So every request carries the version, and the server refuses a mismatch
/// with a clear error instead of misinterpreting the bytes.
pub const PROTOCOL_VERSION: u32 = 1;

/// A request as sent: the version it was written against, plus the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    #[serde(flatten)]
    pub request: Request,
}

impl Envelope {
    /// Wrap a request with this build's protocol version.
    pub fn new(request: Request) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request,
        }
    }
}

/// Every call the engine answers.
///
/// Internally tagged, so on the wire a call reads as
/// `{"version":1,"method":"read_screen","id":"3fa1c2d4"}`. Readable in a log, and
/// writable by hand from any language for debugging -- which matters for an API
/// whose only other client, for now, is a CLI.
///
/// One request, one response, in order, per connection. No request ids and no
/// pipelining. Every client in sight is interactive, and a strict
/// request-then-reply rhythm means a confused client cannot receive the answer
/// to a different question than the one it thinks it asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// Every known session, live or finished.
    List,

    /// Launch an agent described by a manifest.
    Spawn {
        /// Manifest id, e.g. `"claude-code"` or `"shell"`.
        agent: String,
        /// Working directory for the agent. Defaults to the engine's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
    },

    /// Type text at a session, as if from its keyboard.
    SendText { id: String, text: String },

    /// Change a session's terminal size.
    Resize { id: String, cols: u16, rows: u16 },

    /// The emulated screen, exactly as the user would see it.
    ReadScreen { id: String },

    /// Stop a session's child process.
    Kill { id: String },
}

/// The answer to one request.
///
/// A named enum rather than `Result<Reply, RpcError>`, so the JSON says
/// `{"ok": ...}` or `{"err": ...}` in words a person reading a log can act on,
/// instead of serde's default `{"Ok": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Ok(Reply),
    Err(RpcError),
}

impl From<Result<Reply, RpcError>> for Response {
    fn from(result: Result<Reply, RpcError>) -> Self {
        match result {
            Ok(reply) => Response::Ok(reply),
            Err(error) => Response::Err(error),
        }
    }
}

impl From<Response> for Result<Reply, RpcError> {
    fn from(response: Response) -> Self {
        match response {
            Response::Ok(reply) => Ok(reply),
            Response::Err(error) => Err(error),
        }
    }
}

/// A successful result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Session {
        session: SessionInfo,
    },
    Screen {
        screen: ScreenView,
    },
    /// The request was carried out and there is nothing more to say.
    Done,
}

/// One session, summarised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// The manifest id this session was spawned from.
    pub agent: String,
    pub status: Status,
    /// Milliseconds since spawn at which the current status began.
    ///
    /// Relative rather than a wall-clock timestamp: it is all a client needs to
    /// render "idle for 3m", and it sidesteps every question about clock skew
    /// and time zones between processes.
    pub since_ms: u64,
    /// The manifest rule responsible for the current status, if the screen
    /// decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
}

/// An emulated screen, as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenView {
    pub session: SessionInfo,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    /// One string per row, trailing blanks trimmed.
    pub lines: Vec<String>,
}

/// A failed call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct RpcError {
    pub code: ErrorCode,
    /// Human-readable detail. Never parsed; branch on `code` instead.
    pub message: String,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Why a call failed, in a form a client can branch on.
///
/// Distinct codes rather than a single string because a client genuinely does
/// different things with them: an unknown session is a typo to report, a
/// version mismatch means "upgrade one side", and an internal error means the
/// daemon is in trouble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was written against a different protocol version.
    VersionMismatch,
    /// The request could not be parsed.
    BadRequest,
    /// No session has that id.
    UnknownSession,
    /// No manifest has that id.
    UnknownAgent,
    /// The agent's process could not be started.
    SpawnFailed,
    /// The session exists but has already finished.
    SessionFinished,
    /// Something went wrong inside the engine.
    Internal,
}
