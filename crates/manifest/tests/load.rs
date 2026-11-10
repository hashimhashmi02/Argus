//! Loading, validating, and classifying with manifests.
//!
//! The last few tests load the real files from `manifests/`, so a broken pattern
//! or a duplicate rule name in a shipped manifest fails here rather than in a
//! session.

use std::path::PathBuf;

use argus_manifest::{Agent, ManifestError, Start, key_bytes};
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
fn last_lines_counts_from_the_last_content_not_the_bottom_of_the_grid() {
    // Regression: a real emulated screen is padded with blank rows until it
    // fills up. A fresh `cmd.exe` is three lines of banner and prompt above
    // twenty-seven empty ones, and "the last three lines" must mean the prompt,
    // not three blanks. The first live session test caught this; the fixtures
    // above never had trailing blanks, so they could not.
    let json = minimal(
        r#"{
            "name": "prompt",
            "status": "idle",
            "pattern": "(?m)^C:\\\\[^\\n]*>$",
            "region": { "last_lines": 3 }
        }"#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    let mut screen = vec![
        "Microsoft Windows [Version 10.0.26200]".to_string(),
        "(c) Microsoft Corporation. All rights reserved.".to_string(),
        String::new(),
        "C:\\Users\\Hashim>".to_string(),
    ];
    screen.extend(std::iter::repeat_n(String::new(), 26));

    assert_eq!(agent.classify(&screen).status, Some(Status::Idle));

    // And the region still excludes older content above the window.
    let mut scrolled = vec!["C:\\Users\\Hashim>".to_string()];
    scrolled.extend(["one", "two", "three"].map(String::from));
    scrolled.extend(std::iter::repeat_n(String::new(), 10));
    assert_eq!(agent.classify(&scrolled).status, None);
}

#[test]
fn an_entirely_blank_screen_matches_nothing() {
    let json = minimal(
        r#"{ "name": "any", "status": "idle", "pattern": ".*", "region": { "last_lines": 3 } }"#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    // `.*` matches the empty string, so this only holds if a blank screen
    // contributes no lines at all rather than a run of empty ones. Either way it
    // must not panic on the index arithmetic.
    let blank = vec![String::new(); 30];
    let _ = agent.classify(&blank);
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

/// A manifest with the given `launch` and `resume` objects.
fn with_launch(launch: &str, resume: &str) -> String {
    format!(
        r#"{{
          "id": "test",
          "display_name": "Test",
          "launch": {launch},
          {resume}
          "rules": [{{ "name": "r", "status": "idle", "pattern": "x" }}]
        }}"#
    )
}

#[test]
fn a_fresh_start_adds_the_new_session_args_and_fills_in_the_uuid() {
    let json = with_launch(
        r#"{ "program": "agent", "args": ["--quiet"],
             "new_session_args": ["--session-id", "{session_uuid}"] }"#,
        r#""resume": { "args": ["--resume={session_uuid}"] },"#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert!(agent.wants_session_uuid());
    assert!(agent.can_resume());
    assert_eq!(
        agent.args(Start::New, Some("1234")).unwrap(),
        ["--quiet", "--session-id", "1234"]
    );
    // The resume args replace the new-session ones rather than following
    // them, and the placeholder is filled in mid-argument too.
    assert_eq!(
        agent.args(Start::Resume, Some("1234")).unwrap(),
        ["--quiet", "--resume=1234"]
    );
}

#[test]
fn an_agent_without_resume_cannot_build_a_resume_command() {
    let json = with_launch(r#"{ "program": "agent", "args": ["-x"] }"#, "");
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert!(!agent.can_resume());
    assert!(!agent.wants_session_uuid());
    assert_eq!(agent.args(Start::New, None).unwrap(), ["-x"]);
    assert_eq!(agent.args(Start::Resume, None), None);
}

#[test]
fn resume_without_a_placeholder_needs_no_uuid() {
    // `--continue`-style: resumes whatever was last, so nothing to remember.
    let json = with_launch(
        r#"{ "program": "agent" }"#,
        r#""resume": { "args": ["--continue"] },"#,
    );
    let agent = Agent::from_json(&json, "test.json").unwrap();

    assert!(agent.can_resume());
    assert!(!agent.wants_session_uuid());
    assert_eq!(agent.args(Start::Resume, None).unwrap(), ["--continue"]);
}

#[test]
fn resuming_by_a_uuid_no_launch_provides_is_rejected() {
    let json = with_launch(
        r#"{ "program": "agent" }"#,
        r#""resume": { "args": ["--resume", "{session_uuid}"] },"#,
    );
    assert!(matches!(
        Agent::from_json(&json, "test.json"),
        Err(ManifestError::ResumeWithoutSessionUuid { .. })
    ));
}

#[test]
fn the_claude_code_manifest_resumes_the_conversation_it_started() {
    let agent = Agent::from_path(manifest_dir().join("claude-code.json")).unwrap();

    assert!(agent.wants_session_uuid());
    assert_eq!(
        agent.args(Start::New, Some("u")).unwrap(),
        ["--session-id", "u"]
    );
    // Not `--continue`: that is "the latest conversation in this folder", and
    // two sessions sharing a folder would resume into the same one.
    assert_eq!(
        agent.args(Start::Resume, Some("u")).unwrap(),
        ["--resume", "u"]
    );
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

#[cfg(windows)]
#[test]
fn the_shell_manifest_recognises_a_powershell_prompt() {
    // Taken from a real ConPTY capture in step 2.
    let agent = Agent::from_path(manifest_dir().join("shell.json")).unwrap();

    let screen = ["hello-from-powershell", "PS C:\\Users\\Hashim>"];

    let matched = agent.classify(&screen);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("powershell-prompt"));
}

// ---- shell.json against real screen shapes ---------------------------------
//
// Found by the step 6 end-to-end run: with no rule that said *working*, a shell
// that had reached idle stayed idle through a 60-second `Start-Sleep`, because
// a screen no rule matches is -- correctly -- treated by the reducer as no new
// evidence. A shell's real rule is simpler than a list of prompts: a bare
// prompt on the last line means idle, anything else there means busy.

fn shell() -> Agent {
    Agent::from_path(manifest_dir().join("shell.json")).unwrap()
}

#[test]
fn a_shell_running_a_command_is_working() {
    let screen = [
        "Windows PowerShell",
        "",
        r"PS C:\Users\Hashim> Start-Sleep 60",
    ];
    let matched = shell().classify(&screen);
    assert_eq!(
        matched.status,
        Some(Status::Working),
        "rule: {:?}",
        matched.rule
    );
}

#[test]
fn a_shell_printing_output_is_working() {
    let screen = [
        r"PS C:\Users\Hashim> ping localhost",
        "Pinging localhost [::1] with 32 bytes of data:",
        "Reply from ::1: time<1ms",
    ];
    assert_eq!(shell().classify(&screen).status, Some(Status::Working));
}

#[test]
fn an_earlier_bare_prompt_does_not_make_a_busy_shell_idle() {
    // Enter on an empty prompt leaves a bare prompt line behind. It is still
    // within a few lines of the bottom when the next command starts, and must
    // not be mistaken for the shell waiting.
    let screen = [
        r"PS C:\Users\Hashim>",
        r"PS C:\Users\Hashim> Start-Sleep 60",
    ];
    assert_eq!(shell().classify(&screen).status, Some(Status::Working));
}

#[cfg(windows)]
#[test]
fn a_bare_prompt_at_the_bottom_is_idle_even_with_trailing_blank_rows() {
    let mut screen: Vec<String> = [
        r"PS C:\Users\Hashim> Write-Output hi",
        "hi",
        r"PS C:\Users\Hashim>",
    ]
    .map(String::from)
    .to_vec();
    screen.extend(std::iter::repeat_n(String::new(), 20));

    let matched = shell().classify(&screen);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("powershell-prompt"));
}

#[cfg(windows)]
#[test]
fn a_cmd_prompt_at_the_bottom_is_idle() {
    let screen = [r"C:\Users\Hashim>echo hi", "hi", "", r"C:\Users\Hashim>"];
    let matched = shell().classify(&screen);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("cmd-prompt"));
}

#[test]
fn a_confirmation_outranks_busy() {
    let screen = [
        r"PS C:\Users\Hashim> Remove-Item .\build -Recurse",
        "Are you sure you want to continue?",
        "[Y] Yes  [A] Yes to All  [N] No  [L] No to All  [S] Suspend  [?] Help (default is \"Y\"):",
    ];
    assert_eq!(shell().classify(&screen).status, Some(Status::NeedsYou));
}

#[test]
fn a_question_on_the_last_line_needs_you() {
    let screen = [
        r"PS C:\Users\Hashim> Read-Host 'Deploy to production. Are you sure (y/n)'",
        "Deploy to production. Are you sure (y/n):",
    ];
    assert_eq!(shell().classify(&screen).status, Some(Status::NeedsYou));
}

#[cfg(windows)]
#[test]
fn an_answered_question_still_on_screen_does_not_need_you() {
    // Found live in step 6: after answering `n`, the session stayed NEEDS YOU,
    // because the question was still within the last four lines. A question
    // that has a fresh prompt beneath it has been answered. Only the last line
    // says what the shell is waiting for.
    let screen = [
        r"PS C:\Users\Hashim> Read-Host 'Deploy to production. Are you sure (y/n)'",
        "Deploy to production. Are you sure (y/n): n",
        "n",
        r"PS C:\Users\Hashim>",
    ];
    let matched = shell().classify(&screen);
    assert_eq!(
        matched.status,
        Some(Status::Idle),
        "rule: {:?}",
        matched.rule
    );
}

// ---- the same shapes, on the shells Unix actually draws -------------------
//
// `sh-prompt` has to be loose about what comes before the sigil, because a
// real prompt carries whatever the shell puts there: nothing for `dash`, the
// host and directory for `zsh`. That looseness is why the rule is scoped to
// Unix -- on Windows it would read a line of output ending in `45%` as a
// prompt and call a busy session idle.

#[cfg(unix)]
#[test]
fn the_shell_manifest_recognises_an_sh_prompt() {
    let agent = shell();

    // `dash`, as a GitHub Linux runner draws it: the sigil and nothing else.
    let bare = ["hello-from-sh", "$"];
    let matched = agent.classify(&bare);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("sh-prompt"));

    // `zsh`, copied from a macOS runner. The host and the directory come
    // first, which is what defeated a tighter pattern: the single-token form
    // `^\S*[$#%]\s*$` matched the line above and not this one.
    let zsh = [
        "hello-from-zsh",
        "runner@sjc22-bt143-a21a818d-d28d-476f-b38a-8c4a864afbd6-86E5A758F386 engine %",
    ];
    let matched = agent.classify(&zsh);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("sh-prompt"));
}

#[cfg(unix)]
#[test]
fn a_bare_sh_prompt_at_the_bottom_is_idle_even_with_trailing_blank_rows() {
    let mut screen: Vec<String> = ["$ echo hi", "hi", "$"].map(String::from).to_vec();
    screen.extend(std::iter::repeat_n(String::new(), 20));

    let matched = shell().classify(&screen);
    assert_eq!(matched.status, Some(Status::Idle));
    assert_eq!(matched.rule.as_deref(), Some("sh-prompt"));
}

#[cfg(unix)]
#[test]
fn a_running_command_is_working_on_unix_too() {
    // The command's own line ends in a digit, not a sigil, so the catch-all
    // claims it. The prompt rule being loose must not make everything idle.
    let screen = ["$ sleep 60"];
    assert_eq!(shell().classify(&screen).status, Some(Status::Working));
}

#[cfg(unix)]
#[test]
fn an_answered_question_still_on_screen_does_not_need_you_on_unix() {
    let screen = [
        "$ read -p 'Deploy to production. Are you sure (y/n)' a",
        "Deploy to production. Are you sure (y/n)n",
        "$",
    ];
    let matched = shell().classify(&screen);
    assert_eq!(
        matched.status,
        Some(Status::Idle),
        "rule: {:?}",
        matched.rule
    );
}

#[test]
fn a_blank_screen_is_no_information_not_busy() {
    // Before the shell has printed anything, nothing should be claimed. The
    // busy rule must need actual content, or every session would start by
    // asserting "working" from an empty grid.
    let blank = vec![String::new(); 30];
    assert_eq!(shell().classify(&blank).status, None);
}

// ---- claude-code.json against real Claude Code screens ----------------------
//
// Captured from Claude Code v2.1.142 running under argus-engine, read back with
// `argus-cli screen`. These replaced rules written from guesswork: the original
// idle rule expected a `│ >` bordered input box, and the real one is a `>` line
// between two horizontal rules -- so an idle Claude read as working forever.
// Likewise its spinner no longer says "esc to interrupt"; it is a glyph and a
// verb ending in an ellipsis, like `· Manifesting…`.

fn claude() -> Agent {
    Agent::from_path(manifest_dir().join("claude-code.json")).unwrap()
}

fn rule() -> String {
    "─".repeat(120)
}

/// The bottom of the screen as Claude Code draws it, with the spinner (or
/// whatever else) above the input box and a two-line status line below.
fn claude_bottom(above_input: &[&str], input: &str) -> Vec<String> {
    let mut lines: Vec<String> = above_input.iter().map(|s| s.to_string()).collect();
    lines.push(format!("{:>118}", "◉ xhigh · /effort"));
    lines.push(rule());
    lines.push(input.to_string());
    lines.push(rule());
    lines.push("  [CAVEMAN] ⛏  851.1k".to_string());
    lines.push("  ← for agents".to_string());
    // Plus the blank rows a screen that has not filled up carries beneath.
    lines.extend(std::iter::repeat_n(String::new(), 4));
    lines
}

#[test]
fn claude_at_an_empty_input_box_is_idle() {
    // `> ` with its trailing space trimmed by the emulator, as it really arrives.
    let screen = claude_bottom(&[], ">");
    let matched = claude().classify(&screen);
    assert_eq!(
        matched.status,
        Some(Status::Idle),
        "rule: {:?}",
        matched.rule
    );
    assert_eq!(matched.rule.as_deref(), Some("input-box-idle"));
}

#[test]
fn claude_with_the_newer_chevron_prompt_is_idle() {
    // Claude Code draws its input line as `❯ ` now, not `> `. Seen live on
    // v2.1.142 under the engine: with only `>` accepted, an idle Claude read as
    // working forever.
    let screen = claude_bottom(&[], "❯");
    let matched = claude().classify(&screen);
    assert_eq!(
        matched.status,
        Some(Status::Idle),
        "rule: {:?}",
        matched.rule
    );
}

#[test]
fn claude_showing_its_placeholder_hint_is_idle() {
    let screen = claude_bottom(&[], r#"> Try "fix lint errors""#);
    assert_eq!(claude().classify(&screen).status, Some(Status::Idle));
}

#[test]
fn claude_with_its_spinner_up_is_working_even_though_the_input_box_shows() {
    // The input box is drawn the whole time Claude works -- you can type ahead
    // -- so the idle rule matches here too. Priority is what makes this right.
    for spinner in [
        "· Manifesting…",
        "✻ Propagating…",
        "* Whatchamacalliting…",
        "✢ Scurrying…",
    ] {
        let screen = claude_bottom(
            &[
                "> Create a file named demo.txt in this folder.",
                "  ⎿  Retrying in 1s · attempt 1/10 · API_TIMEOUT_MS=900000ms, try increasing it",
                spinner,
            ],
            ">",
        );
        let matched = claude().classify(&screen);
        assert_eq!(
            matched.status,
            Some(Status::Working),
            "spinner {spinner:?}: {:?}",
            matched.rule
        );
        assert_eq!(matched.rule.as_deref(), Some("busy-spinner"));
    }
}

#[test]
fn claudes_finished_marker_is_not_mistaken_for_the_spinner() {
    // When a turn ends the spinner line becomes a past-tense summary with no
    // ellipsis. It must not keep the session "working".
    for summary in ["✻ Churned for 2s", "✻ Cogitated for 2s", "✻ Worked for 3s"] {
        let screen = claude_bottom(
            &[
                "> Create a file named demo.txt in this folder.",
                "  ⎿  Please run /login · API Error: 401 OAuth access token has expired.",
                summary,
            ],
            ">",
        );
        let matched = claude().classify(&screen);
        assert_eq!(
            matched.status,
            Some(Status::Idle),
            "summary {summary:?}: {:?}",
            matched.rule
        );
    }
}

#[test]
fn claudes_welcome_banner_alone_does_not_claim_anything_wrong() {
    // The startup box is full of `│` borders and horizontal rules. None of it
    // should read as a prompt or a spinner.
    let mut screen: Vec<String> = [
        "╭─── Claude Code v2.1.142 ───────────────────────────────────────╮",
        "│                Welcome back Hashim!                │ Tips      │",
        "│      Opus 4.7 (1M context) · Claude Pro ·          │           │",
        "╰────────────────────────────────────────────────────────────────╯",
    ]
    .map(String::from)
    .to_vec();
    screen.extend(claude_bottom(&[], ">"));
    assert_eq!(claude().classify(&screen).status, Some(Status::Idle));
}

// ---- per-platform programs -------------------------------------------------

/// How a load turned out, for a panic message: `Agent` is deliberately not
/// `Debug`, since dumping every compiled rule would bury the thing that failed.
fn described(result: Result<Agent, ManifestError>) -> String {
    match result {
        Ok(agent) => format!("a loaded agent `{}`", agent.id()),
        Err(error) => error.to_string(),
    }
}

/// Parse the `launch` of a manifest, without compiling the rest of it.
fn launch(json: &str) -> argus_manifest::Launch {
    let full = format!(
        r#"{{
          "id": "test",
          "display_name": "Test",
          "launch": {json},
          "rules": [{{ "name": "busy", "status": "working", "pattern": "x" }}]
        }}"#
    );
    let agent = Agent::from_json(&full, "test.json").expect("should load");
    agent.manifest().launch.clone()
}

#[test]
fn one_program_answers_for_every_platform() {
    // The common case, and the one that must not have to change: an agent that
    // is one program with one name says so once.
    let launch = launch(r#"{ "program": "claude", "args": ["--no-color"] }"#);
    for os in ["windows", "macos", "linux", "freebsd"] {
        let resolved = launch.resolve_for(os);
        assert_eq!(resolved.program.as_deref(), Some("claude"), "{os}");
        assert_eq!(resolved.args, ["--no-color"], "{os}");
    }
}

#[test]
fn a_platform_entry_overrides_only_the_fields_it_names() {
    let launch = launch(
        r#"{
          "program": "claude",
          "args": ["--no-color"],
          "on": { "windows": { "program": "claude.cmd" } }
        }"#,
    );

    let windows = launch.resolve_for("windows");
    assert_eq!(windows.program.as_deref(), Some("claude.cmd"));
    assert_eq!(
        windows.args,
        ["--no-color"],
        "an entry that renames the program should not have to restate its arguments"
    );
    assert_eq!(
        launch.resolve_for("linux").program.as_deref(),
        Some("claude")
    );
}

#[test]
fn unix_answers_for_everything_that_is_not_windows() {
    let launch = launch(
        r#"{
          "on": {
            "windows": { "program": "powershell.exe", "args": ["-NoLogo"] },
            "unix": { "program": "/bin/sh", "args": ["-i"] }
          }
        }"#,
    );

    assert_eq!(
        launch.resolve_for("windows").program.as_deref(),
        Some("powershell.exe")
    );
    for os in ["linux", "macos", "freebsd"] {
        assert_eq!(
            launch.resolve_for(os).program.as_deref(),
            Some("/bin/sh"),
            "{os}"
        );
    }
}

#[test]
fn the_more_specific_platform_wins_over_unix() {
    // And each field separately: `macos` renames the program while the
    // arguments still come from `unix`.
    let launch = launch(
        r#"{
          "on": {
            "macos": { "program": "/bin/zsh" },
            "unix": { "program": "/bin/sh", "args": ["-i"] }
          }
        }"#,
    );

    let macos = launch.resolve_for("macos");
    assert_eq!(macos.program.as_deref(), Some("/bin/zsh"));
    assert_eq!(macos.args, ["-i"]);
    assert_eq!(
        launch.resolve_for("linux").program.as_deref(),
        Some("/bin/sh")
    );
}

#[test]
fn a_unix_path_never_reaches_windows() {
    // `unix` is consulted only off Windows, so a manifest cannot hand a
    // Windows machine a `/bin` path by leaving out a `windows` entry. It gets
    // "not here" instead, which is true and fixable.
    let launch = launch(r#"{ "on": { "unix": { "program": "/bin/sh" } } }"#);
    assert_eq!(launch.resolve_for("windows").program, None);
    assert_eq!(
        launch.resolve_for("linux").program.as_deref(),
        Some("/bin/sh")
    );
}

#[test]
fn an_empty_argument_list_overrides_rather_than_being_ignored() {
    // `Option<Vec<_>>` rather than `Vec<_>`, so "no arguments on this
    // platform" is sayable and is not mistaken for "nothing to say".
    let launch = launch(
        r#"{
          "program": "sh",
          "args": ["-i"],
          "on": { "windows": { "program": "cmd.exe", "args": [] } }
        }"#,
    );
    assert!(launch.resolve_for("windows").args.is_empty());
    assert_eq!(launch.resolve_for("linux").args, ["-i"]);
}

#[test]
fn a_manifest_that_names_no_program_at_all_is_refused() {
    // Startable somewhere and not here is a fact to report. Saying nothing
    // about what to run is a mistake, and one unloadable manifest refuses the
    // whole directory, so it is caught at load with a message saying what to
    // add.
    let json = r#"{
      "id": "nowhere",
      "display_name": "Nowhere",
      "launch": { "args": ["-i"] },
      "rules": [{ "name": "busy", "status": "working", "pattern": "x" }]
    }"#;
    match Agent::from_json(json, "test.json") {
        Err(error @ ManifestError::NoProgram { .. }) => {
            assert!(error.to_string().contains("launch.on"), "{error}");
        }
        other => panic!("expected NoProgram, got {}", described(other)),
    }
}

#[test]
fn a_rule_can_name_the_platforms_it_applies_to() {
    // Checked from whichever platform runs the tests, against the table
    // rather than the compiled set, so every answer is visible from one
    // machine.
    let rule: argus_manifest::ScreenRule = serde_json::from_str(
        r#"{ "name": "r", "status": "idle", "pattern": "x", "only_on": ["unix"] }"#,
    )
    .unwrap();
    assert!(!rule.applies_to("windows"));
    for os in ["linux", "macos", "freebsd"] {
        assert!(rule.applies_to(os), "{os}");
    }

    let specific: argus_manifest::ScreenRule = serde_json::from_str(
        r#"{ "name": "r", "status": "idle", "pattern": "x", "only_on": ["macos"] }"#,
    )
    .unwrap();
    assert!(specific.applies_to("macos"));
    assert!(!specific.applies_to("linux"));

    let everywhere: argus_manifest::ScreenRule =
        serde_json::from_str(r#"{ "name": "r", "status": "idle", "pattern": "x" }"#).unwrap();
    for os in ["windows", "macos", "linux"] {
        assert!(everywhere.applies_to(os), "{os}");
    }
}

#[test]
fn a_manifest_whose_every_rule_is_scoped_is_refused() {
    // A platform with no applicable rule could never report a status, and it
    // would fail silently: sessions that never leave `working`, with nothing
    // to point at. Requiring one unscoped rule rules that out without having
    // to enumerate platforms, and the catch-all a manifest needs anyway is
    // exactly such a rule.
    let json = r#"{
      "id": "scoped",
      "display_name": "Scoped",
      "launch": { "program": "sh" },
      "rules": [
        { "name": "win", "status": "idle", "pattern": "x", "only_on": ["windows"] },
        { "name": "nix", "status": "idle", "pattern": "x", "only_on": ["unix"] }
      ]
    }"#;
    match Agent::from_json(json, "test.json") {
        Err(error @ ManifestError::EveryRuleIsScoped { .. }) => {
            assert!(error.to_string().contains("only_on"), "{error}");
        }
        other => panic!("expected EveryRuleIsScoped, got {}", described(other)),
    }
}

#[test]
fn a_bad_pattern_is_refused_even_in_a_rule_this_platform_skips() {
    // Validity cannot depend on the platform, so every pattern is compiled
    // whether or not this machine will use it. Otherwise a typo in a
    // Windows-only rule would lie dormant until someone ran it on Windows.
    let elsewhere = if cfg!(windows) { "linux" } else { "windows" };
    let json = format!(
        r#"{{
          "id": "badpattern",
          "display_name": "Bad Pattern",
          "launch": {{ "program": "sh" }},
          "rules": [
            {{ "name": "busy", "status": "working", "pattern": "x" }},
            {{ "name": "bad", "status": "idle", "pattern": "a(", "only_on": ["{elsewhere}"] }}
          ]
        }}"#
    );
    match Agent::from_json(&json, "test.json") {
        Err(ManifestError::Pattern { rule, .. }) => assert_eq!(rule, "bad"),
        other => panic!("expected a pattern error, got {}", described(other)),
    }
}

#[test]
fn a_misspelt_platform_key_loads_and_reads_as_unavailable() {
    // The deliberate limit of the load-time check. Refusing unknown keys
    // would refuse a manifest written for a platform a newer Argus runs on,
    // and these files are copied between machines. So `windwos` loads, and the
    // symptom is an agent that can start nowhere -- which `platforms_named`
    // makes diagnosable, because the spawn error prints the key that is there
    // next to the one that should be.
    let json = r#"{
      "id": "typo",
      "display_name": "Typo",
      "launch": { "on": { "windwos": { "program": "powershell.exe" } } },
      "rules": [{ "name": "busy", "status": "working", "pattern": "x" }]
    }"#;
    let agent = Agent::from_json(json, "test.json").expect("an unknown key is not a parse error");
    assert!(!agent.available_here());
    assert_eq!(agent.manifest().launch.platforms_named(), ["windwos"]);
}

#[test]
fn whether_a_manifest_is_valid_does_not_depend_on_the_platform() {
    // `resume` asks for a conversation by id, and the Windows launch hands one
    // over while the Unix launch does not. Refusing this only on Unix would
    // mean a manifest directory that starts the engine on one machine and
    // refuses to start it on another, so it is refused on both.
    let json = r#"{
      "id": "lopsided",
      "display_name": "Lopsided",
      "launch": {
        "on": {
          "windows": { "program": "a.exe", "new_session_args": ["--id", "{session_uuid}"] },
          "unix": { "program": "a", "new_session_args": [] }
        }
      },
      "resume": { "args": ["--resume", "{session_uuid}"] },
      "rules": [{ "name": "busy", "status": "working", "pattern": "x" }]
    }"#;
    assert!(
        matches!(
            Agent::from_json(json, "test.json"),
            Err(ManifestError::ResumeWithoutSessionUuid { .. })
        ),
        "a manifest that is wrong on one platform should be refused on all of them"
    );
}

#[test]
fn the_shipped_shell_manifest_starts_on_every_platform_argus_supports() {
    // The manifest this whole schema exists for. It used to name
    // `powershell.exe` for everybody, which made the engine portable and the
    // one agent it ships with unusable off Windows.
    let agent = Agent::from_path(manifest_dir().join("shell.json")).expect("should load");
    let launch = &agent.manifest().launch;
    for os in ["windows", "macos", "linux"] {
        assert!(
            launch.resolve_for(os).program.is_some(),
            "the shell should be startable on {os}"
        );
    }
    assert!(
        agent.available_here(),
        "including the one running this test"
    );
}
