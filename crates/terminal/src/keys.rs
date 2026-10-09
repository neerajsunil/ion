//! Encoding keystrokes as the byte sequences terminals expect (xterm style).
//!
//! Plain printable text is not handled here: it arrives through the platform
//! text input path, which also covers IME and dead keys.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Returns the bytes to send for `keystroke`, or `None` if it is plain text
/// (or a key the terminal doesn't use).
pub(crate) fn encode(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let param = 1 + mods.shift as u8 + 2 * mods.alt as u8 + 4 * mods.control as u8;

    let cursor_key = |final_char: char| {
        if param > 1 {
            format!("\x1b[1;{param}{final_char}")
        } else if app_cursor {
            format!("\x1bO{final_char}")
        } else {
            format!("\x1b[{final_char}")
        }
    };
    let tilde_key = |code: u8| {
        if param > 1 {
            format!("\x1b[{code};{param}~")
        } else {
            format!("\x1b[{code}~")
        }
    };
    let function_key = |final_char: char| {
        if param > 1 {
            format!("\x1b[1;{param}{final_char}")
        } else {
            format!("\x1bO{final_char}")
        }
    };

    let sequence = match keystroke.key.as_str() {
        "enter" if mods.alt => "\x1b\r".to_owned(),
        "enter" => "\r".to_owned(),
        "backspace" if mods.control => "\x08".to_owned(),
        "backspace" if mods.alt => "\x1b\x7f".to_owned(),
        "backspace" => "\x7f".to_owned(),
        "tab" if mods.shift => "\x1b[Z".to_owned(),
        "tab" => "\t".to_owned(),
        "escape" => "\x1b".to_owned(),
        "up" => cursor_key('A'),
        "down" => cursor_key('B'),
        "right" => cursor_key('C'),
        "left" => cursor_key('D'),
        "home" => cursor_key('H'),
        "end" => cursor_key('F'),
        "insert" => tilde_key(2),
        "delete" => tilde_key(3),
        "pageup" => tilde_key(5),
        "pagedown" => tilde_key(6),
        "f1" => function_key('P'),
        "f2" => function_key('Q'),
        "f3" => function_key('R'),
        "f4" => function_key('S'),
        "f5" => tilde_key(15),
        "f6" => tilde_key(17),
        "f7" => tilde_key(18),
        "f8" => tilde_key(19),
        "f9" => tilde_key(20),
        "f10" => tilde_key(21),
        "f11" => tilde_key(23),
        "f12" => tilde_key(24),
        "space" if mods.control => "\0".to_owned(),
        "space" if mods.alt => "\x1b ".to_owned(),
        key => return encode_char_key(key, keystroke.key_char.as_deref(), mods),
    };
    Some(sequence.into_bytes())
}

/// Ctrl and Alt combinations with character keys.
fn encode_char_key(key: &str, key_char: Option<&str>, mods: &gpui::Modifiers) -> Option<Vec<u8>> {
    let mut chars = key.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return None;
    };
    // AltGr arrives as Ctrl+Alt with the produced character in `key_char`
    // (e.g. AltGr+Q = '@' on German layouts). That's text, not a shortcut.
    if mods.control && mods.alt && key_char.is_some_and(|typed| typed != key) {
        return None;
    }
    if mods.control {
        let code = match ch.to_ascii_lowercase() {
            c @ 'a'..='z' => c as u8 - b'a' + 1,
            '@' | '2' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '-' | '7' | '/' => 0x1f,
            '8' => 0x7f,
            _ => return None,
        };
        let mut bytes = Vec::with_capacity(2);
        if mods.alt {
            bytes.push(0x1b);
        }
        bytes.push(code);
        return Some(bytes);
    }
    if mods.alt {
        let typed = key_char.unwrap_or(key);
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(typed.as_bytes());
        return Some(bytes);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(keystroke: &str, mode: TermMode) -> Option<Vec<u8>> {
        encode(&Keystroke::parse(keystroke).unwrap(), mode)
    }

    #[test]
    fn plain_text_is_not_encoded() {
        assert_eq!(enc("a", TermMode::empty()), None);
        assert_eq!(enc("shift-a", TermMode::empty()), None);
    }

    #[test]
    fn control_keys() {
        assert_eq!(enc("ctrl-c", TermMode::empty()), Some(vec![3]));
        assert_eq!(enc("ctrl-shift-c", TermMode::empty()), Some(vec![3]));
        assert_eq!(enc("ctrl-alt-b", TermMode::empty()), Some(vec![0x1b, 2]));
        assert_eq!(enc("enter", TermMode::empty()), Some(b"\r".to_vec()));
        assert_eq!(enc("backspace", TermMode::empty()), Some(vec![0x7f]));
    }

    #[test]
    fn cursor_keys_follow_app_cursor_mode() {
        assert_eq!(enc("up", TermMode::empty()), Some(b"\x1b[A".to_vec()));
        assert_eq!(enc("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(
            enc("ctrl-left", TermMode::empty()),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(enc("delete", TermMode::empty()), Some(b"\x1b[3~".to_vec()));
    }
}
