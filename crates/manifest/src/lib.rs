//! Per-agent definitions, loaded from JSON.
//!
//! # Why this is data and not code
//!
//! Supporting a new agent should mean adding a file, not writing Rust. Every
//! agent-specific behaviour that would otherwise become a `match` arm somewhere
//! inside `argus-session` lives here instead: how to launch it, how to resume
//! it, which environment variables to strip, which keystroke approves a prompt,
//! and the regular expressions that decide what a screen means.
//!
//! The alternative is a growing pile of per-agent special cases buried in code,
//! which only someone willing to recompile Argus can extend. Since the rules are
//! the part most likely to need tuning -- CLIs change their wording between
//! releases -- keeping them editable is the difference between a fix taking a
//! minute and taking a release.
//!
//! # What a manifest looks like
//!
//! ```
//! use argus_manifest::Agent;
//! use argus_reducer::Status;
//!
//! let json = r#"{
//!   "id": "demo",
//!   "display_name": "Demo",
//!   "launch": { "program": "demo" },
//!   "rules": [
//!     {
//!       "name": "permission-prompt",
//!       "status": "needs_you",
//!       "pattern": "Do you want to .*\\?",
//!       "priority": 100,
//!       "region": { "last_lines": 12 }
//!     },
//!     { "name": "busy", "status": "working", "pattern": "esc to interrupt" }
//!   ]
//! }"#;
//!
//! let agent = Agent::from_json(json, "demo.json").unwrap();
//!
//! let screen = ["Editing main.rs", "Do you want to make this edit?"];
//! let matched = agent.classify(&screen);
//!
//! assert_eq!(matched.status, Some(Status::NeedsYou));
//! assert_eq!(matched.rule.as_deref(), Some("permission-prompt"));
//! ```

mod agent;
mod error;
mod keys;
mod schema;

pub use agent::Agent;
pub use error::ManifestError;
pub use keys::{Keys, key_bytes};
pub use schema::{AgentManifest, Launch, Region, Resume, ScreenRule, Timing};
