//! Kitty keyboard protocol encoding.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
//!
//! We encode key events as `CSI key_code [; modifiers] u` when the Term has
//! any KITTY_KEYBOARD_PROTOCOL flags active, returning None otherwise so the
//! caller can fall back to the legacy encoding.

use alacritty_terminal::term::TermMode;
use egui;

pub fn encode(key: egui::Key, mods: egui::Modifiers, term_mode: TermMode) -> Option<Vec<u8>> {
    if !term_mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return None;
    }

    let code = kitty_keycode(key)?;
    let mod_byte = encode_mods(mods);
    let has_mod = mod_byte > 1;

    let emit_csi_u = if term_mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        true
    } else if term_mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        // Modified keys are always ambiguous enough to warrant CSI u.
        // Unmodified Esc is also ambiguous (Esc vs. Alt+key prefix).
        has_mod || key == egui::Key::Escape
    } else {
        // Report event types alone doesn't enable CSI u; we'd need to track
        // key press/release events, which egui already conflates. Skip.
        false
    };

    if !emit_csi_u {
        return None;
    }

    let mut s = String::new();
    s.push_str("\x1b[");
    s.push_str(&code.to_string());
    if has_mod {
        s.push(';');
        s.push_str(&mod_byte.to_string());
    }
    s.push('u');
    Some(s.into_bytes())
}

fn encode_mods(mods: egui::Modifiers) -> u32 {
    let mut m: u32 = 0;
    if mods.shift {
        m |= 0b1;
    }
    if mods.alt {
        m |= 0b10;
    }
    if mods.ctrl {
        m |= 0b100;
    }
    if mods.mac_cmd || mods.command {
        m |= 0b1000;
    } // super
    1 + m
}

/// Kitty codepoint for the given egui key.
///
/// For printable ASCII we use the unshifted ASCII codepoint (e.g. 'A' -> 97).
/// For functional keys we use Kitty's private-use-area codepoints.
fn kitty_keycode(key: egui::Key) -> Option<u32> {
    use egui::Key;
    Some(match key {
        Key::Escape => 27,
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Space => 32,

        Key::Insert => 57348,
        Key::Delete => 57349,
        Key::ArrowLeft => 57350,
        Key::ArrowRight => 57351,
        Key::ArrowUp => 57352,
        Key::ArrowDown => 57353,
        Key::PageUp => 57354,
        Key::PageDown => 57355,
        Key::Home => 57356,
        Key::End => 57357,

        Key::F1 => 57364,
        Key::F2 => 57365,
        Key::F3 => 57366,
        Key::F4 => 57367,
        Key::F5 => 57368,
        Key::F6 => 57369,
        Key::F7 => 57370,
        Key::F8 => 57371,
        Key::F9 => 57372,
        Key::F10 => 57373,
        Key::F11 => 57374,
        Key::F12 => 57375,

        // Letters -> lowercase ASCII.
        Key::A => 97,
        Key::B => 98,
        Key::C => 99,
        Key::D => 100,
        Key::E => 101,
        Key::F => 102,
        Key::G => 103,
        Key::H => 104,
        Key::I => 105,
        Key::J => 106,
        Key::K => 107,
        Key::L => 108,
        Key::M => 109,
        Key::N => 110,
        Key::O => 111,
        Key::P => 112,
        Key::Q => 113,
        Key::R => 114,
        Key::S => 115,
        Key::T => 116,
        Key::U => 117,
        Key::V => 118,
        Key::W => 119,
        Key::X => 120,
        Key::Y => 121,
        Key::Z => 122,

        Key::Num0 => 48,
        Key::Num1 => 49,
        Key::Num2 => 50,
        Key::Num3 => 51,
        Key::Num4 => 52,
        Key::Num5 => 53,
        Key::Num6 => 54,
        Key::Num7 => 55,
        Key::Num8 => 56,
        Key::Num9 => 57,

        Key::Minus => 45,
        Key::Equals => 61,
        Key::OpenBracket => 91,
        Key::CloseBracket => 93,
        Key::Backslash => 92,
        Key::Semicolon => 59,
        Key::Quote => 39,
        Key::Backtick => 96,
        Key::Comma => 44,
        Key::Period => 46,
        Key::Slash => 47,

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disambiguate() -> TermMode {
        TermMode::DISAMBIGUATE_ESC_CODES
    }

    #[test]
    fn no_mode_returns_none() {
        let mods = egui::Modifiers::default();
        assert_eq!(encode(egui::Key::A, mods, TermMode::empty()), None);
    }

    #[test]
    fn unmodified_printable_bypasses_csi_u() {
        // With disambiguate, bare 'a' should fall through to text handling.
        let mods = egui::Modifiers::default();
        assert_eq!(encode(egui::Key::A, mods, disambiguate()), None);
    }

    #[test]
    fn ctrl_letter_emits_csi_u() {
        let mods = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        // Ctrl+C: key=99 (c), modifier byte = 1 + ctrl(4) = 5.
        let bytes = encode(egui::Key::C, mods, disambiguate()).unwrap();
        assert_eq!(bytes, b"\x1b[99;5u");
    }

    #[test]
    fn bare_escape_emits_csi_u_in_disambiguate() {
        let mods = egui::Modifiers::default();
        let bytes = encode(egui::Key::Escape, mods, disambiguate()).unwrap();
        assert_eq!(bytes, b"\x1b[27u");
    }

    #[test]
    fn shift_alt_ctrl_combine() {
        let mods = egui::Modifiers {
            shift: true,
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        // shift(1) | alt(2) | ctrl(4) = 7, modifier byte = 8.
        let bytes = encode(egui::Key::A, mods, disambiguate()).unwrap();
        assert_eq!(bytes, b"\x1b[97;8u");
    }

    #[test]
    fn unknown_key_returns_none() {
        let mods = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        // A key not in our map (e.g. NumLock) should bypass.
        assert_eq!(encode(egui::Key::Copy, mods, disambiguate()), None);
    }
}
