//! ConPTY-backed implementation of [`PtyProcess`].

use std::io::{Read, Write};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

use crate::{ExitStatus, PtyProcess, SpawnConfig};

/// The result of a successful spawn: a control handle and, separately, the
/// output stream.
///
/// They are handed back as two values rather than one object because they have
/// genuinely different threading requirements. `process` is cheap to poke at
/// from anywhere; `reader` blocks, and whoever takes it is accepting
/// responsibility for parking a thread on it.
pub struct SpawnedPty {
    pub process: ConPtyProcess,
    /// Blocking output stream. Give this its own thread.
    pub reader: Box<dyn Read + Send>,
}

/// One child process attached to a ConPTY.
///
/// **Field order is drop order, and drop order matters here.** Rust drops
/// struct fields in declaration order, and dropping the master is what calls
/// `ClosePseudoConsole`. That call blocks until the console's pipes are closed,
/// so if `writer` -- which holds a handle to the pseudo-console's *input* pipe
/// -- were still alive at that moment, the two would wait on each other and the
/// process would hang on exit with every session leaked.
///
/// So: `writer` first, then `child`, then `master` last. This is the kind of
/// bug that does not show up in a unit test, only as "the app takes forever to
/// close," which is why it is written down rather than left to be rediscovered.
pub struct ConPtyProcess {
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
}

impl ConPtyProcess {
    /// Open a pseudo-console and launch `cfg.program` inside it.
    pub fn spawn(cfg: &SpawnConfig) -> Result<SpawnedPty> {
        // `NativePtySystem` resolves to ConPTY on Windows 10 1809 and later,
        // falling back to the bundled winpty shim on older builds. Argus targets
        // Windows 11, so in practice this is always ConPTY.
        let pty_system = NativePtySystem::default();

        let pair = pty_system
            .openpty(PtySize {
                rows: cfg.rows,
                cols: cfg.cols,
                // Pixel dimensions are meaningful to programs drawing inline
                // images (sixel, iTerm2 protocol). We render text, so zero is
                // the honest answer rather than an invented number.
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open a pseudo-console (ConPTY)")?;

        let mut cmd = CommandBuilder::new(&cfg.program);
        cmd.args(&cfg.args);
        if let Some(dir) = &cfg.cwd {
            cmd.cwd(dir);
        }
        scrub_env(&mut cmd, &cfg.scrub_env_prefixes);

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to spawn {}", cfg.program.display()))?;

        // Take the reader and writer while the master is still in the pair, then
        // let the slave go.
        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone the pty reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to take the pty writer")?;

        // This drop is load-bearing. The slave owns a handle to the write end of
        // the console. As long as any copy of that handle is open, the pipe is
        // not considered closed -- so when the child exits, the reader never
        // sees EOF and the thread blocked on it hangs forever. Argus would show
        // a finished agent as still running, permanently. Dropping the slave the
        // moment the child owns its own copy is what makes "the child exited"
        // observable at all.
        drop(pair.slave);

        Ok(SpawnedPty {
            process: ConPtyProcess {
                writer,
                child,
                master: pair.master,
            },
            reader,
        })
    }

    /// OS process id of the child, if it is still alive.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }
}

impl PtyProcess for ConPtyProcess {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        // Flush every write instead of letting the buffer fill. Input to an
        // interactive agent is latency-sensitive and tiny -- a keystroke, an
        // approval -- and a keystroke sitting in a userspace buffer waiting for
        // company is indistinguishable, from the outside, from a hung agent.
        self.writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize the pty")
    }

    fn kill(&mut self) -> Result<()> {
        self.child.kill().context("failed to kill the pty child")?;
        Ok(())
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.child.try_wait().context("failed to poll the child")?;
        Ok(status.map(|s| ExitStatus {
            code: s.exit_code(),
            success: s.success(),
        }))
    }
}

/// Remove every environment variable whose name starts with one of `prefixes`.
///
/// `CommandBuilder` inherits the parent environment, so this is a subtraction
/// from what the child would otherwise receive. Matching is case-insensitive
/// because Windows environment variable names are: `Argus_Token` and
/// `ARGUS_TOKEN` are the same variable to the OS, and a scrub rule that only
/// catches one spelling is a scrub rule that does not work.
fn scrub_env(cmd: &mut CommandBuilder, prefixes: &[String]) {
    if prefixes.is_empty() {
        return;
    }
    let upper: Vec<String> = prefixes.iter().map(|p| p.to_uppercase()).collect();

    for (key, _) in std::env::vars_os() {
        let Some(key_str) = key.to_str() else {
            // A non-UTF-8 variable name cannot match an ASCII prefix rule, so
            // leaving it alone is correct rather than merely convenient.
            continue;
        };
        let key_upper = key_str.to_uppercase();
        if upper.iter().any(|p| key_upper.starts_with(p.as_str())) {
            cmd.env_remove(&key);
        }
    }
}
