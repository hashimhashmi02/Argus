//! The screen as terminal output.
//!
//! A client that starts watching a session halfway through its life needs a
//! starting point: the screen as it is *now*, which the raw byte stream cannot
//! give it -- those bytes were sent long ago, to nobody. A repaint is the
//! answer. It is terminal output that turns a blank terminal into a copy of
//! this one, after which the live bytes carry on from exactly where it left
//! off.
//!
//! "A copy" means more than the characters. It means colours and attributes,
//! because a client showing an agent's screen should show it the way the agent
//! drew it. It means the cursor's pen -- the colour set but not yet used -- or
//! the next characters the agent writes come out in the wrong colour. And it
//! means the input modes, which is the part that is easy to miss: an agent that
//! switched on application cursor keys expects the Up arrow as `ESC O A`, and a
//! client that never saw that switch would send `ESC [ A` until the agent
//! happened to switch it again.
//!
//! Not reproduced: the scroll region, saved cursor, and character set. A TUI
//! that relies on a scroll region set before the client arrived may draw a few
//! lines in the wrong place until it next redraws the whole screen, which TUIs
//! do often.

use std::fmt::Write as _;

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// Terminal output that recreates `term`'s visible screen, attributes, cursor
/// and input modes in a terminal of the same size.
pub(crate) fn repaint<T>(term: &Term<T>, cols: u16, rows: u16) -> String {
    let mode = *term.mode();
    let grid = term.grid();
    let mut out = String::with_capacity(usize::from(cols) * usize::from(rows) + 256);

    // Start from a known state. Insert mode would shift what is painted, and
    // origin mode would make the absolute positions below relative to a scroll
    // region; both are switched off for the paint. The alternate screen comes
    // first, so the paint lands in the buffer the agent is actually using.
    out.push_str("\x1b[0m\x1b[4l\x1b[?6l\x1b[r");
    out.push_str(if mode.contains(TermMode::ALT_SCREEN) {
        "\x1b[?1049h"
    } else {
        "\x1b[?1049l"
    });
    out.push_str("\x1b[H\x1b[2J");

    let mut pen = Style::default();
    for row in 0..i32::from(rows) {
        let cells: Vec<&Cell> = (0..usize::from(cols))
            .map(|col| &grid[Line(row)][Column(col)])
            .collect();
        // Trailing blanks are already there: the screen was just cleared with
        // default attributes, so writing them again would only add bytes.
        let Some(last) = cells.iter().rposition(|cell| !is_blank(cell)) else {
            continue;
        };

        let _ = write!(out, "\x1b[{};1H", row + 1);
        for cell in &cells[..=last] {
            // The second half of a double-width character. The terminal moves
            // past it by itself when it prints the first half.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let style = Style::of(cell);
            if style != pen {
                style.write_sgr(&mut out);
                pen = style;
            }
            out.push(cell.c);
            if let Some(marks) = cell.zerowidth() {
                out.extend(marks);
            }
        }
    }

    // Modes after the paint, since insert and origin mode would have disturbed
    // it, and before the cursor, since switching origin mode homes the cursor.
    // The scroll region is the whole screen, so the position below means the
    // same thing either way.
    for (flag, on, off) in MODES {
        out.push_str(if mode.contains(*flag) { on } else { off });
    }

    // The cursor, where it was and with the pen it had, so whatever the agent
    // writes next lands in the right place in the right colour.
    let cursor = &grid.cursor;
    let _ = write!(
        out,
        "\x1b[{};{}H",
        cursor.point.line.0 + 1,
        cursor.point.column.0 + 1
    );
    Style::of(&cursor.template).write_sgr(&mut out);
    out
}

/// Modes a client must share with the agent, with the sequences that set and
/// clear each one. Most decide what the client's keyboard and mouse send; the
/// first three decide how the agent's next output lands.
const MODES: &[(TermMode, &str, &str)] = &[
    (TermMode::INSERT, "\x1b[4h", "\x1b[4l"),
    (TermMode::ORIGIN, "\x1b[?6h", "\x1b[?6l"),
    (TermMode::LINE_WRAP, "\x1b[?7h", "\x1b[?7l"),
    (TermMode::APP_CURSOR, "\x1b[?1h", "\x1b[?1l"),
    (TermMode::APP_KEYPAD, "\x1b=", "\x1b>"),
    (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", "\x1b[?1000l"),
    (TermMode::MOUSE_DRAG, "\x1b[?1002h", "\x1b[?1002l"),
    (TermMode::MOUSE_MOTION, "\x1b[?1003h", "\x1b[?1003l"),
    (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", "\x1b[?1004l"),
    (TermMode::SGR_MOUSE, "\x1b[?1006h", "\x1b[?1006l"),
    (TermMode::BRACKETED_PASTE, "\x1b[?2004h", "\x1b[?2004l"),
    (TermMode::SHOW_CURSOR, "\x1b[?25h", "\x1b[?25l"),
];

/// A cell that looks exactly like a cleared one.
fn is_blank(cell: &Cell) -> bool {
    cell.c == ' ' && cell.zerowidth().is_none() && Style::of(cell) == Style::default()
}

/// What SGR controls about a cell.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }
}

/// The flags that are attributes rather than bookkeeping about wide characters
/// or wrapped lines.
const ATTRIBUTES: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

impl Style {
    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & ATTRIBUTES,
        }
    }

    /// One SGR sequence that sets exactly this style, from a reset.
    ///
    /// Always from a reset rather than a diff against the previous style:
    /// turning one attribute off without touching the others needs a different
    /// code for each, and getting one wrong leaves an attribute bleeding across
    /// the rest of the screen. The few extra bytes are worth not having that bug.
    fn write_sgr(&self, out: &mut String) {
        out.push_str("\x1b[0");
        let attributes = [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::ALL_UNDERLINES, ";4"),
            (Flags::INVERSE, ";7"),
            (Flags::HIDDEN, ";8"),
            (Flags::STRIKEOUT, ";9"),
        ];
        for (flag, code) in attributes {
            if self.flags.intersects(flag) {
                out.push_str(code);
            }
        }
        write_color(out, self.fg, 30, 90, 38);
        write_color(out, self.bg, 40, 100, 48);
        out.push('m');
    }
}

/// Append the SGR parameters for one colour. `base`, `bright` and `extended`
/// are the codes for that layer: 30/90/38 for foreground, 40/100/48 for
/// background. A default colour needs no parameter after the reset.
fn write_color(out: &mut String, color: Color, base: u8, bright: u8, extended: u8) {
    match color {
        Color::Named(named) => {
            if let Some(index) = ansi_index(named) {
                let code = if index < 8 {
                    base + index
                } else {
                    bright + index - 8
                };
                let _ = write!(out, ";{code}");
            }
        }
        Color::Indexed(index) => {
            let _ = write!(out, ";{extended};5;{index}");
        }
        Color::Spec(rgb) => {
            let _ = write!(out, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

/// The 16-colour palette index of a named colour, or `None` for the
/// terminal's own default foreground and background.
///
/// The dim variants map to their plain colour: dimness is the `DIM` attribute,
/// which is written separately.
fn ansi_index(named: NamedColor) -> Option<u8> {
    use NamedColor::*;
    Some(match named {
        Black | DimBlack => 0,
        Red | DimRed => 1,
        Green | DimGreen => 2,
        Yellow | DimYellow => 3,
        Blue | DimBlue => 4,
        Magenta | DimMagenta => 5,
        Cyan | DimCyan => 6,
        White | DimWhite => 7,
        BrightBlack => 8,
        BrightRed => 9,
        BrightGreen => 10,
        BrightYellow => 11,
        BrightBlue => 12,
        BrightMagenta => 13,
        BrightCyan => 14,
        BrightWhite => 15,
        Foreground | Background | Cursor | BrightForeground | DimForeground => return None,
    })
}
