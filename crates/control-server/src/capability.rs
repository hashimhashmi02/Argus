//! What a connection is allowed to ask for.
//!
//! Until now every connection could do everything, and that was safe for one
//! reason only: the named pipe's security descriptor meant nobody else could
//! open it. See [why a named pipe](../../../DESIGN.md#why-a-named-pipe). The
//! moment a connection can arrive from somewhere other than this user's own
//! session, that argument is gone and the engine has to hold the line itself.
//!
//! So authorization is a property of the *connection*, decided once when it is
//! accepted, and checked here before the request reaches a handler. One
//! function, [`Capability::allows`], is the whole policy: a reviewer can read
//! it in one sitting and a new `Request` variant cannot quietly become
//! reachable, because [`Capability::allows`] matches exhaustively and will not
//! compile until the new variant is placed on one side or the other.

use argus_protocol::{ErrorCode, Request, RpcError};

/// What one connection may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Everything. What a local connection over this user's own pipe gets.
    Full,

    /// Read one named session's terminal, and nothing else.
    ///
    /// Deliberately not "read-only": `list` is also read-only and would
    /// enumerate every session on the machine, which is not something a
    /// watcher of one session has any business learning.
    WatchSession { id: String },
}

impl Capability {
    /// Whether this connection may make this request.
    ///
    /// Matching is exhaustive on purpose. Adding a `Request` variant breaks
    /// this function, which is the point: deciding whether a remote peer can
    /// reach a new call should be a conscious edit, not an omission.
    pub fn allows(&self, request: &Request) -> bool {
        match self {
            Capability::Full => true,
            Capability::WatchSession { id } => match request {
                // The one remote capability. Scoped to its own session: a
                // token for one session must not read another.
                Request::Watch { id: wanted } => wanted == id,

                // `spawn` runs a program as the user. Reachable from off this
                // machine, it is remote code execution, and no authentication
                // story changes that.
                Request::Spawn { .. } => false,

                // These stop or restart work that is not the caller's.
                Request::Kill { .. } | Request::Shutdown | Request::Resume { .. } => false,

                // Typing into an agent that edits files and runs commands is
                // most of the way back to `spawn`. If it is ever allowed it
                // will be a capability of its own, granted per session, not a
                // consequence of being allowed to watch.
                Request::SendText { .. } | Request::Respond { .. } => false,

                // Reading, but wider than one session: `list` and `agents`
                // describe the machine, not the session being watched.
                Request::List | Request::Agents => false,

                // About the session, but still more than watching it. Left out
                // of the first slice so that it is added deliberately, with a
                // reason, rather than by default.
                Request::ReadScreen { .. } | Request::Resize { .. } => false,
            },
        }
    }

    /// The refusal to send when [`allows`](Self::allows) says no.
    ///
    /// Says what is permitted rather than what was attempted. A peer holding a
    /// session token learns nothing from the answer that it did not already
    /// know, so the message cannot be used to probe what exists.
    pub fn refusal(&self, request: &Request) -> RpcError {
        let _ = request;
        let detail = match self {
            Capability::Full => "this connection is not permitted to do that".to_string(),
            Capability::WatchSession { id } => {
                format!("this connection may only watch session {id}")
            }
        };
        RpcError::new(ErrorCode::Forbidden, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_only() -> Capability {
        Capability::WatchSession {
            id: "3fa1c2d4".into(),
        }
    }

    /// Every request a caller could make, so the tests below cannot pass by
    /// forgetting one.
    fn every_request() -> Vec<Request> {
        vec![
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
                text: "rm -rf /".into(),
            },
            Request::Respond {
                id: "3fa1c2d4".into(),
                approve: true,
            },
            Request::Resize {
                id: "3fa1c2d4".into(),
                cols: 80,
                rows: 24,
            },
            Request::ReadScreen {
                id: "3fa1c2d4".into(),
            },
            Request::Kill {
                id: "3fa1c2d4".into(),
            },
            Request::Resume {
                id: "3fa1c2d4".into(),
                cols: None,
                rows: None,
            },
            Request::Watch {
                id: "3fa1c2d4".into(),
            },
        ]
    }

    #[test]
    fn a_local_connection_may_do_everything() {
        for request in every_request() {
            assert!(Capability::Full.allows(&request), "{request:?}");
        }
    }

    #[test]
    fn a_session_token_may_only_watch_that_session() {
        let cap = watch_only();
        for request in every_request() {
            let expected = matches!(&request, Request::Watch { id } if id == "3fa1c2d4");
            assert_eq!(cap.allows(&request), expected, "{request:?}");
        }
    }

    #[test]
    fn a_session_token_cannot_watch_another_session() {
        assert!(!watch_only().allows(&Request::Watch {
            id: "9b0e1172".into()
        }));
    }

    #[test]
    fn spawn_is_never_reachable_with_a_session_token() {
        // The one that would be remote code execution. Spelled out on its own
        // so it survives any rewrite of the loop above.
        assert!(!watch_only().allows(&Request::Spawn {
            agent: "shell".into(),
            cwd: None,
            cols: None,
            rows: None,
            worktree: None,
        }));
    }

    #[test]
    fn a_refusal_names_what_is_allowed_not_what_was_tried() {
        let error = watch_only().refusal(&Request::Spawn {
            agent: "shell".into(),
            cwd: Some(r"C:\secrets".into()),
            cols: None,
            rows: None,
            worktree: None,
        });
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert!(error.message.contains("3fa1c2d4"), "{}", error.message);
        assert!(!error.message.contains("secrets"), "{}", error.message);
        assert!(!error.message.contains("spawn"), "{}", error.message);
    }
}
