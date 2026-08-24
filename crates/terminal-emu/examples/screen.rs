//! Step 2 checkpoint: the same ConPTY stream as step 1, rendered.
//!
//! Step 1 dumped raw PTY bytes to stdout and they were largely unreadable --
//! escape sequences, cursor jumps, characters overwritten in place. This example
//! feeds that identical stream through [`TerminalEmulator`] and prints the
//! resulting *screen* instead, redrawing whenever it changes.
//!
//! Two things from step 1 disappear here, and both are the point:
//!
//! - The hardcoded `ESC[1;1R` reply to ConPTY's cursor query is gone. The
//!   emulator knows where the cursor actually is and answers truthfully,
//!   through the ordinary write-back channel.
//! - The naive substring search for `ESC[6n` is gone. The emulator is a real
//!   parser, so a sequence split across two reads is handled without anyone
//!   having to think about buffer boundaries.
//!
//! This lives in `examples/` rather than `src/bin/` on purpose. Examples may use
//! dev-dependencies, so it can reach for `argus-pty` without `argus-terminal-emu`
//! itself ever depending on the PTY layer. The crate stays drivable from a
//! `&[u8]`, which is what makes `tests/screen.rs` possible.
//!
//! Run with:  cargo run -p argus-terminal-emu --example screen
//!            cargo run -p argus-terminal-emu --example screen -- powershell.exe -NoLogo
//! Leave with: type `exit`, or press Ctrl+C.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use argus_pty::{ConPtyProcess, PtyProcess, SpawnConfig, resolve_shell};
use argus_terminal_emu::TerminalEmulator;

const COLS: u16 = 100;
const ROWS: u16 = 24;

/// How long to wait for the child to speak before forwarding input anyway.
///
/// ConPTY discards anything written before its startup handshake completes.
/// See DESIGN.md.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to give a child to exit on its own before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> Result<()> {
    let (program, args) = choose_program()?;
    eprintln!(
        "[argus] spawning {} on a ConPTY ({COLS}x{ROWS})",
        program.display()
    );

    let cfg = SpawnConfig::new(&program).args(args).size(COLS, ROWS);
    let spawned = ConPtyProcess::spawn(&cfg)?;

    // The reader holds a `Weak`, not an `Arc`: closing the pseudo-console is
    // what ends the reader, and that requires main to hold the only strong
    // reference. See `close_pty`.
    let process = Arc::new(Mutex::new(spawned.process));

    if let Some(pid) = process.lock().unwrap().process_id() {
        eprintln!("[argus] child pid {pid}");
    }
    eprintln!("[argus] rendering the emulated screen -- type `exit` to end\n");

    let (ready_tx, ready_rx) = channel();
    let reader_thread = spawn_renderer(spawned.reader, Arc::downgrade(&process), ready_tx);

    wait_for_first_output(&ready_rx);
    forward_stdin(&process)?;
    wait_for_exit(&process)?;
    close_pty(process);

    match reader_thread.join() {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("renderer thread panicked"),
    }

    eprintln!("[argus] pty closed cleanly");
    Ok(())
}

/// Read the PTY, drive the emulator, redraw the screen when it changes.
fn spawn_renderer(
    mut reader: Box<dyn Read + Send>,
    process: Weak<Mutex<ConPtyProcess>>,
    ready: Sender<()>,
) -> std::thread::JoinHandle<Result<()>> {
    std::thread::spawn(move || -> Result<()> {
        let mut emu = TerminalEmulator::new(COLS, ROWS);
        let mut buf = [0u8; 4096];
        let mut last_render = String::new();
        let mut announced_ready = false;

        loop {
            match reader.read(&mut buf) {
                // The pseudo-console was closed. Note this does not happen
                // merely because the child exited -- see `close_pty`.
                Ok(0) => return Ok(()),
                Ok(n) => {
                    // The whole of step 2 in one line: bytes go in, and what
                    // comes back is not output to display but the terminal's
                    // side of a conversation.
                    let reply = emu.advance(&buf[..n]);

                    if !reply.is_empty()
                        && let Some(process) = process.upgrade()
                    {
                        process.lock().unwrap().write(&reply)?;
                    }

                    let screen = emu.screen();
                    let rendered = screen.text();
                    if rendered != last_render {
                        draw(&screen)?;
                        last_render = rendered;
                    }

                    if !announced_ready {
                        announced_ready = true;
                        let _ = ready.send(());
                    }
                }
                Err(e) => return Err(e).context("pty read failed"),
            }
        }
    })
}

/// Repaint our own terminal with the emulated screen.
///
/// Home-then-clear rather than clear-then-home so the old frame is overwritten
/// in place; clearing first makes the screen visibly blank between frames.
fn draw(screen: &argus_terminal_emu::Screen) -> Result<()> {
    let (cols, rows) = screen.size();
    let (cursor_row, cursor_col) = screen.cursor();

    let mut out = String::with_capacity((cols as usize + 1) * (rows as usize + 2));
    out.push_str("\x1b[H\x1b[2J");
    out.push_str(&format!(
        "-- emulated screen {cols}x{rows}, cursor at row {cursor_row} col {cursor_col} --\n"
    ));
    for line in screen.lines() {
        out.push_str(line);
        out.push('\n');
    }

    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn wait_for_first_output(ready: &Receiver<()>) {
    if ready.recv_timeout(READY_TIMEOUT).is_err() {
        eprintln!("[argus] child produced no output in {READY_TIMEOUT:?}, sending input anyway");
    }
}

fn forward_stdin(process: &Arc<Mutex<ConPtyProcess>>) -> Result<()> {
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 1024];

    loop {
        if process.lock().unwrap().try_wait()?.is_some() {
            return Ok(());
        }
        match stdin.read(&mut buf) {
            Ok(0) => {
                eprintln!("\n[argus] stdin closed, waiting for the child to exit");
                return Ok(());
            }
            Ok(n) => process.lock().unwrap().write(&buf[..n])?,
            Err(e) => return Err(e).context("stdin read failed"),
        }
    }
}

/// Poll until the child exits, killing it if it overstays.
fn wait_for_exit(process: &Arc<Mutex<ConPtyProcess>>) -> Result<()> {
    let deadline = Instant::now() + EXIT_TIMEOUT;

    loop {
        if let Some(status) = process.lock().unwrap().try_wait()? {
            eprintln!("[argus] child exited with code {}", status.code);
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprintln!("[argus] child outlived {EXIT_TIMEOUT:?}, killing it");
            process.lock().unwrap().kill()?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Drop the process, closing the pseudo-console and ending the reader.
///
/// On a Unix pty the child exiting closes the slave and the reader sees EOF for
/// free. ConPTY's output pipe belongs to the pseudo-console rather than the
/// child, so it stays open after the child is gone. `ClosePseudoConsole` -- run
/// when the master is dropped -- is what ends the stream.
fn close_pty(process: Arc<Mutex<ConPtyProcess>>) {
    drop(process);
}

fn choose_program() -> Result<(PathBuf, Vec<OsString>)> {
    let mut argv = std::env::args_os().skip(1);
    if let Some(program) = argv.next() {
        return Ok((PathBuf::from(program), argv.collect()));
    }

    // Unlike step 1's smoke test, this one defaults to the *preferred* shell.
    // PowerShell was a poor choice for a raw byte dump; with an emulator in
    // front of it, its redraws and colour changes resolve into a normal screen,
    // which is exactly what this checkpoint is meant to show.
    let shell = resolve_shell()
        .context("no shell found -- looked for pwsh.exe, powershell.exe and cmd.exe")?;
    Ok((shell.program, shell.args))
}
