//! The set of known sessions, and the file that remembers them.
//!
//! Two halves. [`Registry`] is the in-memory table of live and finished
//! sessions, keyed by short ids a person can type. [`persist`] is the state file
//! that lets that list survive an engine restart, and it is where the two rules
//! that motivated this crate live:
//!
//! 1. A state file that fails to parse is *quarantined* -- renamed aside --
//!    never treated as empty and overwritten.
//! 2. Unknown fields survive a round trip, so a file written by a newer Argus is
//!    not stripped by an older one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

pub mod persist;

pub use persist::{Loaded, STATE_FORMAT, SessionRecord, StateFile, now_unix_ms};

/// One known session.
///
/// Generic over the live handle so the table logic -- ids, prefix lookup,
/// ordering -- can be tested without spawning a process, and so this crate sits
/// *below* `argus-session` in the dependency graph instead of on top of it. The
/// engine uses `Entry<SessionHandle>`.
#[derive(Clone)]
pub struct Entry<H> {
    pub id: String,
    /// The manifest id it was spawned from.
    pub agent: String,
    pub cwd: Option<PathBuf>,

    /// The id of the agent's own conversation, for agents that resume by one.
    pub agent_session: Option<String>,

    /// The branch of the git worktree made for this session, if one was.
    pub branch: Option<String>,

    /// Wall-clock creation time, Unix milliseconds.
    pub created_unix_ms: u64,

    /// Wall-clock time the current process started, Unix milliseconds.
    ///
    /// The same as `created_unix_ms` until the session is resumed. Kept apart
    /// because a live session's times are offsets from its own process's
    /// start, while the list stays in order of creation.
    pub started_unix_ms: u64,

    /// The live session, if this engine spawned it.
    ///
    /// `None` for a session remembered from a previous run. Its process died
    /// with the previous engine, so there is nothing to hold a handle to --
    /// which is exactly why the handle is the one part never written to disk.
    pub handle: Option<H>,

    /// What the state file said about a session restored from a previous run.
    ///
    /// Kept whole, including any fields this build does not understand, so that
    /// saving it again writes back everything that was read.
    pub restored: Option<SessionRecord>,

    /// Fields a restored record carried that this build does not understand,
    /// kept once the session is resumed and `restored` is gone, so a newer
    /// Argus's fields outlive an older one bringing the session back.
    pub extra: Map<String, Value>,

    /// Creation order, so `list` is stable rather than in hash order.
    seq: u64,

    /// A resume has started and not yet attached its process.
    resuming: bool,
}

/// Why a session cannot start resuming.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResumeError {
    #[error("no session `{0}`")]
    NotFound(String),

    #[error("session {0} is still running")]
    Running(String),

    #[error("session {0} is already being resumed")]
    AlreadyResuming(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LookupError {
    #[error("no session matches `{0}`")]
    NotFound(String),

    /// The prefix fits more than one id. Lists them, because the most useful
    /// thing to tell someone who typed `3f` is which `3f`s there are.
    #[error("`{prefix}` is ambiguous: it matches {}", .matches.join(", "))]
    Ambiguous {
        prefix: String,
        matches: Vec<String>,
    },
}

/// Every session the engine knows about, live or finished.
///
/// Finished sessions stay listed. "It exited with code 1 four minutes ago" is
/// exactly what someone coming back to their desk wants to see, and a session
/// that vanished from the list the moment it ended would hide it.
pub struct Registry<H> {
    /// A `std` mutex, not `tokio`'s, on purpose.
    ///
    /// The rule is about what happens while the lock is held. Every critical
    /// section here is a few map operations and is never held across an
    /// `.await`, so a plain mutex is both correct and cheaper. `tokio::Mutex`
    /// exists for locks that *must* be held across an await, and using it here
    /// would only advertise a hazard that does not exist.
    sessions: Mutex<HashMap<String, Entry<H>>>,
    next_seq: AtomicU64,
}

// Written by hand because `#[derive(Default)]` on a generic struct adds an
// `H: Default` bound, and nothing about an empty table needs one -- a
// `SessionHandle` has no sensible default, and an empty registry holds none.
impl<H> Default for Registry<H> {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_seq: AtomicU64::new(0),
        }
    }
}

impl<H: Clone> Registry<H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a newly spawned session and return the id assigned to it.
    pub fn insert(
        &self,
        agent: &str,
        cwd: Option<PathBuf>,
        agent_session: Option<String>,
        branch: Option<String>,
        handle: H,
    ) -> String {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut sessions = self.sessions.lock().unwrap();

        // Short random ids can collide. With 32 bits and a few dozen sessions
        // that is vanishingly rare, but "rare" is not "never", and the fix is a
        // loop.
        let mut salt = 0;
        let id = loop {
            let candidate = short_id(seq, salt);
            if !sessions.contains_key(&candidate) {
                break candidate;
            }
            salt += 1;
        };

        let now = now_unix_ms();
        sessions.insert(
            id.clone(),
            Entry {
                id: id.clone(),
                agent: agent.to_string(),
                cwd,
                agent_session,
                branch,
                created_unix_ms: now,
                started_unix_ms: now,
                handle: Some(handle),
                restored: None,
                extra: Map::new(),
                seq,
                resuming: false,
            },
        );
        id
    }

    /// Bring back sessions remembered from a previous run, oldest first.
    ///
    /// Keeps their ids, so `argus-cli screen 3fa1` names the same session after
    /// a restart as before it. A record whose id is already present -- only
    /// possible in a hand-edited or damaged file -- is skipped rather than
    /// allowed to replace an existing entry.
    pub fn restore(&self, records: impl IntoIterator<Item = SessionRecord>) -> usize {
        let mut records: Vec<SessionRecord> = records.into_iter().collect();
        records.sort_by_key(|r| r.created_unix_ms);

        let mut sessions = self.sessions.lock().unwrap();
        let mut restored = 0;
        for record in records {
            if record.id.is_empty() || sessions.contains_key(&record.id) {
                continue;
            }
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            sessions.insert(
                record.id.clone(),
                Entry {
                    id: record.id.clone(),
                    agent: record.agent.clone(),
                    cwd: record.cwd.clone(),
                    agent_session: record.agent_session.clone(),
                    branch: record.branch.clone(),
                    created_unix_ms: record.created_unix_ms,
                    started_unix_ms: record.created_unix_ms,
                    handle: None,
                    restored: Some(record),
                    extra: Map::new(),
                    seq,
                    resuming: false,
                },
            );
            restored += 1;
        }
        restored
    }

    /// Claim a finished session for resuming, so that nothing else can.
    ///
    /// Takes the full id, as [`find`](Self::find) returns it. `is_live` says
    /// whether a handle's session is still running; the registry is generic
    /// over the handle and cannot tell on its own.
    ///
    /// The claim is made before any process is started and released by
    /// [`finish_resume`](Self::finish_resume) or
    /// [`abort_resume`](Self::abort_resume). Two resumes racing for one
    /// session would otherwise both start the agent, and the loser's process
    /// would run on with nothing holding a handle to it.
    pub fn begin_resume(
        &self,
        id: &str,
        is_live: impl FnOnce(&H) -> bool,
    ) -> Result<Entry<H>, ResumeError> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| ResumeError::NotFound(id.to_string()))?;
        if entry.resuming {
            return Err(ResumeError::AlreadyResuming(id.to_string()));
        }
        if entry.handle.as_ref().is_some_and(is_live) {
            return Err(ResumeError::Running(id.to_string()));
        }
        entry.resuming = true;
        Ok(entry.clone())
    }

    /// Attach the resumed session's process, releasing the claim.
    ///
    /// The entry keeps its id, its place in the list and its creation time.
    /// What it was restored as is dropped -- it is no longer lost -- except
    /// the fields this build did not understand, which move to `extra`.
    pub fn finish_resume(&self, id: &str, handle: H) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(entry) = sessions.get_mut(id) else {
            return;
        };
        if let Some(record) = entry.restored.take() {
            entry.extra = record.extra;
        }
        entry.handle = Some(handle);
        entry.started_unix_ms = now_unix_ms();
        entry.resuming = false;
    }

    /// Release a claim whose resume failed, leaving the entry as it was.
    pub fn abort_resume(&self, id: &str) {
        if let Some(entry) = self.sessions.lock().unwrap().get_mut(id) {
            entry.resuming = false;
        }
    }

    /// Find a session by full id, or by any prefix that names exactly one.
    ///
    /// Prefix matching, the way git accepts a short hash, because ids are typed
    /// by hand: `argus-cli screen 3f` beats copying eight characters.
    pub fn find(&self, id_or_prefix: &str) -> Result<Entry<H>, LookupError> {
        let sessions = self.sessions.lock().unwrap();

        // An exact match wins even when it is also a prefix of a longer id.
        if let Some(entry) = sessions.get(id_or_prefix) {
            return Ok(entry.clone());
        }

        let mut matches: Vec<&Entry<H>> = if id_or_prefix.is_empty() {
            Vec::new()
        } else {
            sessions
                .values()
                .filter(|e| e.id.starts_with(id_or_prefix))
                .collect()
        };

        match matches.len() {
            0 => Err(LookupError::NotFound(id_or_prefix.to_string())),
            1 => Ok(matches.remove(0).clone()),
            _ => {
                matches.sort_by_key(|e| e.seq);
                Err(LookupError::Ambiguous {
                    prefix: id_or_prefix.to_string(),
                    matches: matches.into_iter().map(|e| e.id.clone()).collect(),
                })
            }
        }
    }

    /// Every session, oldest first.
    pub fn list(&self) -> Vec<Entry<H>> {
        let mut entries: Vec<Entry<H>> = self.sessions.lock().unwrap().values().cloned().collect();
        entries.sort_by_key(|e| e.seq);
        entries
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Eight hex characters: short enough to type, long enough not to collide in
/// practice.
///
/// Not cryptographic, and does not need to be: ids are names, not secrets.
/// What matters is that they do not repeat across engine restarts once step 7
/// persists them, which is why the wall clock and the process id go into the
/// mix alongside the sequence number -- a counter alone would hand out the same
/// ids again after every restart.
fn short_id(seq: u64, salt: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = splitmix64(nanos ^ seq.rotate_left(32) ^ u64::from(std::process::id()) ^ salt);
    format!("{:08x}", mixed as u32)
}

/// A small, well-understood integer mixer: every input bit affects every
/// output bit, so ids made a nanosecond apart look nothing alike.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_eight_hex_characters_and_do_not_repeat() {
        let ids: std::collections::HashSet<String> = (0..10_000).map(|i| short_id(i, 0)).collect();
        assert_eq!(ids.len(), 10_000);
        for id in &ids {
            assert_eq!(id.len(), 8);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn splitmix_spreads_adjacent_inputs() {
        // Adjacent inputs should differ in roughly half their bits.
        let diff = (splitmix64(1) ^ splitmix64(2)).count_ones();
        assert!((16..=48).contains(&diff), "only {diff} bits differ");
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    /// A registry holding plain numbers instead of live sessions.
    fn registry_with(ids: &[&str]) -> Registry<u32> {
        let registry = Registry::new();
        let mut sessions = registry.sessions.lock().unwrap();
        for (seq, id) in ids.iter().enumerate() {
            sessions.insert(
                id.to_string(),
                Entry {
                    id: id.to_string(),
                    agent: "test".into(),
                    cwd: None,
                    agent_session: None,
                    branch: None,
                    created_unix_ms: seq as u64,
                    started_unix_ms: seq as u64,
                    handle: Some(seq as u32),
                    restored: None,
                    extra: Map::new(),
                    seq: seq as u64,
                    resuming: false,
                },
            );
        }
        drop(sessions);
        registry
    }

    #[test]
    fn an_exact_id_is_found() {
        let registry = registry_with(&["3fa1c2d4", "9b0e1172"]);
        assert_eq!(registry.find("9b0e1172").unwrap().handle, Some(1));
    }

    #[test]
    fn a_unique_prefix_is_enough() {
        let registry = registry_with(&["3fa1c2d4", "9b0e1172"]);
        assert_eq!(registry.find("3f").unwrap().id, "3fa1c2d4");
    }

    #[test]
    fn an_ambiguous_prefix_lists_the_candidates_in_creation_order() {
        let registry = registry_with(&["3fa1c2d4", "9b0e1172", "3f00aaaa"]);
        assert_eq!(
            registry.find("3f").map(|e| e.id).unwrap_err(),
            LookupError::Ambiguous {
                prefix: "3f".into(),
                matches: vec!["3fa1c2d4".into(), "3f00aaaa".into()],
            }
        );
    }

    #[test]
    fn an_exact_match_wins_over_being_a_prefix_of_another() {
        let registry = registry_with(&["abc", "abcdef"]);
        assert_eq!(registry.find("abc").unwrap().id, "abc");
    }

    #[test]
    fn an_empty_prefix_matches_nothing_rather_than_everything() {
        // Otherwise `kill ""` would be one keystroke away from "kill whichever
        // session happens to be alone".
        let registry = registry_with(&["3fa1c2d4"]);
        assert!(matches!(registry.find(""), Err(LookupError::NotFound(_))));
    }

    #[test]
    fn an_unknown_id_is_not_found() {
        let registry = registry_with(&["3fa1c2d4"]);
        assert!(matches!(registry.find("zz"), Err(LookupError::NotFound(_))));
    }

    #[test]
    fn list_is_in_creation_order_not_hash_order() {
        let registry: Registry<u32> = Registry::new();
        let ids: Vec<String> = (0..20)
            .map(|i| registry.insert("a", None, None, None, i))
            .collect();

        let listed: Vec<String> = registry.list().into_iter().map(|e| e.id).collect();
        assert_eq!(listed, ids);
        assert_eq!(registry.len(), 20);
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use argus_reducer::Status;

    /// A registry holding one session restored as lost, with a field from a
    /// newer Argus. Handles are plain numbers; a number is "live" when odd.
    fn with_lost_session() -> Registry<u32> {
        let registry = Registry::new();
        let mut extra = Map::new();
        extra.insert("from_the_future".into(), Value::from(42));
        registry.restore([SessionRecord {
            id: "3fa1c2d4".into(),
            agent: "claude-code".into(),
            cwd: Some(PathBuf::from(r"C:\work")),
            agent_session: Some("7c1d0a52-3c11-4b5e-9a0e-5f1b2d3c4e5f".into()),
            branch: None,
            created_unix_ms: 1_000,
            status: Status::Done,
            status_since_unix_ms: 2_000,
            rule: None,
            exit_code: None,
            lost: true,
            extra,
        }]);
        registry
    }

    fn is_live(handle: &u32) -> bool {
        handle % 2 == 1
    }

    #[test]
    fn a_resumed_session_keeps_its_id_place_and_unknown_fields() {
        let registry = with_lost_session();
        let claimed = registry.begin_resume("3fa1c2d4", is_live).unwrap();
        assert_eq!(
            claimed.agent_session.as_deref(),
            Some("7c1d0a52-3c11-4b5e-9a0e-5f1b2d3c4e5f")
        );

        registry.finish_resume("3fa1c2d4", 1);

        let entry = registry.find("3fa1c2d4").unwrap();
        assert_eq!(entry.handle, Some(1));
        assert!(entry.restored.is_none(), "no longer lost");
        assert_eq!(entry.created_unix_ms, 1_000, "still listed where it was");
        assert!(entry.started_unix_ms > entry.created_unix_ms);
        assert_eq!(entry.extra["from_the_future"], 42);
    }

    #[test]
    fn a_session_being_resumed_cannot_be_claimed_twice() {
        let registry = with_lost_session();
        registry.begin_resume("3fa1c2d4", is_live).unwrap();
        assert_eq!(
            registry.begin_resume("3fa1c2d4", is_live).map(|e| e.id),
            Err(ResumeError::AlreadyResuming("3fa1c2d4".into()))
        );
    }

    #[test]
    fn a_failed_resume_can_be_tried_again() {
        let registry = with_lost_session();
        registry.begin_resume("3fa1c2d4", is_live).unwrap();
        registry.abort_resume("3fa1c2d4");

        let entry = registry.begin_resume("3fa1c2d4", is_live).unwrap();
        assert!(entry.restored.is_some(), "an abort changes nothing else");
    }

    #[test]
    fn a_live_session_is_not_resumed_but_a_finished_one_is() {
        let registry: Registry<u32> = Registry::new();
        let live = registry.insert("shell", None, None, None, 1);
        let finished = registry.insert("shell", None, None, None, 2);

        assert_eq!(
            registry.begin_resume(&live, is_live).map(|e| e.id),
            Err(ResumeError::Running(live.clone()))
        );
        assert!(registry.begin_resume(&finished, is_live).is_ok());
    }

    #[test]
    fn resuming_takes_a_full_id_not_a_prefix() {
        let registry = with_lost_session();
        assert_eq!(
            registry.begin_resume("3f", is_live).map(|e| e.id),
            Err(ResumeError::NotFound("3f".into()))
        );
    }
}
