//! Headless terminal emulation: raw PTY bytes in, an emulated screen grid out.
//!
//! Status detection must run against a *rendered* screen, not the raw byte
//! stream. Agent CLIs redraw themselves constantly -- cursor moves, line
//! clears, spinner frames, in-place rewrites -- so the bytes that carry
//! "Do you want to allow this edit?" onto the screen may be interleaved with,
//! or overwritten by, bytes that have nothing to do with it. Matching regexes
//! against the byte soup gives false positives on text that was erased and
//! false negatives on text assembled from several writes.
//!
//! Wrapping `alacritty_terminal` (the parser and grid, with no renderer
//! attached) gives us the same screen the user would see, and matching against
//! that is the difference between status detection that works and status
//! detection that mostly works.
//!
//! Implemented in step 2. See DESIGN.md.
