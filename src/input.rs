use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::EraseBinding;
use crate::keybindings::{Action, BindingTable};
use crate::pane::Pane;

pub(crate) struct RawTermKey {
    /// egui's name for the key, for binding lookup and the kitty key code;
    /// None for a character egui cannot name (é, ß, Cyrillic, most non-US
    /// punctuation) — such a key is carried by its bytes alone, no stand-in
    /// name is invented for it.
    pub key: Option<egui::Key>,
    /// The character Shift turned the key into when that is a different one
    /// egui can name (Shift+= is `+` on US); a binding on it matches with
    /// Shift consumed, the way Terminator's lookup works — see `lookup_binding`.
    pub shifted_key: Option<egui::Key>,
    pub mods: egui::Modifiers,
    pub legacy_bytes: Vec<u8>,
}

/// Send one unconsumed key to every PTY in `targets` (scrolling each to the
/// bottom first when `scroll_on_keystroke` is on, as VTE does per key press).
/// Classification never depends on `targets` — a read-only focused pane (or
/// an empty broadcast set) still gets its bindings, exactly as Terminator's
/// `on_keypress` runs before VTE's `input_enabled` gate; only this byte-
/// sending stage needs a target. Returns whether any bytes were sent.
pub(crate) fn send_key(rk: &RawTermKey, targets: &[&Pane], scroll_on_keystroke: bool) -> bool {
    for pane in targets {
        if scroll_on_keystroke {
            pane.scroll_to_bottom();
        }
        pane.send_key(rk.key, rk.mods, Some(rk.legacy_bytes.clone()));
    }
    !targets.is_empty()
}

/// Strip this frame's key presses, text and pastes from egui's input once the
/// raw path has routed them: the terminal owns the keyboard, so no egui widget
/// may see them. egui-winit synthesizes `Event::Paste` (with the OS
/// clipboard) for any Ctrl+V chord on Linux, bound or not, and for the
/// XF86Paste key. The terminal's only paste source is the bound
/// `Action::Paste`; every other chord is encoded by the raw path (plain
/// Ctrl+V is 0x16, as under VTE), so the synthesized event is never fed to
/// the PTY — it is only stripped here so no egui widget sees it either.
pub(crate) fn strip_egui_key_events(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        i.events.retain(|ev| match ev {
            egui::Event::Text(_) => false,
            egui::Event::Key { pressed: true, .. } => false,
            egui::Event::Paste(_) => false,
            _ => true,
        });
    });
}

/// The bindings Terminator's toplevel handles itself, ahead of whichever
/// widget has the keyboard (`Window.on_key_press`, window.py:209-226):
/// `full_screen` and `close_window` work while the search bar or a tab label
/// owns egui focus; every other chord belongs to the focused widget then.
pub(crate) fn is_window_level(action: Action) -> bool {
    matches!(action, Action::ToggleFullscreen | Action::CloseWindow)
}

/// Route a frame's raw keys while an in-window egui text field owns the
/// keyboard: only the window-level bindings are dispatched, and their key
/// events are stripped so the field never sees them; every other key is the
/// field's and is dropped from the raw path here.
pub(crate) fn process_window_keys(
    ctx: &egui::Context,
    bindings: &BindingTable,
    raw_keys: Vec<RawTermKey>,
) -> Vec<Action> {
    let actions: Vec<Action> = raw_keys
        .iter()
        .filter_map(|rk| lookup_binding(bindings, rk).map(|(a, _, _)| a).filter(|a| is_window_level(*a)))
        .collect();
    if !actions.is_empty() {
        ctx.input_mut(|i| {
            i.events.retain(|ev| {
                !matches!(ev, egui::Event::Key { key, pressed: true, modifiers, .. }
                    if bindings.lookup(*key, *modifiers).is_some_and(is_window_level))
            });
        });
    }
    actions
}

/// True when a bound action is one of the Linux navigation bindings that
/// collide with TUIs and is bound to its specific conflicting combo, so it
/// should pass through to the child while the focused pane is in alt-screen.
/// Both focus-cycling directions are in the set — a TUI that binds Ctrl+Tab
/// binds Ctrl+Shift+Tab for the other way — so Shift is part of the chord
/// rather than a disqualifier.
fn is_alt_screen_passthrough(action: Action, key: egui::Key, mods: egui::Modifiers) -> bool {
    if !mods.ctrl || mods.alt || mods.mac_cmd {
        return false;
    }
    matches!(
        (action, key, mods.shift),
        (Action::FocusNext, egui::Key::Tab, false)
            | (Action::FocusPrev, egui::Key::Tab, true)
            | (Action::PrevTab, egui::Key::PageUp, false)
            | (Action::NextTab, egui::Key::PageDown, false)
    )
}

/// Whether classify_key should consume a bound action (true) or let it fall
/// through to the child PTY (false). The TUI-colliding navigation bindings
/// pass through only while the *focused* pane is in alt-screen; this is
/// computed from the focused-pane flag alone, never from background
/// broadcast targets.
///
/// `copy_fallthrough` is Terminator's smart_copy rule (terminal.py:1081-1088):
/// a Copy chord that has Ctrl in it and nothing selected is *not* consumed, so
/// it reaches the child as the ordinary control byte (Ctrl+Shift+C -> 0x03)
/// through the normal, broadcast-aware key path. Without Ctrl in the chord
/// (Cmd+C on macOS) the copy is consumed and simply copies nothing.
fn should_consume_binding(
    action: Action,
    key: egui::Key,
    mods: egui::Modifiers,
    focused_alt_screen: bool,
    copy_fallthrough: bool,
) -> bool {
    if focused_alt_screen && is_alt_screen_passthrough(action, key, mods) {
        return false;
    }
    !(action == Action::Copy && mods.ctrl && copy_fallthrough)
}

/// The binding a raw key press names, with the chord it matched on. The
/// unshifted key with the full modifier set is tried first — the form the
/// config spells (Ctrl+Shift+Equals) — then, when Shift produced a different
/// character, that character with Shift consumed (Ctrl+Plus). Terminator
/// looks up the translated keyval with the consumed modifiers removed
/// (keybindings.py:120-132), which is what makes `<Control>plus` fire on
/// Ctrl+Shift+= and on a dedicated `+` key alike.
fn lookup_binding(bindings: &BindingTable, rk: &RawTermKey) -> Option<(Action, egui::Key, egui::Modifiers)> {
    if let Some(key) = rk.key {
        if let Some(action) = bindings.lookup(key, rk.mods) {
            return Some((action, key, rk.mods));
        }
    }
    let key = rk.shifted_key?;
    let mods = egui::Modifiers { shift: false, ..rk.mods };
    bindings.lookup(key, mods).map(|action| (action, key, mods))
}

/// Classify one key press against the binding table: `Some(action)` when it
/// is swallowed as a binding, `None` when it must fall through to the child
/// PTY. The TUI-colliding nav bindings pass through (no action) while the
/// focused pane is in alt-screen; a Ctrl Copy chord passes through under
/// `copy_fallthrough`. Called per key, in arrival order, so the caller can run
/// a binding before the next key is classified and sent.
pub(crate) fn classify_key(
    bindings: &BindingTable,
    rk: &RawTermKey,
    focused_alt_screen: bool,
    copy_fallthrough: bool,
) -> Option<Action> {
    lookup_binding(bindings, rk)
        .filter(|&(action, key, mods)| should_consume_binding(action, key, mods, focused_alt_screen, copy_fallthrough))
        .map(|(action, _, _)| action)
}

/// A key press VTE acts on itself instead of encoding it for the child
/// (vte.cc `widget_key_press`, the `GDK_KEY_Insert` and keypad/motion cases):
/// the scrollback keys — Shift+Page_Up/Page_Down a page, Shift+Home/End to
/// the top/bottom, Ctrl+Shift+Up/Down a line — and the Insert chords —
/// Shift+Insert pastes PRIMARY, Ctrl+Insert copies, Ctrl+Shift+Insert pastes
/// CLIPBOARD. Only Shift and Ctrl are looked at; Alt and Super do not
/// disqualify a chord. Terminator's bindings run first (`on_keypress`), so
/// Ctrl+Shift+Up is `resize_up` unless the user unbinds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum KeyBuiltin {
    /// The view moves — on the normal screen only; on the alternate screen
    /// the key is the child's as usual. VTE marks the press `scrolled`, so
    /// no scroll-on-keystroke jump follows it.
    Scroll(ViewScroll),
    PastePrimary,
    Copy,
    PasteClipboard,
}

/// Scrollback motion, in the sign of `Pane::scroll_by` (positive is up).
/// Pages are fractional for Terminator's `page_up_half` (`scroll_by_page(0.5)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ViewScroll {
    Pages(f32),
    Lines(i32),
    Top,
    Bottom,
}

impl ViewScroll {
    pub(crate) fn apply(self, pane: &Pane) {
        match self {
            ViewScroll::Pages(pages) => pane.scroll_by_page(pages),
            ViewScroll::Lines(lines) => pane.scroll_by(lines),
            ViewScroll::Top => pane.scroll_to_top(),
            ViewScroll::Bottom => pane.scroll_to_bottom(),
        }
    }
}

/// What VTE does with an unbound key press itself, if anything — see
/// `KeyBuiltin`. Runs after the bindings and before the key is encoded for
/// the child.
pub(crate) fn key_builtin(key: Option<egui::Key>, mods: egui::Modifiers) -> Option<KeyBuiltin> {
    use egui::Key::{ArrowDown, ArrowUp, End, Home, Insert, PageDown, PageUp};
    Some(match (key?, mods.shift, mods.ctrl) {
        (Insert, true, true) => KeyBuiltin::PasteClipboard,
        (Insert, true, false) => KeyBuiltin::PastePrimary,
        (Insert, false, true) => KeyBuiltin::Copy,
        (PageUp, true, _) => KeyBuiltin::Scroll(ViewScroll::Pages(1.0)),
        (PageDown, true, _) => KeyBuiltin::Scroll(ViewScroll::Pages(-1.0)),
        (Home, true, _) => KeyBuiltin::Scroll(ViewScroll::Top),
        (End, true, _) => KeyBuiltin::Scroll(ViewScroll::Bottom),
        (ArrowUp, true, true) => KeyBuiltin::Scroll(ViewScroll::Lines(1)),
        (ArrowDown, true, true) => KeyBuiltin::Scroll(ViewScroll::Lines(-1)),
        _ => return None,
    })
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
    let inert = cmd_is_inert(mods);

    let unmod = event.key_without_modifiers();
    let ctrl_latin = ctrl_latin_fallback(&unmod, event.physical_key, ctrl);
    let key = match &unmod {
        Key::Character(c) => char_to_egui_key(c).or_else(|| ctrl_latin.and_then(char_key)),
        Key::Named(named) => named_to_egui_key(*named),
        _ => None,
    };
    let shifted_key = shifted_char_key(&event.logical_key, shift, key);
    let legacy_bytes = legacy_key_bytes(event, &unmod, ctrl_latin, inert, shift, alt, ctrl);
    // A bare modifier, a dead key: nothing to name and nothing to send.
    if key.is_none() && legacy_bytes.is_empty() {
        return None;
    }
    Some(RawTermKey { key, shifted_key, mods, legacy_bytes })
}

/// The character key Shift turned this press into, when it is a different
/// one egui can name (Shift+= is `+` on US, Shift+é is `2` on AZERTY); None
/// when Shift is up or only changed a letter's case. `logical_key` carries
/// the shifted character on every platform (xkb's full-state keysym on Linux,
/// `charactersIgnoringModifiers` on macOS).
fn shifted_char_key(logical: &Key, shift: bool, key: Option<egui::Key>) -> Option<egui::Key> {
    match logical {
        Key::Character(c) if shift => char_to_egui_key(c).filter(|&k| Some(k) != key),
        _ => None,
    }
}

/// The bytes a key press sends under the legacy (non-kitty) encoding; empty
/// for a press that types nothing (an inert Cmd chord, a bare modifier key).
fn legacy_key_bytes(
    event: &winit::event::KeyEvent,
    unmod: &Key,
    ctrl_latin: Option<char>,
    inert: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Vec<u8> {
    if let Key::Named(named) = &event.logical_key {
        if let Some(bytes) = named_logical_key_bytes(*named, inert, shift, alt, ctrl) {
            return bytes;
        }
    }

    if inert {
        return Vec::new();
    }

    // Deterministic Ctrl+letter -> control byte (Ctrl+A=0x01 ... Ctrl+Z=0x1a).
    // This must take precedence over the raw `text_with_all_modifiers` path so
    // that Ctrl+C always sends 0x03 regardless of platform text quirks. Only
    // applies to plain Ctrl (no Alt) with an unmodified ASCII letter, or the
    // physical key's letter when the layout's own is not Latin.
    if ctrl && !alt {
        let letter = match unmod {
            Key::Character(c) => {
                let mut chars = c.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) if ch.is_ascii() => Some(ch),
                    _ => ctrl_latin,
                }
            }
            _ => None,
        };
        if let Some(b) = letter.and_then(ctrl_letter_byte) {
            return vec![b];
        }
    }

    if let Some(text) = event.text_with_all_modifiers() {
        if !text.is_empty() {
            let mut bytes = text.as_bytes().to_vec();
            if alt {
                bytes.insert(0, 0x1b);
            }
            return bytes;
        }
    }

    if ctrl {
        let ctrl_byte: Option<u8> = match unmod {
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
            let mut bytes = vec![b];
            if alt {
                bytes.insert(0, 0x1b);
            }
            return bytes;
        }
    }

    Vec::new()
}

pub(crate) fn legacy_mod_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    let mut m: u8 = 0;
    if shift { m |= 1; }
    if alt { m |= 2; }
    if ctrl { m |= 4; }
    1 + m
}

/// Whether the held Super/Cmd makes this chord an inert shortcut that must
/// emit no bytes. Only on macOS: Cmd is the menu-accelerator modifier there
/// and never types anything (bug M9). On Linux VTE masks Super out of the
/// modifiers it encodes (`_vte_keymap_map` keeps Shift/Control/Meta only)
/// and the printable fallback never looks at it, so an unbound Super chord
/// sends the key's ordinary bytes: Super+Enter is `\r`, Super+a is `a`.
/// `mods.mac_cmd` itself stays set on every platform — the Super bindings
/// (Super+I, Super+R, ...) and the kitty modifier bit depend on it.
pub(crate) fn cmd_is_inert(mods: egui::Modifiers) -> bool {
    cfg!(target_os = "macos") && mods.mac_cmd
}

/// Legacy bytes for a Named logical key under the given modifier flags,
/// honoring the macOS Cmd guard. When the chord is `inert` (Cmd held on
/// macOS, see `cmd_is_inert`) it must emit no bytes (bug M9): previously the
/// `encode_named_key` block ran before the byte-less guard, so
/// Cmd+Left/Backspace/Enter/Up/Down leaked their legacy sequences. Returns None
/// when the key has no legacy encoding or when `inert` is set, letting the
/// caller fall through to the byte-less RawTermKey. (Cmd+PageUp/Down never reach
/// here — they are consumed earlier as Prev/NextTab bindings.)
fn named_logical_key_bytes(
    named: NamedKey,
    inert: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<Vec<u8>> {
    if inert {
        return None;
    }
    encode_named_key(named, shift, alt, ctrl)
}

/// The Latin letter a Ctrl chord should be read as when the current layout
/// yields a non-ASCII character for the key (Ctrl+С on a Cyrillic group), or
/// None when the layout's own character applies. VTE's `vte_translate_ctrlkey`
/// walks the other xkb groups for a Latin keyval so such a chord still sends
/// 0x03 (and Ctrl+Shift+С still copies); winit exposes only the current
/// group, so the layout-independent physical key (KeyA..KeyZ) stands in for
/// the Latin group — the same fallback egui-winit applies to its own events.
fn ctrl_latin_fallback(unmod: &Key, physical: PhysicalKey, ctrl: bool) -> Option<char> {
    match unmod {
        Key::Character(c) if ctrl && !c.is_ascii() => physical_latin_letter(physical),
        _ => None,
    }
}

/// The lowercase letter printed on a physical KeyA..KeyZ key; None for every
/// other key code.
fn physical_latin_letter(physical: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 'a', KeyCode::KeyB => 'b', KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd', KeyCode::KeyE => 'e', KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g', KeyCode::KeyH => 'h', KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j', KeyCode::KeyK => 'k', KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm', KeyCode::KeyN => 'n', KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p', KeyCode::KeyQ => 'q', KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's', KeyCode::KeyT => 't', KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v', KeyCode::KeyW => 'w', KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y', KeyCode::KeyZ => 'z',
        _ => return None,
    })
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

/// What Backspace sends under an erase binding, as VTE encodes it
/// (vte.cc `widget_key_press`, GDK_KEY_BackSpace): the single-byte forms
/// take the ESC prefix under Alt, and Ctrl swaps ^H for ^? and back; the
/// escape-sequence form is always bare `\e[3~` — it is never ESC-prefixed
/// (its `suppress_alt_esc`), and though VTE asks for modifiers on it,
/// keymap.cc's `_vte_keymap_key_get_modifier_encoding_method` has no entry
/// for BackSpace, so none are ever added. `Automatic` is VTE's fallback for
/// ERASE_AUTO, ^? — see `config::EraseBinding`.
pub(crate) fn encode_backspace(binding: EraseBinding, alt: bool, ctrl: bool) -> Vec<u8> {
    let byte = match binding {
        EraseBinding::ControlH => 0x08,
        EraseBinding::AsciiDel | EraseBinding::Automatic => 0x7f,
        EraseBinding::EscapeSequence => return b"\x1b[3~".to_vec(),
    };
    let byte = match (byte, ctrl) {
        (0x08, true) => 0x7f,
        (0x7f, true) => 0x08,
        (byte, _) => byte,
    };
    if alt {
        vec![0x1b, byte]
    } else {
        vec![byte]
    }
}

/// What Delete sends under an erase binding (vte.cc, GDK_KEY_Delete): ^H or
/// ^? bare — Delete suppresses the Alt ESC prefix and has no Ctrl swap — or
/// the escape sequence with its modifiers, which `Automatic` also is.
pub(crate) fn encode_delete(binding: EraseBinding, shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
    match binding {
        EraseBinding::ControlH => vec![0x08],
        EraseBinding::AsciiDel => vec![0x7f],
        EraseBinding::EscapeSequence | EraseBinding::Automatic => delete_sequence(shift, alt, ctrl),
    }
}

/// Delete's `\e[3~`, with any modifiers as CSI 3;{mod}~ (VTE
/// `add_modifiers`; GDK_KEY_Delete is in keymap.cc's modifier-encoding list).
fn delete_sequence(shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
    if shift || alt || ctrl {
        format!("\x1b[3;{}~", legacy_mod_param(shift, alt, ctrl)).into_bytes()
    } else {
        b"\x1b[3~".to_vec()
    }
}

pub(crate) fn encode_named_key(named: NamedKey, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    let has_mods = shift || alt || ctrl;

    if named == NamedKey::Tab && shift && !alt && !ctrl {
        return Some(b"\x1b[Z".to_vec());
    }

    // Backspace and Delete are whatever the profile's erase bindings say.
    // This is the encoding for Terminator's defaults (config.py:233-234,
    // ascii-del / escape-sequence); `Pane::send_key` re-encodes both for
    // the pane's own profile.
    match named {
        NamedKey::Backspace => return Some(encode_backspace(EraseBinding::AsciiDel, alt, ctrl)),
        NamedKey::Delete => return Some(encode_delete(EraseBinding::EscapeSequence, shift, alt, ctrl)),
        _ => {}
    }

    // Shift does not change these keys: Return, Escape and space go out as
    // their own byte whatever Shift does.
    if !alt && !ctrl {
        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
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
            NamedKey::Space => Some(b' '),
            _ => None,
        };
        if let Some(b) = base {
            return Some(vec![0x1b, b]);
        }
    }

    // Home/End are cursor keys like the arrows: CSI in normal cursor mode
    // (VTE `_vte_keymap_GDK_Home` cursor_default), SS3 only under DECCKM,
    // which `Pane::send_key` rewrites per key (`decckm_override`).
    let cursor: Option<u8> = match named {
        NamedKey::ArrowUp => Some(b'A'),
        NamedKey::ArrowDown => Some(b'B'),
        NamedKey::ArrowRight => Some(b'C'),
        NamedKey::ArrowLeft => Some(b'D'),
        NamedKey::Home => Some(b'H'),
        NamedKey::End => Some(b'F'),
        _ => None,
    };
    if let Some(fb) = cursor {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(vec![0x1b, b'[', fb])
        };
    }

    let tilde_num: Option<u8> = match named {
        NamedKey::Insert => Some(2),
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
    char_key(c.chars().next()?)
}

fn char_key(ch: char) -> Option<egui::Key> {
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
        '+' => egui::Key::Plus,
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

    /// A raw key as the encoder yields it for a press egui can name.
    fn raw(key: egui::Key, mods: egui::Modifiers, legacy_bytes: &[u8]) -> RawTermKey {
        RawTermKey { key: Some(key), shifted_key: None, mods, legacy_bytes: legacy_bytes.to_vec() }
    }

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
        // R-070: CSI H in normal cursor mode (VTE cursor_default); SS3 is the
        // DECCKM rewrite in Pane::send_key, not the legacy encoding.
        assert_eq!(encode_named_key(NamedKey::Home, false, false, false).unwrap(), b"\x1b[H");
    }

    #[test]
    fn end_unmodified() {
        assert_eq!(encode_named_key(NamedKey::End, false, false, false).unwrap(), b"\x1b[F");
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
        assert_eq!(char_to_egui_key("+"), Some(egui::Key::Plus));
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
    fn shift_enter_is_cr() {
        // R-071: Shift is a no-op on Return (VTE sends \r either way).
        assert_eq!(encode_named_key(NamedKey::Enter, true, false, false).unwrap(), b"\r");
    }

    #[test]
    fn shift_backspace_is_del() {
        // R-071: the erase char, not the keysym's xkb text (0x08).
        assert_eq!(encode_named_key(NamedKey::Backspace, true, false, false).unwrap(), b"\x7f");
    }

    #[test]
    fn shift_space_is_space() {
        assert_eq!(encode_named_key(NamedKey::Space, true, false, false).unwrap(), b" ");
    }

    #[test]
    fn ctrl_backspace_swaps_to_ctrl_h() {
        // R-088: Ctrl on the erase char swaps ^? for ^H (VTE's toggle), from
        // the erase binding itself rather than the keysym's xkb text.
        assert_eq!(encode_named_key(NamedKey::Backspace, false, false, true).unwrap(), b"\x08");
    }

    #[test]
    fn shift_escape_is_esc() {
        assert_eq!(encode_named_key(NamedKey::Escape, true, false, false).unwrap(), b"\x1b");
    }

    #[test]
    fn shift_ctrl_backspace_is_ctrl_h() {
        // Ctrl is what swaps ^H/^? (VTE); Shift on top of Ctrl changes nothing.
        assert_eq!(encode_named_key(NamedKey::Backspace, true, false, true).unwrap(), b"\x08");
    }

    #[test]
    fn ctrl_alt_backspace_is_esc_ctrl_h() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, true, true).unwrap(), b"\x1b\x08");
    }

    // R-088: the four erase bindings, as VTE encodes them for each key.

    #[test]
    fn backspace_binding_control_h() {
        let b = EraseBinding::ControlH;
        assert_eq!(encode_backspace(b, false, false), b"\x08");
        assert_eq!(encode_backspace(b, false, true), b"\x7f");
        assert_eq!(encode_backspace(b, true, false), b"\x1b\x08");
        assert_eq!(encode_backspace(b, true, true), b"\x1b\x7f");
    }

    #[test]
    fn backspace_binding_ascii_del_and_automatic() {
        for b in [EraseBinding::AsciiDel, EraseBinding::Automatic] {
            assert_eq!(encode_backspace(b, false, false), b"\x7f");
            assert_eq!(encode_backspace(b, false, true), b"\x08");
            assert_eq!(encode_backspace(b, true, false), b"\x1b\x7f");
            assert_eq!(encode_backspace(b, true, true), b"\x1b\x08");
        }
    }

    #[test]
    fn backspace_binding_escape_sequence() {
        // Bare `\e[3~` whatever is held: no Ctrl swap, no Alt ESC prefix
        // (VTE suppress_alt_esc for this form), and no CSI modifier
        // parameter either — keymap.cc never adds modifiers for BackSpace.
        let b = EraseBinding::EscapeSequence;
        assert_eq!(encode_backspace(b, false, false), b"\x1b[3~");
        assert_eq!(encode_backspace(b, false, true), b"\x1b[3~");
        assert_eq!(encode_backspace(b, true, false), b"\x1b[3~");
        assert_eq!(encode_backspace(b, true, true), b"\x1b[3~");
    }

    #[test]
    fn delete_binding_single_bytes_are_bare() {
        // Delete's single-byte forms: no Ctrl swap, and Alt adds no ESC (VTE
        // sets suppress_meta_esc for every Delete form).
        assert_eq!(encode_delete(EraseBinding::ControlH, false, false, false), b"\x08");
        assert_eq!(encode_delete(EraseBinding::ControlH, false, true, true), b"\x08");
        assert_eq!(encode_delete(EraseBinding::AsciiDel, false, false, false), b"\x7f");
        assert_eq!(encode_delete(EraseBinding::AsciiDel, true, true, true), b"\x7f");
    }

    #[test]
    fn delete_binding_escape_sequence_and_automatic() {
        for b in [EraseBinding::EscapeSequence, EraseBinding::Automatic] {
            assert_eq!(encode_delete(b, false, false, false), b"\x1b[3~");
            assert_eq!(encode_delete(b, false, false, true), b"\x1b[3;5~");
            assert_eq!(encode_delete(b, false, true, false), b"\x1b[3;3~");
            assert_eq!(encode_delete(b, true, false, true), b"\x1b[3;6~");
        }
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
        // R-074: the other cycling direction, Ctrl+Shift+Tab (FocusPrev), is in
        // the set too; Shift is part of that chord, not an extra modifier.
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert!(is_alt_screen_passthrough(Action::FocusPrev, egui::Key::Tab, ctrl_shift));
        assert!(!is_alt_screen_passthrough(Action::FocusPrev, egui::Key::Tab, ctrl));
        assert!(!is_alt_screen_passthrough(Action::FocusNext, egui::Key::Tab, ctrl_shift));
        // Shift on the tab-switching chords is a different binding (MoveTab*).
        assert!(!is_alt_screen_passthrough(Action::MoveTabLeft, egui::Key::PageUp, ctrl_shift));
        // Alt or Super on top -> false.
        let ctrl_alt = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
        assert!(!is_alt_screen_passthrough(Action::FocusNext, egui::Key::Tab, ctrl_alt));
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
        assert!(should_consume_binding(Action::FocusNext, egui::Key::Tab, ctrl, false, false));
        // With the focused pane IN alt-screen, the same nav binding passes through.
        assert!(!should_consume_binding(Action::FocusNext, egui::Key::Tab, ctrl, true, false));
        // A non-nav binding is consumed even when the focused pane is alt-screen.
        assert!(should_consume_binding(Action::Paste, egui::Key::V, ctrl, true, false));
    }

    #[test]
    fn smart_copy_fallthrough_needs_ctrl_in_chord() {
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let cmd = egui::Modifiers { mac_cmd: true, command: true, ..Default::default() };
        // Nothing selected + smart_copy: Ctrl+Shift+C is left unconsumed so the
        // child gets 0x03 through the normal key path (Terminator
        // terminal.py:1081-1088).
        assert!(!should_consume_binding(Action::Copy, egui::Key::C, ctrl_shift, false, true));
        // With a selection (or smart_copy off) the chord is consumed as Copy.
        assert!(should_consume_binding(Action::Copy, egui::Key::C, ctrl_shift, false, false));
        // Cmd+C has no Ctrl in the chord: consumed, never a ^C injection.
        assert!(should_consume_binding(Action::Copy, egui::Key::C, cmd, false, true));
        // The rule is specific to Copy.
        assert!(should_consume_binding(Action::Paste, egui::Key::V, ctrl_shift, false, true));
    }

    #[test]
    fn classify_key_alt_screen_passes_only_nav_bindings() {
        let bindings = BindingTable::new(true);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        // Focused pane in alt-screen: Ctrl+Tab (FocusNext) passes through to the
        // child (no action), while Ctrl+Shift+C (Copy) is still consumed.
        assert_eq!(classify_key(&bindings, &raw(egui::Key::Tab, ctrl, b""), true, false), None);
        assert_eq!(classify_key(&bindings, &raw(egui::Key::C, ctrl_shift, &[0x03]), true, false), Some(Action::Copy));
    }

    #[test]
    fn classify_key_smart_copy_passes_ctrl_copy_chord_through() {
        let bindings = BindingTable::new(true);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        // smart_copy with no selection: Ctrl+Shift+C is not consumed (falls
        // through to the child as 0x03); Ctrl+Shift+V is still consumed as Paste.
        assert_eq!(classify_key(&bindings, &raw(egui::Key::C, ctrl_shift, &[0x03]), false, true), None);
        assert_eq!(classify_key(&bindings, &raw(egui::Key::V, ctrl_shift, &[0x16]), false, true), Some(Action::Paste));
    }

    #[test]
    fn classify_key_off_alt_screen_consumes_nav_and_passes_unbound() {
        let bindings = BindingTable::new(true);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let none = egui::Modifiers::default();
        // Not alt-screen: Ctrl+Tab is consumed as FocusNext; a plain unbound key
        // (A) falls through to the child.
        assert_eq!(classify_key(&bindings, &raw(egui::Key::Tab, ctrl, b""), false, false), Some(Action::FocusNext));
        assert_eq!(classify_key(&bindings, &raw(egui::Key::A, none, b"a"), false, false), None);
    }

    #[test]
    fn key_builtins_are_vte_scrollback_and_insert_chords() {
        use egui::Key::{ArrowDown, ArrowUp, End, Home, Insert, PageDown, PageUp, A};
        let shift = egui::Modifiers::SHIFT;
        let ctrl = egui::Modifiers::CTRL;
        let ctrl_shift = ctrl | shift;
        let alt_shift = egui::Modifiers::ALT | shift;
        // R-009: Shift+PgUp/PgDn page, Shift+Home/End go to the ends,
        // Ctrl+Shift+Up/Down step a line (vte.cc `widget_key_press`).
        assert_eq!(key_builtin(Some(PageUp), shift), Some(KeyBuiltin::Scroll(ViewScroll::Pages(1.0))));
        assert_eq!(key_builtin(Some(PageDown), shift), Some(KeyBuiltin::Scroll(ViewScroll::Pages(-1.0))));
        assert_eq!(key_builtin(Some(Home), shift), Some(KeyBuiltin::Scroll(ViewScroll::Top)));
        assert_eq!(key_builtin(Some(End), shift), Some(KeyBuiltin::Scroll(ViewScroll::Bottom)));
        assert_eq!(key_builtin(Some(ArrowUp), ctrl_shift), Some(KeyBuiltin::Scroll(ViewScroll::Lines(1))));
        assert_eq!(key_builtin(Some(ArrowDown), ctrl_shift), Some(KeyBuiltin::Scroll(ViewScroll::Lines(-1))));
        // Only Shift and Ctrl are examined: Alt rides along, and Ctrl on a
        // page key is not a disqualifier (Ctrl+Shift+PgUp is a binding first).
        assert_eq!(key_builtin(Some(PageUp), alt_shift), Some(KeyBuiltin::Scroll(ViewScroll::Pages(1.0))));
        assert_eq!(key_builtin(Some(PageUp), ctrl_shift), Some(KeyBuiltin::Scroll(ViewScroll::Pages(1.0))));
        // Without Shift these keys are the child's; Shift+Up alone is too.
        assert_eq!(key_builtin(Some(PageUp), egui::Modifiers::NONE), None);
        assert_eq!(key_builtin(Some(Home), ctrl), None);
        assert_eq!(key_builtin(Some(ArrowUp), shift), None);
        // R-035: the Insert chords, on either screen.
        assert_eq!(key_builtin(Some(Insert), shift), Some(KeyBuiltin::PastePrimary));
        assert_eq!(key_builtin(Some(Insert), ctrl), Some(KeyBuiltin::Copy));
        assert_eq!(key_builtin(Some(Insert), ctrl_shift), Some(KeyBuiltin::PasteClipboard));
        assert_eq!(key_builtin(Some(Insert), egui::Modifiers::NONE), None);
        // An ordinary key, or one egui cannot name, is never a built-in.
        assert_eq!(key_builtin(Some(A), shift), None);
        assert_eq!(key_builtin(None, shift), None);
    }

    #[test]
    fn bindings_classify_and_nothing_is_sent_without_targets() {
        // R-015: a read-only focused pane (or an empty broadcast set) yields no
        // PTY targets, but bindings must still be classified so the user can
        // split, switch tabs or leave the pane by keyboard — classification
        // never sees the targets. Nothing is sent to nobody, so the "sent
        // input" flag stays false even for an unconsumed key.
        let bindings = BindingTable::new(true);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(
            classify_key(&bindings, &raw(egui::Key::O, ctrl_shift, &[0x0f]), false, false),
            Some(Action::SplitHorizontal)
        );
        let a = raw(egui::Key::A, egui::Modifiers::default(), b"a");
        assert!(!send_key(&a, &[], true));
    }

    #[test]
    fn window_level_bindings_are_fullscreen_and_close_window_only() {
        // R-068: Terminator's Window.on_key_press handles exactly these two
        // ahead of the focus widget; everything else is the widget's.
        assert!(is_window_level(Action::ToggleFullscreen));
        assert!(is_window_level(Action::CloseWindow));
        assert!(!is_window_level(Action::SplitHorizontal));
        assert!(!is_window_level(Action::Copy));
        assert!(!is_window_level(Action::ToggleSearch));
    }

    #[test]
    fn process_window_keys_dispatches_only_window_level_and_strips_their_events() {
        // R-068: with the search bar focused, F11 and Ctrl+Shift+Q still act
        // and their key events are taken away from the text field; a split
        // chord and a plain letter are neither dispatched nor stripped (they
        // are the field's, and the egui Text event for the letter survives).
        let ctx = egui::Context::default();
        let bindings = BindingTable::new(true);
        let none = egui::Modifiers::default();
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let key_event = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let raw_input = egui::RawInput {
            events: vec![
                key_event(egui::Key::F11, none),
                key_event(egui::Key::O, ctrl_shift),
                key_event(egui::Key::A, none),
                egui::Event::Text("a".into()),
                key_event(egui::Key::Q, ctrl_shift),
            ],
            ..Default::default()
        };
        let mut raw = Some(vec![
            raw(egui::Key::F11, none, b"\x1b[23~"),
            raw(egui::Key::O, ctrl_shift, &[0x0f]),
            raw(egui::Key::A, none, b"a"),
            raw(egui::Key::Q, ctrl_shift, &[0x11]),
        ]);
        let mut out = None;
        let _ = ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx();
            let actions = process_window_keys(ctx, &bindings, raw.take().unwrap_or_default());
            let left: Vec<egui::Event> = ctx.input(|i| i.events.clone());
            out = Some((actions, left));
        });
        let (actions, left) = out.unwrap();
        assert_eq!(actions, vec![Action::ToggleFullscreen, Action::CloseWindow]);
        assert_eq!(
            left,
            vec![
                key_event(egui::Key::O, ctrl_shift),
                key_event(egui::Key::A, none),
                egui::Event::Text("a".into()),
            ]
        );
    }

    #[test]
    fn strip_egui_key_events_removes_presses_text_and_paste_only() {
        let ctx = egui::Context::default();
        let none = egui::Modifiers::default();
        let raw_input = egui::RawInput {
            events: vec![
                egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: none },
                egui::Event::Text("a".into()),
                egui::Event::Paste("clip".into()),
                egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: false, repeat: false, modifiers: none },
                egui::Event::WindowFocused(true),
            ],
            ..Default::default()
        };
        let mut left = None;
        let _ = ctx.run_ui(raw_input, |ui| {
            strip_egui_key_events(ui.ctx());
            left = Some(ui.ctx().input(|i| i.events.clone()));
        });
        // Releases and non-key events stay; presses, text and the synthesized
        // paste are gone.
        assert_eq!(
            left.unwrap(),
            vec![
                egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: false, repeat: false, modifiers: none },
                egui::Event::WindowFocused(true),
            ]
        );
    }

    #[test]
    fn ctrl_latin_fallback_only_for_non_ascii_under_ctrl() {
        // R-020: Ctrl+С on a Cyrillic group reads as the physical key's Latin
        // letter (VTE's vte_translate_ctrlkey walks to the Latin group).
        let key_c = PhysicalKey::Code(KeyCode::KeyC);
        let cyr_es = Key::Character("с".into());
        assert_eq!(ctrl_latin_fallback(&cyr_es, key_c, true), Some('c'));
        // Without Ctrl the layout's own character stands (Alt+С sends ESC с).
        assert_eq!(ctrl_latin_fallback(&cyr_es, key_c, false), None);
        // An ASCII key never falls back, even under Ctrl (Dvorak users keep
        // their layout's letter, not the QWERTY cap).
        assert_eq!(ctrl_latin_fallback(&Key::Character("c".into()), PhysicalKey::Code(KeyCode::KeyI), true), None);
        // A non-letter physical key has no Latin stand-in (Ctrl+Ж on ';').
        assert_eq!(ctrl_latin_fallback(&Key::Character("ж".into()), PhysicalKey::Code(KeyCode::Semicolon), true), None);
        // Named keys are never remapped.
        assert_eq!(ctrl_latin_fallback(&Key::Named(NamedKey::Enter), key_c, true), None);
    }

    #[test]
    fn physical_latin_letter_maps_key_a_to_z_only() {
        assert_eq!(physical_latin_letter(PhysicalKey::Code(KeyCode::KeyA)), Some('a'));
        assert_eq!(physical_latin_letter(PhysicalKey::Code(KeyCode::KeyC)), Some('c'));
        assert_eq!(physical_latin_letter(PhysicalKey::Code(KeyCode::KeyZ)), Some('z'));
        assert_eq!(physical_latin_letter(PhysicalKey::Code(KeyCode::Digit1)), None);
        assert_eq!(physical_latin_letter(PhysicalKey::Code(KeyCode::BracketLeft)), None);
        assert_eq!(physical_latin_letter(PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified)), None);
    }

    #[test]
    fn ctrl_latin_fallback_feeds_binding_lookup_and_control_byte() {
        // The fallback letter resolves to the same egui key and control byte a
        // Latin layout would give, so Ctrl+Shift+С looks up Copy and Ctrl+С
        // encodes 0x03.
        let letter = ctrl_latin_fallback(&Key::Character("с".into()), PhysicalKey::Code(KeyCode::KeyC), true).unwrap();
        assert_eq!(char_key(letter), Some(egui::Key::C));
        assert_eq!(ctrl_letter_byte(letter), Some(0x03));
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
        // M9 regression: Cmd + a Named key must be an inert shortcut, not leak
        // the key's legacy sequence. The named-key path returns None when the
        // chord is inert, so encode_raw_key yields a byte-less key.
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
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn super_chord_is_not_inert_on_linux() {
        // R-069: Super is nothing to VTE's keymap, so an unbound Super chord
        // sends the key's ordinary bytes (Super+Enter is \r); only macOS Cmd
        // is the inert accelerator modifier.
        let sup = egui::Modifiers { mac_cmd: true, command: true, ..Default::default() };
        assert!(!cmd_is_inert(sup));
        assert_eq!(
            named_logical_key_bytes(NamedKey::Enter, cmd_is_inert(sup), false, false, false),
            Some(b"\r".to_vec())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_chord_is_inert_on_macos() {
        let cmd = egui::Modifiers { mac_cmd: true, command: true, ..Default::default() };
        assert!(cmd_is_inert(cmd));
        assert_eq!(named_logical_key_bytes(NamedKey::Enter, cmd_is_inert(cmd), false, false, false), None);
    }

    #[test]
    fn shifted_char_key_is_the_other_character_shift_produced() {
        // R-073: Shift+= on US yields "+", a different nameable character.
        let plus = Key::Character("+".into());
        assert_eq!(shifted_char_key(&plus, true, Some(egui::Key::Equals)), Some(egui::Key::Plus));
        // Shift only changing a letter's case is not a different key.
        let upper_a = Key::Character("A".into());
        assert_eq!(shifted_char_key(&upper_a, true, Some(egui::Key::A)), None);
        // Shift up: no shifted form, whatever the logical key says.
        assert_eq!(shifted_char_key(&plus, false, Some(egui::Key::Plus)), None);
        // A shifted character egui cannot name ("_" from Shift+-) is nothing.
        assert_eq!(shifted_char_key(&Key::Character("_".into()), true, Some(egui::Key::Minus)), None);
        // Named keys never have a shifted character form.
        assert_eq!(shifted_char_key(&Key::Named(NamedKey::Tab), true, Some(egui::Key::Tab)), None);
    }

    #[test]
    fn lookup_binding_matches_shifted_character_with_shift_consumed() {
        // R-073: Terminator's `<Control>plus` fires on Ctrl+Shift+= (US) and
        // on a dedicated `+` key (German) alike — the translated keyval with
        // the consumed Shift removed (keybindings.py:120-132).
        let bindings = BindingTable::new(true);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let us = RawTermKey {
            key: Some(egui::Key::Equals),
            shifted_key: Some(egui::Key::Plus),
            mods: ctrl_shift,
            legacy_bytes: b"+".to_vec(),
        };
        assert_eq!(lookup_binding(&bindings, &us), Some((Action::ZoomIn, egui::Key::Plus, ctrl)));
        let german = raw(egui::Key::Plus, ctrl, b"+");
        assert_eq!(lookup_binding(&bindings, &german), Some((Action::ZoomIn, egui::Key::Plus, ctrl)));
        // Shift+* on German gives "*" (unnamed): nothing to match, "+" chord not consumed.
        let german_shift = RawTermKey { key: Some(egui::Key::Plus), shifted_key: None, mods: ctrl_shift, legacy_bytes: b"*".to_vec() };
        assert_eq!(lookup_binding(&bindings, &german_shift), None);
    }

    #[test]
    fn lookup_binding_prefers_the_unshifted_form_the_config_spells() {
        // A user's Ctrl+Shift+Equals entry still matches on the unshifted key
        // with the full modifier set, ahead of the shifted-character form.
        let mut bindings = BindingTable::new(true);
        bindings.apply_user(&[("split_vertical".into(), "Ctrl+Shift+Equals".into())]);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let us = RawTermKey {
            key: Some(egui::Key::Equals),
            shifted_key: Some(egui::Key::Plus),
            mods: ctrl_shift,
            legacy_bytes: b"+".to_vec(),
        };
        assert_eq!(lookup_binding(&bindings, &us), Some((Action::SplitVertical, egui::Key::Equals, ctrl_shift)));
    }

    #[test]
    fn lookup_binding_unnamed_key_matches_only_through_its_shifted_form() {
        // R-072: a key egui cannot name (é on AZERTY) has no stand-in — it
        // matches no binding by itself; Shift+é is "2" there, and that
        // character is what a Ctrl+2 binding sees, as in Terminator.
        let mut bindings = BindingTable::new(true);
        bindings.apply_user(&[("switch_to_tab_2".into(), "Ctrl+2".into())]);
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let e_acute = RawTermKey { key: None, shifted_key: None, mods: ctrl, legacy_bytes: "é".as_bytes().to_vec() };
        assert_eq!(lookup_binding(&bindings, &e_acute), None);
        assert_eq!(classify_key(&bindings, &e_acute, false, false), None);
        let shifted = RawTermKey { key: None, shifted_key: Some(egui::Key::Num2), mods: ctrl_shift, legacy_bytes: b"2".to_vec() };
        assert_eq!(lookup_binding(&bindings, &shifted), Some((Action::SwitchToTab(2), egui::Key::Num2, ctrl)));
    }

    #[test]
    fn classify_key_alt_screen_passes_both_cycling_directions() {
        // R-074: Ctrl+Shift+Tab (FocusPrev) passes through under a full-screen
        // TUI exactly like Ctrl+Tab (FocusNext); off alt-screen both are bindings.
        let bindings = BindingTable::new(true);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let back = raw(egui::Key::Tab, ctrl_shift, b"");
        assert_eq!(classify_key(&bindings, &back, true, false), None);
        assert_eq!(classify_key(&bindings, &back, false, false), Some(Action::FocusPrev));
    }
}
