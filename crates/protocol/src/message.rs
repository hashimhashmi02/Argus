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
        /// Run the agent in a git worktree of `cwd`'s repository, on this
        /// branch: created from the repository's HEAD if it does not exist,
        /// reused if Argus already made a worktree for it.
        ///
        /// Additive: an older engine ignores the field and spawns in `cwd`
        /// itself, which a client can tell from the reply's `branch`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
    },

    /// Type text at a session, as if from its keyboard.
    SendText { id: String, text: String },

    /// Change a session's terminal size.
    Resize { id: String, cols: u16, rows: u16 },

    /// The emulated screen, exactly as the user would see it.
    ReadScreen { id: String },

    /// Stop a session's child process.
    Kill { id: String },

    /// Every agent the engine can launch.
    ///
    /// Added for the UI, which offers a list to pick from instead of asking for
    /// a manifest id to be typed.
    Agents,

    /// Answer the prompt a session is blocked on.
    ///
    /// The client says *what* -- approve or deny -- and the engine works out
    /// *how*, from the agent's manifest. One agent approves with Enter, another
    /// wants `y`, a third a menu choice; that knowledge lives in the manifest,
    /// and a client hardcoding "press Enter" would be wrong for every agent that
    /// is not the one it was tested against.
    ///
    /// Added after v1 without a version bump, like `Agents`. A newer client
    /// sending either to an older engine gets a clear `bad_request` back -- an
    /// unknown method -- rather than a misread, which is the test for whether a
    /// change needs one.
    Respond { id: String, approve: bool },

    /// Follow a session's terminal: its screen as it is now, then everything
    /// it prints.
    ///
    /// The one call that breaks "one request, one response". The reply is
    /// [`Reply::Watching`], and from then on the connection carries
    /// [`WatchEvent`] frames from the engine until the session ends or the
    /// client hangs up. Nothing more is read from the client: a connection
    /// that is watching is only for watching, and the client opens another
    /// to type. Keeping the two apart means a request can never be answered
    /// out of order behind a backlog of output.
    ///
    /// Additive, like `Agents`: an older engine answers `bad_request`.
    Watch { id: String },

    /// Start a finished session again, through its manifest's `resume.args`,
    /// in the folder it ran in and with the conversation it had.
    ///
    /// The session keeps its id and its place in the list; the reply is the
    /// session, live again. A session still running is returned as it is --
    /// the caller wanted it running, and it is. One that cannot be resumed
    /// (see [`SessionInfo::resumable`]) is refused with `bad_request` and the
    /// reason.
    ///
    /// Additive, like `Agents`: an older engine answers `bad_request`.
    Resume {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
    },

    /// Stop every running session, then the engine itself.
    ///
    /// The sessions are killed rather than left to die with the engine, so
    /// they are remembered as finished -- and resumable, where their agent
    /// allows it -- not lost. Answered with `done` before the engine exits.
    ///
    /// Additive, like `Agents`: an older engine answers `bad_request`.
    Shutdown,
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
    Agents {
        agents: Vec<AgentInfo>,
    },
    /// The request was carried out and there is nothing more to say.
    Done,
    /// The answer to [`Request::Watch`]: [`WatchEvent`] frames follow.
    Watching,
}

/// What a watching connection receives, one frame each.
///
/// Text rather than bytes, because frames are JSON. The engine never splits a
/// character between two events, so each `data` is valid UTF-8 on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WatchEvent {
    /// Clear the terminal and write this: it draws the session's screen,
    /// colours, cursor and input modes as they are now, in a terminal of
    /// `cols` by `rows`. Always the first event, and sent again whenever the
    /// session is resized or the watcher fell too far behind to catch up.
    Repaint { cols: u16, rows: u16, data: String },
    /// The session printed this, exactly as it came from the agent.
    Output { data: String },
    /// The session has finished. The last event on the connection.
    Ended,
}

/// An agent the engine can launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// The manifest id, which `spawn` takes.
    pub id: String,
    /// What a person should see.
    pub display_name: String,
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
    /// How long the session has held its current status, in milliseconds.
    ///
    /// Computed by the engine at the moment of the reply, because a client
    /// cannot: it knows `since_ms` relative to spawn, but not how long ago spawn
    /// was. And this is the number that matters -- "needs you *for four
    /// minutes*" is what tells someone which window to look at first.
    ///
    /// Added after v1 shipped, and deliberately without a version bump. An
    /// older client ignores the unknown field, and a newer client talking to an
    /// older engine reads the `serde(default)` zero. Neither side misreads
    /// anything, which is the test for whether a change needs a bump.
    #[serde(default)]
    pub status_for_ms: u64,
    /// The manifest rule responsible for the current status, if the screen
    /// decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    /// The session was still running when a previous engine stopped, and its
    /// process went with it.
    ///
    /// Always paired with `status: done` and no exit code. A separate flag
    /// rather than a fifth `Status`, because `Status` is the reducer's judgement
    /// of a *live* session and this is a fact about history -- no live session
    /// is ever "lost".
    ///
    /// Additive, like `status_for_ms`: omitted when false, defaulted when
    /// absent, so no version bump.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,
    /// The session has finished and [`Request::Resume`] would start it again:
    /// its agent says how to resume, and the engine knows the folder (and,
    /// where the agent needs it, the conversation) to resume it in.
    ///
    /// Decided by the engine so that a client only has to show it. Additive,
    /// like `lost`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumable: bool,
    /// The folder the agent runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The branch of the git worktree Argus made for the session, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
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
