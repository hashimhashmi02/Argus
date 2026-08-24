//! Loading, validating, and classifying with manifests.
//!
//! The last few tests load the real files from `manifests/`, so a broken pattern
//! or a duplicate rule name in a shipped manifest fails here rather than in a
//! session.

use std::path::PathBuf;

use argus_manifest::{Agent, ManifestError, key_bytes};
use argus_reducer::Status;

fn manifest_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is this crate; the manifests live at the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../manifests")
}

fn minimal(rules: &str) -> String {
    format!(
        r#"{{
          "id": "test",
          "display_name": "Test",
          "launch": {{ "program": "test" }},
          "rules": [{rules}]
        }}"#
    )
}

#[test]
fn a_minimal_manifest_loads() {
    let json = minimal(r#"{ "name": "busy", "status": "working", "pattern": "working" }"#);
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert_eq!(agent.id(), "test");
    assert_eq!(agent.manifest().display_name, "Test");
    // Defaults fill in for everything omitted.
    assert_eq!(agent.manifest().keys.approve, "enter");
    assert!(agent.manifest().resume.is_none());
}

#[test]
fn classification_finds_the_matching_rule() {
    let json = minimal(r#"{ "name": "busy", "status": "working", "pattern": "esc to interrupt" }"#);
    let agent = Agent::from_json(&json, "test.json").unwrap();

    let matched = agent.classify(&["Thinking... (esc to interrupt)"]);
    assert_eq!(matched.status, Some(Status::Working));
    assert_eq!(matched.rule.as_deref(), Some("busy"));
}

#[test]
fn no_match_is_no_information() {
    // Distinct from "idle". The reducer treats an empty match as an absence of
    // evidence and leaves the status alone.
    let json = minimal(r#"{ "name": "busy", "status": "working", "pattern": "esc to interrupt" }"#);
    let agent = Agent::from_json(&json, "test.json").unwrap();

    let matched = agent.classify(&["nothing interesting here"]);
    assert_eq!(matched.status, None);
    assert_eq!(matched.rule, None);
}

#[test]
fn the_highest_priority_rule_wins() {
    let json = minimal(
        r#"
        { "name": "busy", "status": "working", "pattern": "interrupt", "priority": 50 },
        { "name": "blocked", "status": "needs_you", "pattern": "Do you want", "priority": 100 }
        "#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    // Both rules match this screen. A spinner is still drawn while a permission
    // prompt is up, which is exactly why priority exists.
    let matched = agent.classify(&["Do you want to edit? (esc to interrupt)"]);
    assert_eq!(matched.status, Some(Status::NeedsYou));
    assert_eq!(matched.rule.as_deref(), Some("blocked"));
}

#[test]
fn priority_ties_go_to_the_earlier_rule() {
    let json = minimal(
        r#"
        { "name": "first", "status": "working", "pattern": "match me" },
        { "name": "second", "status": "idle", "pattern": "match me" }
        "#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert_eq!(agent.classify(&["match me"]).rule.as_deref(), Some("first"));
}

#[test]
fn a_region_restricts_where_a_rule_looks() {
    let json = minimal(
        r#"{
            "name": "blocked",
            "status": "needs_you",
            "pattern": "Do you want",
            "region": { "last_lines": 2 }
        }"#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    // The question is up at the top: answered long ago, still on screen. This is
    // the false positive that regions exist to prevent.
    let scrolled_away = ["Do you want to edit?", "yes", "...", "building", "done"];
    assert_eq!(agent.classify(&scrolled_away).status, None);

    // Same text, but at the bottom, where a live prompt actually sits.
    let live = ["building", "done", "Do you want to edit?", ""];
    assert_eq!(agent.classify(&live).status, Some(Status::NeedsYou));
}

#[test]
fn rules_match_across_line_boundaries() {
    // The region is joined with newlines, so a multiline pattern sees the shape
    // of the screen rather than a single run-on string.
    let json = minimal(r#"{ "name": "two-lines", "status": "needs_you", "pattern": "(?m)^b$" }"#);
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert_eq!(
        agent.classify(&["a", "b", "c"]).status,
        Some(Status::NeedsYou)
    );
    assert_eq!(agent.classify(&["abc"]).status, None);
}

#[test]
fn an_unknown_field_is_ignored() {
    // Forward compatibility: a manifest written for a newer Argus must still
    // load here. Deliberately not `deny_unknown_fields`.
    let json = r#"{
      "id": "test",
      "display_name": "Test",
      "launch": { "program": "test" },
      "some_future_field": { "nested": true },
      "rules": [{ "name": "r", "status": "idle", "pattern": "x" }]
    }"#;

    assert!(Agent::from_json(json, "test.json").is_ok());
}

#[test]
fn a_broken_pattern_is_rejected_at_load_time() {
    // And names the rule, so the error points at a line of JSON.
    let json = minimal(r#"{ "name": "bad", "status": "idle", "pattern": "a(" }"#);

    match Agent::from_json(&json, "test.json") {
        Err(ManifestError::Pattern { rule, .. }) => assert_eq!(rule, "bad"),
        other => panic!(
            "expected a Pattern error, got {other:?}",
            other = other.err()
        ),
    }
}

#[test]
fn lookaround_is_rejected_rather_than_silently_wrong() {
    // The regex crate is RE2-style. Someone arriving from JavaScript will reach
    // for a lookbehind sooner or later, and it is much better that it fails loudly.
    let json = minimal(r#"{ "name": "bad", "status": "idle", "pattern": "(?<=foo)bar" }"#);

    assert!(matches!(
        Agent::from_json(&json, "test.json"),
        Err(ManifestError::Pattern { .. })
    ));
}

#[test]
fn duplicate_rule_names_are_rejected() {
    let json = minimal(
        r#"
        { "name": "same", "status": "idle", "pattern": "a" },
        { "name": "same", "status": "working", "pattern": "b" }
        "#,
    );

    assert!(matches!(
        Agent::from_json(&json, "test.json"),
        Err(ManifestError::DuplicateRule { .. })
    ));
}

#[test]
fn a_manifest_with_no_rules_is_rejected() {
    // It would load fine and then never report anything, which is worse than
    // failing.
    assert!(matches!(
        Agent::from_json(&minimal(""), "test.json"),
        Err(ManifestError::NoRules { .. })
    ));
}

#[test]
fn malformed_json_names_the_file() {
    match Agent::from_json("{ not json", "broken.json") {
        Err(ManifestError::Parse { path, .. }) => {
            assert_eq!(path.file_name().unwrap(), "broken.json");
        }
        other => panic!("expected a Parse error, got {other:?}", other = other.err()),
    }
}

#[test]
fn key_names_resolve_to_terminal_bytes() {
    // \r, not \n: a terminal sends carriage return for Enter, and an agent
    // reading raw input will not recognise a line feed.
    assert_eq!(key_bytes("enter"), b"\r");
    assert_eq!(key_bytes("escape"), &[0x1b]);
    assert_eq!(key_bytes("Escape"), &[0x1b], "names are case-insensitive");
    assert_eq!(key_bytes("ctrl-c"), &[0x03]);
    assert_eq!(key_bytes("ctrl-a"), &[0x01]);
    assert_eq!(key_bytes("down"), b"\x1b[B");
    assert_eq!(
        key_bytes("yes"),
        b"yes",
        "anything unrecognised is literal text"
    );
}

#[test]
fn timing_overrides_the_reducer_policy() {
    let json = r#"{
      "id": "test",
      "display_name": "Test",
      "launch": { "program": "test" },
      "timing": { "idle_debounce_ms": 2000 },
      "rules": [{ "name": "r", "status": "idle", "pattern": "x" }]
    }"#;
    let agent = Agent::from_json(json, "test.json").unwrap();

    let policy = agent
        .manifest()
        .timing
        .apply(argus_reducer::Policy::default());
    assert_eq!(policy.idle_debounce, std::time::Duration::from_millis(2000));
    // Untouched fields keep their defaults.
    assert_eq!(policy.blocker_debounce, std::time::Duration::ZERO);
}

#[test]
fn every_shipped_manifest_loads() {
    // A broken pattern in a manifest we ship should fail the build, not a
    // session.
    let dir = manifest_dir();
    let mut loaded = 0;

    for entry in std::fs::read_dir(&dir).expect("manifests/ should exist") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        Agent::from_path(&path)
            .unwrap_or_else(|e| panic!("{} failed to load: {e}", path.display()));
        loaded += 1;
    }

    assert!(
        loaded >= 2,
        "expected to find the shipped manifests, found {loaded}"
    );
}

#[test]
fn the_claude_code_manifest_classifies_a_permission_prompt() {
    let agent = Agent::from_path(manifest_dir().join("claude-code.json")).unwrap();

    let screen = [
        "  Edit file  src/main.rs",
        "",
        "  1  fn main() {",
        "",
        "Do you want to make this edit to main.rs?",
        "  1. Yes",
        "  2. No",
        "",
    ];

    let matched = agent.classify(&screen);
    assert_eq!(matched.status, Some(Status::NeedsYou));
    assert_eq!(matched.rule.as_deref(), Some("permission-prompt"));
}

#[test]
fn the_claude_code_manifest_classifies_a_busy_screen() {
    let agent = Agent::from_path(manifest_dir().join("claude-code.json")).unwrap();

    let screen = ["", "* Thinking... (3s - esc to interrupt)", ""];

    let matched = agent.classify(&screen);
    assert_eq!(matched.status, Some(Status::Working));
    assert_eq!(matched.rule.as_deref(), Some("busy-spinner"));
}

#[test]
fn a_blocker_outranks_the_spinner_on_the_claude_code_manifest() {
    // Both are on screen at once during a permission prompt mid-turn.
    let agent = Agent::from_path(manifest_dir().join("claude-code.json")).unwrap();

    let screen = [
        "* Thinking... (esc to interrupt)",
        "Do you want to make this edit to main.rs?",
        "",
    ];

    assert_eq!(agent.classify(&screen).status, Some(Status::NeedsYou));
}

#[test]
fn the_shell_manifest_recognises_a_powershell_prompt() {
    // Taken from a real ConPTY capture in step 2.
    let agent = Agent::from_path(manifest_dir().join("shell.json")).unwrap();

    let screen = ["hello-from-powershell", "PS C:\\Users\\Hashim>"];

    let matched = agent.classify(&screen);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("powershell-prompt"));
}
