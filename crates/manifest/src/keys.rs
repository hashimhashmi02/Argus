//! Turning key names into the bytes a terminal actually sends.

/// The keystrokes Argus sends to answer a prompt on the user's behalf.
///
/// Written as *names* -- `"enter"`, `"escape"`, `"ctrl-c"` -- rather than as the
/// literal bytes. Raw control characters in a hand-edited config file are a bug
/// waiting to happen: they are invisible in an editor, they do not survive being
/// pasted into a chat message, and JSON forbids most of them in strings anyway,
/// so the file would have to carry a unicode escape for every one of them and
/// hope nothing along the way mangles it. A name is greppable and diffable.
///
/// Anything unrecognised is sent as literal text, which is what makes
/// `"approve": "yes"` work for an agent that wants a word typed rather than a
/// key pressed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Keys {
    /// What to send to approve a prompt.
    pub approve: String,
    /// What to send to decline one.
    pub deny: String,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            approve: "enter".to_string(),
            deny: "escape".to_string(),
        }
    }
}

impl Keys {
    pub fn approve_bytes(&self) -> Vec<u8> {
        key_bytes(&self.approve)
    }

    pub fn deny_bytes(&self) -> Vec<u8> {
        key_bytes(&self.deny)
    }
}

/// Resolve one key specification into the bytes a terminal would send.
pub fn key_bytes(spec: &str) -> Vec<u8> {
    // `\r`, not `\n`, for Enter. A terminal sends carriage return when the key
    // is pressed; an agent reading raw input will not recognise a line feed and
    // will simply sit there, which looks exactly like Argus having done nothing.
    match spec.to_ascii_lowercase().as_str() {
        "enter" | "return" => vec![b'\r'],
        "escape" | "esc" => vec![0x1b],
        "tab" => vec![b'\t'],
        "space" => vec![b' '],
        "backspace" => vec![0x7f],
        // Arrow keys are escape sequences, not single bytes. Menu-driven prompts
        // need these to move a selection before confirming it.
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "y" => vec![b'y'],
        "n" => vec![b'n'],
        other => {
            // Ctrl chords: `ctrl-c` is byte 3, `ctrl-a` is 1 -- the letter's
            // position in the alphabet. This is the actual wire encoding, not a
            // convention Argus invented.
            if let Some(rest) = other.strip_prefix("ctrl-")
                && rest.len() == 1
                && let Some(c) = rest.chars().next()
                && c.is_ascii_alphabetic()
            {
                return vec![(c.to_ascii_lowercase() as u8) - b'a' + 1];
            }
            spec.as_bytes().to_vec()
        }
    }
}
