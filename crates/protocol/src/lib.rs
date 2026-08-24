//! Shared request/response types for the engine's control API: spawn, list,
//! send_text, resize, read_screen, kill.
//!
//! This crate performs no I/O whatsoever -- it is types and their serde
//! derives, nothing else. Both the engine and every client (the CLI, later the
//! Tauri UI) depend on it, so the wire format cannot drift out of sync between
//! them: a breaking change fails to compile on both sides at once.
//!
//! Implemented in step 5. See DESIGN.md.
