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

mod capability;
mod client;
mod invite;
mod quic;
mod server;

pub use capability::{Capability, Viewer};
pub use client::{Client, ClientError, ENTER_GAP, Watcher};
pub use invite::{INVITE_LIFETIME, Invite, InviteError};
pub use quic::{QuicError, QuicListener, connect as quic_connect};
pub use server::{Handler, Server};

/// The pipe the engine listens on by default: one per Windows user.
///
/// Per-user so that two people logged into the same machine each get their own
/// engine, and neither can see the other's sessions -- which the default DACL
/// would enforce anyway, but a name collision would surface as a confusing
/// "pipe already exists" error instead.
#[cfg(windows)]
pub fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
    format!(r"\\.\pipe\argus-{}", sanitise(&user))
}

/// Where sockets live: the user's own runtime directory when the system
/// provides one, the temp directory otherwise.
///
/// One function rather than three copies, because [`pipe_path`] resolving a
/// bare name somewhere other than [`unique_pipe_name`] binds would make a
/// bare name mean a different endpoint than the full one it came from.
#[cfg(unix)]
fn runtime_dir() -> std::path::PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// The socket the engine listens on by default: one per user.
///
/// `XDG_RUNTIME_DIR` when the system provides it, because that directory is
/// already the user's alone and is cleaned up at logout -- which is exactly
/// the lifetime a socket wants. Falling back to the temp directory means
/// sharing a directory with every other user on the machine, so the name
/// carries the user and [`Server::bind`] sets the socket's mode itself rather
/// than trusting the directory.
#[cfg(unix)]
pub fn default_pipe_name() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".to_string());
    runtime_dir()
        .join(format!("argus-{}.sock", sanitise(&user)))
        .to_string_lossy()
        .into_owned()
}

/// The local pipe prefix: every named pipe on this machine lives under it.
#[cfg(windows)]
const LOCAL_PIPE_PREFIX: &str = r"\\.\pipe\";

/// Turn a user-supplied pipe name into a full pipe path.
///
/// Accepts a bare name -- `argus-test` -- and supplies the `\\.\pipe\` prefix.
/// A full path is passed through untouched.
///
/// This exists because typing the prefix on a command line does not reliably
/// survive the trip. Git Bash in particular rewrites `\\.\pipe\name` somewhere
/// between the prompt and the process, and the program receives a path that
/// does not exist -- so `argus-cli --pipe '\\.\pipe\x' list` failed with "path
/// not found" instead of "engine not running", depending purely on which shell
/// ran it. A bare name has no backslashes for any shell to mangle.
#[cfg(windows)]
pub fn pipe_path(name: &str) -> String {
    if name.starts_with(r"\\") {
        name.to_string()
    } else {
        format!("{LOCAL_PIPE_PREFIX}{name}")
    }
}

/// Turn a user-supplied name into a socket path.
///
/// A bare name becomes a socket beside the default one, so `--pipe argus-test`
/// means the same thing on both platforms: a second engine that does not
/// disturb the first. Anything containing a separator is already a path.
#[cfg(unix)]
pub fn pipe_path(name: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    runtime_dir()
        .join(format!("{}.sock", sanitise(name)))
        .to_string_lossy()
        .into_owned()
}

/// A unique pipe name for tests, so parallel test binaries never collide.
pub fn unique_pipe_name(label: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let leaf = format!("argus-test-{}-{}-{n}", sanitise(label), std::process::id());

    #[cfg(windows)]
    {
        format!(r"\\.\pipe\{leaf}")
    }
    // A socket is a real file, so tests put theirs where `pipe_path` would
    // resolve a bare name -- otherwise a test that strips the directory off
    // this name would be asking for a different endpoint. Each name is
    // unique, so parallel test binaries still never collide.
    #[cfg(unix)]
    {
        runtime_dir()
            .join(format!("{leaf}.sock"))
            .to_string_lossy()
            .into_owned()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn a_bare_name_gets_the_local_pipe_prefix() {
        assert_eq!(pipe_path("argus-test"), r"\\.\pipe\argus-test");
    }

    #[cfg(windows)]
    #[test]
    fn a_full_path_is_left_alone() {
        assert_eq!(pipe_path(r"\\.\pipe\argus-test"), r"\\.\pipe\argus-test");
    }

    #[cfg(unix)]
    #[test]
    fn a_bare_name_becomes_a_socket_beside_the_default_one() {
        let path = pipe_path("argus-test");
        assert!(path.ends_with("argus-test.sock"), "{path}");
        assert!(path.contains('/'), "{path}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_is_left_alone() {
        assert_eq!(
            pipe_path("/tmp/somewhere/argus.sock"),
            "/tmp/somewhere/argus.sock"
        );
    }

    #[test]
    fn the_default_name_is_already_a_full_path() {
        assert_eq!(pipe_path(&default_pipe_name()), default_pipe_name());
    }
}
