//! Byte fixtures in, screen out.
//!
//! Every test here drives the emulator from a `&[u8]` literal. No PTY, no
//! process, no sleeping -- which is the point of keeping this crate free of the
//! PTY layer. The escape sequences are the real ones agent CLIs emit.

use argus_terminal_emu::TerminalEmulator;

/// Feed bytes, ignore any write-back, return the screen text.
fn render(bytes: &[u8]) -> String {
    let mut emu = TerminalEmulator::new(40, 5);
    let _ = emu.advance(bytes);
    emu.screen().text()
}

#[test]
fn plain_text_lands_on_the_screen() {
    assert_eq!(render(b"hello").lines().next().unwrap(), "hello");
}

#[test]
fn trailing_blanks_are_trimmed() {
    // Unwritten cells hold spaces. If they survived into the extracted text,
    // every line would be padded to the terminal width and `$` would never
    // anchor.
    let screen = render(b"hi");
    assert_eq!(screen.lines().next().unwrap(), "hi");
    assert!(!screen.contains("hi  "));
}

#[test]
fn carriage_return_overwrites_rather_than_appends() {
    // The single most common redraw: print something, return to column zero,
    // print over it. A naive scan of the byte stream sees both strings; the
    // user only ever saw the second.
    let screen = render(b"working...\rdone.     ");
    assert_eq!(screen.lines().next().unwrap(), "done.");
    assert!(!screen.contains("working"));
}

#[test]
fn cleared_text_is_not_on_the_screen() {
    // `ESC[2K` erases the line, `ESC[G` returns to column one. Text that was
    // printed and then erased is exactly the false positive that makes raw-byte
    // matching unreliable: the bytes are in the stream forever, the text is on
    // screen for a moment.
    let screen = render(b"Allow this edit?\x1b[2K\x1b[Gnever mind");
    assert_eq!(screen.lines().next().unwrap(), "never mind");
    assert!(!screen.contains("Allow this edit?"));
}

#[test]
fn text_assembled_from_several_writes_is_matched_whole() {
    // The mirror-image failure: the prompt is never present as a contiguous run
    // of bytes, because it is written in fragments with cursor moves between
    // them. Matching the stream misses it; matching the screen finds it.
    let mut emu = TerminalEmulator::new(40, 5);
    let _ = emu.advance(b"Do you want ");
    let _ = emu.advance(b"to allow ");
    let _ = emu.advance(b"this edit?");

    assert_eq!(emu.screen().line(0), "Do you want to allow this edit?");
}

#[test]
fn escape_sequences_split_across_reads_still_parse() {
    // A 4096-byte read boundary lands mid-sequence sooner or later. The parser
    // holds state across calls; a substring search cannot.
    let mut emu = TerminalEmulator::new(40, 5);
    let _ = emu.advance(b"first\x1b");
    let _ = emu.advance(b"[2;1Hsecond");

    let screen = emu.screen();
    assert_eq!(screen.line(0), "first");
    assert_eq!(screen.line(1), "second");
}

#[test]
fn real_powershell_echo_renders_as_the_user_saw_it() {
    // Captured verbatim from the step 1 ConPTY smoke test. PowerShell prints
    // `e`, backspaces over it, then rewrites `echo` -- with four colour changes
    // along the way. Matching the raw bytes for a word is hopeless here.
    let bytes = b"\x1b[93me\x1b[?25h\x1b[m\x1b[93m\x08echo \x1b[37mhello-from-powershell";

    assert_eq!(
        render(bytes).lines().next().unwrap(),
        "echo hello-from-powershell"
    );
}

#[test]
fn cursor_position_is_tracked() {
    let mut emu = TerminalEmulator::new(40, 5);
    let _ = emu.advance(b"\x1b[3;7Hx");

    // ANSI coordinates are 1-based; ours are 0-based. Row 3 column 7, then one
    // character printed, leaves the cursor at row 2, column 7.
    assert_eq!(emu.screen().cursor(), (2, 7));
}

#[test]
fn cursor_query_is_answered_with_the_real_position() {
    // Step 1 answered ConPTY's `ESC[6n` with a hardcoded "row 1, column 1",
    // because a raw byte pipe genuinely does not know any better. The emulator
    // does know, and this is the payoff.
    let mut emu = TerminalEmulator::new(40, 5);

    let reply = emu.advance(b"hi\x1b[6n");

    // 1-based, so column 3 is "just after `hi`".
    assert_eq!(reply, b"\x1b[1;3R");
}

#[test]
fn nothing_is_written_back_for_ordinary_output() {
    let mut emu = TerminalEmulator::new(40, 5);
    assert!(emu.advance(b"just some text\r\n").is_empty());
}

#[test]
fn wide_characters_are_not_duplicated() {
    // A double-width glyph occupies two cells: the character, then a spacer
    // holding a copy of it. Emitting both would double every CJK character in
    // the extracted text.
    assert_eq!(
        render("日本語".as_bytes()).lines().next().unwrap(),
        "日本語"
    );
}

#[test]
fn resize_rewraps_the_screen() {
    let mut emu = TerminalEmulator::new(10, 4);
    let _ = emu.advance(b"abcdefghijklmno");

    // 15 characters in a 10-column terminal wrap onto a second line.
    assert_eq!(emu.screen().line(0), "abcdefghij");
    assert_eq!(emu.screen().line(1), "klmno");

    emu.resize(20, 4);
    assert_eq!(emu.size(), (20, 4));
}
