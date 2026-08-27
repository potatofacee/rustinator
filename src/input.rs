use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::keybindings::{Action, BindingTable};
use crate::pane::Pane;

pub(crate) struct RawTermKey {
    pub key: egui::Key,
    pub mods: egui::Modifiers,
    pub legacy_bytes: Vec<u8>,
}

pub(crate) fn process_keys(
    ctx: &egui::Context,
    bindings: &BindingTable,
    raw_keys: Vec<RawTermKey>,
    targets: &[&Pane],
    focused_alt_screen: bool,
    scroll_on_keystroke: bool,
) -> (Vec<Action>, bool) {
    if targets.is_empty() {
        return (Vec::new(), false);
    }

    // When the focused pane is in alt-screen, the three default Linux bindings
    // that collide with TUIs (Ctrl+Tab -> FocusNext, Ctrl+PageUp -> PrevTab,
    // Ctrl+PageDown -> NextTab) should pass through to the child rather than be
    // consumed. This keys off the FOCUSED pane only: under broadcast `targets`
    // is every non-read_only pane, so a background alt-screen pane must not
    // change how the focused pane's keys are routed.
    let keys: Vec<(egui::Key, egui::Modifiers)> =
        raw_keys.iter().map(|rk| (rk.key, rk.mods)).collect();
    let (actions, consumed) = classify_keys(bindings, &keys, focused_alt_screen);

    ctx.input_mut(|i| {
        i.events.retain(|ev| {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                if bindings.lookup(*key, *modifiers).is_some() {
                    return false;
                }
            }
            true
        });
    });

    let has_unconsumed = consumed.iter().any(|c| !c);
    if has_unconsumed && scroll_on_keystroke {
        for pane in targets {
            pane.scroll_to_bottom();
        }
    }

    raw_keys
        .iter()
        .zip(consumed.iter())
        .filter(|&(_, c)| !c)
        .for_each(|(rk, _)| {
            for pane in targets {
                pane.send_key(rk.key, rk.mods, Some(rk.legacy_bytes.clone()));
            }
        });

    let handled_paste = actions.iter().any(|a| matches!(a, Action::Paste));
    ctx.input(|i| {
        if !handled_paste {
            for event in &i.events {
                if let egui::Event::Paste(text) = event {
                    for pane in targets {
                        pane.send_paste(text);
                    }
                }
            }
        }
    });

    ctx.input_mut(|i| {
        i.events.retain(|ev| match ev {
            egui::Event::Text(_) => false,
            egui::Event::Key { pressed: true, .. } => false,
            egui::Event::Paste(_) => false,
            _ => true,
        });
    });

    (actions, has_unconsumed)
}

/// True when a bound action is one of the three Linux navigation bindings that
/// collide with TUIs and is bound to its specific conflicting combo, so it
/// should pass through to the child while the focused pane is in alt-screen.
fn is_alt_screen_passthrough(action: Action, key: egui::Key, mods: egui::Modifiers) -> bool {
    let plain_ctrl = mods.ctrl && !mods.shift && !mods.alt && !mods.mac_cmd;
    if !plain_ctrl {
        return false;
    }
    matches!(
        (action, key),
        (Action::FocusNext, egui::Key::Tab)
            | (Action::PrevTab, egui::Key::PageUp)
            | (Action::NextTab, egui::Key::PageDown)
    )
}

/// Whether process_keys should consume a bound action (true) or let it fall
/// through to the child PTY (false). The three TUI-colliding navigation
/// bindings pass through only while the *focused* pane is in alt-screen; this
/// is computed from the focused-pane flag alone, never from background
/// broadcast targets.
fn should_consume_binding(
    action: Action,
    key: egui::Key,
    mods: egui::Modifiers,
    focused_alt_screen: bool,
) -> bool {
    !(focused_alt_screen && is_alt_screen_passthrough(action, key, mods))
}

/// Classify a batch of key presses against the binding table. Returns the
/// actions to dispatch (in key order) plus a `consumed` flag parallel to
/// `keys`: true when the key was swallowed as a binding, false when it must
/// fall through to the child PTY. The three TUI-colliding nav bindings pass
/// through (consumed=false, no action emitted) while the focused pane is in
/// alt-screen.
fn classify_keys(
    bindings: &BindingTable,
    keys: &[(egui::Key, egui::Modifiers)],
    focused_alt_screen: bool,
) -> (Vec<Action>, Vec<bool>) {
    let mut actions: Vec<Action> = Vec::new();
    let consumed: Vec<bool> = keys
        .iter()
        .map(|&(key, mods)| {
            if let Some(action) = bindings.lookup(key, mods) {
                if !should_consume_binding(action, key, mods, focused_alt_screen) {
                    return false;
                }
                actions.push(action);
                true
            } else {
                false
            }
        })
        .collect();
    (actions, consumed)
}

/// Normalize pasted text (CRLF/LF -> CR) and, when the terminal has bracketed
/// paste enabled, wrap it in `\x1b[200~` / `\x1b[201~` markers. Mirrors
/// `Pane::send_paste`; see the cross-file note in the review (this is the pure
/// core that `send_paste` should route through).
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let fixed = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut v = Vec::with_capacity(fixed.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(fixed.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        fixed.into_bytes()
    }
}

/// Focus-reporting bytes for a focus-in/out transition, gated on the terminal
/// having FOCUS_IN_OUT (mode 1004) enabled. `\x1b[I` on focus-in, `\x1b[O` on
/// focus-out, None when the mode is off. Mirrors `Pane::send_focus_event`.
pub(crate) fn focus_event_bytes(mode: TermMode, focused: bool) -> Option<Vec<u8>> {
    if mode.contains(TermMode::FOCUS_IN_OUT) {
        let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
        Some(seq.to_vec())
    } else {
        None
    }
}

pub(crate) fn encode_raw_key(event: &winit::event::KeyEvent, modifiers: winit::event::Modifiers) -> Option<RawTermKey> {
    if !event.state.is_pressed() {
        return None;
    }
    let mods = winit_mods_to_egui(modifiers);
    let state = modifiers.state();
    let alt = state.alt_key();
    let ctrl = state.control_key();
    let shift = state.shift_key();

    let egui_key = match &event.key_without_modifiers() {
        Key::Character(c) => char_to_egui_key(c),
        Key::Named(named) => named_to_egui_key(*named),
        _ => None,
    };

    if let Key::Named(named) = &event.logical_key {
        if let Some(bytes) = named_logical_key_bytes(*named, mods.mac_cmd, shift, alt, ctrl) {
            return Some(RawTermKey {
                key: egui_key.unwrap_or(egui::Key::Escape),
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    if mods.mac_cmd {
        return egui_key.map(|key| RawTermKey { key, mods, legacy_bytes: Vec::new() });
    }

    // Deterministic Ctrl+letter -> control byte (Ctrl+A=0x01 ... Ctrl+Z=0x1a).
    // This must take precedence over the raw `text_with_all_modifiers` path so
    // that Ctrl+C always sends 0x03 regardless of platform text quirks. Only
    // applies to plain Ctrl (no Alt, no Cmd) with an unmodified ASCII letter.
    if ctrl && !alt && !mods.mac_cmd {
        if let Key::Character(c) = &event.key_without_modifiers() {
            let s = c.as_ref();
            let mut chars = s.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                if let Some(b) = ctrl_letter_byte(ch) {
                    return Some(RawTermKey {
                        key: egui_key.unwrap_or(egui::Key::Space),
                        mods,
                        legacy_bytes: vec![b],
                    });
                }
            }
        }
    }

    if let Some(text) = event.text_with_all_modifiers() {
        if !text.is_empty() {
            let egui_key = egui_key.unwrap_or(egui::Key::Space);
            let mut bytes = text.as_bytes().to_vec();
            if alt {
                bytes.insert(0, 0x1b);
            }
            return Some(RawTermKey {
                key: egui_key,
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    if ctrl {
        let unmod = event.key_without_modifiers();
        let ctrl_byte: Option<u8> = match &unmod {
            Key::Character(c) => match c.as_ref() {
                "[" => Some(0x1b),
                "\\" => Some(0x1c),
                "]" => Some(0x1d),
                "^" | "6" => Some(0x1e),
                "_" | "-" => Some(0x1f),
                "2" | "@" | " " => Some(0x00),
                _ => None,
            },
            _ => None,
        };
        if let Some(b) = ctrl_byte {
            let egui_key = egui_key?;
            let mut bytes = vec![b];
            if alt {
                bytes.insert(0, 0x1b);
            }
            return Some(RawTermKey {
                key: egui_key,
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    egui_key.map(|key| RawTermKey {
        key,
        mods,
        legacy_bytes: Vec::new(),
    })
}

pub(crate) fn legacy_mod_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    let mut m: u8 = 0;
    if shift { m |= 1; }
    if alt { m |= 2; }
    if ctrl { m |= 4; }
    1 + m
}

/// Legacy bytes for a Named logical key under the given modifier flags,
/// honoring the macOS Cmd/Super guard. When `mac_cmd` is held the combo is an
/// inert shortcut and must emit no bytes (bug M9): previously the
/// `encode_named_key` block ran before the mac_cmd byte-less guard, so
/// Cmd+Left/Backspace/Enter/Up/Down leaked their legacy sequences. Returns None
/// when the key has no legacy encoding or when `mac_cmd` is set, letting the
/// caller fall through to the byte-less RawTermKey. (Cmd+PageUp/Down never reach
/// here — they are consumed earlier as Prev/NextTab bindings.)
fn named_logical_key_bytes(
    named: NamedKey,
    mac_cmd: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<Vec<u8>> {
    if mac_cmd {
        return None;
    }
    encode_named_key(named, shift, alt, ctrl)
}

/// Maps an ASCII letter to its Ctrl control byte (Ctrl+A=0x01 .. Ctrl+Z=0x1a),
/// case-insensitively. Returns None for any non-letter.
fn ctrl_letter_byte(ch: char) -> Option<u8> {
    if ch.is_ascii_alphabetic() {
        Some((ch.to_ascii_uppercase() as u8) - 0x40)
    } else {
        None
    }
}

pub(crate) fn encode_named_key(named: NamedKey, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    let has_mods = shift || alt || ctrl;

    if named == NamedKey::Tab && shift && !alt && !ctrl {
        return Some(b"\x1b[Z".to_vec());
    }

    if !has_mods {
        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f",
            NamedKey::Tab => b"\t",
            NamedKey::Escape => b"\x1b",
            NamedKey::Space => b" ",
            _ => &[],
        };
        if !seq.is_empty() {
            return Some(seq.to_vec());
        }
    }

    if alt && !ctrl {
        let base: Option<u8> = match named {
            NamedKey::Enter => Some(b'\r'),
            NamedKey::Backspace => Some(0x7f),
            NamedKey::Space => Some(b' '),
            _ => None,
        };
        if let Some(b) = base {
            return Some(vec![0x1b, b]);
        }
    }

    let home_end: Option<u8> = match named {
        NamedKey::Home => Some(b'H'),
        NamedKey::End => Some(b'F'),
        _ => None,
    };
    if let Some(fb) = home_end {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(vec![0x1b, b'O', fb])
        };
    }

    let arrow: Option<u8> = match named {
        NamedKey::ArrowUp => Some(b'A'),
        NamedKey::ArrowDown => Some(b'B'),
        NamedKey::ArrowRight => Some(b'C'),
        NamedKey::ArrowLeft => Some(b'D'),
        _ => None,
    };
    if let Some(fb) = arrow {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(vec![0x1b, b'[', fb])
        };
    }

    let tilde_num: Option<u8> = match named {
        NamedKey::Insert => Some(2),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(num) = tilde_num {
        return if has_mods {
            Some(format!("\x1b[{};{}~", num, legacy_mod_param(shift, alt, ctrl)).into_bytes())
        } else {
            Some(format!("\x1b[{}~", num).into_bytes())
        };
    }

    let fkey: Option<(&[u8], u8)> = match named {
        NamedKey::F1 => Some((b"\x1bOP", b'P')),
        NamedKey::F2 => Some((b"\x1bOQ", b'Q')),
        NamedKey::F3 => Some((b"\x1bOR", b'R')),
        NamedKey::F4 => Some((b"\x1bOS", b'S')),
        _ => None,
    };
    if let Some((unmod, fb)) = fkey {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(unmod.to_vec())
        };
    }
    let fkey_tilde: Option<u8> = match named {
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        _ => None,
    };
    if let Some(num) = fkey_tilde {
        return if has_mods {
            Some(format!("\x1b[{};{}~", num, legacy_mod_param(shift, alt, ctrl)).into_bytes())
        } else {
            Some(format!("\x1b[{}~", num).into_bytes())
        };
    }

    None
}

pub(crate) fn winit_mods_to_egui(mods: winit::event::Modifiers) -> egui::Modifiers {
    let state = mods.state();
    egui::Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        shift: state.shift_key(),
        mac_cmd: state.super_key(),
        command: state.control_key() || state.super_key(),
    }
}

pub(crate) fn char_to_egui_key(c: &str) -> Option<egui::Key> {
    let ch = c.chars().next()?;
    Some(match ch.to_ascii_lowercase() {
        'a' => egui::Key::A, 'b' => egui::Key::B, 'c' => egui::Key::C,
        'd' => egui::Key::D, 'e' => egui::Key::E, 'f' => egui::Key::F,
        'g' => egui::Key::G, 'h' => egui::Key::H, 'i' => egui::Key::I,
        'j' => egui::Key::J, 'k' => egui::Key::K, 'l' => egui::Key::L,
        'm' => egui::Key::M, 'n' => egui::Key::N, 'o' => egui::Key::O,
        'p' => egui::Key::P, 'q' => egui::Key::Q, 'r' => egui::Key::R,
        's' => egui::Key::S, 't' => egui::Key::T, 'u' => egui::Key::U,
        'v' => egui::Key::V, 'w' => egui::Key::W, 'x' => egui::Key::X,
        'y' => egui::Key::Y, 'z' => egui::Key::Z,
        '0' => egui::Key::Num0, '1' => egui::Key::Num1, '2' => egui::Key::Num2,
        '3' => egui::Key::Num3, '4' => egui::Key::Num4, '5' => egui::Key::Num5,
        '6' => egui::Key::Num6, '7' => egui::Key::Num7, '8' => egui::Key::Num8,
        '9' => egui::Key::Num9,
        '[' => egui::Key::OpenBracket, ']' => egui::Key::CloseBracket,
        '\\' => egui::Key::Backslash, ';' => egui::Key::Semicolon,
        '\'' => egui::Key::Quote, '`' => egui::Key::Backtick,
        ',' => egui::Key::Comma, '.' => egui::Key::Period,
        '/' => egui::Key::Slash, '-' => egui::Key::Minus, '=' => egui::Key::Equals,
        _ => return None,
    })
}

pub(crate) fn named_to_egui_key(named: NamedKey) -> Option<egui::Key> {
    Some(match named {
        NamedKey::Enter => egui::Key::Enter,
        NamedKey::Tab => egui::Key::Tab,
        NamedKey::Backspace => egui::Key::Backspace,
        NamedKey::Escape => egui::Key::Escape,
        NamedKey::Space => egui::Key::Space,
        NamedKey::Delete => egui::Key::Delete,
        NamedKey::Insert => egui::Key::Insert,
        NamedKey::Home => egui::Key::Home,
        NamedKey::End => egui::Key::End,
        NamedKey::PageUp => egui::Key::PageUp,
        NamedKey::PageDown => egui::Key::PageDown,
        NamedKey::ArrowUp => egui::Key::ArrowUp,
        NamedKey::ArrowDown => egui::Key::ArrowDown,
        NamedKey::ArrowLeft => egui::Key::ArrowLeft,
        NamedKey::ArrowRight => egui::Key::ArrowRight,
        NamedKey::F1 => egui::Key::F1, NamedKey::F2 => egui::Key::F2,
        NamedKey::F3 => egui::Key::F3, NamedKey::F4 => egui::Key::F4,
        NamedKey::F5 => egui::Key::F5, NamedKey::F6 => egui::Key::F6,
        NamedKey::F7 => egui::Key::F7, NamedKey::F8 => egui::Key::F8,
        NamedKey::F9 => egui::Key::F9, NamedKey::F10 => egui::Key::F10,
        NamedKey::F11 => egui::Key::F11, NamedKey::F12 => egui::Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_param_no_mods() {
        assert_eq!(legacy_mod_param(false, false, false), 1);
    }

    #[test]
    fn mod_param_shift() {
        assert_eq!(legacy_mod_param(true, false, false), 2);
    }

    #[test]
    fn mod_param_alt() {
        assert_eq!(legacy_mod_param(false, true, false), 3);
    }

    #[test]
    fn mod_param_ctrl() {
        assert_eq!(legacy_mod_param(false, false, true), 5);
    }

    #[test]
    fn mod_param_shift_alt_ctrl() {
        assert_eq!(legacy_mod_param(true, true, true), 8);
    }

    #[test]
    fn enter_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, false, false).unwrap(), b"\r");
    }

    #[test]
    fn backspace_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, false, false).unwrap(), b"\x7f");
    }

    #[test]
    fn tab_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, false, false).unwrap(), b"\t");
    }

    #[test]
    fn escape_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Escape, false, false, false).unwrap(), b"\x1b");
    }

    #[test]
    fn space_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Space, false, false, false).unwrap(), b" ");
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(encode_named_key(NamedKey::Tab, true, false, false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn alt_enter() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, true, false).unwrap(), b"\x1b\r");
    }

    #[test]
    fn alt_backspace() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, true, false).unwrap(), b"\x1b\x7f");
    }

    #[test]
    fn alt_space() {
        assert_eq!(encode_named_key(NamedKey::Space, false, true, false).unwrap(), b"\x1b ");
    }

    #[test]
    fn arrow_up() {
        assert_eq!(encode_named_key(NamedKey::ArrowUp, false, false, false).unwrap(), b"\x1b[A");
    }

    #[test]
    fn arrow_down() {
        assert_eq!(encode_named_key(NamedKey::ArrowDown, false, false, false).unwrap(), b"\x1b[B");
    }

    #[test]
    fn arrow_right() {
        assert_eq!(encode_named_key(NamedKey::ArrowRight, false, false, false).unwrap(), b"\x1b[C");
    }

    #[test]
    fn arrow_left() {
        assert_eq!(encode_named_key(NamedKey::ArrowLeft, false, false, false).unwrap(), b"\x1b[D");
    }

    #[test]
    fn home_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Home, false, false, false).unwrap(), b"\x1bOH");
    }

    #[test]
    fn end_unmodified() {
        assert_eq!(encode_named_key(NamedKey::End, false, false, false).unwrap(), b"\x1bOF");
    }

    #[test]
    fn ctrl_arrow_up() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowUp, false, false, true).unwrap(),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn shift_arrow_left() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowLeft, true, false, false).unwrap(),
            b"\x1b[1;2D"
        );
    }

    #[test]
    fn alt_shift_arrow_right() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowRight, true, true, false).unwrap(),
            b"\x1b[1;4C"
        );
    }

    #[test]
    fn insert_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Insert, false, false, false).unwrap(), b"\x1b[2~");
    }

    #[test]
    fn delete_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Delete, false, false, false).unwrap(), b"\x1b[3~");
    }

    #[test]
    fn page_up_unmodified() {
        assert_eq!(encode_named_key(NamedKey::PageUp, false, false, false).unwrap(), b"\x1b[5~");
    }

    #[test]
    fn page_down_unmodified() {
        assert_eq!(encode_named_key(NamedKey::PageDown, false, false, false).unwrap(), b"\x1b[6~");
    }

    #[test]
    fn ctrl_delete() {
        assert_eq!(
            encode_named_key(NamedKey::Delete, false, false, true).unwrap(),
            b"\x1b[3;5~"
        );
    }

    #[test]
    fn shift_page_up() {
        assert_eq!(
            encode_named_key(NamedKey::PageUp, true, false, false).unwrap(),
            b"\x1b[5;2~"
        );
    }

    #[test]
    fn f1_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F1, false, false, false).unwrap(), b"\x1bOP");
    }

    #[test]
    fn f2_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F2, false, false, false).unwrap(), b"\x1bOQ");
    }

    #[test]
    fn f3_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F3, false, false, false).unwrap(), b"\x1bOR");
    }

    #[test]
    fn f4_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F4, false, false, false).unwrap(), b"\x1bOS");
    }

    #[test]
    fn shift_f1() {
        assert_eq!(
            encode_named_key(NamedKey::F1, true, false, false).unwrap(),
            b"\x1b[1;2P"
        );
    }

    #[test]
    fn ctrl_f3() {
        assert_eq!(
            encode_named_key(NamedKey::F3, false, false, true).unwrap(),
            b"\x1b[1;5R"
        );
    }

    #[test]
    fn f5_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F5, false, false, false).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn f6_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F6, false, false, false).unwrap(), b"\x1b[17~");
    }

    #[test]
    fn f7_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F7, false, false, false).unwrap(), b"\x1b[18~");
    }

    #[test]
    fn f8_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F8, false, false, false).unwrap(), b"\x1b[19~");
    }

    #[test]
    fn f9_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F9, false, false, false).unwrap(), b"\x1b[20~");
    }

    #[test]
    fn f10_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F10, false, false, false).unwrap(), b"\x1b[21~");
    }

    #[test]
    fn f11_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F11, false, false, false).unwrap(), b"\x1b[23~");
    }

    #[test]
    fn f12_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F12, false, false, false).unwrap(), b"\x1b[24~");
    }

    #[test]
    fn alt_f12() {
        assert_eq!(
            encode_named_key(NamedKey::F12, false, true, false).unwrap(),
            b"\x1b[24;3~"
        );
    }

    #[test]
    fn unknown_named_key() {
        assert_eq!(encode_named_key(NamedKey::CapsLock, false, false, false), None);
    }

    #[test]
    fn char_to_key_letter() {
        assert_eq!(char_to_egui_key("a"), Some(egui::Key::A));
        assert_eq!(char_to_egui_key("A"), Some(egui::Key::A));
    }

    #[test]
    fn char_to_key_digit() {
        assert_eq!(char_to_egui_key("5"), Some(egui::Key::Num5));
    }

    #[test]
    fn char_to_key_punctuation() {
        assert_eq!(char_to_egui_key("["), Some(egui::Key::OpenBracket));
        assert_eq!(char_to_egui_key("]"), Some(egui::Key::CloseBracket));
        assert_eq!(char_to_egui_key("/"), Some(egui::Key::Slash));
        assert_eq!(char_to_egui_key("-"), Some(egui::Key::Minus));
    }

    #[test]
    fn char_to_key_unknown() {
        assert_eq!(char_to_egui_key("€"), None);
    }

    #[test]
    fn named_to_egui_arrows() {
        assert_eq!(named_to_egui_key(NamedKey::ArrowUp), Some(egui::Key::ArrowUp));
        assert_eq!(named_to_egui_key(NamedKey::ArrowDown), Some(egui::Key::ArrowDown));
    }

    #[test]
    fn named_to_egui_fkeys() {
        assert_eq!(named_to_egui_key(NamedKey::F1), Some(egui::Key::F1));
        assert_eq!(named_to_egui_key(NamedKey::F12), Some(egui::Key::F12));
    }

    #[test]
    fn named_to_egui_unknown() {
        assert_eq!(named_to_egui_key(NamedKey::CapsLock), None);
    }

    #[test]
    fn mod_param_shift_alt() {
        assert_eq!(legacy_mod_param(true, true, false), 4);
    }

    #[test]
    fn mod_param_shift_ctrl() {
        assert_eq!(legacy_mod_param(true, false, true), 6);
    }

    #[test]
    fn mod_param_alt_ctrl() {
        assert_eq!(legacy_mod_param(false, true, true), 7);
    }

    #[test]
    fn alt_arrow_up() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowUp, false, true, false).unwrap(),
            b"\x1b[1;3A"
        );
    }

    #[test]
    fn ctrl_alt_arrow_down() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowDown, false, true, true).unwrap(),
            b"\x1b[1;7B"
        );
    }

    #[test]
    fn shift_ctrl_arrow_right() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowRight, true, false, true).unwrap(),
            b"\x1b[1;6C"
        );
    }

    #[test]
    fn shift_alt_ctrl_arrow_left() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowLeft, true, true, true).unwrap(),
            b"\x1b[1;8D"
        );
    }

    #[test]
    fn ctrl_home() {
        assert_eq!(
            encode_named_key(NamedKey::Home, false, false, true).unwrap(),
            b"\x1b[1;5H"
        );
    }

    #[test]
    fn shift_end() {
        assert_eq!(
            encode_named_key(NamedKey::End, true, false, false).unwrap(),
            b"\x1b[1;2F"
        );
    }

    #[test]
    fn alt_insert() {
        assert_eq!(
            encode_named_key(NamedKey::Insert, false, true, false).unwrap(),
            b"\x1b[2;3~"
        );
    }

    #[test]
    fn shift_ctrl_delete() {
        assert_eq!(
            encode_named_key(NamedKey::Delete, true, false, true).unwrap(),
            b"\x1b[3;6~"
        );
    }

    #[test]
    fn alt_page_down() {
        assert_eq!(
            encode_named_key(NamedKey::PageDown, false, true, false).unwrap(),
            b"\x1b[6;3~"
        );
    }

    #[test]
    fn alt_f1() {
        assert_eq!(
            encode_named_key(NamedKey::F1, false, true, false).unwrap(),
            b"\x1b[1;3P"
        );
    }

    #[test]
    fn ctrl_alt_f4() {
        assert_eq!(
            encode_named_key(NamedKey::F4, false, true, true).unwrap(),
            b"\x1b[1;7S"
        );
    }

    #[test]
    fn shift_f5() {
        assert_eq!(
            encode_named_key(NamedKey::F5, true, false, false).unwrap(),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn ctrl_f9() {
        assert_eq!(
            encode_named_key(NamedKey::F9, false, false, true).unwrap(),
            b"\x1b[20;5~"
        );
    }

    #[test]
    fn shift_alt_ctrl_f12() {
        assert_eq!(
            encode_named_key(NamedKey::F12, true, true, true).unwrap(),
            b"\x1b[24;8~"
        );
    }

    #[test]
    fn ctrl_tab_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, false, true), None);
    }

    #[test]
    fn alt_tab_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, true, false), None);
    }

    #[test]
    fn ctrl_enter_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, false, true), None);
    }

    #[test]
    fn shift_enter_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Enter, true, false, false), None);
    }

    #[test]
    fn ctrl_backspace_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, false, true), None);
    }

    #[test]
    fn shift_escape_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Escape, true, false, false), None);
    }

    #[test]
    fn ctrl_space_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Space, false, false, true), None);
    }

    #[test]
    fn char_to_key_all_punct() {
        assert_eq!(char_to_egui_key("\\"), Some(egui::Key::Backslash));
        assert_eq!(char_to_egui_key(";"), Some(egui::Key::Semicolon));
        assert_eq!(char_to_egui_key("'"), Some(egui::Key::Quote));
        assert_eq!(char_to_egui_key("`"), Some(egui::Key::Backtick));
        assert_eq!(char_to_egui_key(","), Some(egui::Key::Comma));
        assert_eq!(char_to_egui_key("."), Some(egui::Key::Period));
        assert_eq!(char_to_egui_key("="), Some(egui::Key::Equals));
    }

    #[test]
    fn char_to_key_empty_string() {
        assert_eq!(char_to_egui_key(""), None);
    }

    #[test]
    fn char_to_key_multi_char_uses_first() {
        assert_eq!(char_to_egui_key("abc"), Some(egui::Key::A));
    }

    #[test]
    fn named_to_egui_enter_tab_space() {
        assert_eq!(named_to_egui_key(NamedKey::Enter), Some(egui::Key::Enter));
        assert_eq!(named_to_egui_key(NamedKey::Tab), Some(egui::Key::Tab));
        assert_eq!(named_to_egui_key(NamedKey::Space), Some(egui::Key::Space));
    }

    #[test]
    fn named_to_egui_navigation() {
        assert_eq!(named_to_egui_key(NamedKey::Home), Some(egui::Key::Home));
        assert_eq!(named_to_egui_key(NamedKey::End), Some(egui::Key::End));
        assert_eq!(named_to_egui_key(NamedKey::PageUp), Some(egui::Key::PageUp));
        assert_eq!(named_to_egui_key(NamedKey::PageDown), Some(egui::Key::PageDown));
        assert_eq!(named_to_egui_key(NamedKey::Insert), Some(egui::Key::Insert));
        assert_eq!(named_to_egui_key(NamedKey::Delete), Some(egui::Key::Delete));
    }

    #[test]
    fn alt_screen_passthrough_matches_only_nav_combos() {
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        // The three Linux nav bindings on plain Ctrl are passthrough candidates.
        assert!(is_alt_screen_passthrough(Action::FocusNext, egui::Key::Tab, ctrl));
        assert!(is_alt_screen_passthrough(Action::PrevTab, egui::Key::PageUp, ctrl));
        assert!(is_alt_screen_passthrough(Action::NextTab, egui::Key::PageDown, ctrl));
        // Right action, wrong key -> not a passthrough.
        assert!(!is_alt_screen_passthrough(Action::FocusNext, egui::Key::PageUp, ctrl));
        // Right action + key but an extra modifier (not plain Ctrl) -> false.
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert!(!is_alt_screen_passthrough(Action::FocusNext, egui::Key::Tab, ctrl_shift));
        // A non-nav action on Ctrl+Tab -> false.
        assert!(!is_alt_screen_passthrough(Action::Paste, egui::Key::Tab, ctrl));
    }

    #[test]
    fn alt_screen_uses_focused_not_any_target() {
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        // M3: the passthrough decision is driven by the FOCUSED pane's
        // alt-screen flag, not by any (e.g. background broadcast) target. With
        // the focused pane NOT in alt-screen, the nav binding is consumed even
        // though a background target may be in alt-screen.
        assert!(should_consume_binding(Action::FocusNext, egui::Key::Tab, ctrl, false));
        // With the focused pane IN alt-screen, the same nav binding passes through.
        assert!(!should_consume_binding(Action::FocusNext, egui::Key::Tab, ctrl, true));
        // A non-nav binding is consumed even when the focused pane is alt-screen.
        assert!(should_consume_binding(Action::Paste, egui::Key::V, ctrl, true));
    }

    #[test]
    fn classify_keys_alt_screen_passes_only_nav_bindings() {
        let bindings = BindingTable::new(true);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        // Focused pane in alt-screen: Ctrl+Tab (FocusNext) passes through to the
        // child (consumed=false, no action), while Ctrl+Shift+C (Copy) is still
        // consumed normally.
        let (actions, consumed) = classify_keys(
            &bindings,
            &[(egui::Key::Tab, ctrl), (egui::Key::C, ctrl_shift)],
            true,
        );
        assert_eq!(actions, vec![Action::Copy]);
        assert_eq!(consumed, vec![false, true]);
    }

    #[test]
    fn classify_keys_off_alt_screen_consumes_nav_and_passes_unbound() {
        let bindings = BindingTable::new(true);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let none = egui::Modifiers::default();
        // Not alt-screen: Ctrl+Tab is consumed as FocusNext; a plain unbound key
        // (A) falls through to the child (consumed=false, no action).
        let (actions, consumed) = classify_keys(
            &bindings,
            &[(egui::Key::Tab, ctrl), (egui::Key::A, none)],
            false,
        );
        assert_eq!(actions, vec![Action::FocusNext]);
        assert_eq!(consumed, vec![true, false]);
    }

    #[test]
    fn ctrl_letter_byte_maps_az_and_rejects_nonletters() {
        assert_eq!(ctrl_letter_byte('c'), Some(0x03));
        assert_eq!(ctrl_letter_byte('a'), Some(0x01));
        assert_eq!(ctrl_letter_byte('z'), Some(0x1a));
        assert_eq!(ctrl_letter_byte('C'), Some(0x03));
        assert_eq!(ctrl_letter_byte('6'), None);
        assert_eq!(ctrl_letter_byte('['), None);
    }

    #[test]
    fn encode_paste_normalizes_crlf_and_lf_to_cr() {
        // CRLF is collapsed before bare LF, so "\r\n" -> "\r" (not "\r\r").
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn encode_paste_wraps_in_bracketed_markers() {
        assert_eq!(
            encode_paste("a\nb", true),
            b"\x1b[200~a\rb\x1b[201~".to_vec()
        );
        // No markers when bracketed paste is off.
        assert_eq!(encode_paste("x", false), b"x".to_vec());
    }

    #[test]
    fn focus_event_bytes_gates_on_focus_in_out() {
        // Mode off -> nothing emitted, regardless of focus direction.
        assert_eq!(focus_event_bytes(TermMode::empty(), true), None);
        assert_eq!(focus_event_bytes(TermMode::empty(), false), None);
        // Mode on -> CSI I on focus-in, CSI O on focus-out.
        assert_eq!(
            focus_event_bytes(TermMode::FOCUS_IN_OUT, true),
            Some(b"\x1b[I".to_vec())
        );
        assert_eq!(
            focus_event_bytes(TermMode::FOCUS_IN_OUT, false),
            Some(b"\x1b[O".to_vec())
        );
    }

    #[test]
    fn cmd_named_key_yields_no_bytes() {
        // M9 regression: Cmd/Super + a Named key must be an inert shortcut, not
        // leak the key's legacy sequence. The named-key path returns None when
        // mac_cmd is set, so encode_raw_key falls through to a byte-less key.
        for named in [
            NamedKey::ArrowLeft,
            NamedKey::Backspace,
            NamedKey::Enter,
            NamedKey::ArrowUp,
            NamedKey::ArrowDown,
        ] {
            assert_eq!(named_logical_key_bytes(named, true, false, false, false), None);
        }
        // Without Cmd, the same keys still produce their normal legacy bytes.
        assert_eq!(
            named_logical_key_bytes(NamedKey::ArrowLeft, false, false, false, false),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            named_logical_key_bytes(NamedKey::Enter, false, false, false, false),
            Some(b"\r".to_vec())
        );
    }
}
