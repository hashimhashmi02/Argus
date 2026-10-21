//! Keeping the state file in step with the registry.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argus_registry::persist::{self, Loaded};
use argus_registry::{Entry, STATE_FORMAT, SessionRecord, StateFile, now_unix_ms};
use argus_session::SessionHandle;
use serde_json::{Map, Value};

/// Where the engine remembers sessions across restarts.
pub(crate) struct Persistence {
    path: PathBuf,
    /// Held open, exclusively, for as long as this engine lives.
    ///
    /// Two engines sharing one state file would be a quiet disaster: the second
    /// to start would read the first one's *live* sessions, conclude they were
    /// lost in a crash, and write that down -- then the two would take turns
    /// overwriting each other's view of the world every second. The pipe name
    /// usually prevents two engines running at once, but only when both use the
    /// same pipe. This lock makes the state file itself the thing that can only
    /// have one owner.
    ///
    /// A separate `.lock` file rather than locking `state.json`, so the state
    /// file stays readable by anyone -- including a person debugging it.
    _lock: std::fs::File,
    /// Top-level fields of the loaded file that this build does not understand,
    /// written back unchanged on every save.
    extra: Map<String, Value>,
    /// The bytes most recently written.
    ///
    /// Saves are skipped when nothing changed. The persister wakes every second
    /// so that status changes reach the disk promptly, and without this every
    /// one of those wakeups would rewrite an identical file.
    ///
    /// Held for the whole of a save, so two saves can never interleave: the
    /// periodic one and the final one at shutdown can otherwise overlap,
    /// because a blocking save keeps running even after the task waiting on it
    /// has been cancelled.
    last_written: Mutex<Option<Vec<u8>>>,
}

/// What starting from a state file found.
#[derive(Debug, Default)]
pub struct StartupReport {
    /// Sessions brought back from the previous run.
    pub restored: usize,
    /// How many of those were still running when the previous engine stopped.
    pub lost: usize,
    /// Where an unparseable state file was moved, if there was one.
    pub quarantined: Option<PathBuf>,
}

impl Persistence {
    /// Load the file and prepare the records to restore.
    pub(crate) fn open(path: &Path) -> io::Result<(Self, Vec<SessionRecord>, StartupReport)> {
        // Lock before reading anything: the whole point is that a second engine
        // never gets as far as interpreting the file.
        let lock = acquire_lock(path)?;
        let Loaded { state, quarantined } = persist::load(path)?;

        let now = now_unix_ms();
        let records: Vec<SessionRecord> = state
            .sessions
            .into_iter()
            .map(|record| record.after_restart(now))
            .collect();

        let report = StartupReport {
            restored: records.len(),
            lost: records.iter().filter(|r| r.lost).count(),
            quarantined,
        };

        let persistence = Self {
            _lock: lock,
            path: path.to_path_buf(),
            extra: state.extra,
            last_written: Mutex::new(None),
        };
        Ok((persistence, records, report))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Write the registry to disk if it differs from the last write.
    ///
    /// Blocking file I/O. The engine only ever calls this through
    /// `spawn_blocking`, so it never occupies a runtime worker.
    pub(crate) fn save_if_changed(&self, entries: &[Entry<SessionHandle>]) -> io::Result<bool> {
        let state = StateFile {
            format: STATE_FORMAT,
            sessions: entries.iter().map(record_for).collect(),
            extra: self.extra.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&state).map_err(io::Error::other)?;

        let mut last = self.last_written.lock().unwrap();
        if last.as_deref() == Some(bytes.as_slice()) {
            return Ok(false);
        }
        persist::save(&self.path, &state)?;
        *last = Some(bytes);
        Ok(true)
    }
}

/// Open `<state>.lock` with no sharing at all.
///
/// Windows enforces share modes in the kernel, so while this handle is open no
/// other process can open the file -- and if the engine dies, the OS closes the
/// handle and the lock goes with it. No stale lock files to clean up after a
/// crash, which is the usual weakness of lock files.
fn acquire_lock(state_path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    if let Some(dir) = state_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = state_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let lock_path = state_path.with_file_name(name);

    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(&lock_path)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{} is locked -- another engine is already using this state file ({e})",
                    lock_path.display()
                ),
            )
        })
}

/// The record to persist for one registry entry.
fn record_for(entry: &Entry<SessionHandle>) -> SessionRecord {
    match (&entry.handle, &entry.restored) {
        (Some(handle), _) => {
            let snapshot = handle.snapshot();
            let since_spawn = snapshot
                .since
                .saturating_since(argus_reducer::Timestamp::ZERO)
                .as_millis() as u64;
            SessionRecord {
                id: entry.id.clone(),
                agent: entry.agent.clone(),
                cwd: entry.cwd.clone(),
                created_unix_ms: entry.created_unix_ms,
                status: snapshot.status,
                // Creation time plus the offset at which the status began --
                // both fixed once the status is entered. Computing it as "now
                // minus how long it has held" would drift by a millisecond or
                // two on every call, the file would never compare equal to the
                // last one written, and an idle engine would rewrite it every
                // second forever.
                status_since_unix_ms: entry.created_unix_ms + since_spawn,
                rule: snapshot.rule,
                exit_code: snapshot.exit_code,
                lost: false,
                extra: Map::new(),
            }
        }
        // Remembered from a previous run: written back exactly as read,
        // unknown fields and all.
        (None, Some(record)) => record.clone(),
        (None, None) => SessionRecord {
            id: entry.id.clone(),
            agent: entry.agent.clone(),
            cwd: entry.cwd.clone(),
            created_unix_ms: entry.created_unix_ms,
            status: argus_reducer::Status::Done,
            status_since_unix_ms: entry.created_unix_ms,
            rule: None,
            exit_code: None,
            lost: true,
            extra: Map::new(),
        },
    }
}

/// `%LOCALAPPDATA%\Argus\state.json`, the conventional home for per-user
/// application state on Windows.
///
/// Falls back to `.argus\state.json` in the working directory -- which the
/// repository's `.gitignore` already excludes -- if `LOCALAPPDATA` is unset,
/// which in practice means a stripped-down service or test environment.
pub fn default_state_path() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) => PathBuf::from(dir).join("Argus").join("state.json"),
        None => PathBuf::from(".argus").join("state.json"),
    }
}
