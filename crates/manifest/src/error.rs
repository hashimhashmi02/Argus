//! What can go wrong loading a manifest.

use std::path::PathBuf;

/// Failure loading or validating an agent manifest.
///
/// Typed rather than `anyhow` because this is a library and the caller may
/// reasonably want to tell these apart -- a missing file is a configuration
/// problem, a broken regex is an authoring problem, and they deserve different
/// messages. Each variant carries enough to point at the offending line.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("could not read manifest at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("manifest at {path} is not valid JSON")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("rule `{rule}` has an invalid pattern")]
    Pattern {
        rule: String,
        #[source]
        source: regex::Error,
    },

    #[error("manifest `{id}` declares two rules named `{rule}`")]
    DuplicateRule { id: String, rule: String },

    #[error("manifest is missing an id")]
    MissingId,

    #[error("manifest `{id}` has no rules, so it could never report a status")]
    NoRules { id: String },

    /// Every rule names an `only_on`, so some platform would have none at all
    /// -- and would fail silently, with sessions that never leave `working`
    /// rather than an error anyone would see.
    #[error(
        "manifest `{id}` scopes every rule with `only_on`, so a platform not \
         named could never report a status; leave the catch-all rule unscoped"
    )]
    EveryRuleIsScoped { id: String },

    /// Neither `launch.program` nor any entry under `launch.on` names a
    /// program, so the manifest never says what to run.
    ///
    /// A manifest startable on some platforms and not others is fine and is
    /// not this error: that is reported at spawn instead. This is only for one
    /// that says nothing at all.
    #[error(
        "manifest `{id}` names no program: set `launch.program`, or name one \
         under `launch.on` for each platform it runs on"
    )]
    NoProgram { id: String },

    /// `resume.args` asks for a conversation by id, but no fresh launch ever
    /// gives the agent that id -- so there would never be anything to resume.
    #[error(
        "manifest `{id}` resumes by `{{session_uuid}}` but never starts a session with one; \
         add it to `launch.new_session_args`"
    )]
    ResumeWithoutSessionUuid { id: String },
}
