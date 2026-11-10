//! The on-disk shape of an agent definition.

use std::collections::BTreeMap;
use std::time::Duration;

use argus_reducer::Status;

use crate::keys::Keys;
use serde::{Deserialize, Serialize};

/// Everything Argus needs to know about one kind of agent.
///
/// Note the absence of `#[serde(deny_unknown_fields)]`. A manifest written for a
/// newer Argus -- one that has grown a field this build has never heard of --
/// must still load and work here, because manifests are shared, copied between
/// machines, and outlive any single version. The cost is that a typo'd field is
/// silently ignored rather than rejected, which is a real downside and the
/// reason `argus-manifest` validates everything it *does* understand strictly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Stable identifier, used on the command line and in the registry.
    pub id: String,

    /// Human-readable name for the UI.
    pub display_name: String,

    pub launch: Launch,

    /// How to reattach to previous work, if the CLI supports it at all.
    #[serde(default)]
    pub resume: Option<Resume>,

    /// Environment variables to strip before spawning, matched by prefix.
    ///
    /// A spawned agent inherits Argus's environment, which would hand it
    /// credentials and identity meant for the orchestrator. Prefix matching
    /// rather than an exact allowlist so the rule keeps working as new variables
    /// appear. Applied case-insensitively, because Windows environment variable
    /// names are.
    #[serde(default)]
    pub env_scrub_prefixes: Vec<String>,

    /// The keystrokes that answer a prompt.
    #[serde(default)]
    pub keys: Keys,

    /// Rules that classify a screen into a status.
    pub rules: Vec<ScreenRule>,

    /// Per-agent debounce overrides.
    #[serde(default)]
    pub timing: Timing,
}

/// How to start the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    /// What to run, on any platform that `on` does not answer.
    ///
    /// Optional, because an agent may exist only on some platforms: a manifest
    /// that names programs under `on` and nothing here can start on exactly
    /// the platforms it lists and says so plainly everywhere else. Most agents
    /// are one program with one name, so most manifests fill this in and leave
    /// `on` out entirely.
    #[serde(default)]
    pub program: Option<String>,

    /// Arguments for every start, fresh or resumed.
    #[serde(default)]
    pub args: Vec<String>,

    /// Arguments appended to `args` for a fresh session only, never a resumed
    /// one.
    ///
    /// Exists for agents that can be told which conversation a new session is
    /// -- `claude --session-id {session_uuid}` -- and that refuse to be told
    /// again on the way back in, where `--resume` already says it.
    #[serde(default)]
    pub new_session_args: Vec<String>,

    /// Per-platform answers, each overriding the fields above.
    ///
    /// Keyed by [`std::env::consts::OS`] -- `windows`, `macos`, `linux` -- plus
    /// `unix`, which answers for every platform that is not Windows. A key no
    /// build has heard of is ignored rather than rejected, like any other
    /// unknown part of a manifest: these files are copied between machines and
    /// outlive the version that wrote them.
    #[serde(default)]
    pub on: BTreeMap<String, PlatformLaunch>,
}

/// One platform's answer, as far as it differs from [`Launch`].
///
/// Every field is optional and each is consulted separately, so a platform that
/// only changes the program's name does not have to restate its arguments.
/// Argument lists replace rather than merge: order carries meaning on a command
/// line, and a merge could add but never remove.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformLaunch {
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub new_session_args: Option<Vec<String>>,
}

/// What one platform actually runs, after [`Launch::resolve_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    /// `None` when nothing names a program for this platform, which is how a
    /// manifest says "not here" without saying it twice.
    pub program: Option<String>,
    pub args: Vec<String>,
    pub new_session_args: Vec<String>,
}

impl Launch {
    /// What `os` runs, where `os` is an [`std::env::consts::OS`] value.
    ///
    /// Takes the platform as an argument rather than reading it, so the table
    /// can be tested for every platform from whichever one the tests run on --
    /// which is the whole difficulty this schema exists to address.
    pub fn resolve_for(&self, os: &str) -> ResolvedLaunch {
        // Most specific first. `unix` is only consulted off Windows, so a
        // manifest cannot accidentally hand a Windows machine a `/bin` path.
        let keys: &[&str] = if os == "windows" {
            &["windows"]
        } else {
            &["", "unix"]
        };
        let platforms = keys
            .iter()
            .map(|key| if key.is_empty() { os } else { key })
            .filter_map(|key| self.on.get(key));

        let mut resolved = ResolvedLaunch {
            program: self.program.clone(),
            args: self.args.clone(),
            new_session_args: self.new_session_args.clone(),
        };
        // Each field takes the first platform that names it, so a `unix` entry
        // can supply the arguments while `linux` overrides only the program.
        let mut program = None;
        let mut args = None;
        let mut new_session_args = None;
        for platform in platforms {
            program = program.or(platform.program.clone());
            args = args.or(platform.args.clone());
            new_session_args = new_session_args.or(platform.new_session_args.clone());
        }
        if let Some(program) = program {
            resolved.program = Some(program);
        }
        if let Some(args) = args {
            resolved.args = args;
        }
        if let Some(new_session_args) = new_session_args {
            resolved.new_session_args = new_session_args;
        }
        resolved
    }

    /// Whether anything at all in this `launch` names a program.
    ///
    /// False only for a manifest that says nothing about what to run, which
    /// cannot be anything but a mistake and is refused at load. It is
    /// deliberately not "could start on a platform this build knows about":
    /// checking the keys against a fixed list would refuse a manifest written
    /// for a platform a newer Argus runs on, and manifests are copied between
    /// machines and outlive the version that wrote them.
    ///
    /// The cost is that a misspelt key -- `windwos` -- loads, and shows up as
    /// an agent that is unavailable everywhere. [`Self::platforms_named`] is
    /// what makes that diagnosable.
    pub fn names_a_program_somewhere(&self) -> bool {
        self.program.is_some() || self.on.values().any(|p| p.program.is_some())
    }

    /// The platform keys this `launch` names a program for, in order.
    ///
    /// For the message when an agent cannot start here: printing the keys the
    /// manifest actually has turns a misspelt one from a mystery into an
    /// obvious typo, without the loader having to decide which keys are real.
    pub fn platforms_named(&self) -> Vec<&str> {
        self.on
            .iter()
            .filter(|(_, platform)| platform.program.is_some())
            .map(|(key, _)| key.as_str())
            .collect()
    }

    /// Every command line this manifest can produce, on any platform.
    ///
    /// Validity has to be the same answer everywhere: one unloadable manifest
    /// refuses a whole manifest directory, so a set that starts the engine on
    /// Windows and refuses to start it on Linux would be a trap. Whatever the
    /// loader checks about arguments, it therefore checks against all of these
    /// rather than against the platform it happens to be running on.
    ///
    /// Finite because the keys are: each entry under `on`, plus the two shapes
    /// a *missing* key can take -- Windows with no `windows` entry, and
    /// anything else, which still picks up `unix`.
    pub fn every_resolution(&self) -> Vec<ResolvedLaunch> {
        self.on
            .keys()
            .map(String::as_str)
            .chain(["windows", "linux"])
            .map(|os| self.resolve_for(os))
            .collect()
    }
}

/// How to resume a previous session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    /// Arguments appended to `launch.args` to reattach, in place of
    /// `launch.new_session_args`.
    pub args: Vec<String>,
}

/// Replaced, wherever it appears in an argument, by the id of the agent's own
/// conversation: a UUID Argus makes up when the session is first started and
/// remembers, so that it can ask for that same conversation back.
///
/// The only placeholder there is. An agent that resumes "the latest
/// conversation in this folder" -- `claude --continue` -- needs none, but then
/// two sessions sharing a folder resume into one conversation, which is the
/// problem this exists to solve.
pub const SESSION_UUID: &str = "{session_uuid}";

/// Per-agent overrides for the reducer's debounce policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    /// Milliseconds an idle screen must persist before the session is called
    /// idle. Omitted means "use the reducer's default".
    #[serde(default)]
    pub idle_debounce_ms: Option<u64>,
}

impl Timing {
    /// Fold these overrides into a base policy.
    pub fn apply(&self, base: argus_reducer::Policy) -> argus_reducer::Policy {
        let mut policy = base;
        if let Some(ms) = self.idle_debounce_ms {
            policy.idle_debounce = Duration::from_millis(ms);
        }
        policy
    }
}

/// One screen-match rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRule {
    /// Unique within a manifest. Surfaces in logs and in `SessionState::rule`,
    /// so a surprising status can be traced to the line of JSON that caused it.
    pub name: String,

    /// The status this rule claims when it matches.
    pub status: Status,

    /// A regular expression, in the `regex` crate's dialect.
    ///
    /// Worth knowing before writing one: this is RE2-style, so there are **no
    /// backreferences and no lookaround**. `(?<=foo)` and `\1` are compile
    /// errors, not silent misbehaviour. The trade is that matching is linear in
    /// the input, so a pathological rule cannot hang the daemon -- which matters
    /// when the patterns are user-editable data.
    pub pattern: String,

    /// Higher wins when several rules match. Defaults to zero.
    #[serde(default)]
    pub priority: i32,

    /// Which part of the screen to match against.
    #[serde(default)]
    pub region: Region,

    /// Platforms this rule applies to. Empty means all of them.
    ///
    /// Same vocabulary as [`Launch::on`]: `windows`, `macos`, `linux`, or
    /// `unix` for everything that is not Windows.
    ///
    /// Needed because one manifest serves several shells, and their prompts
    /// are not equally distinctive. `sh` draws `$`, `zsh` draws `%`, and a
    /// rule loose enough to catch a real `zsh` prompt -- which carries the
    /// host and the directory, so the sigil can follow anything -- comes down
    /// to "the line ends with `%`". Live on Windows, that would read a line of
    /// output ending in `45%` as a prompt and call a busy session idle.
    /// Scoping the rule costs a key; guessing costs a wrong status.
    #[serde(default)]
    pub only_on: Vec<String>,
}

impl ScreenRule {
    /// Whether this rule applies on `os`, an [`std::env::consts::OS`] value.
    pub fn applies_to(&self, os: &str) -> bool {
        self.only_on.is_empty()
            || self
                .only_on
                .iter()
                .any(|key| key == os || (key == "unix" && os != "windows"))
    }
}

/// Where on the screen a rule looks.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// The whole visible screen.
    #[default]
    Screen,

    /// Only the last N lines *of content*.
    ///
    /// Prompts live at the bottom. Restricting a blocker rule to the last few
    /// lines is the cheapest defence against the most common false positive:
    /// matching a question that scrolled up and was answered ten seconds ago but
    /// is still sitting on screen.
    ///
    /// Counted back from the last non-blank line, not from the bottom edge of
    /// the terminal. A screen that has not filled up yet has blank rows beneath
    /// its content, and those are not "the bottom" in any sense a rule author
    /// means.
    LastLines(usize),
}
