//! Live output, for a client that wants the terminal itself rather than a
//! summary of it.

use std::sync::Arc;

use tokio::sync::broadcast;

/// What a watcher receives after its starting repaint.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// Output, exactly as the agent wrote it.
    Output(Arc<str>),
    /// Start over from this. Sent after a resize: the emulator reflows its
    /// grid to the new size, and a client drawing at the old size would no
    /// longer match it.
    Repaint(Repaint),
}

/// Terminal output that recreates the session's screen in a blank terminal of
/// this size. See `TerminalEmulator::repaint`.
#[derive(Debug, Clone)]
pub struct Repaint {
    pub cols: u16,
    pub rows: u16,
    pub data: Arc<str>,
}

/// A starting point and everything after it.
///
/// Both halves are produced by the session task in one step, between two
/// reads from the child, so nothing can fall between them: every byte the
/// repaint does not show arrives on the stream, and nothing the repaint shows
/// arrives twice.
#[derive(Debug)]
pub struct Watch {
    pub repaint: Repaint,
    /// Closed when the session finishes. A watcher that falls more than
    /// [`STREAM_DEPTH`] items behind gets `Lagged` and should watch again
    /// for a fresh repaint, because the bytes it missed are gone.
    pub stream: broadcast::Receiver<StreamItem>,
}

/// How many items a watcher may fall behind before it loses its place.
///
/// Items are reads from the child, a few KiB at most each, so this is a
/// generous window for a client that is merely slow; one that is further
/// behind than this is better served by a repaint than by a backlog.
pub const STREAM_DEPTH: usize = 1024;

/// Turns arbitrary reads from the child into whole UTF-8 text.
///
/// A read can end in the middle of a multi-byte character -- the PTY knows
/// nothing about characters -- and a watcher receives text, not bytes. So the
/// incomplete tail of each read is held back and put in front of the next.
#[derive(Debug, Default)]
pub(crate) struct Utf8Chunks {
    carry: Vec<u8>,
}

impl Utf8Chunks {
    pub(crate) fn push(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(chunk);
        let complete = complete_prefix(&bytes);
        self.carry = bytes.split_off(complete);
        // Lossy only for bytes that are not UTF-8 at all, which ConPTY does not
        // produce; a watcher shows U+FFFD rather than the stream being dropped.
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// The length of `bytes` without a trailing character that has not finished
/// arriving.
fn complete_prefix(bytes: &[u8]) -> usize {
    // A UTF-8 character is at most four bytes, so an unfinished one starts in
    // the last three.
    for back in 1..=bytes.len().min(3) {
        let byte = bytes[bytes.len() - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue; // a continuation byte; its lead is further back
        }
        let needed = match byte {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        return if needed > back {
            bytes.len() - back
        } else {
            bytes.len()
        };
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_passes_straight_through() {
        let mut chunks = Utf8Chunks::default();
        assert_eq!(chunks.push(b"hello"), "hello");
        assert_eq!(chunks.push(b""), "");
    }

    #[test]
    fn a_character_split_across_reads_comes_out_whole() {
        let bytes = "a日b".as_bytes(); // `日` is three bytes
        for split in 1..bytes.len() {
            let mut chunks = Utf8Chunks::default();
            let mut text = chunks.push(&bytes[..split]);
            text += &chunks.push(&bytes[split..]);
            assert_eq!(text, "a日b", "split at byte {split}");
        }
    }

    #[test]
    fn a_character_split_three_ways_comes_out_whole() {
        let bytes = "😀".as_bytes(); // four bytes
        let mut chunks = Utf8Chunks::default();
        assert_eq!(chunks.push(&bytes[..1]), "");
        assert_eq!(chunks.push(&bytes[1..2]), "");
        assert_eq!(chunks.push(&bytes[2..]), "😀");
    }

    #[test]
    fn invalid_bytes_do_not_stall_the_stream() {
        let mut chunks = Utf8Chunks::default();
        assert_eq!(chunks.push(b"a\xFFb"), "a\u{FFFD}b");
    }
}
