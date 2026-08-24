//! The set of known sessions, plus persistence to disk.
//!
//! Deliberately a serde + JSON file for v1, not a database. The data is a list
//! of tens of records read and written by one process.
//!
//! Two rules this crate exists to enforce:
//!
//! 1. A state file that fails to parse is *quarantined* -- renamed aside --
//!    never treated as empty and overwritten. Silently starting fresh on a
//!    parse error deletes the user's session history at exactly the moment
//!    they most need it.
//! 2. Deserialization is permissive and preserves unknown fields. Other
//!    processes and future versions of Argus have to keep reading this file,
//!    so round-tripping must not drop keys written by a newer version.
//!
//! Implemented in step 7. See DESIGN.md.
