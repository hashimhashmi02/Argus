//! The Argus daemon.
//!
//! Owns every live session and serves the control API on a named pipe. It runs
//! as its own process, apart from any UI, which is the whole reason the project
//! is split daemon/client: closing the Argus window must never kill a running
//! agent.
//!
//! ```text
//! argus-engine [--manifests DIR] [--pipe NAME]
//! ```
//!
//! For now it runs in the foreground and stops on Ctrl+C, taking its sessions
//! with it. Surviving an engine restart is step 7; running as a background
//! service is later still.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use argus_control_server::{Server, default_pipe_name};
use argus_engine::Engine;

struct Args {
    manifests: PathBuf,
    pipe: String,
}

/// Two flags do not justify an argument-parsing dependency. The CLI in step 6,
/// with subcommands and help text, will.
fn parse_args() -> Result<Args> {
    let mut args = Args {
        manifests: PathBuf::from("manifests"),
        pipe: default_pipe_name(),
    };

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--manifests" => {
                args.manifests = argv.next().context("--manifests needs a directory")?.into();
            }
            "--pipe" => args.pipe = argv.next().context("--pipe needs a name")?,
            "-h" | "--help" => {
                println!("usage: argus-engine [--manifests DIR] [--pipe NAME]");
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

    let server = Server::bind(&args.pipe).with_context(|| {
        format!(
            "could not create {} -- is another engine already running?",
            args.pipe
        )
    })?;
    eprintln!("[argus-engine] listening on {}", server.name());
    eprintln!("[argus-engine] Ctrl+C to stop");

    server
        .run(Arc::new(engine), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    eprintln!("[argus-engine] stopped");
    Ok(())
}
