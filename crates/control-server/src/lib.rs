//! The daemon's entire external API surface: a Windows named pipe server
//! (`tokio::net::windows::named_pipe`) serving the `argus-protocol` methods.
//!
//! A named pipe rather than a TCP socket because it is the native Windows IPC
//! primitive and carries an ACL, so access control is the OS's job rather than
//! ours. Note that `accept()` on a named pipe genuinely blocks; it must not be
//! parked on the async cooperative pool where it can starve other tasks on a
//! low-core machine.
//!
//! Implemented in step 5. See DESIGN.md.
