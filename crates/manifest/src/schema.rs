//! The on-disk shape of an agent definition.

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
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// How to resume a previous session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    /// Arguments appended to the launch command to reattach.
    pub args: Vec<String>,
}

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
}

/// Where on the screen a rule looks.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// The whole visible screen.
    #[default]
    Screen,

    /// Only the last N lines.
    ///
    /// Prompts live at the bottom. Restricting a blocker rule to the last few
    /// lines is the cheapest defence against the most common false positive:
    /// matching a question that scrolled up and was answered ten seconds ago but
    /// is still sitting on screen.
    LastLines(usize),
}
