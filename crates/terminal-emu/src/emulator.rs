//! The emulator itself: bytes in, screen out, write-backs surfaced.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::screen::Screen;

/// A headless terminal.
///
/// Feed it PTY output with [`advance`](Self::advance); read the resulting
/// screen with [`screen`](Self::screen).
pub struct TerminalEmulator {
    term: Term<PtyWriteSink>,
    parser: Processor,
    /// Second handle on the same buffer the terminal writes replies into.
    sink: PtyWriteSink,
    cols: u16,
    rows: u16,
}

impl TerminalEmulator {
    /// A blank terminal of the given size.
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = ScreenSize::new(cols, rows);
        let sink = PtyWriteSink::default();

        // `Config::default()` gives 10k lines of scrollback. Argus never looks
        // at scrollback -- status is always a question about the *current*
        // screen -- so the history is dead weight held per session, and there
        // may be a dozen sessions. Zero keeps a session's memory proportional to
        // its window rather than to how long it has been running.
        let config = Config {
            scrolling_history: 0,
            ..Config::default()
        };

        Self {
            term: Term::new(config, &size, sink.clone()),
            parser: Processor::new(),
            sink,
            cols,
            rows,
        }
    }

    /// Feed bytes from the PTY and return anything a real terminal would have
    /// written back.
    ///
    /// The return value is the interesting part. A terminal is not a passive
    /// sink: the far end asks it questions -- where is the cursor, what is your
    /// size, what colour is index 4 -- and *waits for answers*. Step 1 found
    /// this the hard way, when ConPTY's startup `ESC[6n` went unanswered and the
    /// shell simply never started.
    ///
    /// Rather than performing that write itself, which would drag a PTY handle
    /// into this crate and destroy its testability, the emulator hands the bytes
    /// back and lets the caller decide. The contract is simply: whatever comes
    /// out of here must be written to the PTY, promptly.
    ///
    /// Note also that this is a *streaming* parser holding state across calls.
    /// An escape sequence split across two reads -- which happens whenever a
    /// buffer boundary lands mid-sequence -- is handled correctly, and that is
    /// precisely what a substring search over the raw stream cannot do.
    #[must_use = "these bytes must be written back to the PTY or the child may stall"]
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        self.sink.take()
    }

    /// Snapshot the current screen.
    pub fn screen(&self) -> Screen {
        let grid = self.term.grid();
        let mut lines = Vec::with_capacity(self.rows as usize);

        for row in 0..self.rows as i32 {
            let mut text = String::with_capacity(self.cols as usize);
            for col in 0..self.cols as usize {
                let cell = &grid[Line(row)][Column(col)];

                // A double-width character (CJK, many emoji) occupies two cells:
                // the character itself, then a spacer holding a copy. Emitting
                // both would duplicate every wide glyph in the extracted text
                // and quietly break any rule that counts characters.
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
            }

            // Every unwritten cell holds a space, so without this each line
            // would be padded to the full terminal width. Rules would then have
            // to tolerate arbitrary trailing whitespace, and `$` anchors would
            // never match.
            while text.ends_with(' ') {
                text.pop();
            }
            lines.push(text);
        }

        let cursor = grid.cursor.point;
        Screen::new(
            lines,
            self.cols,
            self.rows,
            cursor.line.0.clamp(0, i32::from(self.rows)) as u16,
            cursor.column.0 as u16,
        )
    }

    /// Tell the terminal its window changed size.
    ///
    /// Not cosmetic. A TUI lays itself out against the size it believes it has,
    /// so if the emulator and the child disagree, the emulated screen wraps text
    /// in places the child never intended and matching starts failing on lines
    /// that look fine in a real terminal. Whoever calls this must send the same
    /// size to the PTY.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.term.resize(ScreenSize::new(cols, rows));
    }

    /// Current size as (columns, rows).
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}

/// Terminal dimensions, in the shape `alacritty_terminal` wants them.
///
/// `alacritty_terminal` ships a `TermSize` that would do, but it lives in a
/// module named `test`, and building the production path on a crate's test
/// helper is a dependency that breaks the day upstream tidies up. It is four
/// lines to own it.
struct ScreenSize {
    cols: usize,
    rows: usize,
}

impl ScreenSize {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols as usize,
            rows: rows as usize,
        }
    }
}

impl Dimensions for ScreenSize {
    /// Visible lines plus scrollback. Equal to `screen_lines` here because this
    /// emulator is configured with no scrollback -- see `TerminalEmulator::new`.
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Collects the bytes the terminal wants written back to the PTY.
///
/// `EventListener::send_event` takes `&self`, so the listener needs interior
/// mutability. `Arc<Mutex<..>>` rather than `Rc<RefCell<..>>` because `Term`
/// owns the listener, and in step 4 a `Term` lives inside a session owned by a
/// `tokio` task -- which requires the whole thing to be `Send`.
#[derive(Clone, Default)]
struct PtyWriteSink {
    pending: Arc<Mutex<Vec<u8>>>,
}

impl PtyWriteSink {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
}

impl EventListener for PtyWriteSink {
    fn send_event(&self, event: Event) {
        // Every other event is about being a *window* -- set the title, ring the
        // bell, load the clipboard, mark the cursor dirty. Argus has no window,
        // so they are all correctly ignored. `PtyWrite` is the only one that is
        // a protocol obligation rather than a UI nicety: something upstream is
        // blocked waiting for these bytes.
        if let Event::PtyWrite(text) = event {
            self.pending
                .lock()
                .unwrap()
                .extend_from_slice(text.as_bytes());
        }
    }
}
