//! The Argus daemon.
//!
//! Owns every live session and serves the control API on a named pipe. It runs
//! as its own process, apart from any UI, which is the whole reason the project
//! is split daemon/client: closing the Argus window must never kill a running
//! agent.
//!
//! ```text
//! argus-engine [--manifests DIR] [--pipe NAME] [--state FILE]
//! ```
//!
//! Remembers its sessions in `--state` (by default
//! `%LOCALAPPDATA%\Argus\state.json`), so the list survives a restart. The
//! sessions themselves do not: when the engine stops, its pseudo-consoles close
//! and every child process goes with them. Those come back listed as `lost`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use argus_control_server::{Server, default_pipe_name, pipe_path};
use argus_engine::{Engine, default_state_path};
use tokio::sync::watch;

struct Args {
    manifests: PathBuf,
    pipe: String,
    state: PathBuf,
}

/// Three flags still do not justify an argument-parsing dependency in a binary
/// nobody types much. The CLI, with subcommands and help text, has one.
fn parse_args() -> Result<Args> {
    let mut args = Args {
        manifests: PathBuf::from("manifests"),
        pipe: default_pipe_name(),
        state: default_state_path(),
    };

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--manifests" => {
                args.manifests = argv.next().context("--manifests needs a directory")?.into();
            }
            "--pipe" => args.pipe = pipe_path(&argv.next().context("--pipe needs a name")?),
            "--state" => args.state = argv.next().context("--state needs a file path")?.into(),
            "-h" | "--help" => {
                println!("usage: argus-engine [--manifests DIR] [--pipe NAME] [--state FILE]");
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try --help)"),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;

    let engine = Engine::load_manifest_dir(&args.manifests)?;
    let agents: Vec<&str> = engine.agent_ids().collect();
    eprintln!("[argus-engine] agents: {}", agents.join(", "));

    // The pipe is claimed *before* the state file is read. A second engine on
    // the same pipe has to fail here, before it gets a chance to read the first
    // engine's live sessions, decide they were lost in a crash, and write that
    // down. The state file's own lock covers engines on different pipes.
    let server = Server::bind(&args.pipe).with_context(|| {
        format!(
            "could not create {} -- is another engine already running?",
            args.pipe
        )
    })?;

    // Reading and writing the state file is blocking I/O; at startup nothing
    // else is running yet, but it goes through the blocking pool anyway so the
    // rule has no exceptions to remember.
    let state_path = args.state.clone();
    let (engine, report) = tokio::task::spawn_blocking(move || engine.with_state_file(&state_path))
        .await
        .context("loading state panicked")??;

    eprintln!("[argus-engine] state: {}", args.state.display());
    if let Some(aside) = &report.quarantined {
        eprintln!(
            "[argus-engine] WARNING: the state file could not be read and was moved to {}",
            aside.display()
        );
        eprintln!(
            "[argus-engine]          starting with an empty session list; nothing was deleted"
        );
    }
    if report.restored > 0 {
        eprintln!(
            "[argus-engine] restored {} session(s) from the last run ({} lost when it stopped)",
            report.restored, report.lost
        );
    }

    eprintln!("[argus-engine] listening on {}", server.name());
    eprintln!("[argus-engine] Ctrl+C to stop");

    // One shutdown signal, observed by both the server and the persister.
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        // Only an actual Ctrl+C stops the engine. If the handler cannot be
        // registered at all, `ctrl_c()` returns an error at once -- and treating
        // that as a signal would shut the engine down the moment it started.
        // The UI launches the engine detached, with no console, which is exactly
        // the kind of process where that could happen. No handler means no
        // Ctrl+C, not "stop now".
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                let _ = stop_tx.send(true);
            }
            Err(error) => {
                eprintln!(
                    "[argus-engine] Ctrl+C handling unavailable ({error}); running until killed"
                );
                std::future::pending::<()>().await;
            }
        }
    });
    let stopped = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };

    let engine = Arc::new(engine);
    let persister = tokio::spawn(Arc::clone(&engine).run_persister(stopped(stop_rx.clone())));

    server.run(Arc::clone(&engine), stopped(stop_rx)).await?;

    // The persister does a final save on the way out. Waiting for it means a
    // Ctrl+C never loses the last second of changes.
    let _ = persister.await;
    eprintln!("[argus-engine] stopped");
    Ok(())
}
