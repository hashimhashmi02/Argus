//! Per-agent definitions loaded from JSON: how to launch an agent, how to
//! resume it, which environment variables to scrub, which keystrokes mean
//! "approve" and "deny", and the regex screen-match rules that classify the
//! current screen into a status.
//!
//! The point of this crate is that adding support for a new agent is a JSON
//! file, not a Rust patch. Every agent-specific behaviour that would otherwise
//! become a `match` arm somewhere in `session` belongs in here as data.
//!
//! Implemented in step 3. See DESIGN.md.
