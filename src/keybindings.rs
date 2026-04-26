//! Keybinding parsing and lookup.
//!
//! Bindings are stored in config as `[[keybindings]]` entries mapping an action
//! name to a key combo string like "Ctrl+Shift+O".

use std::collections::HashMap;

use egui;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    NewTab,
    NextTab,
    PrevTab,
    FocusNext,
    FocusPrev,
    Copy,
    Paste,
    OpenPrefs,
    ToggleZoom,
    ToggleBroadcast,
    ToggleSearch,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    CloseWindow,
    ToggleFullscreen,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    ResetTerminal,
    ResetClear,
    NewWindow,
    QuitHotkeyWindow,
}

impl Action {
    pub fn from_str(s: &str) -> Option<Action> {
        Some(match s.trim() {
            "split_horizontal" => Action::SplitHorizontal,
            "split_vertical" => Action::SplitVertical,
            "close_pane" => Action::ClosePane,
            "new_tab" => Action::NewTab,
            "next_tab" => Action::NextTab,
            "prev_tab" => Action::PrevTab,
            "focus_next" => Action::FocusNext,
            "focus_prev" => Action::FocusPrev,
            "copy" => Action::Copy,
            "paste" => Action::Paste,
            "open_prefs" => Action::OpenPrefs,
            "toggle_zoom" => Action::ToggleZoom,
            "toggle_broadcast" => Action::ToggleBroadcast,
            "toggle_search" => Action::ToggleSearch,
            "zoom_in" => Action::ZoomIn,
            "zoom_out" => Action::ZoomOut,
            "zoom_reset" => Action::ZoomReset,
            "close_window" => Action::CloseWindow,
            "toggle_fullscreen" => Action::ToggleFullscreen,
            "resize_left" => Action::ResizeLeft,
            "resize_right" => Action::ResizeRight,
            "resize_up" => Action::ResizeUp,
            "resize_down" => Action::ResizeDown,
            "reset_terminal" => Action::ResetTerminal,
            "reset_clear" => Action::ResetClear,
            "new_window" => Action::NewWindow,
            "quit_hotkey_window" => Action::QuitHotkeyWindow,
            _ => return None,
        })
    }
}

/// Hashable modifier state (egui::Modifiers isn't Hash).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct Mods {
    shift: bool,
    alt: bool,
    ctrl: bool,
    mac_cmd: bool,
}

impl Mods {
    fn from_egui(m: egui::Modifiers) -> Self {
        Self {
            shift: m.shift,
            alt: m.alt,
            ctrl: m.ctrl,
            mac_cmd: m.mac_cmd,
        }
    }
}

pub struct BindingTable {
    map: HashMap<(Mods, egui::Key), Action>,
}

impl BindingTable {
    pub fn new() -> Self {
        let mut t = Self { map: HashMap::new() };
        for (combo, action) in defaults() {
            if let Some(parsed) = parse_combo(combo) {
                t.map.insert(parsed, action);
            }
        }
        t
    }

    /// Override / extend the table with user bindings. Unknown actions or unparseable
    /// combos are logged and skipped.
    pub fn apply_user(&mut self, entries: &[(String, String)]) {
        for (action_str, combo) in entries {
            let Some(action) = Action::from_str(action_str) else {
                eprintln!("keybinding: unknown action '{}'", action_str);
                continue;
            };
            let Some(parsed) = parse_combo(combo) else {
                eprintln!("keybinding: can't parse combo '{}'", combo);
                continue;
            };
            self.map.insert(parsed, action);
        }
    }

    pub fn lookup(&self, key: egui::Key, modifiers: egui::Modifiers) -> Option<Action> {
        self.map.get(&(Mods::from_egui(modifiers), key)).copied()
    }
}

fn defaults() -> Vec<(&'static str, Action)> {
    vec![
        ("Ctrl+Shift+O", Action::SplitHorizontal),
        ("Ctrl+Shift+E", Action::SplitVertical),
        ("Ctrl+Shift+W", Action::ClosePane),
        ("Ctrl+Shift+T", Action::NewTab),
        ("Ctrl+PageDown", Action::NextTab),
        ("Ctrl+PageUp", Action::PrevTab),
        ("Ctrl+Tab", Action::FocusNext),
        ("Alt+Right", Action::FocusNext),
        ("Alt+Down", Action::FocusNext),
        ("Ctrl+Shift+Tab", Action::FocusPrev),
        ("Alt+Left", Action::FocusPrev),
        ("Alt+Up", Action::FocusPrev),
        ("Ctrl+Shift+C", Action::Copy),
        ("Ctrl+Shift+V", Action::Paste),
        ("Ctrl+Comma", Action::OpenPrefs),
        ("Ctrl+Shift+X", Action::ToggleZoom),
        ("Ctrl+Shift+B", Action::ToggleBroadcast),
        ("Ctrl+F", Action::ToggleSearch),
        ("Ctrl+Shift+F", Action::ToggleSearch),
        ("Ctrl+Equals", Action::ZoomIn),
        ("Ctrl+Shift+Equals", Action::ZoomIn),
        ("Ctrl+Minus", Action::ZoomOut),
        ("Ctrl+0", Action::ZoomReset),
        ("Ctrl+Shift+Q", Action::CloseWindow),
        ("F11", Action::ToggleFullscreen),
        ("Ctrl+Shift+Left", Action::ResizeLeft),
        ("Ctrl+Shift+Right", Action::ResizeRight),
        ("Ctrl+Shift+Up", Action::ResizeUp),
        ("Ctrl+Shift+Down", Action::ResizeDown),
        ("Ctrl+Shift+R", Action::ResetTerminal),
        ("Ctrl+Shift+G", Action::ResetClear),
        ("Ctrl+Shift+I", Action::NewWindow),
    ]
}

fn parse_combo(s: &str) -> Option<(Mods, egui::Key)> {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let mut mods = Mods { shift: false, alt: false, ctrl: false, mac_cmd: false };
    for part in &parts[..parts.len() - 1] {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" | "meta" => mods.alt = true,
            "super" | "cmd" | "command" => mods.mac_cmd = true,
            _ => return None,
        }
    }
    let key = parse_key(parts[parts.len() - 1])?;
    Some((mods, key))
}

fn parse_key(s: &str) -> Option<egui::Key> {
    use egui::Key;
    Some(match s.to_ascii_lowercase().as_str() {
        "a" => Key::A, "b" => Key::B, "c" => Key::C, "d" => Key::D,
        "e" => Key::E, "f" => Key::F, "g" => Key::G, "h" => Key::H,
        "i" => Key::I, "j" => Key::J, "k" => Key::K, "l" => Key::L,
        "m" => Key::M, "n" => Key::N, "o" => Key::O, "p" => Key::P,
        "q" => Key::Q, "r" => Key::R, "s" => Key::S, "t" => Key::T,
        "u" => Key::U, "v" => Key::V, "w" => Key::W, "x" => Key::X,
        "y" => Key::Y, "z" => Key::Z,
        "0" => Key::Num0, "1" => Key::Num1, "2" => Key::Num2, "3" => Key::Num3,
        "4" => Key::Num4, "5" => Key::Num5, "6" => Key::Num6, "7" => Key::Num7,
        "8" => Key::Num8, "9" => Key::Num9,
        "tab" => Key::Tab,
        "enter" | "return" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "space" => Key::Space,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" | "arrowup" => Key::ArrowUp,
        "down" | "arrowdown" => Key::ArrowDown,
        "left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrowright" => Key::ArrowRight,
        "comma" | "," => Key::Comma,
        "period" | "." => Key::Period,
        "semicolon" | ";" => Key::Semicolon,
        "slash" | "/" => Key::Slash,
        "backslash" | "\\" => Key::Backslash,
        "minus" | "-" => Key::Minus,
        "equals" | "=" => Key::Equals,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_combo() {
        let (mods, key) = parse_combo("Ctrl+Shift+O").unwrap();
        assert!(mods.ctrl && mods.shift && !mods.alt);
        assert_eq!(key, egui::Key::O);
    }

    #[test]
    fn parse_case_insensitive() {
        let (mods, key) = parse_combo("ctrl+tab").unwrap();
        assert!(mods.ctrl);
        assert_eq!(key, egui::Key::Tab);
    }

    #[test]
    fn unknown_modifier_rejected() {
        assert!(parse_combo("Hyper+A").is_none());
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(parse_combo("Ctrl+UnknownKey").is_none());
    }

    #[test]
    fn defaults_all_parse() {
        for (combo, _) in defaults() {
            assert!(parse_combo(combo).is_some(), "failed: {combo}");
        }
    }

    #[test]
    fn lookup_matches_default() {
        let table = BindingTable::new();
        let mods = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            table.lookup(egui::Key::O, mods),
            Some(Action::SplitHorizontal)
        );
    }

    #[test]
    fn user_override_replaces_default() {
        let mut table = BindingTable::new();
        table.apply_user(&[("split_horizontal".into(), "Ctrl+Alt+H".into())]);
        let mods = egui::Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            table.lookup(egui::Key::H, mods),
            Some(Action::SplitHorizontal)
        );
    }

    // ── Gap inventory guardrails ──────────────────────────────────────
    // Each #[ignore] test validates that a new Action variant exists and
    // its string parses. Remove #[ignore] once implemented.

    // Gap #9: pane rotation
    #[test]
    #[ignore = "gap #9: Action::RotateCW not yet implemented"]
    fn action_rotate_cw() {
        assert!(Action::from_str("rotate_cw").is_some());
    }

    #[test]
    #[ignore = "gap #9: Action::RotateCCW not yet implemented"]
    fn action_rotate_ccw() {
        assert!(Action::from_str("rotate_ccw").is_some());
    }

    // Gap #29: tab reorder by keyboard
    #[test]
    #[ignore = "gap #29: Action::MoveTabLeft not yet implemented"]
    fn action_move_tab_left() {
        assert!(Action::from_str("move_tab_left").is_some());
    }

    #[test]
    #[ignore = "gap #29: Action::MoveTabRight not yet implemented"]
    fn action_move_tab_right() {
        assert!(Action::from_str("move_tab_right").is_some());
    }

    // Gap #30: direct tab switching (1-10)
    #[test]
    #[ignore = "gap #30: Action::SwitchToTab1..10 not yet implemented"]
    fn action_switch_to_tab_by_number() {
        assert!(Action::from_str("switch_to_tab_1").is_some());
        assert!(Action::from_str("switch_to_tab_5").is_some());
        assert!(Action::from_str("switch_to_tab_10").is_some());
    }

    // Gap #15: scaled zoom (distinct from maximize)
    #[test]
    #[ignore = "gap #15: Action::ScaledZoom not yet implemented"]
    fn action_scaled_zoom() {
        assert!(Action::from_str("scaled_zoom").is_some());
    }

    // Gap #46: focus next/prev terminal (Ctrl+Shift+N/P)
    #[test]
    #[ignore = "gap #46: Action::FocusNextTerminal not yet implemented"]
    fn action_focus_next_terminal() {
        assert!(Action::from_str("focus_next_terminal").is_some());
    }

    #[test]
    #[ignore = "gap #46: Action::FocusPrevTerminal not yet implemented"]
    fn action_focus_prev_terminal() {
        assert!(Action::from_str("focus_prev_terminal").is_some());
    }

    // Gap #25: toggle scrollbar
    #[test]
    #[ignore = "gap #25: Action::ToggleScrollbar not yet implemented"]
    fn action_toggle_scrollbar() {
        assert!(Action::from_str("toggle_scrollbar").is_some());
    }

    // Gap #27: insert terminal number
    #[test]
    #[ignore = "gap #27: Action::InsertTermNumber not yet implemented"]
    fn action_insert_term_number() {
        assert!(Action::from_str("insert_term_number").is_some());
    }

    // Gap #17: global hide/show hotkey
    #[test]
    #[ignore = "gap #17: Action::ToggleWindowVisibility not yet implemented"]
    fn action_toggle_window_visibility() {
        assert!(Action::from_str("toggle_visibility").is_some());
    }

    // Gap #13: broadcast scopes (all/group/off)
    #[test]
    #[ignore = "gap #13: Action::BroadcastAll/Group/Off not yet implemented"]
    fn action_broadcast_scopes() {
        assert!(Action::from_str("broadcast_all").is_some());
        assert!(Action::from_str("broadcast_group").is_some());
        assert!(Action::from_str("broadcast_off").is_some());
    }

    // Gap #13: default bindings for broadcast scopes
    #[test]
    #[ignore = "gap #13: default broadcast scope keybindings not yet added"]
    fn defaults_include_broadcast_scopes() {
        let defs = defaults();
        let actions: Vec<_> = defs.iter().map(|(_, a)| a).collect();
        assert!(actions.contains(&&Action::from_str("broadcast_all").unwrap()));
    }

    // Gap #9: default bindings for rotation
    #[test]
    #[ignore = "gap #9: default rotation keybindings not yet added"]
    fn defaults_include_rotation() {
        let table = BindingTable::new();
        let mods = egui::Modifiers {
            mac_cmd: true,
            ..Default::default()
        };
        assert!(table.lookup(egui::Key::R, mods).is_some());
    }
}
