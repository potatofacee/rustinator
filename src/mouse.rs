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
    Motion,
}

/// Decide whether a pointer-motion event should be forwarded to the PTY.
///
/// Mirrors xterm/alacritty: MOUSE_DRAG (1002, button-event) reports motion only
/// while a button is held; MOUSE_MOTION (1003, any-event) reports motion always.
/// `cell_changed` gates spam — a terminal only cares about cell transitions, not
/// per-pixel moves, so we suppress reports that resolve to the same grid cell.
pub fn should_report_motion(
    term_mode: TermMode,
    button_held: bool,
    cell_changed: bool,
) -> bool {
    if !cell_changed {
        return false;
    }
    if term_mode.contains(TermMode::MOUSE_MOTION) {
        return true;
    }
    if term_mode.contains(TermMode::MOUSE_DRAG) {
        return button_held;
    }
    false
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
        // Legacy X10 / normal tracking. Release uses button code 3 but keeps the
        // shift/alt/ctrl modifier bits (matching xterm/alacritty: `3 + mods`).
        let cb_out = if matches!(kind, MouseKind::Release) { 3 | (cb & (4 | 8 | 16)) } else { cb };
        let cb_byte = (cb_out + 32).min(255) as u8;
        // Clamp the 1-based coordinate to 223 (223 + 32 = 255) before adding the
        // 32 offset, so positions beyond 223 saturate at the max cell rather than
        // wrapping/decoding to a wrong cell.
        let col_byte = (col1.min(223) + 32) as u8;
        let row_byte = (row1.min(223) + 32) as u8;
        Some(vec![0x1b, b'[', b'M', cb_byte, col_byte, row_byte])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr_mode() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
    }

    fn legacy_mode() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK
    }

    #[test]
    fn encode_legacy_x10_press_and_release_offsets() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        // Press Left at (0,0): cb=0, +32 offset on every byte; col1=row1=1, +32 = 33.
        let press = encode(MouseKind::Press, MouseButton::Left, 0, 0, mods, legacy_mode()).unwrap();
        assert_eq!(press, vec![0x1b, b'[', b'M', 32, 33, 33]);

        // Release forces button code 3, +32 = 35; coords unchanged.
        let release = encode(MouseKind::Release, MouseButton::Left, 0, 0, mods, legacy_mode()).unwrap();
        assert_eq!(release, vec![0x1b, b'[', b'M', 35, 33, 33]);

        // L1: a release with Alt held keeps the alt bit (3 | 8 = 11), +32 = 43,
        // rather than discarding the modifier and emitting bare 3.
        let alt = MouseMods { shift: false, alt: true, ctrl: false };
        let alt_release = encode(MouseKind::Release, MouseButton::Left, 0, 0, alt, legacy_mode()).unwrap();
        assert_eq!(alt_release, vec![0x1b, b'[', b'M', 43, 33, 33]);
    }

    #[test]
    fn encode_legacy_x10_clamps_coords_at_255() {
        let mods = MouseMods { shift: false, alt: false, ctrl: false };
        // col 300 -> col1=301, clamped to 223, +32 = 255 (the max byte).
        // row 7 -> row1=8, +32 = 40.
        let bytes = encode(MouseKind::Press, MouseButton::Left, 300, 7, mods, legacy_mode()).unwrap();
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 255, 40]);
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
    fn should_report_motion_gating() {
        let drag = TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG;
        let any = TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION;
        let click_only = TermMode::MOUSE_REPORT_CLICK;

        // Cell unchanged: never report, regardless of mode/button.
        assert!(!should_report_motion(any, true, false));
        assert!(!should_report_motion(drag, true, false));

        // DRAG (1002): only while a button is held.
        assert!(should_report_motion(drag, true, true));
        assert!(!should_report_motion(drag, false, true));

        // MOTION (1003): always, even with no button held.
        assert!(should_report_motion(any, false, true));
        assert!(should_report_motion(any, true, true));

        // Click-only / no motion mode: never report motion.
        assert!(!should_report_motion(click_only, true, true));
        assert!(!should_report_motion(TermMode::empty(), true, true));
    }

    #[test]
    fn modifiers_or_into_code() {
        let mods = MouseMods { shift: true, alt: false, ctrl: true };
        let bytes = encode(MouseKind::Press, MouseButton::Left, 0, 0, mods, sgr_mode()).unwrap();
        // Left(0) | shift(4) | ctrl(16) = 20.
        assert_eq!(bytes, b"\x1b[<20;1;1M");
    }
}
