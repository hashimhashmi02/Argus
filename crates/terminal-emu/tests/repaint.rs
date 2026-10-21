//! A repaint must turn a blank terminal into an exact copy.
//!
//! "Exact" is checked two ways. The copy's screen text and cursor must match
//! the original's, which is what status detection and a human both see. And
//! the copy's own repaint must equal the original's, which covers everything
//! the text does not show -- colours, attributes, the cursor's pen, input
//! modes -- because the repaint is written from all of them.

use argus_terminal_emu::TerminalEmulator;

const COLS: u16 = 40;
const ROWS: u16 = 6;

/// An emulator that has been fed `bytes`.
fn fed(bytes: &[u8]) -> TerminalEmulator {
    let mut emu = TerminalEmulator::new(COLS, ROWS);
    let _ = emu.advance(bytes);
    emu
}

/// A fresh emulator fed nothing but `original`'s repaint.
fn copy_of(original: &TerminalEmulator) -> TerminalEmulator {
    fed(original.repaint().as_bytes())
}

fn assert_identical(a: &TerminalEmulator, b: &TerminalEmulator) {
    assert_eq!(a.screen(), b.screen(), "screen text or cursor differs");
    assert_eq!(a.repaint(), b.repaint(), "attributes, pen or modes differ");
}

#[test]
fn a_blank_terminal_copies_to_a_blank_terminal() {
    let original = fed(b"");
    assert_identical(&original, &copy_of(&original));
}

#[test]
fn colours_and_attributes_survive() {
    let original = fed(concat!(
        "\x1b[31mred\x1b[0m plain \x1b[1;44mbold on blue\x1b[0m\r\n",
        "\x1b[38;5;208morange\x1b[0m \x1b[38;2;10;20;30;48;2;200;100;50mrgb\x1b[0m\r\n",
        "\x1b[2;3;4;9mdim italic underlined struck\x1b[0m \x1b[7minverse\x1b[0m\r\n",
        "\x1b[92mbright green\x1b[0m",
    )
    .as_bytes());
    let copy = copy_of(&original);
    assert_identical(&original, &copy);
    assert!(
        original.repaint().contains("\x1b[0;31m"),
        "plain red is written as SGR 31"
    );
}

#[test]
fn the_cursor_and_its_pen_carry_over() {
    // Colour set, nothing written in it yet, cursor mid-screen. An agent that
    // carries on writing expects green, at row 3, column 6.
    let mut original = fed(b"line one\r\nline two\r\n\x1b[5Cx\x1b[32m");
    let mut copy = copy_of(&original);
    assert_identical(&original, &copy);

    let _ = original.advance(b"more");
    let _ = copy.advance(b"more");
    assert_identical(&original, &copy);
}

#[test]
fn input_modes_carry_over() {
    // Application cursor keys, bracketed paste, a hidden cursor, mouse
    // reporting: all invisible on screen, all decisive for what a client's
    // keyboard and mouse send.
    let original = fed(b"\x1b[?1h\x1b[?2004h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b=prompt> ");
    let repaint = original.repaint();
    for sequence in [
        "\x1b[?1h",
        "\x1b[?2004h",
        "\x1b[?25l",
        "\x1b[?1000h",
        "\x1b[?1006h",
        "\x1b=",
    ] {
        assert!(
            repaint.contains(sequence),
            "repaint does not set {sequence:?}"
        );
    }
    assert_identical(&original, &copy_of(&original));
}

#[test]
fn modes_that_are_off_are_switched_off() {
    // A client reused across sessions may have a mode left on from the last
    // one; the repaint has to clear it, not just leave it alone.
    let repaint = fed(b"$ ").repaint();
    assert!(repaint.contains("\x1b[?1l"));
    assert!(repaint.contains("\x1b[?2004l"));
    assert!(repaint.contains("\x1b[?25h"));
}

#[test]
fn the_alternate_screen_is_entered_before_painting() {
    let original = fed(b"main screen\x1b[?1049h\x1b[Hfull-screen app");
    let repaint = original.repaint();
    let enter = repaint
        .find("\x1b[?1049h")
        .expect("enters the alternate screen");
    let text = repaint.find("full-screen app").expect("paints the app");
    assert!(enter < text, "the paint must land on the alternate screen");
    assert_identical(&original, &copy_of(&original));
}

#[test]
fn wide_characters_are_written_once() {
    // A double-width character fills two cells, the second a spacer. Writing
    // both would push everything after it one column right.
    let original = fed("日本 after".as_bytes());
    let copy = copy_of(&original);
    assert_identical(&original, &copy);
    assert_eq!(copy.screen().line(0), "日本 after");
}

#[test]
fn a_full_last_row_does_not_scroll_the_copy() {
    // Writing the very last cell of the screen leaves a pending wrap. If the
    // repaint then wrote anything else, the copy would scroll by one line.
    let full_row = "x".repeat(usize::from(COLS));
    let original = fed(format!("top\x1b[{ROWS};1H{full_row}").as_bytes());
    let copy = copy_of(&original);
    assert_eq!(copy.screen().line(0), "top");
    assert_identical(&original, &copy);
}

#[test]
fn a_plain_screen_repaints_its_text_and_cursor() {
    // The fallback for a finished session, which keeps text but no colours.
    let original = fed(b"first\r\n\r\nthird\r\n  >");
    let screen = original.screen();
    let copy = fed(screen.repaint().as_bytes());
    assert_eq!(copy.screen(), screen);
}
