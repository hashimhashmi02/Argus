//! The one thread in the system that is allowed to block.

use std::io::Read;

use tokio::sync::mpsc;

/// How many chunks may be in flight before the reader thread is made to wait.
///
/// Bounded, not unbounded, and that is a deliberate choice about what happens
/// under load. An agent that dumps a megabyte of build output faster than the
/// session can emulate it would, on an unbounded channel, simply grow the queue
/// until memory ran out. Bounded, the reader thread blocks on `blocking_send`,
/// stops draining the PTY, the console's buffer fills, and the *child* is made
/// to wait -- which is exactly the backpressure a real terminal applies.
const CHANNEL_DEPTH: usize = 64;

/// Move a blocking PTY reader onto its own OS thread, feeding chunks into a
/// channel the session task can await.
///
/// This is the hardening rule from DESIGN.md at its most concrete. The reader
/// `portable-pty` hands back genuinely blocks: there is no way to ask it whether
/// data is available without committing to wait. Parked on a `tokio` worker it
/// would hold that worker hostage for as long as the child stays quiet, and a
/// handful of idle sessions on a low-core machine would starve the runtime of
/// threads to run anything else.
///
/// A dedicated thread rather than `spawn_blocking` because this one blocks for
/// the entire life of the session rather than for a bounded piece of work, and
/// filling a slot in the blocking pool forever is just the same problem with
/// extra steps.
pub(crate) fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
) -> (mpsc::Receiver<Vec<u8>>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(CHANNEL_DEPTH);

    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                // The pseudo-console was closed. Note that this does not happen
                // merely because the child exited: on Windows the output pipe
                // belongs to the console, not to the process, so the session
                // task has to close it explicitly. See `SessionTask::finish`.
                Ok(0) => break,
                Ok(n) => {
                    // Blocking send, on a thread that is allowed to block. If
                    // the session task has gone away the channel is closed and
                    // there is nothing left to read for.
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // Dropping `tx` here is what tells the session task the PTY is finished.
    });

    (rx, handle)
}
