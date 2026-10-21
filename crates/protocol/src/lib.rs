//! The engine's control API: message types and wire framing.
//!
//! # A crate with no I/O
//!
//! This crate defines what the engine and its clients say to each other and
//! how those messages are cut out of a byte stream. It never opens a pipe,
//! reads a socket, or depends on an async runtime. Both the engine and every
//! client -- the CLI today, the Tauri UI later -- depend on it, so the wire
//! format is one definition rather than two that have to be kept in step: a
//! breaking change fails to compile on both sides at once.
//!
//! What sharing a crate cannot do is protect two *separately built* binaries
//! from each other, which is why every request also carries
//! [`PROTOCOL_VERSION`]. See [`Envelope`].
//!
//! # On the wire
//!
//! Length-prefixed JSON frames; see [`codec`].
//!
//! ```
//! use argus_protocol::{Envelope, FrameDecoder, Request, encode};
//!
//! let frame = encode(&Envelope::new(Request::ReadScreen { id: "3fa1c2d4".into() })).unwrap();
//!
//! // The 4-byte length prefix, then readable JSON.
//! assert_eq!(&frame[..4], &(frame.len() as u32 - 4).to_be_bytes());
//! assert_eq!(
//!     std::str::from_utf8(&frame[4..]).unwrap(),
//!     r#"{"version":1,"method":"read_screen","id":"3fa1c2d4"}"#,
//! );
//!
//! // Delivered in awkward pieces, reassembled whole.
//! let mut decoder = FrameDecoder::new();
//! decoder.push(&frame[..3]);
//! assert!(decoder.next_frame::<Envelope>().unwrap().is_none());
//! decoder.push(&frame[3..]);
//! let envelope: Envelope = decoder.next_frame().unwrap().unwrap();
//! assert_eq!(envelope.request, Request::ReadScreen { id: "3fa1c2d4".into() });
//! ```

pub mod codec;
mod message;

pub use argus_reducer::Status;
pub use codec::{CodecError, FrameDecoder, MAX_FRAME_LEN, encode};
pub use message::{
    AgentInfo, Envelope, ErrorCode, PROTOCOL_VERSION, Reply, Request, Response, RpcError,
    ScreenView, SessionInfo,
};
