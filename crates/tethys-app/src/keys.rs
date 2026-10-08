//! Translates GPUI keystrokes into the bytes a terminal program expects.

use std::borrow::Cow;

use gpui_kit::Keystroke;

/// Bytes to send for `keystroke`, or `None` if the terminal shouldn't handle it.
///
/// `app_cursor` is DECCKM: arrow keys send `ESC O x` instead of `ESC [ x`.
pub fn to_bytes(keystroke: &Keystroke, app_cursor: bool) -> Option<Cow<'static, [u8]>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    // Ctrl+Shift is reserved for Tethys shortcuts; Win key for the OS.
    if (m.control && m.shift) || m.platform {
        return None;
    }

    let fixed: Option<&'static [u8]> = match (key, m.shift, m.alt) {
        ("enter", true, _) => Some(b"\x1b\r"), // newline in Claude Code
        ("enter", _, true) => Some(b"\x1b\r"),
        ("enter", _, _) => Some(b"\r"),
        ("backspace", _, true) => Some(b"\x1b\x7f"),
        ("backspace", _, _) if m.control => Some(b"\x17"), // delete word
        ("backspace", _, _) => Some(b"\x7f"),
        ("tab", true, _) => Some(b"\x1b[Z"),
        ("tab", _, _) if m.control => None,
        ("tab", _, _) => Some(b"\t"),
        ("escape", _, _) => Some(b"\x1b"),
        ("insert", _, _) => Some(b"\x1b[2~"),
        ("delete", _, _) => Some(b"\x1b[3~"),
        ("pageup", _, _) => Some(b"\x1b[5~"),
        ("pagedown", _, _) => Some(b"\x1b[6~"),
        ("f1", _, _) => Some(b"\x1bOP"),
        ("f2", _, _) => Some(b"\x1bOQ"),
        ("f3", _, _) => Some(b"\x1bOR"),
        ("f4", _, _) => Some(b"\x1bOS"),
        ("f5", _, _) => Some(b"\x1b[15~"),
        ("f6", _, _) => Some(b"\x1b[17~"),
        ("f7", _, _) => Some(b"\x1b[18~"),
        ("f8", _, _) => Some(b"\x1b[19~"),
        ("f9", _, _) => Some(b"\x1b[20~"),
        ("f10", _, _) => Some(b"\x1b[21~"),
        ("f11", _, _) => Some(b"\x1b[23~"),
        ("f12", _, _) => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(bytes) = fixed {
        return Some(Cow::Borrowed(bytes));
    }
    if key == "tab" {
        return None;
    }

    // Cursor keys, with xterm modifier encoding.
    let cursor = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(c) = cursor {
        let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8;
        let seq = if modifier > 1 {
            format!("\x1b[1;{modifier}{c}")
        } else if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        };
        return Some(Cow::Owned(seq.into_bytes()));
    }

    // Ctrl+letter and friends → C0 control codes.
    if m.control && !m.alt {
        let byte = match key {
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => {
                Some(k.as_bytes()[0].to_ascii_lowercase() & 0x1f)
            }
            "space" | "@" | "2" => Some(0x00),
            "[" | "3" => Some(0x1b),
            "\\" | "4" => Some(0x1c),
            "]" | "5" => Some(0x1d),
            "6" => Some(0x1e),
            "-" | "/" | "7" => Some(0x1f),
            _ => None,
        };
        return byte.map(|b| Cow::Owned(vec![b]));
    }

    let text = match (key, keystroke.key_char.as_deref()) {
        (_, Some(text)) => text.to_string(),
        ("space", None) => " ".to_string(),
        _ => return None,
    };
    // Alt+x sends ESC x. Ctrl+Alt is AltGr on Windows: send the character as is.
    if m.alt && !m.control {
        return Some(Cow::Owned(format!("\x1b{text}").into_bytes()));
    }
    Some(Cow::Owned(text.into_bytes()))
}

/// Wraps pasted text in bracketed-paste markers if the program asked for them.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    // Terminals send CR for newlines.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::Modifiers;

    use super::*;

    fn ks(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn bytes(k: &Keystroke) -> Option<Vec<u8>> {
        to_bytes(k, false).map(|c| c.into_owned())
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Default::default()
        }
    }

    #[test]
    fn plain_text_and_enter() {
        assert_eq!(
            bytes(&ks("a", Some("a"), Modifiers::default())),
            Some(b"a".to_vec())
        );
        assert_eq!(
            bytes(&ks("enter", None, Modifiers::default())),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            bytes(&ks("space", Some(" "), Modifiers::default())),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn control_codes() {
        assert_eq!(bytes(&ks("c", None, ctrl())), Some(vec![3]));
        assert_eq!(bytes(&ks("w", None, ctrl())), Some(vec![0x17]));
    }

    #[test]
    fn ctrl_shift_is_left_for_tethys() {
        let m = Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(bytes(&ks("t", None, m)), None);
        assert_eq!(bytes(&ks("tab", None, ctrl())), None);
    }

    #[test]
    fn arrows() {
        let up = ks("up", None, Modifiers::default());
        assert_eq!(bytes(&up), Some(b"\x1b[A".to_vec()));
        assert_eq!(to_bytes(&up, true).unwrap().as_ref(), b"\x1bOA");
        assert_eq!(
            bytes(&ks("left", None, ctrl())),
            Some(b"\x1b[1;5D".to_vec())
        );
    }

    #[test]
    fn paste() {
        assert_eq!(paste_bytes("a\r\nb", false), b"a\rb");
        assert_eq!(paste_bytes("x", true), b"\x1b[200~x\x1b[201~");
    }
}
