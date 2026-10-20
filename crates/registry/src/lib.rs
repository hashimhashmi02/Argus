//! The set of known sessions.
//!
//! This step builds the in-memory half: a table of live and finished sessions,
//! keyed by short ids a person can type. Persistence to disk -- so the list
//! survives an engine restart -- arrives in step 7, and with it the two rules
//! that motivated giving this its own crate:
//!
//! 1. A state file that fails to parse is *quarantined* -- renamed aside --
//!    never treated as empty and overwritten. Silently starting fresh on a parse
//!    error deletes the user's session history at exactly the moment they most
//!    need it.
//! 2. Deserialization is permissive and preserves unknown fields, so a file
//!    written by a newer Argus survives a round trip through an older one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// One known session.
///
/// Generic over the live handle so the table logic -- ids, prefix lookup,
/// ordering -- can be tested without spawning a process, and so this crate sits
/// *below* `argus-session` in the dependency graph instead of on top of it. The
/// engine uses `Entry<SessionHandle>`. When step 7 persists entries, `H` is
/// exactly the part that is not written to disk: a live handle means nothing
/// after a restart.
#[derive(Clone)]
pub struct Entry<H> {
    pub id: String,
    /// The manifest id it was spawned from.
    pub agent: String,
    pub cwd: Option<PathBuf>,
    pub handle: H,
    /// Creation order, so `list` is stable rather than in hash order.
    seq: u64,
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
    pub fn insert(&self, agent: &str, cwd: Option<PathBuf>, handle: H) -> String {
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

        sessions.insert(
            id.clone(),
            Entry {
                id: id.clone(),
                agent: agent.to_string(),
                cwd,
                handle,
                seq,
            },
        );
        id
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
                    handle: seq as u32,
                    seq: seq as u64,
                },
            );
        }
        drop(sessions);
        registry
    }

    #[test]
    fn an_exact_id_is_found() {
        let registry = registry_with(&["3fa1c2d4", "9b0e1172"]);
        assert_eq!(registry.find("9b0e1172").unwrap().handle, 1);
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
        let ids: Vec<String> = (0..20).map(|i| registry.insert("a", None, i)).collect();

        let listed: Vec<String> = registry.list().into_iter().map(|e| e.id).collect();
        assert_eq!(listed, ids);
        assert_eq!(registry.len(), 20);
    }
}
