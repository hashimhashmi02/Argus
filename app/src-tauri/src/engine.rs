//! Starting the engine when it is not already running.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// No console, and not tied to the console of whoever started it.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// Its own process group, so a Ctrl+C aimed at the UI's group never reaches it.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// Out of whatever Windows Job object the UI is running in.
///
/// The other two flags are not enough, which was found by watching a
/// "detached" engine die the moment the UI exited. A child process inherits its
/// parent's Job object regardless of console or process group, and when a job
/// configured to kill-on-close closes, every process in it dies. `cargo run`
/// runs programs in exactly such a job, so it can clean up on Ctrl+C -- and so
/// can other launchers. Breaking away is the only way a child outlives a job.
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
/// `ERROR_ACCESS_DENIED`: what `CreateProcess` returns when asked to break away
/// from a job that does not permit it.
const ERROR_ACCESS_DENIED: i32 = 5;

/// Knows where the engine binary and its manifests are.
pub struct EngineLauncher {
    exe: PathBuf,
    manifests: Option<PathBuf>,
}

impl EngineLauncher {
    /// Look beside this executable for `argus-engine.exe`, and upward from it
    /// for a `manifests` directory.
    ///
    /// Installed, all three binaries and `manifests\` sit in one folder, and
    /// the search finds it at its first step. In development both binaries are
    /// built into the same `target\debug`, and the repository's `manifests\` is
    /// a few directories up. `ARGUS_MANIFESTS` overrides either.
    pub fn locate() -> io::Result<Self> {
        let here = std::env::current_exe()?;
        let dir = here.parent().unwrap_or(Path::new("."));
        let exe = dir.join("argus-engine.exe");

        let manifests = std::env::var_os("ARGUS_MANIFESTS")
            .map(PathBuf::from)
            .or_else(|| {
                dir.ancestors()
                    .map(|a| a.join("manifests"))
                    .find(|m| m.is_dir())
            });

        Ok(Self { exe, manifests })
    }

    /// Launch the engine, detached, so it outlives this window.
    ///
    /// This is the daemon/client split doing its job. The engine is started as
    /// its own process, with no console and in its own process group, so
    /// closing the UI -- or the UI crashing -- leaves every agent running. The
    /// next time the UI opens, it simply connects to the engine that is already
    /// there.
    ///
    /// The engine's own log goes to `engine.log` beside its state file, since a
    /// detached process has nowhere else to write.
    pub fn start(&self, pipe: &str) -> anyhow::Result<()> {
        use anyhow::Context;

        if !self.exe.is_file() {
            anyhow::bail!(
                "argus-engine.exe was not found next to the UI at {} -- build it with \
                 `cargo build -p argus-engine`",
                self.exe.display()
            );
        }

        let log_path = log_path();
        if let Some(dir) = log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("could not open {}", log_path.display()))?;

        let mut command = Command::new(&self.exe);
        command.arg("--pipe").arg(pipe);
        if let Some(manifests) = &self.manifests {
            command.arg("--manifests").arg(manifests);
        }
        // Lets a test or a second UI run against a scratch state file instead
        // of the real one. Like every ARGUS_ variable, never passed on to agents.
        if let Some(state) = std::env::var_os("ARGUS_STATE") {
            command.arg("--state").arg(state);
        }
        // A session spawned without a folder runs in the engine's, and the
        // engine is remembered as having run there. Started from the Start
        // menu, the UI's own working directory is wherever Explorer chose --
        // often System32 -- so the engine starts in the user's home instead.
        if let Some(home) = std::env::var_os("USERPROFILE").filter(|h| Path::new(h).is_dir()) {
            command.current_dir(home);
        }
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);

        // Ask to leave the UI's Job object. A job that does not permit
        // breakaway refuses the whole CreateProcess call with access-denied, so
        // in that case start the engine anyway, inside the job -- it works, it
        // just will not outlive whatever owns the job. That only happens under
        // launchers like `cargo run`; started from the Start menu or Explorer,
        // the UI is in no job at all and the breakaway is simply a no-op.
        let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        command.creation_flags(detached | CREATE_BREAKAWAY_FROM_JOB);
        let spawned = match command.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                eprintln!(
                    "[argus-app] could not break the engine out of this process's Job \
                     object; it will stop when whatever launched the UI stops"
                );
                command.creation_flags(detached);
                command.spawn()
            }
            other => other,
        };

        // The child handle is dropped immediately: the UI does not own the
        // engine and must not wait on it, kill it on exit, or hold it open.
        spawned.with_context(|| format!("could not start {}", self.exe.display()))?;
        Ok(())
    }
}

fn log_path() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) => PathBuf::from(dir).join("Argus").join("engine.log"),
        None => PathBuf::from(".argus").join("engine.log"),
    }
}
