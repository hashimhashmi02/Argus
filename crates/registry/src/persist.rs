//! The state file: what survives an engine restart.
//!
//! Two rules this module exists to enforce. Both are about not destroying the
//! user's history while trying to be helpful.
//!
//! 1. **A file that fails to parse is quarantined, never overwritten.** It is
//!    renamed aside and the engine starts with an empty list. The alternative --
//!    "could not parse, so start fresh" -- writes a new empty file over the old
//!    one on the first save, and the history is gone at exactly the moment
//!    someone most needs it: right after something went wrong.
//! 2. **Unknown fields survive a round trip.** Every persisted struct carries a
//!    `#[serde(flatten)]` map of fields this build does not know. A file written
//!    by a newer Argus, read and re-saved by an older one, keeps everything the
//!    newer one wrote. Explicitly *not* `deny_unknown_fields`.
//!
//! And a third that makes the first two safe: **writes are atomic**. The new
//! contents go to a temporary file which is then renamed over the old one, so a
//! crash or power cut mid-save leaves either the old file or the new one --
//! never half of each, which would be a corrupt file manufactured by Argus
//! itself.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use argus_reducer::Status;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The on-disk format this build writes.
///
/// Read permissively regardless: an older or newer number is not by itself a
/// reason to refuse a file. See the module docs.
pub const STATE_FORMAT: u32 = 1;

/// The whole state file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub format: u32,

    #[serde(default)]
    pub sessions: Vec<SessionRecord>,

    /// Top-level fields this build does not understand, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One session, as remembered across a restart.
///
/// Only what still means something after the engine is gone. No pid -- the
/// process is dead, and a pid can be reused by an unrelated process, so storing
/// one invites someone to act on a stranger. No screen -- it would be most of
/// the file and stale by definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub agent: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,

    /// Wall-clock creation time, in Unix milliseconds.
    ///
    /// Wall clock here, unlike everywhere else in Argus, because it has to mean
    /// the same thing to a different process on a different day. A monotonic
    /// `Instant` does not survive the process that read it.
    pub created_unix_ms: u64,

    pub status: Status,

    /// When `status` began, in Unix milliseconds.
    pub status_since_unix_ms: u64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,

    /// The engine stopped while this session was still running.
    ///
    /// When the engine dies its pseudo-consoles close and every child goes with
    /// them -- verified by hard-killing a real engine with live sessions. So a
    /// session that was working or waiting when the engine went down is not
    /// paused somewhere; it is gone, without an exit code. Saying `done` would
    /// imply it finished; `lost` says what actually happened.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,

    /// Per-session fields this build does not understand, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SessionRecord {
    /// This record as it should be remembered by an engine that is starting up.
    ///
    /// Anything not already finished was running when the last engine stopped,
    /// and its process stopped with it.
    pub fn after_restart(mut self, now_unix_ms: u64) -> Self {
        if self.status != Status::Done {
            self.status = Status::Done;
            self.lost = true;
            self.exit_code = None;
            self.rule = None;
            // When it was lost is unknowable -- somewhere between the last save
            // and now. "Now" at least makes the list read correctly: lost
            // sessions appear as recent news rather than ancient history.
            self.status_since_unix_ms = now_unix_ms;
        }
        self
    }
}

/// What loading found.
#[derive(Debug)]
pub struct Loaded {
    pub state: StateFile,
    /// Where an unparseable file was moved to, if one was found.
    pub quarantined: Option<PathBuf>,
}

/// Load the state file at `path`.
///
/// - **Missing:** an empty state. A first run is not an error.
/// - **Unparseable:** quarantined -- renamed aside -- and an empty state
///   returned, along with where the old file went so the caller can say so.
/// - **Unreadable** (permissions, a sharing violation, a disk error): an
///   **error**. Not quarantined, because nothing is known to be wrong with the
///   contents, and renaming a perfectly good file because of a transient lock
///   would be the same history loss by a different route.
pub fn load(path: &Path) -> io::Result<Loaded> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                state: StateFile::default(),
                quarantined: None,
            });
        }
        Err(e) => return Err(e),
    };

    match serde_json::from_str::<StateFile>(&text) {
        Ok(state) => Ok(Loaded {
            state,
            quarantined: None,
        }),
        Err(_) => {
            let aside = quarantine_path(path);
            // If even the rename fails, refuse to continue. Carrying on would
            // mean the next save overwrites the file this function just
            // declined to understand.
            fs::rename(path, &aside)?;
            Ok(Loaded {
                state: StateFile::default(),
                quarantined: Some(aside),
            })
        }
    }
}

/// Write `state` to `path` atomically.
pub fn save(path: &Path, state: &StateFile) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }

    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;

    let tmp = temp_path(path);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&json)?;
        // Flushed to the disk, not just to the OS cache, before the rename makes
        // it the real file. Otherwise a power cut can leave a renamed file whose
        // contents never reached the platter.
        file.sync_all()?;
    }

    // On Windows `rename` replaces an existing destination (MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING), so the switch from old to new is a single
    // filesystem operation.
    fs::rename(&tmp, path)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `state.json` -> `state.corrupt-1729512345678.json`.
///
/// Timestamped so a second corruption does not overwrite the first one's
/// evidence, and still ending in `.json` so it opens in an editor.
fn quarantine_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    let mut ms = now_unix_ms();
    loop {
        let candidate = path.with_file_name(format!("{stem}.corrupt-{ms}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        ms += 1;
    }
}

/// Current wall-clock time in Unix milliseconds.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
