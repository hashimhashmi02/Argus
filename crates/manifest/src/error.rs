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

    /// `resume.args` asks for a conversation by id, but no fresh launch ever
    /// gives the agent that id -- so there would never be anything to resume.
    #[error(
        "manifest `{id}` resumes by `{{session_uuid}}` but never starts a session with one; \
         add it to `launch.new_session_args`"
    )]
    ResumeWithoutSessionUuid { id: String },
}
