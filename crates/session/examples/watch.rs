//! Step 4 checkpoint: a live agent session with status, end to end.
//!
//! Spawns whatever a manifest describes, forwards your keystrokes to it, and
//! prints every status transition as it happens -- together with the manifest
//! rule responsible, so a surprising classification can be traced straight to a
//! line of JSON.
//!
//! Run with:  cargo run -p argus-session --example watch
//!            cargo run -p argus-session --example watch -- manifests/claude-code.json
//!
//! Type commands and watch the status move: `working` while the shell runs
//! something, `idle` once the prompt comes back and holds for the debounce.
//! `exit` ends it.

use std::io::BufRead;
use std::sync::Arc;

use anyhow::{Context, Result};
use argus_manifest::Agent;
use argus_reducer::Status;
use argus_session::{Session, SessionConfig, SessionSnapshot};

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "manifests/shell.json".to_string());
    let agent = Agent::from_path(&path).with_context(|| format!("loading {path}"))?;
    eprintln!("[argus] agent `{}` from {path}", agent.id());

    let session = Session::spawn(Arc::new(agent), SessionConfig::default())?;
    let mut handle = session.handle();

    // Reading stdin blocks, so it goes on its own OS thread -- the same rule that
    // puts the PTY reader on one. It hands each line to the runtime to deliver.
    let input = handle.clone();
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            // `\r`, not `\n`: Enter on a real terminal is a carriage return, and
            // an interactive program reading raw input may not treat a line feed
            // as one.
            if !runtime.block_on(input.send_text(&format!("{line}\r"))) {
                break;
            }
        }
    });

    let mut last: Option<(Status, Option<String>)> = None;
    loop {
        let snapshot = handle.snapshot();
        let current = (snapshot.status, snapshot.rule.clone());

        if last.as_ref() != Some(&current) {
            report(&snapshot);
            last = Some(current);
        }

        if !handle.changed().await {
            break;
        }
    }

    session.wait().await?;
    eprintln!("[argus] session finished");
    Ok(())
}

/// One line per transition, plus the bottom of the screen for context.
fn report(snapshot: &SessionSnapshot) {
    let since = snapshot
        .since
        .saturating_since(argus_reducer::Timestamp::ZERO);
    let rule = snapshot.rule.as_deref().unwrap_or("-");
    let exit = snapshot
        .exit_code
        .map(|code| format!("  exit={code}"))
        .unwrap_or_default();

    println!(
        "[{:>6.2}s] {:<9} rule={rule}{exit}",
        since.as_secs_f64(),
        format!("{:?}", snapshot.status),
    );

    // Last two non-blank lines: enough to see *why* without reprinting the
    // whole screen on every transition.
    let tail: Vec<&String> = snapshot
        .screen
        .lines()
        .iter()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(2)
        .collect();
    for line in tail.into_iter().rev() {
        println!("           | {line}");
    }
}
