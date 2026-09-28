//! Kitty keyboard protocol encoding.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
//!
//! A port of alacritty's `input/keyboard.rs` (`key_input`, `key_release`,
//! `should_build_sequence`, `build_sequence` and its `SequenceBuilder`),
//! operating on the winit event the key arrived as (`KeyInput`) rather than
//! on egui's name for it, so every key winit knows is encodable: é, ß,
//! Cyrillic, F13-F35, the keypad, media keys and the bare modifiers.
//!
//! `encode` answers for the kitty modes only; outside them (and for the
//! presses the kitty rules leave as text) it returns None and the caller
//! sends the legacy encoding, byte-for-byte what it sends without this module.

use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use winit::event::ElementState;
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// The parts of a `winit::event::KeyEvent` the encoder reads. A plain copy
/// because `KeyEvent` itself cannot be built outside winit (its
/// `platform_specific` field is private), and the encoder is tested on
/// constructed keys.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KeyInput {
    pub logical_key: Key,
    /// `KeyEventExtModifierSupplement::key_without_modifiers`.
    pub key_without_modifiers: Key,
    /// `KeyEventExtModifierSupplement::text_with_all_modifiers`.
    pub text_with_all_modifiers: Option<String>,
    pub location: KeyLocation,
    /// Which physical key this is: a release is matched to its press by it.
    pub physical_key: PhysicalKey,
    pub state: ElementState,
    pub repeat: bool,
}

impl KeyInput {
    pub(crate) fn from_event(event: &winit::event::KeyEvent) -> Self {
        Self {
            logical_key: event.logical_key.clone(),
            key_without_modifiers: event.key_without_modifiers(),
            text_with_all_modifiers: event.text_with_all_modifiers().map(str::to_owned),
            location: event.location,
            physical_key: event.physical_key,
            state: event.state,
            repeat: event.repeat,
        }
    }

    /// alacritty `key.text_with_all_modifiers().unwrap_or_default()`.
    fn text(&self) -> &str {
        self.text_with_all_modifiers.as_deref().unwrap_or_default()
    }
}

/// What a pane in `mode` sends for `key`, or None when the legacy encoding
/// applies (no kitty flag set, a key alacritty's default bindings send in
/// its legacy form, or a press that goes out as plain text). A release never
/// falls back to legacy: it is the (possibly empty) kitty report or nothing.
pub(crate) fn encode(key: &KeyInput, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    if key.state == ElementState::Released {
        return Some(key_release(key, mods, mode));
    }
    if !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) || legacy_binding_precedes(key, mods, mode) {
        return None;
    }

    // Mask `Alt` modifier from input when we won't send esc.
    let text = key.text();
    let mods = if alt_send_esc(key, text, mods) { mods } else { mods & !ModifiersState::ALT };

    if should_build_sequence(key, text, mode, mods) {
        Some(build_sequence(key, mods, mode))
    } else {
        // alacritty sends `ESC? text` here — the legacy encoding.
        None
    }
}

/// Whether one of alacritty's default key bindings (`default_key_bindings`,
/// config/bindings.rs) fires for this press ahead of `build_sequence`; its
/// bytes are the legacy encoding the caller already has (DECCKM rewrite,
/// the profile's erase binding, SS3 F1-F4, `CSI Z`). Bindings match the raw
/// modifier state exactly, before the Alt masking.
fn legacy_binding_precedes(key: &KeyInput, mods: ModifiersState, mode: TermMode) -> bool {
    let named = match key.logical_key {
        Key::Named(named) => named,
        _ => return false,
    };
    let kitty_esc = mode.intersects(TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::DISAMBIGUATE_ESC_CODES);
    let shift_alt = ModifiersState::SHIFT | ModifiersState::ALT;
    match named {
        // App cursor mode: SS3 even under the kitty flags.
        NamedKey::ArrowUp
        | NamedKey::ArrowDown
        | NamedKey::ArrowLeft
        | NamedKey::ArrowRight
        | NamedKey::Home
        | NamedKey::End => mode.contains(TermMode::APP_CURSOR) && mods.is_empty(),
        // "Legacy keys handling which can't be automatically encoded."
        NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4 => !kitty_esc && mods.is_empty(),
        NamedKey::Tab => !kitty_esc && (mods == ModifiersState::SHIFT || mods == shift_alt),
        NamedKey::Backspace => {
            (!mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) && mods.is_empty())
                || (!kitty_esc && (mods == ModifiersState::ALT || mods == ModifiersState::SHIFT))
        },
        NamedKey::Enter => !kitty_esc && key.location == KeyLocation::Numpad && mods.is_empty(),
        _ => false,
    }
}

/// alacritty `Processor::key_release`.
fn key_release(key: &KeyInput, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
    if !mode.contains(TermMode::REPORT_EVENT_TYPES) {
        return Vec::new();
    }

    // Mask `Alt` modifier from input when we won't send esc. A release
    // carries no text, and alacritty's empty text here masks Alt on every
    // character key (Alt+b release `\e[98;1:3u`); kitty reports the
    // modifiers held at the event (`\e[98;3:3u`, key_encoding.c
    // `convert_glfw_mods`). So judge by the key's own character, the text its
    // press had, to keep Alt exactly when the press kept it.
    let text = match (key.logical_key.as_ref(), key.key_without_modifiers.as_ref()) {
        (Key::Character(c), _) | (_, Key::Character(c)) => c,
        _ => key.text(),
    };
    let mods = if alt_send_esc(key, text, mods) { mods } else { mods & !ModifiersState::ALT };

    // Spec "Report event types": Enter, Tab and Backspace have no release
    // events unless all keys are reported, so `reset` still works at a shell
    // prompt a crashed program left in this mode.
    match key.logical_key.as_ref() {
        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) | Key::Named(NamedKey::Backspace)
            if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) =>
        {
            Vec::new()
        },
        _ => build_sequence(key, mods, mode),
    }
}

/// alacritty `Processor::alt_send_esc`. Rustinator has no macOS
/// `option_as_alt` setting, so Alt is Alt on every platform (alacritty's
/// non-macOS arm), as the legacy encoding already treats it.
fn alt_send_esc(key: &KeyInput, text: &str, mods: ModifiersState) -> bool {
    let alt_send_esc = mods.alt_key();
    match key.logical_key {
        Key::Named(named) => {
            if named.to_text().is_some() {
                alt_send_esc
            } else {
                // Treat `Alt` as modifier for named keys without text, like ArrowUp.
                mods.alt_key()
            }
        },
        _ => alt_send_esc && text.chars().count() == 1,
    }
}

/// alacritty `Processor::is_modifier_key`: a press of one of these writes
/// its report (under REPORT_ALL_KEYS_AS_ESC) without clearing the selection
/// or scrolling to the bottom.
pub(crate) fn is_modifier_key(key: &KeyInput) -> bool {
    matches!(
        key.logical_key.as_ref(),
        Key::Named(NamedKey::Shift)
            | Key::Named(NamedKey::Control)
            | Key::Named(NamedKey::Alt)
            | Key::Named(NamedKey::Super)
    )
}

/// Check whether we should try to build escape sequence for the key
/// (alacritty `Processor::should_build_sequence`).
fn should_build_sequence(key: &KeyInput, text: &str, mode: TermMode, mods: ModifiersState) -> bool {
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }

    // A keypad key is forced into a sequence only when it has no text
    // (kitty `encode_glfw_key_event`: below REPORT_ALL_KEYS_AS_ESC a
    // press/repeat whose text is non-empty and not an ASCII control character
    // is SEND_TEXT_TO_CHILD, ahead of `encode_key`). So NumLock KP_1 sends
    // "1" under disambiguate while KP_Enter ("\r") stays CSI 57414 u. Kitty's
    // glfw drops the text under Ctrl/Alt/Super (xkb_glfw.c), and those
    // modifiers take the next arm here anyway.
    let keypad_without_text =
        key.location == KeyLocation::Numpad && (text.is_empty() || is_control_character(text));
    let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)
        && (key.logical_key == Key::Named(NamedKey::Escape)
            || keypad_without_text
            || (!mods.is_empty()
                && (mods != ModifiersState::SHIFT
                    || matches!(
                        key.logical_key,
                        Key::Named(NamedKey::Tab) | Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Backspace)
                    ))));

    match key.logical_key {
        _ if disambiguate => true,
        // Exclude all the named keys unless they have textual representation.
        Key::Named(named) => named.to_text().is_none(),
        _ => text.is_empty(),
    }
}

/// Build a key's keyboard escape sequence based on the given `key`, `mods`,
/// and `mode` (alacritty `build_sequence`).
fn build_sequence(key: &KeyInput, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
    let mut modifiers = SequenceModifiers::from(mods);

    let kitty_seq = mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES,
    );

    let kitty_encode_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    // The default parameter is 1, so we can omit it.
    let kitty_event_type =
        mode.contains(TermMode::REPORT_EVENT_TYPES) && (key.repeat || key.state == ElementState::Released);

    let context = SequenceBuilder { mode, modifiers, kitty_seq, kitty_encode_all, kitty_event_type };

    let associated_text = key.text_with_all_modifiers.as_deref().filter(|text| {
        mode.contains(TermMode::REPORT_ASSOCIATED_TEXT)
            && key.state != ElementState::Released
            && !text.is_empty()
            && !is_control_character(text)
    });

    let sequence_base = context
        .try_build_numpad(key)
        .or_else(|| context.try_build_named_kitty(key))
        .or_else(|| context.try_build_named_normal(key, associated_text.is_some()))
        .or_else(|| context.try_build_control_char_or_mod(key, &mut modifiers))
        .or_else(|| context.try_build_textual(key, associated_text));

    let (payload, terminator) = match sequence_base {
        Some(SequenceBase { payload, terminator }) => (payload, terminator),
        _ => return Vec::new(),
    };

    let mut payload = format!("\x1b[{payload}");

    // Add modifiers information.
    if kitty_event_type || !modifiers.is_empty() || associated_text.is_some() {
        payload.push_str(&format!(";{}", modifiers.encode_esc_sequence()));
    }

    // Push event type.
    if kitty_event_type {
        payload.push(':');
        let event_type = match key.state {
            _ if key.repeat => '2',
            ElementState::Pressed => '1',
            ElementState::Released => '3',
        };
        payload.push(event_type);
    }

    if let Some(text) = associated_text {
        let mut codepoints = text.chars().map(u32::from);
        if let Some(codepoint) = codepoints.next() {
            payload.push_str(&format!(";{codepoint}"));
        }
        for codepoint in codepoints {
            payload.push_str(&format!(":{codepoint}"));
        }
    }

    payload.push(terminator.encode_esc_sequence());

    payload.into_bytes()
}

/// Helper to build escape sequence payloads from a [`KeyInput`].
struct SequenceBuilder {
    mode: TermMode,
    /// The emitted sequence should follow the kitty keyboard protocol.
    kitty_seq: bool,
    /// Encode all the keys according to the protocol.
    kitty_encode_all: bool,
    /// Report event types.
    kitty_event_type: bool,
    modifiers: SequenceModifiers,
}

impl SequenceBuilder {
    /// Try building sequence from the event's emitting text.
    fn try_build_textual(&self, key: &KeyInput, associated_text: Option<&str>) -> Option<SequenceBase> {
        let character = match key.logical_key.as_ref() {
            Key::Character(character) if self.kitty_seq => character,
            _ => return None,
        };

        if character.chars().count() == 1 {
            let shift = self.modifiers.contains(SequenceModifiers::SHIFT);

            let ch = character.chars().next().unwrap();
            let unshifted_ch = if shift { ch.to_lowercase().next().unwrap() } else { ch };

            let alternate_key_code = u32::from(ch);
            let mut unicode_key_code = u32::from(unshifted_ch);

            // Try to get the base for keys which change based on modifier, like `1` for `!`.
            //
            // However it should only be performed when `SHIFT` is pressed.
            if shift && alternate_key_code == unicode_key_code {
                if let Key::Character(unmodded) = key.key_without_modifiers.as_ref() {
                    unicode_key_code = u32::from(unmodded.chars().next().unwrap_or(unshifted_ch));
                }
            }

            // NOTE: Base layouts are ignored, since winit doesn't expose this information
            // yet.
            let payload = if self.mode.contains(TermMode::REPORT_ALTERNATE_KEYS)
                && alternate_key_code != unicode_key_code
            {
                format!("{unicode_key_code}:{alternate_key_code}")
            } else {
                unicode_key_code.to_string()
            };

            Some(SequenceBase::new(payload.into(), SequenceTerminator::Kitty))
        } else if self.kitty_encode_all && associated_text.is_some() {
            // Fallback when need to report text, but we don't have any key associated with this
            // text.
            Some(SequenceBase::new("0".into(), SequenceTerminator::Kitty))
        } else {
            None
        }
    }

    /// Try building from numpad key.
    ///
    /// `None` is returned when the key is neither known nor numpad.
    fn try_build_numpad(&self, key: &KeyInput) -> Option<SequenceBase> {
        if !self.kitty_seq || key.location != KeyLocation::Numpad {
            return None;
        }

        let base = match key.logical_key.as_ref() {
            Key::Character("0") => "57399",
            Key::Character("1") => "57400",
            Key::Character("2") => "57401",
            Key::Character("3") => "57402",
            Key::Character("4") => "57403",
            Key::Character("5") => "57404",
            Key::Character("6") => "57405",
            Key::Character("7") => "57406",
            Key::Character("8") => "57407",
            Key::Character("9") => "57408",
            Key::Character(".") => "57409",
            Key::Character("/") => "57410",
            Key::Character("*") => "57411",
            Key::Character("-") => "57412",
            Key::Character("+") => "57413",
            Key::Character("=") => "57415",
            Key::Named(named) => match named {
                NamedKey::Enter => "57414",
                NamedKey::ArrowLeft => "57417",
                NamedKey::ArrowRight => "57418",
                NamedKey::ArrowUp => "57419",
                NamedKey::ArrowDown => "57420",
                NamedKey::PageUp => "57421",
                NamedKey::PageDown => "57422",
                NamedKey::Home => "57423",
                NamedKey::End => "57424",
                NamedKey::Insert => "57425",
                NamedKey::Delete => "57426",
                _ => return None,
            },
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
    }

    /// Try building from [`NamedKey`] using the kitty keyboard protocol encoding
    /// for functional keys.
    fn try_build_named_kitty(&self, key: &KeyInput) -> Option<SequenceBase> {
        let named = match key.logical_key {
            Key::Named(named) if self.kitty_seq => named,
            _ => return None,
        };

        let (base, terminator) = match named {
            // F3 in kitty protocol diverges from alacritty's terminfo.
            NamedKey::F3 => ("13", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("57376", SequenceTerminator::Kitty),
            NamedKey::F14 => ("57377", SequenceTerminator::Kitty),
            NamedKey::F15 => ("57378", SequenceTerminator::Kitty),
            NamedKey::F16 => ("57379", SequenceTerminator::Kitty),
            NamedKey::F17 => ("57380", SequenceTerminator::Kitty),
            NamedKey::F18 => ("57381", SequenceTerminator::Kitty),
            NamedKey::F19 => ("57382", SequenceTerminator::Kitty),
            NamedKey::F20 => ("57383", SequenceTerminator::Kitty),
            NamedKey::F21 => ("57384", SequenceTerminator::Kitty),
            NamedKey::F22 => ("57385", SequenceTerminator::Kitty),
            NamedKey::F23 => ("57386", SequenceTerminator::Kitty),
            NamedKey::F24 => ("57387", SequenceTerminator::Kitty),
            NamedKey::F25 => ("57388", SequenceTerminator::Kitty),
            NamedKey::F26 => ("57389", SequenceTerminator::Kitty),
            NamedKey::F27 => ("57390", SequenceTerminator::Kitty),
            NamedKey::F28 => ("57391", SequenceTerminator::Kitty),
            NamedKey::F29 => ("57392", SequenceTerminator::Kitty),
            NamedKey::F30 => ("57393", SequenceTerminator::Kitty),
            NamedKey::F31 => ("57394", SequenceTerminator::Kitty),
            NamedKey::F32 => ("57395", SequenceTerminator::Kitty),
            NamedKey::F33 => ("57396", SequenceTerminator::Kitty),
            NamedKey::F34 => ("57397", SequenceTerminator::Kitty),
            NamedKey::F35 => ("57398", SequenceTerminator::Kitty),
            NamedKey::ScrollLock => ("57359", SequenceTerminator::Kitty),
            NamedKey::PrintScreen => ("57361", SequenceTerminator::Kitty),
            NamedKey::Pause => ("57362", SequenceTerminator::Kitty),
            NamedKey::ContextMenu => ("57363", SequenceTerminator::Kitty),
            NamedKey::MediaPlay => ("57428", SequenceTerminator::Kitty),
            NamedKey::MediaPause => ("57429", SequenceTerminator::Kitty),
            NamedKey::MediaPlayPause => ("57430", SequenceTerminator::Kitty),
            NamedKey::MediaStop => ("57432", SequenceTerminator::Kitty),
            NamedKey::MediaFastForward => ("57433", SequenceTerminator::Kitty),
            NamedKey::MediaRewind => ("57434", SequenceTerminator::Kitty),
            NamedKey::MediaTrackNext => ("57435", SequenceTerminator::Kitty),
            NamedKey::MediaTrackPrevious => ("57436", SequenceTerminator::Kitty),
            NamedKey::MediaRecord => ("57437", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeDown => ("57438", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeUp => ("57439", SequenceTerminator::Kitty),
            NamedKey::AudioVolumeMute => ("57440", SequenceTerminator::Kitty),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building from [`NamedKey`].
    fn try_build_named_normal(&self, key: &KeyInput, has_associated_text: bool) -> Option<SequenceBase> {
        let named = match key.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        // The default parameter is 1, so we can omit it.
        let one_based = if self.modifiers.is_empty() && !self.kitty_event_type && !has_associated_text {
            ""
        } else {
            "1"
        };
        let (base, terminator) = match named {
            NamedKey::PageUp => ("5", SequenceTerminator::Normal('~')),
            NamedKey::PageDown => ("6", SequenceTerminator::Normal('~')),
            NamedKey::Insert => ("2", SequenceTerminator::Normal('~')),
            NamedKey::Delete => ("3", SequenceTerminator::Normal('~')),
            NamedKey::Home => (one_based, SequenceTerminator::Normal('H')),
            NamedKey::End => (one_based, SequenceTerminator::Normal('F')),
            NamedKey::ArrowLeft => (one_based, SequenceTerminator::Normal('D')),
            NamedKey::ArrowRight => (one_based, SequenceTerminator::Normal('C')),
            NamedKey::ArrowUp => (one_based, SequenceTerminator::Normal('A')),
            NamedKey::ArrowDown => (one_based, SequenceTerminator::Normal('B')),
            NamedKey::F1 => (one_based, SequenceTerminator::Normal('P')),
            NamedKey::F2 => (one_based, SequenceTerminator::Normal('Q')),
            NamedKey::F3 => (one_based, SequenceTerminator::Normal('R')),
            NamedKey::F4 => (one_based, SequenceTerminator::Normal('S')),
            NamedKey::F5 => ("15", SequenceTerminator::Normal('~')),
            NamedKey::F6 => ("17", SequenceTerminator::Normal('~')),
            NamedKey::F7 => ("18", SequenceTerminator::Normal('~')),
            NamedKey::F8 => ("19", SequenceTerminator::Normal('~')),
            NamedKey::F9 => ("20", SequenceTerminator::Normal('~')),
            NamedKey::F10 => ("21", SequenceTerminator::Normal('~')),
            NamedKey::F11 => ("23", SequenceTerminator::Normal('~')),
            NamedKey::F12 => ("24", SequenceTerminator::Normal('~')),
            NamedKey::F13 => ("25", SequenceTerminator::Normal('~')),
            NamedKey::F14 => ("26", SequenceTerminator::Normal('~')),
            NamedKey::F15 => ("28", SequenceTerminator::Normal('~')),
            NamedKey::F16 => ("29", SequenceTerminator::Normal('~')),
            NamedKey::F17 => ("31", SequenceTerminator::Normal('~')),
            NamedKey::F18 => ("32", SequenceTerminator::Normal('~')),
            NamedKey::F19 => ("33", SequenceTerminator::Normal('~')),
            NamedKey::F20 => ("34", SequenceTerminator::Normal('~')),
            _ => return None,
        };

        Some(SequenceBase::new(base.into(), terminator))
    }

    /// Try building escape from control characters (e.g. Enter) and modifiers.
    fn try_build_control_char_or_mod(
        &self,
        key: &KeyInput,
        mods: &mut SequenceModifiers,
    ) -> Option<SequenceBase> {
        if !self.kitty_encode_all && !self.kitty_seq {
            return None;
        }

        let named = match key.logical_key {
            Key::Named(named) => named,
            _ => return None,
        };

        let base = match named {
            NamedKey::Tab => "9",
            NamedKey::Enter => "13",
            NamedKey::Escape => "27",
            NamedKey::Space => "32",
            NamedKey::Backspace => "127",
            _ => "",
        };

        // Fail when the key is not a named control character and the active mode prohibits us
        // from encoding modifier keys.
        if !self.kitty_encode_all && base.is_empty() {
            return None;
        }

        let base = match (named, key.location) {
            (NamedKey::Shift, KeyLocation::Left) => "57441",
            (NamedKey::Control, KeyLocation::Left) => "57442",
            (NamedKey::Alt, KeyLocation::Left) => "57443",
            (NamedKey::Super, KeyLocation::Left) => "57444",
            (NamedKey::Hyper, KeyLocation::Left) => "57445",
            (NamedKey::Meta, KeyLocation::Left) => "57446",
            (NamedKey::Shift, _) => "57447",
            (NamedKey::Control, _) => "57448",
            (NamedKey::Alt, _) => "57449",
            (NamedKey::Super, _) => "57450",
            (NamedKey::Hyper, _) => "57451",
            (NamedKey::Meta, _) => "57452",
            (NamedKey::CapsLock, _) => "57358",
            (NamedKey::NumLock, _) => "57360",
            _ => base,
        };

        // NOTE: Kitty's protocol mandates that the modifier state is applied before
        // key press, however winit sends them after the key press, so for modifiers
        // itself apply the state based on keysyms and not the _actual_ modifiers
        // state, which is how kitty is doing so and what is suggested in such case.
        let press = key.state.is_pressed();
        match named {
            NamedKey::Shift => mods.set(SequenceModifiers::SHIFT, press),
            NamedKey::Control => mods.set(SequenceModifiers::CONTROL, press),
            NamedKey::Alt => mods.set(SequenceModifiers::ALT, press),
            NamedKey::Super => mods.set(SequenceModifiers::SUPER, press),
            _ => (),
        }

        if base.is_empty() {
            None
        } else {
            Some(SequenceBase::new(base.into(), SequenceTerminator::Kitty))
        }
    }
}

struct SequenceBase {
    /// The base of the payload, which is the `number` and optionally an alt base from the kitty
    /// spec.
    payload: Cow<'static, str>,
    terminator: SequenceTerminator,
}

impl SequenceBase {
    fn new(payload: Cow<'static, str>, terminator: SequenceTerminator) -> Self {
        Self { payload, terminator }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceTerminator {
    /// The normal key esc sequence terminator defined by xterm/dec.
    Normal(char),
    /// The terminator is for kitty escape sequence.
    Kitty,
}

impl SequenceTerminator {
    fn encode_esc_sequence(self) -> char {
        match self {
            SequenceTerminator::Normal(char) => char,
            SequenceTerminator::Kitty => 'u',
        }
    }
}

/// The modifiers encoding for escape sequence (alacritty's `bitflags`
/// `SequenceModifiers`, spelled out to avoid the dependency). Kitty also
/// defines Hyper (16), Meta (32), Caps Lock (64) and Num Lock (128), but
/// winit 0.30's `ModifiersState` carries none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SequenceModifiers(u8);

impl SequenceModifiers {
    const SHIFT: Self = Self(0b0000_0001);
    const ALT: Self = Self(0b0000_0010);
    const CONTROL: Self = Self(0b0000_0100);
    const SUPER: Self = Self(0b0000_1000);

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn set(&mut self, other: Self, value: bool) {
        if value {
            self.0 |= other.0;
        } else {
            self.0 &= !other.0;
        }
    }

    /// Get the value which should be passed to escape sequence.
    fn encode_esc_sequence(self) -> u8 {
        self.0 + 1
    }
}

impl From<ModifiersState> for SequenceModifiers {
    fn from(mods: ModifiersState) -> Self {
        let mut modifiers = Self(0);
        modifiers.set(Self::SHIFT, mods.shift_key());
        modifiers.set(Self::ALT, mods.alt_key());
        modifiers.set(Self::CONTROL, mods.control_key());
        modifiers.set(Self::SUPER, mods.super_key());
        modifiers
    }
}

/// Check whether the `text` is `0x7f`, `C0` or `C1` control code.
fn is_control_character(text: &str) -> bool {
    // 0x7f (DEL) is included here since it has a dedicated control code (`^?`) which generally
    // does not match the reported text (`^H`), despite not technically being part of C0 or C1.
    let codepoint = text.bytes().next().unwrap();
    text.len() == 1 && (codepoint < 0x20 || (0x7f..=0x9f).contains(&codepoint))
}

#[cfg(test)]
mod tests {
    //! Vectors from kitty's `kitty_tests/keys.py` and the protocol spec,
    //! expressed as the winit events a real keyboard produces (alacritty has
    //! no unit tests of its own for this encoder).

    use super::*;
    use winit::keyboard::NativeKey;

    const SHIFT: ModifiersState = ModifiersState::SHIFT;
    const ALT: ModifiersState = ModifiersState::ALT;
    const CTRL: ModifiersState = ModifiersState::CONTROL;
    const SUPER: ModifiersState = ModifiersState::SUPER;
    const NONE: ModifiersState = ModifiersState::empty();

    const DISAMBIGUATE: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;
    const EVENT_TYPES: TermMode = TermMode::REPORT_EVENT_TYPES;
    const ALTERNATES: TermMode = TermMode::REPORT_ALTERNATE_KEYS;
    const ALL_KEYS: TermMode = TermMode::REPORT_ALL_KEYS_AS_ESC;
    const TEXT: TermMode = TermMode::REPORT_ASSOCIATED_TEXT;

    fn key(logical: Key, unmodded: Key, text: Option<&str>) -> KeyInput {
        KeyInput {
            logical_key: logical,
            key_without_modifiers: unmodded,
            text_with_all_modifiers: text.map(str::to_owned),
            location: KeyLocation::Standard,
            physical_key: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            state: ElementState::Pressed,
            repeat: false,
        }
    }

    /// A character key; `text` is what `text_with_all_modifiers` reports.
    fn ch(logical: &str, unmodded: &str, text: &str) -> KeyInput {
        key(Key::Character(logical.into()), Key::Character(unmodded.into()), Some(text))
    }

    fn named(named: NamedKey) -> KeyInput {
        key(Key::Named(named), Key::Named(named), named.to_text())
    }

    fn at(mut k: KeyInput, location: KeyLocation) -> KeyInput {
        k.location = location;
        k
    }

    fn released(mut k: KeyInput) -> KeyInput {
        k.state = ElementState::Released;
        k
    }

    fn repeated(mut k: KeyInput) -> KeyInput {
        k.repeat = true;
        k
    }

    fn enc(k: &KeyInput, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
        encode(k, mods, mode)
    }

    fn sent(k: &KeyInput, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
        enc(k, mods, mode).expect("kitty-encoded, not legacy")
    }

    #[test]
    fn no_kitty_flag_is_legacy() {
        assert_eq!(enc(&ch("a", "a", "\x01"), CTRL, TermMode::empty()), None);
        assert_eq!(enc(&named(NamedKey::Escape), NONE, TermMode::empty()), None);
        assert_eq!(enc(&named(NamedKey::ArrowLeft), CTRL, TermMode::APP_CURSOR), None);
        // A release outside the kitty modes is reported as nothing, never legacy.
        assert_eq!(enc(&released(ch("a", "a", "a")), NONE, TermMode::empty()), Some(Vec::new()));
    }

    #[test]
    fn disambiguate() {
        // keys.py "test disambiguate" (flags 0b1).
        assert_eq!(enc(&ch("a", "a", "a"), NONE, DISAMBIGUATE), None);
        assert_eq!(sent(&named(NamedKey::Escape), NONE, DISAMBIGUATE), b"\x1b[27u");
        assert_eq!(enc(&named(NamedKey::Enter), NONE, DISAMBIGUATE), None);
        assert_eq!(sent(&named(NamedKey::Enter), SHIFT, DISAMBIGUATE), b"\x1b[13;2u");
        assert_eq!(enc(&named(NamedKey::Tab), NONE, DISAMBIGUATE), None);
        assert_eq!(enc(&named(NamedKey::Backspace), NONE, DISAMBIGUATE), None);
        assert_eq!(sent(&named(NamedKey::Tab), SHIFT, DISAMBIGUATE), b"\x1b[9;2u");
        assert_eq!(sent(&ch("a", "a", "\x01"), CTRL, DISAMBIGUATE), b"\x1b[97;5u");
        assert_eq!(sent(&ch("a", "a", "a"), ALT, DISAMBIGUATE), b"\x1b[97;3u");
        assert_eq!(sent(&ch("A", "a", "\x01"), CTRL | SHIFT, DISAMBIGUATE), b"\x1b[97;6u");
        assert_eq!(sent(&ch("A", "a", "A"), ALT | SHIFT, DISAMBIGUATE), b"\x1b[97;4u");
        assert_eq!(sent(&named(NamedKey::Space), CTRL, DISAMBIGUATE), b"\x1b[32;5u");
        assert_eq!(enc(&named(NamedKey::ArrowUp), NONE, DISAMBIGUATE).unwrap(), b"\x1b[A");
        assert_eq!(sent(&named(NamedKey::ArrowUp), CTRL, DISAMBIGUATE), b"\x1b[1;5A");
        // Shift alone on a text key is text: "A", "@".
        assert_eq!(enc(&ch("A", "a", "A"), SHIFT, DISAMBIGUATE), None);
        assert_eq!(enc(&ch("@", "2", "@"), SHIFT, DISAMBIGUATE), None);
    }

    #[test]
    fn disambiguate_keypad() {
        // keys.py: KP_PAGE_UP and KP_0 are CSI u under disambiguate.
        let kp_page_up = at(named(NamedKey::PageUp), KeyLocation::Numpad);
        assert_eq!(sent(&kp_page_up, NONE, DISAMBIGUATE), b"\x1b[57421u");
        assert_eq!(sent(&kp_page_up, CTRL, DISAMBIGUATE), b"\x1b[57421;5u");
        // keys.py passes KP_0 with no text: the encoder alone.
        let kp_0 = at(key(Key::Character("0".into()), Key::Character("0".into()), None), KeyLocation::Numpad);
        assert_eq!(sent(&kp_0, NONE, DISAMBIGUATE), b"\x1b[57399u");
        assert_eq!(sent(&kp_0, CTRL, DISAMBIGUATE), b"\x1b[57399;5u");
    }

    #[test]
    fn disambiguate_keypad_text() {
        // kitty `encode_glfw_key_event`: with NumLock on, KP_1 has text "1"
        // and goes out as that text below REPORT_ALL_KEYS_AS_ESC.
        let kp_1 = at(ch("1", "1", "1"), KeyLocation::Numpad);
        assert_eq!(enc(&kp_1, NONE, DISAMBIGUATE), None);
        assert_eq!(enc(&repeated(kp_1.clone()), NONE, DISAMBIGUATE), None);
        assert_eq!(enc(&kp_1, NONE, DISAMBIGUATE | EVENT_TYPES), None);
        // Its release still has a report: releases never go out as text.
        assert_eq!(sent(&released(kp_1.clone()), NONE, DISAMBIGUATE | EVENT_TYPES), b"\x1b[57400;1:3u");
        // KP_Enter's text is "\r", a control character: not text, so CSI u.
        let kp_enter = at(named(NamedKey::Enter), KeyLocation::Numpad);
        assert_eq!(sent(&kp_enter, NONE, DISAMBIGUATE), b"\x1b[57414u");
        // Report-all encodes every keypad key.
        assert_eq!(sent(&kp_1, NONE, ALL_KEYS), b"\x1b[57400u");
        // Ctrl+KP_1: kitty's glfw sends no text under Ctrl (xkb_glfw.c), so
        // `encode_key` -> `encode_function_key` serializes the keypad key.
        // (kitty also sets its NumLock bit, 128, giving `\e[57400;133u`;
        // winit reports no lock modifiers, so it is absent here as for every
        // other key.)
        assert_eq!(sent(&kp_1, CTRL, DISAMBIGUATE), b"\x1b[57400;5u");
    }

    #[test]
    fn legacy_functional_keys_with_modifiers() {
        assert_eq!(sent(&named(NamedKey::ArrowLeft), CTRL, DISAMBIGUATE), b"\x1b[1;5D");
        assert_eq!(sent(&named(NamedKey::ArrowRight), SHIFT | ALT, DISAMBIGUATE), b"\x1b[1;4C");
        assert_eq!(sent(&named(NamedKey::Delete), CTRL, DISAMBIGUATE), b"\x1b[3;5~");
        assert_eq!(sent(&named(NamedKey::F3), CTRL, DISAMBIGUATE), b"\x1b[13;5~");
        assert_eq!(sent(&named(NamedKey::F5), CTRL, DISAMBIGUATE), b"\x1b[15;5~");
        assert_eq!(sent(&named(NamedKey::F1), SHIFT, DISAMBIGUATE), b"\x1b[1;2P");
        // Unmodified: the implicit 1 is dropped for letter terminators.
        assert_eq!(sent(&named(NamedKey::ArrowLeft), NONE, ALL_KEYS), b"\x1b[D");
        assert_eq!(sent(&named(NamedKey::PageUp), NONE, ALL_KEYS), b"\x1b[5~");
        assert_eq!(sent(&named(NamedKey::F3), NONE, ALL_KEYS), b"\x1b[13~");
        assert_eq!(sent(&named(NamedKey::F1), NONE, ALL_KEYS), b"\x1b[P");
        assert_eq!(sent(&named(NamedKey::Delete), NONE, DISAMBIGUATE), b"\x1b[3~");
    }

    #[test]
    fn app_cursor_unmodified_arrows_stay_ss3() {
        // alacritty's APP_CURSOR bindings run ahead of build_sequence in every
        // kitty mode: the caller's DECCKM rewrite sends `ESC O A`.
        let mode = DISAMBIGUATE | TermMode::APP_CURSOR;
        for k in [NamedKey::ArrowUp, NamedKey::ArrowDown, NamedKey::Home, NamedKey::End] {
            assert_eq!(enc(&named(k), NONE, mode), None);
            assert_eq!(enc(&named(k), NONE, ALL_KEYS | TermMode::APP_CURSOR), None);
        }
        // Any modifier, Super included, is the kitty form.
        assert_eq!(sent(&named(NamedKey::ArrowUp), CTRL, mode), b"\x1b[1;5A");
        assert_eq!(sent(&named(NamedKey::ArrowUp), SUPER, mode), b"\x1b[1;9A");
    }

    #[test]
    fn event_types_only_keeps_legacy_bindings() {
        // With only 0b10, alacritty's legacy bindings still apply: SS3 F1-F4,
        // CSI Z for Shift+Tab, the erase binding for Backspace.
        assert_eq!(enc(&named(NamedKey::F1), NONE, EVENT_TYPES), None);
        assert_eq!(enc(&named(NamedKey::Tab), SHIFT, EVENT_TYPES), None);
        assert_eq!(enc(&named(NamedKey::Backspace), ALT, EVENT_TYPES), None);
        assert_eq!(enc(&at(named(NamedKey::Enter), KeyLocation::Numpad), NONE, EVENT_TYPES), None);
        // Text keys are text, keys without text are sequences.
        assert_eq!(enc(&ch("a", "a", "a"), NONE, EVENT_TYPES), None);
        assert_eq!(sent(&named(NamedKey::ArrowUp), NONE, EVENT_TYPES), b"\x1b[A");
        // F1 once disambiguating is its CSI form.
        assert_eq!(sent(&named(NamedKey::F1), NONE, DISAMBIGUATE), b"\x1b[P");
    }

    #[test]
    fn event_types() {
        // keys.py "test event type reporting" (0b10, 0b11).
        let a = ch("a", "a", "a");
        assert_eq!(enc(&a, NONE, EVENT_TYPES), None);
        assert_eq!(sent(&released(a.clone()), NONE, EVENT_TYPES), b"\x1b[97;1:3u");
        assert_eq!(sent(&released(ch("A", "a", "A")), SHIFT, EVENT_TYPES), b"\x1b[97;2:3u");
        let flags = DISAMBIGUATE | EVENT_TYPES;
        assert_eq!(sent(&released(a.clone()), NONE, flags), b"\x1b[97;1:3u");
        // A repeat that types text is text; one that doesn't is reported as `:2`.
        assert_eq!(enc(&repeated(a.clone()), NONE, flags), None);
        assert_eq!(sent(&repeated(ch("a", "a", "\x01")), CTRL, flags), b"\x1b[97;5:2u");
        assert_eq!(sent(&repeated(named(NamedKey::ArrowLeft)), NONE, flags), b"\x1b[1;1:2D");
        assert_eq!(sent(&released(named(NamedKey::ArrowLeft)), CTRL, flags), b"\x1b[1;5:3D");
        assert_eq!(sent(&released(named(NamedKey::Escape)), NONE, flags), b"\x1b[27;1:3u");
        // Enter, Tab and Backspace have no release events unless all keys are reported.
        assert_eq!(enc(&named(NamedKey::Backspace), NONE, flags), None);
        for k in [NamedKey::Enter, NamedKey::Tab, NamedKey::Backspace] {
            assert_eq!(sent(&released(named(k)), NONE, flags), b"");
        }
        assert_eq!(sent(&released(named(NamedKey::Enter)), NONE, flags | ALL_KEYS), b"\x1b[13;1:3u");
        // Without 0b10 a release is nothing.
        assert_eq!(sent(&released(a), NONE, DISAMBIGUATE | ALL_KEYS), b"");
    }

    #[test]
    fn alternate_keys() {
        // keys.py "test alternate key reporting" (0b100) — with disambiguate,
        // as the flag only affects keys already sent as escape codes.
        let flags = DISAMBIGUATE | ALTERNATES;
        assert_eq!(enc(&ch("a", "a", "a"), NONE, flags), None);
        assert_eq!(sent(&ch("A", "a", "A"), SHIFT, ALL_KEYS | ALTERNATES), b"\x1b[97:65;2u");
        assert_eq!(sent(&ch("A", "a", "\x01"), SHIFT | CTRL, flags), b"\x1b[97:65;6u");
        // A shifted symbol reports its base key from the unmodified layout.
        assert_eq!(sent(&ch("!", "1", "!"), SHIFT | ALT, flags), b"\x1b[49:33;4u");
        // Without the flag only the base key.
        assert_eq!(sent(&ch("A", "a", "A"), SHIFT, ALL_KEYS), b"\x1b[97;2u");
    }

    #[test]
    fn report_all_keys() {
        // keys.py "test report all keys" (0b1000).
        let a = ch("a", "a", "a");
        assert_eq!(sent(&a, NONE, ALL_KEYS), b"\x1b[97u");
        assert_eq!(sent(&repeated(a.clone()), NONE, ALL_KEYS), b"\x1b[97u");
        assert_eq!(sent(&ch("a", "a", "\x01"), CTRL, ALL_KEYS), b"\x1b[97;5u");
        assert_eq!(sent(&named(NamedKey::ArrowUp), NONE, ALL_KEYS), b"\x1b[A");
        assert_eq!(sent(&named(NamedKey::Enter), NONE, ALL_KEYS), b"\x1b[13u");
        assert_eq!(sent(&named(NamedKey::Enter), CTRL, ALL_KEYS), b"\x1b[13;5u");
        assert_eq!(sent(&named(NamedKey::Tab), NONE, ALL_KEYS), b"\x1b[9u");
        assert_eq!(sent(&named(NamedKey::Backspace), NONE, ALL_KEYS), b"\x1b[127u");
    }

    #[test]
    fn modifier_keys_under_report_all() {
        // The key's own bit is applied on press and cleared on release
        // (winit reports the modifier state after the key event).
        let lshift = at(named(NamedKey::Shift), KeyLocation::Left);
        assert_eq!(sent(&lshift, NONE, ALL_KEYS), b"\x1b[57441;2u");
        assert_eq!(sent(&released(lshift.clone()), SHIFT, ALL_KEYS | EVENT_TYPES), b"\x1b[57441;1:3u");
        let rctrl = at(named(NamedKey::Control), KeyLocation::Right);
        assert_eq!(sent(&rctrl, NONE, ALL_KEYS), b"\x1b[57448;5u");
        let lsuper = at(named(NamedKey::Super), KeyLocation::Left);
        assert_eq!(sent(&lsuper, SHIFT, ALL_KEYS), b"\x1b[57444;10u");
        assert_eq!(sent(&named(NamedKey::CapsLock), NONE, ALL_KEYS), b"\x1b[57358u");
        // Below report-all a bare modifier sends nothing, press or release.
        assert_eq!(sent(&lshift, NONE, DISAMBIGUATE), b"");
        assert_eq!(sent(&released(lshift), SHIFT, DISAMBIGUATE | EVENT_TYPES), b"");
        assert!(is_modifier_key(&rctrl));
        assert!(!is_modifier_key(&named(NamedKey::CapsLock)));
    }

    #[test]
    fn keypad_under_report_all() {
        let kp_5 = at(ch("5", "5", "5"), KeyLocation::Numpad);
        assert_eq!(sent(&kp_5, NONE, ALL_KEYS), b"\x1b[57404u");
        let kp_enter = at(named(NamedKey::Enter), KeyLocation::Numpad);
        assert_eq!(sent(&kp_enter, NONE, ALL_KEYS), b"\x1b[57414u");
        assert_eq!(sent(&kp_enter, NONE, DISAMBIGUATE), b"\x1b[57414u");
    }

    #[test]
    fn super_is_encoded() {
        // Super is bit 8 on every platform (winit's SUPER: Linux Super/Logo, macOS Cmd).
        assert_eq!(sent(&ch("a", "a", "a"), SUPER, DISAMBIGUATE), b"\x1b[97;9u");
        assert_eq!(sent(&ch("a", "a", "a"), SUPER | CTRL | ALT | SHIFT, ALL_KEYS), b"\x1b[97;16u");
    }

    #[test]
    fn associated_text() {
        // keys.py "test embed text" (0b11000).
        let flags = ALL_KEYS | TEXT;
        assert_eq!(sent(&ch("a", "a", "a"), NONE, flags), b"\x1b[97;1;97u");
        assert_eq!(sent(&ch("A", "a", "A"), SHIFT, flags), b"\x1b[97;2;65u");
        // Control characters are not text; nor is there text on release.
        assert_eq!(sent(&ch("a", "a", "\x01"), CTRL, flags), b"\x1b[97;5u");
        assert_eq!(sent(&released(ch("a", "a", "a")), NONE, flags | EVENT_TYPES), b"\x1b[97;1:3u");
        // Text with no single-character key (a compose/IME-like multi-char
        // result) is reported against key 0.
        let multi = key(Key::Character("ab".into()), Key::Character("ab".into()), Some("ab"));
        assert_eq!(sent(&multi, NONE, flags), b"\x1b[0;1;97:98u");
    }

    #[test]
    fn non_ascii_keys() {
        // Keys egui cannot name are encoded from winit's own key.
        assert_eq!(sent(&ch("é", "é", "é"), NONE, ALL_KEYS), "\x1b[233u".as_bytes());
        assert_eq!(sent(&ch("É", "é", "É"), SHIFT, ALL_KEYS | ALTERNATES), "\x1b[233:201;2u".as_bytes());
        assert_eq!(sent(&ch("с", "с", "\x03"), CTRL, DISAMBIGUATE), "\x1b[1089;5u".as_bytes());
        assert_eq!(enc(&ch("ß", "ß", "ß"), NONE, DISAMBIGUATE), None);
    }

    #[test]
    fn private_use_functional_keys() {
        assert_eq!(sent(&named(NamedKey::F13), NONE, DISAMBIGUATE), b"\x1b[57376u");
        assert_eq!(sent(&named(NamedKey::F35), CTRL, DISAMBIGUATE), b"\x1b[57398;5u");
        assert_eq!(sent(&named(NamedKey::ContextMenu), NONE, DISAMBIGUATE), b"\x1b[57363u");
        assert_eq!(sent(&named(NamedKey::AudioVolumeUp), NONE, ALL_KEYS), b"\x1b[57439u");
        assert_eq!(sent(&named(NamedKey::MediaPlayPause), NONE, DISAMBIGUATE), b"\x1b[57430u");
    }

    #[test]
    fn alt_is_masked_when_it_cannot_prefix_esc() {
        // A key producing several characters under Alt cannot carry the ESC
        // prefix: Alt is dropped and the text goes out as text.
        let multi = key(Key::Character("ab".into()), Key::Character("ab".into()), Some("ab"));
        assert_eq!(enc(&multi, ALT, DISAMBIGUATE), None);
        // A named key without text keeps Alt.
        assert_eq!(sent(&named(NamedKey::ArrowUp), ALT, DISAMBIGUATE), b"\x1b[1;3A");
        assert_eq!(sent(&named(NamedKey::Backspace), ALT, DISAMBIGUATE), b"\x1b[127;3u");
    }

    #[test]
    fn alt_is_kept_on_release() {
        // A release has no text; Alt stays when the press kept it, as kitty
        // reports the modifiers held at the event.
        let flags = DISAMBIGUATE | EVENT_TYPES;
        assert_eq!(sent(&ch("b", "b", "b"), ALT, flags), b"\x1b[98;3u");
        let release = released(key(Key::Character("b".into()), Key::Character("b".into()), None));
        assert_eq!(sent(&release, ALT, flags), b"\x1b[98;3:3u");
    }

    #[test]
    fn dead_and_unidentified_keys_send_nothing() {
        let dead = key(Key::Dead(Some('´')), Key::Dead(Some('´')), None);
        assert_eq!(sent(&dead, NONE, DISAMBIGUATE), b"");
        let unknown = key(Key::Unidentified(NativeKey::Unidentified), Key::Unidentified(NativeKey::Unidentified), None);
        assert_eq!(sent(&unknown, CTRL, ALL_KEYS), b"");
    }
}
