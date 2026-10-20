//! The engine's external API surface: a Windows named pipe.
//!
//! Both ends live here. [`Server`] is what the engine runs; [`Client`] is what
//! the CLI -- and later the UI -- uses to talk to it. Keeping them together means
//! the transport has one owner, even though the *message* definitions live in
//! `argus-protocol` and are shared more widely still.
//!
//! # Why a named pipe
//!
//! It is the native Windows IPC primitive, and it carries a security descriptor,
//! so access control is the operating system's job rather than something Argus
//! invents. The pipe is created with the default DACL of the process that made
//! it, which in practice admits the user who started the engine, SYSTEM and
//! Administrators. A localhost TCP socket would be portable and would also
//! accept any process on the machine.
//!
//! # Security properties set explicitly
//!
//! This API can spawn programs as the user. Treat it accordingly.
//!
//! - **Remote clients are rejected.** Named pipes are reachable over SMB as
//!   `\\host\pipe\name` unless told otherwise. Remote access to an API that runs
//!   programs is remote code execution. tokio already defaults to rejecting
//!   them; the call is spelled out anyway so the property is visible in review
//!   and survives any change of default.
//! - **The first instance must be ours.** If another process has already
//!   created a pipe with this name, the engine refuses to start rather than
//!   sharing the name with it. See [`Server::bind`].
//!
//! Not yet done, and worth doing before this is trusted widely: a client can
//! verify that the process on the other end is really the engine, by checking
//! its owner via `GetNamedPipeServerProcessId`. Until then, a process that
//! creates the pipe *before* the engine starts can impersonate it.

use std::sync::atomic::{AtomicU32, Ordering};

mod client;
mod server;

pub use client::{Client, ClientError};
pub use server::{Handler, Server};

/// The pipe the engine listens on by default: one per Windows user.
///
/// Per-user so that two people logged into the same machine each get their own
/// engine, and neither can see the other's sessions -- which the default DACL
/// would enforce anyway, but a name collision would surface as a confusing
/// "pipe already exists" error instead.
pub fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
    format!(r"\\.\pipe\argus-{}", sanitise(&user))
}

/// A unique pipe name for tests, so parallel test binaries never collide.
pub fn unique_pipe_name(label: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\argus-test-{}-{}-{n}",
        sanitise(label),
        std::process::id()
    )
}

/// Keep a name component to characters a pipe name will accept.
///
/// A backslash in particular would be read as a path separator and silently
/// change which pipe is meant.
fn sanitise(component: &str) -> String {
    component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}
