//! Framing: how messages are cut out of a byte stream.
//!
//! # Why framing is needed at all
//!
//! A named pipe in byte mode, like a TCP socket, delivers a *stream*. It has no
//! idea where one message ends and the next begins. Write two 100-byte messages
//! and the reader may get one 200-byte read, or a 37-byte read followed by a
//! 163-byte one. Any code that assumes "one read = one message" works on a
//! developer's idle machine and corrupts messages under load.
//!
//! So every message is a frame: a 4-byte length, then exactly that many bytes of
//! JSON.
//!
//! ```text
//! +----------------+-------------------------------+
//! | len: u32, BE   | JSON payload, `len` bytes     |
//! +----------------+-------------------------------+
//! ```
//!
//! Big-endian ("network byte order") by convention. Both ends always run on the
//! same machine, so the choice itself does not matter -- what matters is that it
//! is fixed and written down.
//!
//! # Why sans-I/O
//!
//! [`FrameDecoder`] never reads from anything. You push bytes in, however they
//! arrived and in whatever sized pieces, and pull complete messages out. That
//! keeps this crate free of any async runtime, lets the server and every client
//! share the exact same framing logic, and makes the nasty cases -- a header
//! split across reads, two frames in one read -- testable with byte literals
//! rather than with a real pipe and luck.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Bytes in the length prefix.
pub const HEADER_LEN: usize = 4;

/// The largest payload either side will accept.
///
/// Without a limit, the length prefix is an allocation request from whoever is
/// on the other end of the pipe. A buggy or hostile client sends `0xFFFFFFFF`
/// and the daemon dutifully tries to buffer four gigabytes. A 200x60 screen
/// serialises to around 15 KB, so 1 MiB leaves two orders of magnitude of
/// headroom while keeping the worst case survivable.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The peer announced a frame larger than [`MAX_FRAME_LEN`].
    ///
    /// **Fatal for the connection.** The decoder cannot skip the frame without
    /// reading it, and reading it is exactly what the limit exists to refuse, so
    /// there is no way to find where the next frame starts. Drop the connection.
    #[error("frame of {len} bytes exceeds the {max} byte limit")]
    TooLarge { len: usize, max: usize },

    /// A complete frame arrived but its payload was not the expected JSON.
    ///
    /// **Recoverable.** The frame boundary was known, so the bad frame has
    /// already been consumed and the next one can be read normally. A client
    /// that sent one malformed request deserves an error reply, not a dropped
    /// connection.
    #[error("frame payload is not a valid message")]
    Payload(#[source] serde_json::Error),

    /// The message could not be serialised. Only possible for types whose
    /// `Serialize` impl can fail, which ours cannot; present for completeness.
    #[error("message could not be serialised")]
    Encode(#[source] serde_json::Error),
}

impl CodecError {
    /// Whether the connection can carry on after this error.
    pub fn is_fatal(&self) -> bool {
        matches!(self, CodecError::TooLarge { .. })
    }
}

/// Serialise a message into one complete frame.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, CodecError> {
    let payload = serde_json::to_vec(message).map_err(CodecError::Encode)?;
    if payload.len() > MAX_FRAME_LEN {
        // Checked on the way out too. Sending a frame the peer is obliged to
        // reject would just turn our bug into the peer's fatal error.
        return Err(CodecError::TooLarge {
            len: payload.len(),
            max: MAX_FRAME_LEN,
        });
    }

    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    // The cast cannot truncate: `payload.len()` is at most MAX_FRAME_LEN.
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Reassembles frames from a byte stream delivered in arbitrary pieces.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes as they arrived from the transport.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Take the next complete message, if one has fully arrived.
    ///
    /// `Ok(None)` means "not yet": keep reading and pushing. Call this in a loop
    /// after every push, because one read can deliver several frames at once.
    pub fn next_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        let Some(header) = self.buffer.first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*header) as usize;

        // Rejected the moment the header arrives, *before* a single byte of the
        // body is buffered. Checking after accumulating the body would defeat
        // the point of the limit entirely.
        if len > MAX_FRAME_LEN {
            return Err(CodecError::TooLarge {
                len,
                max: MAX_FRAME_LEN,
            });
        }

        let total = HEADER_LEN + len;
        if self.buffer.len() < total {
            return Ok(None);
        }

        // Consume the frame whether or not it parses, so a malformed payload
        // costs one error reply rather than wedging every later frame behind it.
        let frame: Vec<u8> = self.buffer.drain(..total).collect();
        serde_json::from_slice(&frame[HEADER_LEN..])
            .map(Some)
            .map_err(CodecError::Payload)
    }

    /// Bytes received but not yet returned as a message.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}
