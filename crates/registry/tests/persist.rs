//! The state file, against real files on disk.
//!
//! Each test works in its own fresh directory under the system temp dir, so
//! they run in parallel without touching each other or any real Argus state.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_reducer::Status;
use argus_registry::persist::{load, save};
use argus_registry::{Registry, STATE_FORMAT, SessionRecord, StateFile};
use serde_json::{Map, Value, json};

fn scratch_dir(label: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argus-persist-{label}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(id: &str, status: Status) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        agent: "shell".into(),
        cwd: Some(PathBuf::from(r"C:\work")),
        agent_session: None,
        created_unix_ms: 1_700_000_000_000,
        status,
        status_since_unix_ms: 1_700_000_005_000,
        rule: Some("powershell-prompt".into()),
        exit_code: None,
        lost: false,
        extra: Map::new(),
    }
}

fn files_in(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---- the basics -------------------------------------------------------------

#[test]
fn a_missing_file_is_an_empty_first_run_not_an_error() {
    let path = scratch_dir("missing").join("state.json");

    let loaded = load(&path).unwrap();
    assert!(loaded.state.sessions.is_empty());
    assert!(loaded.quarantined.is_none());
}

#[test]
fn what_is_saved_is_what_is_loaded() {
    let path = scratch_dir("roundtrip").join("state.json");
    let state = StateFile {
        format: STATE_FORMAT,
        sessions: vec![
            record("3fa1c2d4", Status::Idle),
            record("9b0e1172", Status::Done),
        ],
        extra: Map::new(),
    };

    save(&path, &state).unwrap();
    assert_eq!(load(&path).unwrap().state, state);
}

#[test]
fn save_creates_the_directory_it_needs() {
    // A first run, where %LOCALAPPDATA%\Argus does not exist yet.
    let path = scratch_dir("mkdir")
        .join("nested")
        .join("deeper")
        .join("state.json");
    save(&path, &StateFile::default()).unwrap();
    assert!(path.exists());
}

#[test]
fn a_save_leaves_no_temporary_file_behind() {
    let dir = scratch_dir("tmp");
    let path = dir.join("state.json");

    save(&path, &StateFile::default()).unwrap();
    save(&path, &StateFile::default()).unwrap();

    assert_eq!(files_in(&dir), ["state.json"]);
}

// ---- rule 1: quarantine, never overwrite ------------------------------------

#[test]
fn a_corrupt_file_is_moved_aside_with_its_contents_intact() {
    let dir = scratch_dir("corrupt");
    let path = dir.join("state.json");
    let garbage = "{ \"sessions\": [ this is not json";
    fs::write(&path, garbage).unwrap();

    let loaded = load(&path).unwrap();

    // Started empty...
    assert!(loaded.state.sessions.is_empty());
    // ...but the original is nowhere near being overwritten: it is gone from
    // the live path and preserved, byte for byte, beside it.
    assert!(
        !path.exists(),
        "the corrupt file must not stay where a save would overwrite it"
    );
    let aside = loaded
        .quarantined
        .expect("should report where the file went");
    assert_eq!(fs::read_to_string(&aside).unwrap(), garbage);
    let name = aside.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("state.corrupt-") && name.ends_with(".json"),
        "{name}"
    );
}

#[test]
fn saving_after_a_quarantine_does_not_touch_the_quarantined_file() {
    // The whole point: the very next thing the engine does after starting
    // fresh is save. That save must not be the thing that destroys history.
    let dir = scratch_dir("then-save");
    let path = dir.join("state.json");
    fs::write(&path, "garbage").unwrap();

    let aside = load(&path).unwrap().quarantined.unwrap();
    save(&path, &StateFile::default()).unwrap();

    assert_eq!(fs::read_to_string(&aside).unwrap(), "garbage");
    assert_eq!(files_in(&dir).len(), 2, "{:?}", files_in(&dir));
}

#[test]
fn two_corruptions_keep_two_quarantined_files() {
    let dir = scratch_dir("twice");
    let path = dir.join("state.json");

    fs::write(&path, "first bad file").unwrap();
    let first = load(&path).unwrap().quarantined.unwrap();
    fs::write(&path, "second bad file").unwrap();
    let second = load(&path).unwrap().quarantined.unwrap();

    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(first).unwrap(), "first bad file");
    assert_eq!(fs::read_to_string(second).unwrap(), "second bad file");
}

#[test]
fn an_empty_file_is_quarantined_too() {
    // A zero-byte state file is not "no sessions" -- that would be `{}` or a
    // missing file. It means something went wrong, and the cautious reading is
    // to keep it.
    let path = scratch_dir("empty").join("state.json");
    fs::write(&path, "").unwrap();

    assert!(load(&path).unwrap().quarantined.is_some());
}

#[test]
fn a_file_that_is_valid_json_but_the_wrong_shape_is_quarantined() {
    let path = scratch_dir("shape").join("state.json");
    fs::write(&path, r#"{"sessions": "should be a list"}"#).unwrap();

    assert!(load(&path).unwrap().quarantined.is_some());
}

// ---- rule 2: unknown fields survive -----------------------------------------

#[test]
fn fields_from_a_newer_argus_survive_a_load_and_save() {
    let path = scratch_dir("forward").join("state.json");
    let written_by_the_future = json!({
        "format": STATE_FORMAT + 1,
        "workspace_layout": { "columns": 3 },
        "sessions": [{
            "id": "3fa1c2d4",
            "agent": "claude-code",
            "created_unix_ms": 1_700_000_000_000u64,
            "status": "idle",
            "status_since_unix_ms": 1_700_000_005_000u64,
            "worktree_branch": "feature/login",
            "tags": ["urgent"]
        }]
    });
    fs::write(&path, written_by_the_future.to_string()).unwrap();

    let loaded = load(&path).unwrap();
    assert!(
        loaded.quarantined.is_none(),
        "a newer file must not be treated as corrupt"
    );
    save(&path, &loaded.state).unwrap();

    let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["workspace_layout"], json!({ "columns": 3 }));
    assert_eq!(after["sessions"][0]["worktree_branch"], "feature/login");
    assert_eq!(after["sessions"][0]["tags"], json!(["urgent"]));
}

#[test]
fn a_conversation_id_round_trips_and_older_files_load_without_one() {
    let path = scratch_dir("agent-session").join("state.json");
    let mut with_id = record("3fa1c2d4", Status::Idle);
    with_id.agent_session = Some("7c1d0a52-3c11-4b5e-9a0e-5f1b2d3c4e5f".into());
    let state = StateFile {
        format: STATE_FORMAT,
        sessions: vec![with_id.clone(), record("9b0e1172", Status::Done)],
        extra: Map::new(),
    };
    save(&path, &state).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        text.matches("agent_session").count(),
        1,
        "omitted when there is none, so files from before step 9 look the same"
    );
    let loaded = load(&path).unwrap().state;
    assert_eq!(loaded.sessions[0], with_id);
    assert_eq!(loaded.sessions[1].agent_session, None);
}

// ---- restoring --------------------------------------------------------------

#[test]
fn a_session_running_when_the_engine_stopped_comes_back_as_lost() {
    // Its process died with the engine. "Done" would claim it finished.
    let restored = record("3fa1c2d4", Status::Working).after_restart(2_000);

    assert_eq!(restored.status, Status::Done);
    assert!(restored.lost);
    assert_eq!(restored.exit_code, None);
    assert_eq!(restored.rule, None);
    assert_eq!(restored.status_since_unix_ms, 2_000);
}

#[test]
fn a_session_that_had_already_finished_comes_back_unchanged() {
    let mut finished = record("3fa1c2d4", Status::Done);
    finished.exit_code = Some(0);

    assert_eq!(finished.clone().after_restart(2_000), finished);
}

#[test]
fn restored_sessions_keep_their_ids_and_order() {
    let registry: Registry<()> = Registry::new();
    let mut older = record("aaaa1111", Status::Done);
    older.created_unix_ms = 1;
    let mut newer = record("bbbb2222", Status::Done);
    newer.created_unix_ms = 2;

    assert_eq!(registry.restore([newer, older]), 2);
    let new_id = registry.insert("shell", None, None, ());

    let ids: Vec<String> = registry.list().into_iter().map(|e| e.id).collect();
    assert_eq!(ids, ["aaaa1111", "bbbb2222", new_id.as_str()]);

    let entry = registry.find("aaaa").unwrap();
    assert!(
        entry.handle.is_none(),
        "a restored session has no live handle"
    );
    assert!(entry.restored.is_some());
}

#[test]
fn a_duplicate_id_in_the_file_does_not_replace_an_existing_session() {
    let registry: Registry<()> = Registry::new();
    let mut first = record("aaaa1111", Status::Done);
    first.agent = "first".into();
    let mut second = record("aaaa1111", Status::Done);
    second.agent = "second".into();
    second.created_unix_ms += 1;

    assert_eq!(registry.restore([first, second]), 1);
    assert_eq!(registry.find("aaaa1111").unwrap().agent, "first");
}

#[test]
fn a_locked_file_is_an_error_not_a_quarantine() {
    // Unreadable is not unparseable. Another process holding the file open
    // exclusively says nothing about its contents, and renaming a perfectly good
    // file aside over a transient lock would lose history by a different route.
    use std::os::windows::fs::OpenOptionsExt;

    let dir = scratch_dir("locked");
    let path = dir.join("state.json");
    save(&path, &StateFile::default()).unwrap();

    // Share mode 0: nobody else may read, write or delete while this is open.
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();

    assert!(
        load(&path).is_err(),
        "a locked file must be reported, not quarantined"
    );
    drop(_lock);

    assert_eq!(
        files_in(&dir),
        ["state.json"],
        "nothing should have been moved"
    );
}
