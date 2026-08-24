//! Step 1 checkpoint: prove ConPTY works end to end on this machine.
//!
//! Spawns a shell on a pseudo-console, streams its raw output to stdout, and
//! forwards this terminal's stdin into it. Nothing is emulated or parsed --
//! what you see is the exact byte stream the child produced, escape sequences
//! and all. That is intentional: the unreadable parts are the argument for the
//! `terminal-emu` crate in step 2. Trying to regex a status out of *this* is how
//! you build status detection that fails in the field.
//!
//! Deliberately synchronous. No tokio, no async. One thread reads, one thread
//! writes, and the threading question stays visible instead of disappearing
//! behind an executor.
//!
//! Run with:  cargo run -p argus-pty --bin pty-smoke
//!            cargo run -p argus-pty --bin pty-smoke -- powershell.exe -NoLogo
//! Leave with: type `exit`, or press Ctrl+C.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use argus_pty::{ConPtyProcess, PtyProcess, SpawnConfig, resolve_shell};

/// Device Status Report: "terminal, where is the cursor?"
///
/// ConPTY sends this to the terminal *itself* at startup, before any shell has
/// said a word, and then holds output back until it gets an answer. It is not
/// the shell asking, so swapping PowerShell for cmd.exe does not avoid it --
/// this was found the hard way, by watching a smoke test hang with exactly six
/// bytes of output.
const DSR_CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// The reply: "cursor is at row 1, column 1."
///
/// A lie, but a harmless one at startup, and the only thing ConPTY is waiting
/// for. Step 2 replaces this with a real answer, because `alacritty_terminal`
/// will actually know where the cursor is. That this crate has to fake it at all
/// is a small proof that a PTY without an emulator behind it is only half a
/// terminal.
const DSR_CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// How long to wait for the child to produce its first output before forwarding
/// input anyway.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to give a child to exit on its own before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> Result<()> {
    let (program, args) = choose_program()?;
    eprintln!(
        "[argus] spawning {} on a ConPTY (120x30)",
        program.display()
    );

    let cfg = SpawnConfig::new(&program).args(args);
    let spawned = ConPtyProcess::spawn(&cfg)?;

    // Shared because two threads write to the child: the main thread forwards
    // your keystrokes, and the reader thread answers ConPTY's cursor query.
    //
    // The reader gets a `Weak`, not a second `Arc`, and that is load-bearing.
    // Closing the pseudo-console is what finally ends the reader (see
    // `close_pty` below), and that requires main to hold the *only* strong
    // reference. A reader holding an `Arc` would keep alive the very thing whose
    // destruction it is waiting for.
    let process = Arc::new(Mutex::new(spawned.process));

    if let Some(pid) = process.lock().unwrap().process_id() {
        eprintln!("[argus] child pid {pid}");
    }
    eprintln!("[argus] type `exit` to end the session\n");

    let (ready_tx, ready_rx) = channel();
    let reader_thread = spawn_reader(spawned.reader, Arc::downgrade(&process), ready_tx);

    // Do not send anything until the child has spoken once.
    //
    // ConPTY performs a startup handshake with the terminal, and bytes written
    // into the pty before that handshake completes are silently dropped -- no
    // error, the input simply never reaches the child. A human never notices,
    // because a human waits to see a prompt. A pipe does not wait, which is how
    // this was found: piping `exit` into the smoke test hung forever, because
    // the shell never received the word `exit` and so never exited.
    //
    // Waiting for first output is the cheap version of "wait until ready". Step
    // 4 gets a real readiness signal from the manifest's screen-match rules;
    // until then, first byte is a good enough proxy.
    wait_for_first_output(&ready_rx);

    forward_stdin(&process)?;
    wait_for_exit(&process)?;
    close_pty(process);

    match reader_thread.join() {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("reader thread panicked"),
    }

    eprintln!("[argus] pty closed cleanly");
    Ok(())
}

/// Pump PTY output to stdout on a dedicated thread, answering ConPTY's cursor
/// query along the way.
///
/// The read blocks. It gets its own OS thread rather than sharing one with
/// anything else, because there is no way to ask it "is there data?" without
/// committing to wait. This is the same constraint that will force
/// `spawn_blocking` (or a dedicated thread) in the async engine later -- the
/// shape of the problem does not change, only the machinery around it.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    process: Weak<Mutex<ConPtyProcess>>,
    ready: Sender<()>,
) -> std::thread::JoinHandle<Result<()>> {
    std::thread::spawn(move || -> Result<()> {
        let mut buf = [0u8; 4096];
        let mut stdout = std::io::stdout();
        let mut announced_ready = false;

        loop {
            match reader.read(&mut buf) {
                // Zero bytes means the pseudo-console was closed. Note that this
                // does *not* happen merely because the child exited -- see
                // `close_pty`.
                Ok(0) => return Ok(()),
                Ok(n) => {
                    let chunk = &buf[..n];
                    stdout.write_all(chunk)?;
                    // Flush per chunk. Buffered terminal output arriving in
                    // bursts makes a responsive agent look frozen.
                    stdout.flush()?;

                    // Naive substring match, and knowingly so: a query split
                    // across two reads would be missed. A real emulator parses a
                    // byte stream with a state machine precisely because escape
                    // sequences do not respect buffer boundaries. That is step 2.
                    if contains(chunk, DSR_CURSOR_QUERY)
                        && let Some(process) = process.upgrade()
                    {
                        process.lock().unwrap().write(DSR_CURSOR_REPLY)?;
                    }

                    if !announced_ready {
                        announced_ready = true;
                        // A closed receiver just means the main thread stopped
                        // caring; not an error worth failing over.
                        let _ = ready.send(());
                    }
                }
                Err(e) => return Err(e).context("pty read failed"),
            }
        }
    })
}

fn wait_for_first_output(ready: &Receiver<()>) {
    if ready.recv_timeout(READY_TIMEOUT).is_err() {
        eprintln!("[argus] child produced no output in {READY_TIMEOUT:?}, sending input anyway");
    }
}

/// Pump our stdin into the child until one of them stops.
///
/// Reads raw bytes rather than lines so that control characters -- Ctrl+C,
/// arrow keys, the single-keypress answers that agent prompts expect -- pass
/// straight through instead of being swallowed by line buffering.
fn forward_stdin(process: &Arc<Mutex<ConPtyProcess>>) -> Result<()> {
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 1024];

    loop {
        // If the child died, stop forwarding into a dead pipe.
        if process.lock().unwrap().try_wait()?.is_some() {
            return Ok(());
        }
        match stdin.read(&mut buf) {
            // Our own stdin ended (Ctrl+Z, or input was piped in). Stop
            // forwarding, but do not kill the child -- it may still be busy
            // finishing what it was told to do, and the whole point of the
            // reader is to capture that final output.
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
///
/// Polling rather than blocking on `wait()` because a hung agent must not hang
/// Argus with it. A bounded wait followed by a kill is the behaviour the engine
/// needs anyway, so it may as well be the behaviour here.
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

/// Drop the process, which closes the pseudo-console and finally ends the
/// reader.
///
/// This is the part that Unix intuition gets wrong. On a Unix pty, the child
/// exiting closes the slave and the reader sees EOF for free. ConPTY does not
/// work that way: the output pipe belongs to the *pseudo-console*, not to the
/// child, so it stays open after the child is gone and the reader blocks
/// forever. `ClosePseudoConsole` -- which runs when the master is dropped -- is
/// what actually ends the stream, and only the owner can call it.
///
/// So child exit and pty EOF are two separate events on Windows, and the engine
/// has to sequence them by hand: observe the exit, then close the console.
fn close_pty(process: Arc<Mutex<ConPtyProcess>>) {
    drop(process);
}

/// Whether `haystack` contains `needle`.
///
/// Hand-rolled to keep this crate's dependency list at two. `windows()` panics
/// on a zero-length needle, which cannot happen with the constants above, but
/// the guard costs nothing and outlives the assumption.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Pick what to run: `argv[1..]` if given, otherwise `cmd.exe`.
///
/// The default is deliberately *not* `resolve_shell()`, which prefers
/// PowerShell. PowerShell on a bare pipe produces a wall of escape sequences
/// before it produces a prompt, which makes a poor first checkpoint. `cmd.exe`
/// is the quietest shell on the machine, and this test's job is to prove the
/// *pipe* works, not the shell. Pass a program explicitly to try another one.
fn choose_program() -> Result<(PathBuf, Vec<OsString>)> {
    let mut argv = std::env::args_os().skip(1);
    if let Some(program) = argv.next() {
        return Ok((PathBuf::from(program), argv.collect()));
    }

    if let Some(root) = std::env::var_os("SystemRoot") {
        let cmd = PathBuf::from(root).join("System32").join("cmd.exe");
        if cmd.is_file() {
            return Ok((cmd, Vec::new()));
        }
    }

    // No cmd.exe is an odd machine, but falling back to whatever shell exists
    // beats refusing to run at all.
    let shell = resolve_shell()
        .context("no shell found -- looked for pwsh.exe, powershell.exe and cmd.exe")?;
    Ok((shell.program, shell.args))
}
