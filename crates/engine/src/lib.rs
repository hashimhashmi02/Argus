//! The engine: the process that owns every live session.
//!
//! This is the composition root. Every other crate was built to know as little
//! as possible about the rest; this one knows all of them, and its job is to
//! connect them -- manifests to sessions, sessions to the registry, the registry
//! to the control API.
//!
//! The binary in `main.rs` is deliberately thin. It parses two arguments and
//! runs the server. Everything with behaviour lives here, so the integration
//! tests can drive a real engine over a real pipe without a command line in the
//! way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use argus_control_server::Handler;
use argus_manifest::Agent;
use argus_protocol::{ErrorCode, Reply, Request, Response, RpcError, ScreenView, SessionInfo};
use argus_reducer::Timestamp;
use argus_registry::{Entry, LookupError, Registry};
use argus_session::{Session, SessionConfig, SessionHandle};

/// Every agent the engine can launch, and every session it has launched.
pub struct Engine {
    /// Keyed by manifest id. A `BTreeMap` so anything listing agents does so in
    /// a stable order.
    agents: BTreeMap<String, Arc<Agent>>,
    registry: Registry<SessionHandle>,
}

impl Engine {
    /// An engine that can launch exactly these agents.
    pub fn new(agents: impl IntoIterator<Item = Agent>) -> Result<Self> {
        let mut by_id = BTreeMap::new();
        for agent in agents {
            let id = agent.id().to_string();
            // Two manifests claiming one id would make `spawn` silently pick
            // whichever was loaded last. Refusing to start is better than
            // launching the wrong program.
            if by_id.insert(id.clone(), Arc::new(agent)).is_some() {
                bail!("two manifests declare the agent id `{id}`");
            }
        }
        Ok(Self {
            agents: by_id,
            registry: Registry::new(),
        })
    }

    /// Load every `*.json` manifest in a directory.
    ///
    /// Fails on the first broken one, naming the file. A daemon that started
    /// with one agent quietly missing would surface the problem much later, as
    /// "unknown agent", with nothing pointing back at the typo that caused it.
    pub fn load_manifest_dir(dir: &Path) -> Result<Self> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("could not read manifest directory {}", dir.display()))?;

        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        // Sorted so that a duplicate-id error is reported the same way every
        // time, rather than depending on directory iteration order.
        paths.sort();

        let agents = paths
            .iter()
            .map(|path| Agent::from_path(path).with_context(|| format!("in {}", path.display())))
            .collect::<Result<Vec<_>>>()?;

        if agents.is_empty() {
            bail!("no manifests found in {}", dir.display());
        }
        Self::new(agents)
    }

    /// Ids of every agent this engine can launch, sorted.
    pub fn agent_ids(&self) -> impl Iterator<Item = &str> {
        self.agents.keys().map(String::as_str)
    }

    async fn dispatch(&self, request: Request) -> Result<Reply, RpcError> {
        match request {
            Request::List => Ok(Reply::Sessions {
                sessions: self.registry.list().iter().map(info).collect(),
            }),

            Request::Spawn {
                agent,
                cwd,
                cols,
                rows,
            } => self.spawn(&agent, cwd, cols, rows),

            Request::SendText { id, text } => {
                let entry = self.find(&id)?;
                if !entry.handle.send_text(&text).await {
                    // Typing into a session that has already ended is almost
                    // always a mistake worth telling the user about -- they
                    // think they are talking to something that is not there.
                    return Err(finished(&entry));
                }
                Ok(Reply::Done)
            }

            Request::Resize { id, cols, rows } => {
                let entry = self.find(&id)?;
                if !entry.handle.resize(cols, rows).await {
                    return Err(finished(&entry));
                }
                Ok(Reply::Done)
            }

            Request::ReadScreen { id } => {
                let entry = self.find(&id)?;
                let snapshot = entry.handle.snapshot();
                let (cols, rows) = snapshot.screen.size();
                let (cursor_row, cursor_col) = snapshot.screen.cursor();
                Ok(Reply::Screen {
                    screen: ScreenView {
                        session: info(&entry),
                        cols,
                        rows,
                        cursor_row,
                        cursor_col,
                        lines: snapshot.screen.lines().to_vec(),
                    },
                })
            }

            Request::Kill { id } => {
                let entry = self.find(&id)?;
                // Idempotent: killing something already dead succeeds. The
                // caller wanted it stopped, and it is. Making a script check
                // status before every kill to avoid a spurious error would just
                // move a race from here into every caller.
                let _ = entry.handle.kill().await;
                Ok(Reply::Done)
            }
        }
    }

    fn spawn(
        &self,
        agent_id: &str,
        cwd: Option<String>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<Reply, RpcError> {
        let agent = self.agents.get(agent_id).ok_or_else(|| {
            let known: Vec<&str> = self.agent_ids().collect();
            RpcError::new(
                ErrorCode::UnknownAgent,
                format!("no agent `{agent_id}`; known agents: {}", known.join(", ")),
            )
        })?;

        let mut config = SessionConfig::default();
        if let (Some(cols), Some(rows)) = (cols, rows) {
            config = config.size(cols, rows);
        }

        let cwd = cwd.map(PathBuf::from);
        if let Some(dir) = &cwd {
            // Checked here so the error says what is actually wrong. Left to the
            // OS, a missing directory surfaces as a generic spawn failure that
            // names the program, not the path.
            if !dir.is_dir() {
                return Err(RpcError::new(
                    ErrorCode::BadRequest,
                    format!("working directory {} does not exist", dir.display()),
                ));
            }
            config = config.cwd(dir);
        }

        let session = Session::spawn(Arc::clone(agent), config)
            .map_err(|e| RpcError::new(ErrorCode::SpawnFailed, format!("{e:#}")))?;

        // The `Session` value is dropped here and the session keeps running --
        // its task owns everything it needs. The registry keeps a handle, which
        // is all anything will ever need to reach it.
        let id = self.registry.insert(agent_id, cwd, session.handle());
        let entry = self.find(&id)?;
        Ok(Reply::Session {
            session: info(&entry),
        })
    }

    fn find(&self, id: &str) -> Result<Entry<SessionHandle>, RpcError> {
        self.registry.find(id).map_err(|error| match error {
            LookupError::NotFound(_) => RpcError::new(ErrorCode::UnknownSession, error.to_string()),
            // The request was understood, but does not say which session it
            // means. The message lists the candidates.
            LookupError::Ambiguous { .. } => {
                RpcError::new(ErrorCode::BadRequest, error.to_string())
            }
        })
    }
}

impl Handler for Engine {
    async fn handle(&self, request: Request) -> Response {
        self.dispatch(request).await.into()
    }
}

fn info(entry: &Entry<SessionHandle>) -> SessionInfo {
    let snapshot = entry.handle.snapshot();
    SessionInfo {
        id: entry.id.clone(),
        agent: entry.agent.clone(),
        status: snapshot.status,
        since_ms: snapshot.since.saturating_since(Timestamp::ZERO).as_millis() as u64,
        status_for_ms: entry.handle.status_age().as_millis() as u64,
        rule: snapshot.rule,
        pid: snapshot.pid,
        exit_code: snapshot.exit_code,
    }
}

fn finished(entry: &Entry<SessionHandle>) -> RpcError {
    RpcError::new(
        ErrorCode::SessionFinished,
        format!("session {} has already finished", entry.id),
    )
}
