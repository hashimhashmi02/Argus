//! A snapshot of the emulated screen.

use std::fmt;

/// What the terminal looks like right now: plain text, one `String` per row.
///
/// A snapshot rather than a view into the emulator. Status detection wants to
/// compare the current screen against the previous one, and later to hand a
/// screen across a thread or an RPC boundary; both are far simpler when the
/// thing being passed around owns its data and cannot change underneath the
/// reader.
///
/// Colour and text attributes are dropped. Argus classifies sessions by what
/// the text *says*, and every attribute kept would be an attribute the matching
/// rules in `argus-manifest` could accidentally depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    lines: Vec<String>,
    cols: u16,
    rows: u16,
    cursor_row: u16,
    cursor_col: u16,
}

impl Screen {
    pub(crate) fn new(
        lines: Vec<String>,
        cols: u16,
        rows: u16,
        cursor_row: u16,
        cursor_col: u16,
    ) -> Self {
        Self {
            lines,
            cols,
            rows,
            cursor_row,
            cursor_col,
        }
    }

    /// Every visible row, top to bottom, with trailing blanks trimmed.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// One row by zero-based index, or `""` if out of range.
    ///
    /// Out of range returns empty rather than panicking because callers are
    /// mostly matching rules that ask about "the last line" or "line 3" without
    /// knowing the terminal's height.
    pub fn line(&self, row: usize) -> &str {
        self.lines.get(row).map_or("", String::as_str)
    }

    /// The whole screen as one newline-joined string.
    ///
    /// This is what the screen-match rules run against, so it is worth being
    /// precise about: rows are joined by `\n` with no trailing newline, and each
    /// row has already had its trailing blanks removed. A rule can therefore
    /// anchor on a line ending without having to tolerate a run of spaces.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Terminal size in cells, as (columns, rows).
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Cursor position as (row, column), both zero-based from the top-left of
    /// the visible screen.
    ///
    /// Where the cursor sits is a real signal, not decoration: a prompt waiting
    /// for input parks the cursor after the question, which is often what
    /// distinguishes "asking you something" from "printed something that looks
    /// like a question".
    pub fn cursor(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    /// Terminal output that draws this text into a blank terminal, with the
    /// cursor where it was.
    ///
    /// The plain-text cousin of [`TerminalEmulator::repaint`], for when the
    /// emulator itself is gone -- a finished session keeps its last `Screen`,
    /// not its terminal -- so there are no colours left to reproduce.
    ///
    /// [`TerminalEmulator::repaint`]: crate::TerminalEmulator::repaint
    pub fn repaint(&self) -> String {
        let mut out = String::from("\x1b[0m\x1b[H\x1b[2J");
        for (row, line) in self.lines.iter().enumerate() {
            if !line.is_empty() {
                out.push_str(&format!("\x1b[{};1H{line}", row + 1));
            }
        }
        out.push_str(&format!(
            "\x1b[{};{}H",
            self.cursor_row + 1,
            self.cursor_col + 1
        ));
        out
    }
}

impl fmt::Display for Screen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}
