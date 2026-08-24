//! Headless terminal emulation: raw PTY bytes in, an emulated screen grid out.
//!
//! # Why this crate exists
//!
//! Status detection must run against a *rendered screen*, not the raw byte
//! stream. Agent CLIs redraw themselves constantly -- cursor moves, line
//! clears, spinner frames, in-place rewrites -- so the bytes that carry
//! "Do you want to allow this edit?" onto the screen may be interleaved with,
//! or later erased by, bytes that have nothing to do with it.
//!
//! This is not a theoretical worry. Here is PowerShell echoing the word `echo`,
//! captured from the step 1 smoke test:
//!
//! ```text
//! ^[[93me^[[?25h^[[m^[[93m^Hecho ^[[37mhello-from-powershell
//! ```
//!
//! The letter `e` is printed, the cursor is moved back over it, and then `echo`
//! is printed on top -- with four colour changes along the way. A regex looking
//! for `echo` against those bytes matches something the user never saw in that
//! form, and a regex looking for text that was later cleared matches text that
//! is no longer on screen at all.
//!
//! Feeding the bytes through `alacritty_terminal` -- the parser and grid, with
//! no renderer attached -- yields the same screen a human would see. Matching
//! against that is the difference between status detection that works and
//! status detection that works in the demo.
//!
//! # Boundary
//!
//! This crate knows nothing about PTYs, processes, async, or agents. It is a
//! function from bytes to a screen, plus the bytes a real terminal would have
//! written back. That keeps it testable with fixtures (see `tests/`) and leaves
//! the question of *where the bytes came from* to `argus-session` in step 4.
//!
//! ```no_run
//! use argus_terminal_emu::TerminalEmulator;
//!
//! let mut emu = TerminalEmulator::new(80, 24);
//! let reply = emu.advance(b"hello");
//! assert!(reply.is_empty());
//! assert_eq!(emu.screen().line(0), "hello");
//! ```

mod emulator;
mod screen;

pub use emulator::TerminalEmulator;
pub use screen::Screen;
