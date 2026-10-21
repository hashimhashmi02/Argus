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
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use argus_control_server::Handler;
use argus_manifest::Agent;
use argus_protocol::{
    AgentInfo, ErrorCode, Reply, Request, Response, RpcError, ScreenView, SessionInfo, WatchEvent,
};
use argus_reducer::Timestamp;
use argus_registry::{Entry, LookupError, Registry, now_unix_ms};
use argus_session::{Repaint, Session, SessionConfig, SessionHandle, StreamItem, Watch};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Notify, mpsc};

mod persistence;

use persistence::Persistence;
pub use persistence::{StartupReport, default_state_path};

/// How often the persister checks for changes when nothing has prompted it.
///
/// Status changes happen inside session tasks the engine is not told about, so
/// they are picked up by this tick. A second is short enough that a crash loses
/// very little, and cheap because an unchanged registry is not rewritten.
const PERSIST_INTERVAL: Duration = Duration::from_secs(1);

/// How many watch events may wait for a slow client before the engine stops
/// reading the session's stream for it.
///
/// Small, because waiting here is how back-pressure reaches the session's
/// broadcast: a client that cannot keep up falls behind there, gets a fresh
/// repaint, and skips the backlog instead of replaying it.
const WATCH_DEPTH: usize = 64;

/// Every agent the engine can launch, and every session it has launched.
pub struct Engine {
    /// Keyed by manifest id. A `BTreeMap` so anything listing agents does so in
    /// a stable order.
    agents: BTreeMap<String, Arc<Agent>>,
    registry: Registry<SessionHandle>,
    /// `None` for an in-memory engine, as the tests mostly use.
    persistence: Option<Persistence>,
    /// Poked whenever the registry changes shape, so a new session reaches the
    /// disk immediately rather than at the next tick.
    dirty: Notify,
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
            persistence: None,
            dirty: Notify::new(),
        })
    }

    /// Remember sessions in `path`, restoring whatever it holds from a previous
    /// run.
    ///
    /// Sessions that were still running when the previous engine stopped come
    /// back as finished and lost: their processes went down with it. An
    /// unparseable file is quarantined rather than overwritten, and the report
    /// says where it went. An *unreadable* file is an error -- the engine
    /// refuses to start rather than risk writing over history it could not see.
    ///
    /// Writes the restored state straight back, so the file reflects "lost"
    /// even if this engine is itself killed before its first tick.
    pub fn with_state_file(mut self, path: &Path) -> Result<(Self, StartupReport)> {
        let (persistence, records, report) = Persistence::open(path)
            .with_context(|| format!("could not read the state file {}", path.display()))?;

        self.registry.restore(records);
        persistence
            .save_if_changed(&self.registry.list())
            .with_context(|| format!("could not write the state file {}", path.display()))?;

        self.persistence = Some(persistence);
        Ok((self, report))
    }

    /// Where this engine remembers sessions, if it does.
    pub fn state_path(&self) -> Option<&Path> {
        self.persistence.as_ref().map(Persistence::path)
    }

    /// Write the registry to the state file if anything changed.
    ///
    /// Blocking I/O, so it runs on the blocking pool rather than a runtime
    /// worker. Returns whether a write happened.
    pub async fn save(self: &Arc<Self>) -> Result<bool> {
        let engine = Arc::clone(self);
        tokio::task::spawn_blocking(move || match &engine.persistence {
            Some(p) => p.save_if_changed(&engine.registry.list()),
            None => Ok(false),
        })
        .await
        .context("state save panicked")?
        .context("could not save the state file")
    }

    /// Keep the state file up to date until `shutdown` completes, then save one
    /// last time.
    ///
    /// Wakes on every new session and once a second regardless. A failed save
    /// is reported and retried on the next wake rather than stopping the engine:
    /// a full disk should not take running agents down with it.
    pub async fn run_persister(self: Arc<Self>, shutdown: impl Future<Output = ()>) {
        if self.persistence.is_none() {
            return;
        }
        tokio::pin!(shutdown);
        let mut tick = tokio::time::interval(PERSIST_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                () = &mut shutdown => break,
                () = self.dirty.notified() => {}
                _ = tick.tick() => {}
            }
            if let Err(error) = self.save().await {
                eprintln!("[argus-engine] warning: {error:#}");
            }
        }

        if let Err(error) = self.save().await {
            eprintln!("[argus-engine] warning: final save failed: {error:#}");
        }
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
                // Typing into a session that has already ended is almost always
                // a mistake worth telling the user about -- they think they are
                // talking to something that is not there. A session restored
                // from a previous run has no handle at all, and is the same case.
                match &entry.handle {
                    Some(handle) if handle.send_text(&text).await => Ok(Reply::Done),
                    _ => Err(finished(&entry)),
                }
            }

            Request::Resize { id, cols, rows } => {
                let entry = self.find(&id)?;
                match &entry.handle {
                    Some(handle) if handle.resize(cols, rows).await => Ok(Reply::Done),
                    _ => Err(finished(&entry)),
                }
            }

            Request::ReadScreen { id } => {
                let entry = self.find(&id)?;
                let session = info(&entry);
                let Some(handle) = &entry.handle else {
                    // Screens are not persisted -- they would be most of the
                    // state file and stale by definition -- so a session from a
                    // previous run has none to show. An empty screen rather than
                    // an error, so `screen` on any listed id still says what
                    // happened to the session in its header.
                    return Ok(Reply::Screen {
                        screen: ScreenView {
                            session,
                            cols: 0,
                            rows: 0,
                            cursor_row: 0,
                            cursor_col: 0,
                            lines: Vec::new(),
                        },
                    });
                };
                let snapshot = handle.snapshot();
                let (cols, rows) = snapshot.screen.size();
                let (cursor_row, cursor_col) = snapshot.screen.cursor();
                Ok(Reply::Screen {
                    screen: ScreenView {
                        session,
                        cols,
                        rows,
                        cursor_row,
                        cursor_col,
                        lines: snapshot.screen.lines().to_vec(),
                    },
                })
            }

            Request::Agents => Ok(Reply::Agents {
                agents: self
                    .agents
                    .values()
                    .map(|agent| AgentInfo {
                        id: agent.id().to_string(),
                        display_name: agent.manifest().display_name.clone(),
                    })
                    .collect(),
            }),

            Request::Respond { id, approve } => {
                let entry = self.find(&id)?;
                // The keystrokes come from the manifest of the agent this
                // session was spawned from -- not from the client, which cannot
                // know them, and not from a default, which would be right for
                // one agent and wrong for the rest.
                let agent = self.agents.get(&entry.agent).ok_or_else(|| {
                    // Only possible for a session restored from a run whose
                    // manifests included an agent this run does not. A restored
                    // session is finished anyway, so say that rather than
                    // confusing the caller with a missing-agent error.
                    finished(&entry)
                })?;
                let keys = &agent.manifest().keys;
                let bytes = if approve {
                    keys.approve_bytes()
                } else {
                    keys.deny_bytes()
                };
                match &entry.handle {
                    Some(handle) if handle.send_bytes(bytes).await => Ok(Reply::Done),
                    _ => Err(finished(&entry)),
                }
            }

            // The server routes a watch to `Handler::watch`, because its answer
            // is a stream rather than one reply. Reaching here means something
            // called `handle` with it directly.
            Request::Watch { .. } => Err(RpcError::new(
                ErrorCode::BadRequest,
                "`watch` is answered as a stream, on a connection of its own",
            )),

            Request::Kill { id } => {
                let entry = self.find(&id)?;
                // Idempotent: killing something already dead -- or restored
                // from a previous run, which is the same thing -- succeeds. The
                // caller wanted it stopped, and it is. Making a script check
                // status before every kill would just move a race from here into
                // every caller.
                if let Some(handle) = &entry.handle {
                    let _ = handle.kill().await;
                }
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
        // Persist the new session now rather than at the next tick: a spawn is
        // the one change a person would be surprised to see missing after a
        // crash.
        self.dirty.notify_one();
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

    async fn watch(&self, id: String) -> Result<mpsc::Receiver<WatchEvent>, RpcError> {
        let entry = self.find(&id)?;
        // A session from a previous run has no terminal left to follow; say so
        // rather than streaming an empty screen.
        let handle = entry.handle.clone().ok_or_else(|| finished(&entry))?;
        let (events, receiver) = mpsc::channel(WATCH_DEPTH);
        tokio::spawn(forward_watch(handle, events));
        Ok(receiver)
    }
}

/// Pass a session's terminal on to one watching client, until either the
/// session ends or the client goes.
async fn forward_watch(handle: SessionHandle, events: mpsc::Sender<WatchEvent>) {
    loop {
        let Some(Watch {
            repaint,
            mut stream,
        }) = handle.watch().await
        else {
            // Finished before (or while) being watched. Its last screen
            // survives as text, which is worth showing before saying so.
            let screen = handle.snapshot().screen;
            let (cols, rows) = screen.size();
            let last = WatchEvent::Repaint {
                cols,
                rows,
                data: screen.repaint(),
            };
            if events.send(last).await.is_ok() {
                let _ = events.send(WatchEvent::Ended).await;
            }
            return;
        };

        if events.send(repaint_event(repaint)).await.is_err() {
            return;
        }

        loop {
            // Also waiting on the client, so one that leaves while the session
            // is quiet releases its subscription now, not at the next output.
            let item = tokio::select! {
                item = stream.recv() => item,
                () = events.closed() => return,
            };
            let event = match item {
                Ok(StreamItem::Output(data)) => WatchEvent::Output {
                    data: data.to_string(),
                },
                Ok(StreamItem::Repaint(repaint)) => repaint_event(repaint),
                // Too far behind: the output it missed is gone. Watching again
                // starts it from a fresh repaint, which is what it needs anyway.
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => {
                    let _ = events.send(WatchEvent::Ended).await;
                    return;
                }
            };
            if events.send(event).await.is_err() {
                return;
            }
        }
    }
}

fn repaint_event(repaint: Repaint) -> WatchEvent {
    WatchEvent::Repaint {
        cols: repaint.cols,
        rows: repaint.rows,
        data: repaint.data.to_string(),
    }
}

fn info(entry: &Entry<SessionHandle>) -> SessionInfo {
    match (&entry.handle, &entry.restored) {
        (Some(handle), _) => {
            let snapshot = handle.snapshot();
            SessionInfo {
                id: entry.id.clone(),
                agent: entry.agent.clone(),
                status: snapshot.status,
                since_ms: snapshot.since.saturating_since(Timestamp::ZERO).as_millis() as u64,
                status_for_ms: handle.status_age().as_millis() as u64,
                rule: snapshot.rule,
                pid: snapshot.pid,
                exit_code: snapshot.exit_code,
                lost: false,
            }
        }
        (None, Some(record)) => SessionInfo {
            id: entry.id.clone(),
            agent: entry.agent.clone(),
            status: record.status,
            since_ms: record
                .status_since_unix_ms
                .saturating_sub(record.created_unix_ms),
            // Wall clock on both sides, because the monotonic clock the live
            // path uses did not survive the restart.
            status_for_ms: now_unix_ms().saturating_sub(record.status_since_unix_ms),
            rule: record.rule.clone(),
            pid: None,
            exit_code: record.exit_code,
            lost: record.lost,
        },
        // The registry only ever creates entries one way or the other. If that
        // changes, an honest "done, unknown" is a better answer than a panic in
        // the middle of serving a request.
        (None, None) => SessionInfo {
            id: entry.id.clone(),
            agent: entry.agent.clone(),
            status: argus_reducer::Status::Done,
            since_ms: 0,
            status_for_ms: 0,
            rule: None,
            pid: None,
            exit_code: None,
            lost: true,
        },
    }
}

fn finished(entry: &Entry<SessionHandle>) -> RpcError {
    // "Finished" would be exactly the claim `lost` exists to avoid making.
    if entry.restored.as_ref().is_some_and(|r| r.lost) {
        return RpcError::new(
            ErrorCode::SessionFinished,
            format!(
                "session {} was lost when the previous engine stopped; spawn a new one",
                entry.id
            ),
        );
    }
    RpcError::new(
        ErrorCode::SessionFinished,
        format!("session {} has already finished", entry.id),
    )
}
