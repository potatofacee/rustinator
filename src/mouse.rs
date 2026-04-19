//! Mouse event encoding for terminal mouse-reporting modes.

use alacritty_terminal::term::TermMode;

#[derive(Copy, Clone, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    fn base_code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum MouseKind {
    Press,
    Release,
    #[allow(dead_code)] // Reserved for drag-motion forwarding.
    Motion,
}

#[derive(Copy, Clone, Debug)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Encode a mouse event as an escape sequence, if the term has any mouse mode active
/// and the event's kind is supported by the current modes.
pub fn encode(
    kind: MouseKind,
    button: MouseButton,
    col: i32,
    row: i32,
    mods: MouseMods,
    term_mode: TermMode,
) -> Option<Vec<u8>> {
    if !term_mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }

    // Motion reporting is only enabled when MOUSE_MOTION (any-event) or MOUSE_DRAG
    // (button-event) modes are on.
    if matches!(kind, MouseKind::Motion)
        && !term_mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
    {
        return None;
    }

    let col1 = (col + 1).max(1);
    let row1 = (row + 1).max(1);

    let mut cb = button.base_code();
    if matches!(kind, MouseKind::Motion) {
        cb |= 32;
    }
    if mods.shift {
        cb |= 4;
    }
    if mods.alt {
        cb |= 8;
    }
    if mods.ctrl {
        cb |= 16;
    }

    if term_mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if matches!(kind, MouseKind::Release) { 'm' } else { 'M' };
        Some(format!("\x1b[<{};{};{}{}", cb, col1, row1, suffix).into_bytes())
    } else {
        // Legacy X10 / normal tracking. Release uses button code 3.
        let cb_out = if matches!(kind, MouseKind::Release) { 3 } else { cb };
        let cb_byte = (cb_out + 32).min(255) as u8;
        let col_byte = (col1 + 32).min(255) as u8;
        let row_byte = (row1 + 32).min(255) as u8;
        Some(vec![0x1b, b'[', b'M', cb_byte, col_byte, row_byte])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr_mode() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
    }

    #[test]
    fn no_mouse_mode_returns_none() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        assert!(encode(MouseKind::Press, MouseButton::Left, 0, 0, mods, TermMode::empty()).is_none());
    }

    #[test]
    fn sgr_press_left() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        let bytes = encode(MouseKind::Press, MouseButton::Left, 4, 2, mods, sgr_mode()).unwrap();
        assert_eq!(bytes, b"\x1b[<0;5;3M");
    }

    #[test]
    fn sgr_release_uses_lowercase_m() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        let bytes = encode(MouseKind::Release, MouseButton::Left, 4, 2, mods, sgr_mode()).unwrap();
        assert_eq!(bytes, b"\x1b[<0;5;3m");
    }

    #[test]
    fn wheel_up_has_code_64() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        let bytes = encode(MouseKind::Press, MouseButton::WheelUp, 0, 0, mods, sgr_mode()).unwrap();
        assert_eq!(bytes, b"\x1b[<64;1;1M");
    }

    #[test]
    fn motion_requires_motion_mode() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        assert!(encode(MouseKind::Motion, MouseButton::Left, 0, 0, mods, sgr_mode()).is_none());
        let mode = sgr_mode() | TermMode::MOUSE_DRAG;
        let bytes = encode(MouseKind::Motion, MouseButton::Left, 0, 0, mods, mode).unwrap();
        // Motion adds 32 to base code, so Left+motion = 32.
        assert_eq!(bytes, b"\x1b[<32;1;1M");
    }

    #[test]
    fn modifiers_or_into_code() {
        let mods = MouseMods { shift: true, alt: false, ctrl: true };
        let bytes = encode(MouseKind::Press, MouseButton::Left, 0, 0, mods, sgr_mode()).unwrap();
        // Left(0) | shift(4) | ctrl(16) = 20.
        assert_eq!(bytes, b"\x1b[<20;1;1M");
    }
}
