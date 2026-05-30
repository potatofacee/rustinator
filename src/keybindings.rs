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
    MoveTabLeft,
    MoveTabRight,
    SwitchToTab(u8),
    GoUp,
    GoDown,
    GoLeft,
    GoRight,
    GoNext,
    GoPrev,
    RotateCW,
    RotateCCW,
    SplitAuto,
    ToggleScrollbar,
    HideWindow,
    ToggleReadOnly,
    SetTitle,
    OpenTerminalHere,
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
            "move_tab_left" => Action::MoveTabLeft,
            "move_tab_right" => Action::MoveTabRight,
            "switch_to_tab_1" => Action::SwitchToTab(1),
            "switch_to_tab_2" => Action::SwitchToTab(2),
            "switch_to_tab_3" => Action::SwitchToTab(3),
            "switch_to_tab_4" => Action::SwitchToTab(4),
            "switch_to_tab_5" => Action::SwitchToTab(5),
            "switch_to_tab_6" => Action::SwitchToTab(6),
            "switch_to_tab_7" => Action::SwitchToTab(7),
            "switch_to_tab_8" => Action::SwitchToTab(8),
            "switch_to_tab_9" => Action::SwitchToTab(9),
            "switch_to_tab_10" => Action::SwitchToTab(10),
            "go_up" => Action::GoUp,
            "go_down" => Action::GoDown,
            "go_left" => Action::GoLeft,
            "go_right" => Action::GoRight,
            "go_next" => Action::GoNext,
            "go_prev" => Action::GoPrev,
            "rotate_cw" => Action::RotateCW,
            "rotate_ccw" => Action::RotateCCW,
            "split_auto" => Action::SplitAuto,
            "toggle_scrollbar" => Action::ToggleScrollbar,
            "hide_window" => Action::HideWindow,
            "toggle_read_only" => Action::ToggleReadOnly,
            "set_title" => Action::SetTitle,
            "open_terminal_here" => Action::OpenTerminalHere,
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
    pub fn new(force_linux: bool) -> Self {
        let mut t = Self { map: HashMap::new() };
        for (combo, action) in defaults_for_platform(force_linux) {
            if let Some(parsed) = parse_combo(&combo) {
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

pub fn defaults_for_platform(force_linux: bool) -> Vec<(String, Action)> {
    if cfg!(target_os = "macos") && !force_linux {
        macos_defaults()
    } else {
        linux_defaults()
    }
}

fn macos_defaults() -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = linux_defaults()
        .into_iter()
        .filter(|(combo, _)| !combo.starts_with("Super+"))
        .map(|(combo, action)| (ctrl_to_cmd(&combo), action))
        .collect();
    v.push(("Cmd+C".into(), Action::Copy));
    v.push(("Cmd+V".into(), Action::Paste));
    v
}

/// Convert a Linux/Windows-style combo to its macOS equivalent by mapping the
/// `Ctrl` modifier to `Cmd` at the *structured* level rather than via string
/// substitution.
///
/// We tokenize the combo on `+`, identify which tokens are modifiers (matching
/// the same vocabulary `parse_combo` accepts), and rewrite only a `Ctrl`/
/// `Control` modifier token to `Cmd`. The final (non-modifier) token is the key
/// and is never touched — so a key whose name happens to contain "Ctrl" cannot
/// be mangled, and multi-modifier combos (e.g. `Ctrl+Shift+T`, `Ctrl+Shift+Alt+A`)
/// convert correctly because each modifier is handled independently.
///
/// If the combo doesn't parse into a recognizable shape, it is returned
/// unchanged so it can later be reported by `parse_combo`'s error path.
fn ctrl_to_cmd(combo: &str) -> String {
    let parts: Vec<&str> = combo
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return combo.to_string();
    }
    let last = parts.len() - 1;
    let converted: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            // Only the modifier slots (everything before the final key token)
            // are candidates for remapping.
            if i != last && is_ctrl_modifier(part) {
                "Cmd".to_string()
            } else {
                part.to_string()
            }
        })
        .collect();
    converted.join("+")
}

fn is_ctrl_modifier(token: &str) -> bool {
    matches!(token.to_ascii_lowercase().as_str(), "ctrl" | "control")
}

fn linux_defaults() -> Vec<(String, Action)> {
    vec![
        // ── Creation & destruction ───────────────────────────────────
        ("Ctrl+Shift+O".into(), Action::SplitHorizontal),   // split_horiz
        ("Ctrl+Shift+E".into(), Action::SplitVertical),      // split_vert
        ("Ctrl+Shift+A".into(), Action::SplitAuto),
        ("Ctrl+Shift+W".into(), Action::ClosePane),          // close_term
        ("Ctrl+Shift+Q".into(), Action::CloseWindow),        // close_window
        ("Ctrl+Shift+T".into(), Action::NewTab),             // new_tab
        ("Ctrl+Shift+I".into(), Action::NewWindow),          // new_window
        // ("Super+I", Action::NewTerminator),         // new_terminator — not implemented
        // ("Alt+L", Action::LayoutLauncher),          // layout_launcher — not implemented

        // ── Navigation (focus) ───────────────────────────────────────
        ("Ctrl+Tab".into(), Action::FocusNext),              // cycle_next
        ("Ctrl+Shift+Tab".into(), Action::FocusPrev),        // cycle_prev
        ("Ctrl+Shift+N".into(), Action::GoNext),             // go_next
        ("Ctrl+Shift+P".into(), Action::GoPrev),             // go_prev
        ("Alt+Up".into(), Action::GoUp),                     // go_up
        ("Alt+Down".into(), Action::GoDown),                 // go_down
        ("Alt+Left".into(), Action::GoLeft),                 // go_left
        ("Alt+Right".into(), Action::GoRight),               // go_right

        // ── Tab management ───────────────────────────────────────────
        ("Ctrl+PageDown".into(), Action::NextTab),           // next_tab
        ("Ctrl+PageUp".into(), Action::PrevTab),             // prev_tab
        ("Ctrl+Shift+PageDown".into(), Action::MoveTabRight),
        ("Ctrl+Shift+PageUp".into(), Action::MoveTabLeft),
        // switch_to_tab_1..10 — unbound by default in Terminator

        // ── Resize ───────────────────────────────────────────────────
        ("Ctrl+Shift+Up".into(), Action::ResizeUp),          // resize_up
        ("Ctrl+Shift+Down".into(), Action::ResizeDown),      // resize_down
        ("Ctrl+Shift+Left".into(), Action::ResizeLeft),      // resize_left
        ("Ctrl+Shift+Right".into(), Action::ResizeRight),    // resize_right
        ("Super+R".into(), Action::RotateCW),
        ("Super+Shift+R".into(), Action::RotateCCW),

        // ── Zoom & fullscreen ────────────────────────────────────────
        ("F11".into(), Action::ToggleFullscreen),             // full_screen
        ("Ctrl+Shift+X".into(), Action::ToggleZoom),          // toggle_zoom
        // ("Ctrl+Shift+Z", Action::ScaledZoom),       // scaled_zoom — not implemented
        ("Ctrl+Shift+Alt+A".into(), Action::HideWindow),
        ("Ctrl+Equals".into(), Action::ZoomIn),               // zoom_in (Ctrl+Plus)
        ("Ctrl+Shift+Equals".into(), Action::ZoomIn),         // zoom_in (shifted = literal +)
        ("Ctrl+Minus".into(), Action::ZoomOut),                // zoom_out
        ("Ctrl+0".into(), Action::ZoomReset),                  // zoom_normal
        // ("", Action::ZoomInAll),                     // zoom_in_all — not implemented
        // ("", Action::ZoomOutAll),                    // zoom_out_all — not implemented
        // ("", Action::ZoomResetAll),                  // zoom_normal_all — not implemented

        // ── Clipboard ────────────────────────────────────────────────
        ("Ctrl+Shift+C".into(), Action::Copy),                // copy
        ("Ctrl+Shift+V".into(), Action::Paste),               // paste
        // ("", Action::PasteSelection),               // paste_selection — not implemented

        // ── Search ───────────────────────────────────────────────────
        ("Ctrl+Shift+F".into(), Action::ToggleSearch),        // search

        // ── Terminal reset ───────────────────────────────────────────
        ("Ctrl+Shift+R".into(), Action::ResetTerminal),       // reset
        ("Ctrl+Shift+G".into(), Action::ResetClear),          // reset_clear

        // ── Scrollbar & profiles ─────────────────────────────────────
        ("Ctrl+Shift+S".into(), Action::ToggleScrollbar),
        // ("", Action::NextProfile),                   // next_profile — not implemented
        // ("", Action::PreviousProfile),               // previous_profile — not implemented

        // ── Grouping & broadcasting ──────────────────────────────────
        ("Ctrl+Shift+B".into(), Action::ToggleBroadcast),     // (rustinator toggle — Terminator uses separate scopes)
        // ("Super+G", Action::GroupAll),               // group_all — not implemented
        // ("Super+Shift+G", Action::UngroupAll),       // ungroup_all — not implemented
        // ("Super+T", Action::GroupTab),               // group_tab — not implemented
        // ("Super+Shift+T", Action::UngroupTab),       // ungroup_tab — not implemented
        // ("Super+Shift+W", Action::UngroupWin),       // ungroup_win — not implemented
        // ("", Action::BroadcastOff),                  // broadcast_off — not implemented
        // ("", Action::BroadcastGroup),                // broadcast_group — not implemented
        // ("", Action::BroadcastAll),                  // broadcast_all — not implemented

        // ── Title editing ────────────────────────────────────────────
        // ("Ctrl+Alt+W", Action::EditWindowTitle),     // edit_window_title — not implemented
        // ("Ctrl+Alt+A", Action::EditTabTitle),        // edit_tab_title — not implemented
        // ("Ctrl+Alt+X", Action::EditTerminalTitle),   // edit_terminal_title — not implemented

        // ── Terminal index insert ────────────────────────────────────
        // ("Super+1", Action::InsertNumber),           // insert_number — not implemented
        // ("Super+0", Action::InsertPadded),           // insert_padded — not implemented

        // ── Preferences & help ───────────────────────────────────────
        // ("", Action::OpenPrefs),                     // preferences — unbound in Terminator
        // ("Ctrl+Shift+K", Action::PrefsKeybindings),  // preferences_keybindings — not implemented
        // ("F1", Action::Help),                        // help — not implemented
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
    fn linux_defaults_all_parse() {
        for (combo, _) in linux_defaults() {
            assert!(parse_combo(&combo).is_some(), "failed: {combo}");
        }
    }

    #[test]
    fn macos_defaults_all_parse() {
        for (combo, _) in macos_defaults() {
            assert!(parse_combo(&combo).is_some(), "failed: {combo}");
        }
    }

    #[test]
    fn user_override_replaces_default() {
        let mut table = BindingTable::new(true);
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

    #[test]
    fn force_linux_on_any_platform() {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::O, mods), Some(Action::SplitHorizontal));
    }

    // ── Linux per-binding coverage ───────────────────────────────────
    // All use force_linux=true so they pass on any platform.

    fn linux_ctrl_shift(key: egui::Key) -> Option<Action> {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        table.lookup(key, mods)
    }

    fn linux_ctrl_only(key: egui::Key) -> Option<Action> {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { ctrl: true, ..Default::default() };
        table.lookup(key, mods)
    }

    fn linux_no_mods(key: egui::Key) -> Option<Action> {
        let table = BindingTable::new(true);
        table.lookup(key, egui::Modifiers::default())
    }

    #[test]
    fn binding_ctrl_shift_o_split_horizontal() {
        assert_eq!(linux_ctrl_shift(egui::Key::O), Some(Action::SplitHorizontal));
    }

    #[test]
    fn binding_ctrl_shift_e_split_vertical() {
        assert_eq!(linux_ctrl_shift(egui::Key::E), Some(Action::SplitVertical));
    }

    #[test]
    fn binding_ctrl_shift_w_close_pane() {
        assert_eq!(linux_ctrl_shift(egui::Key::W), Some(Action::ClosePane));
    }

    #[test]
    fn binding_ctrl_shift_q_close_window() {
        assert_eq!(linux_ctrl_shift(egui::Key::Q), Some(Action::CloseWindow));
    }

    #[test]
    fn binding_ctrl_shift_t_new_tab() {
        assert_eq!(linux_ctrl_shift(egui::Key::T), Some(Action::NewTab));
    }

    #[test]
    fn binding_ctrl_shift_i_new_window() {
        assert_eq!(linux_ctrl_shift(egui::Key::I), Some(Action::NewWindow));
    }

    #[test]
    fn binding_ctrl_tab_focus_next() {
        assert_eq!(linux_ctrl_only(egui::Key::Tab), Some(Action::FocusNext));
    }

    #[test]
    fn binding_ctrl_shift_tab_focus_prev() {
        assert_eq!(linux_ctrl_shift(egui::Key::Tab), Some(Action::FocusPrev));
    }

    #[test]
    fn binding_ctrl_pagedown_next_tab() {
        assert_eq!(linux_ctrl_only(egui::Key::PageDown), Some(Action::NextTab));
    }

    #[test]
    fn binding_ctrl_pageup_prev_tab() {
        assert_eq!(linux_ctrl_only(egui::Key::PageUp), Some(Action::PrevTab));
    }

    #[test]
    fn binding_ctrl_shift_up_resize_up() {
        assert_eq!(linux_ctrl_shift(egui::Key::ArrowUp), Some(Action::ResizeUp));
    }

    #[test]
    fn binding_ctrl_shift_down_resize_down() {
        assert_eq!(linux_ctrl_shift(egui::Key::ArrowDown), Some(Action::ResizeDown));
    }

    #[test]
    fn binding_ctrl_shift_left_resize_left() {
        assert_eq!(linux_ctrl_shift(egui::Key::ArrowLeft), Some(Action::ResizeLeft));
    }

    #[test]
    fn binding_ctrl_shift_right_resize_right() {
        assert_eq!(linux_ctrl_shift(egui::Key::ArrowRight), Some(Action::ResizeRight));
    }

    #[test]
    fn binding_f11_toggle_fullscreen() {
        assert_eq!(linux_no_mods(egui::Key::F11), Some(Action::ToggleFullscreen));
    }

    #[test]
    fn binding_ctrl_shift_x_toggle_zoom() {
        assert_eq!(linux_ctrl_shift(egui::Key::X), Some(Action::ToggleZoom));
    }

    #[test]
    fn binding_ctrl_equals_zoom_in() {
        assert_eq!(linux_ctrl_only(egui::Key::Equals), Some(Action::ZoomIn));
    }

    #[test]
    fn binding_ctrl_shift_equals_zoom_in() {
        assert_eq!(linux_ctrl_shift(egui::Key::Equals), Some(Action::ZoomIn));
    }

    #[test]
    fn binding_ctrl_minus_zoom_out() {
        assert_eq!(linux_ctrl_only(egui::Key::Minus), Some(Action::ZoomOut));
    }

    #[test]
    fn binding_ctrl_0_zoom_reset() {
        assert_eq!(linux_ctrl_only(egui::Key::Num0), Some(Action::ZoomReset));
    }

    #[test]
    fn binding_ctrl_shift_c_copy() {
        assert_eq!(linux_ctrl_shift(egui::Key::C), Some(Action::Copy));
    }

    #[test]
    fn binding_ctrl_shift_v_paste() {
        assert_eq!(linux_ctrl_shift(egui::Key::V), Some(Action::Paste));
    }

    #[test]
    fn binding_ctrl_shift_f_toggle_search() {
        assert_eq!(linux_ctrl_shift(egui::Key::F), Some(Action::ToggleSearch));
    }

    #[test]
    fn binding_ctrl_shift_r_reset_terminal() {
        assert_eq!(linux_ctrl_shift(egui::Key::R), Some(Action::ResetTerminal));
    }

    #[test]
    fn binding_ctrl_shift_g_reset_clear() {
        assert_eq!(linux_ctrl_shift(egui::Key::G), Some(Action::ResetClear));
    }

    #[test]
    fn binding_ctrl_shift_b_toggle_broadcast() {
        assert_eq!(linux_ctrl_shift(egui::Key::B), Some(Action::ToggleBroadcast));
    }

    // ── macOS per-binding coverage ──────────────────────────────────

    fn mac_table() -> BindingTable {
        let mut t = BindingTable { map: HashMap::new() };
        for (combo, action) in macos_defaults() {
            if let Some(parsed) = parse_combo(&combo) {
                t.map.insert(parsed, action);
            }
        }
        t
    }

    fn mac_cmd(key: egui::Key) -> Option<Action> {
        let mods = egui::Modifiers { mac_cmd: true, ..Default::default() };
        mac_table().lookup(key, mods)
    }

    fn mac_cmd_shift(key: egui::Key) -> Option<Action> {
        let mods = egui::Modifiers { mac_cmd: true, shift: true, ..Default::default() };
        mac_table().lookup(key, mods)
    }

    #[test]
    fn mac_cmd_shift_o_split_horizontal() {
        assert_eq!(mac_cmd_shift(egui::Key::O), Some(Action::SplitHorizontal));
    }

    #[test]
    fn mac_cmd_shift_e_split_vertical() {
        assert_eq!(mac_cmd_shift(egui::Key::E), Some(Action::SplitVertical));
    }

    #[test]
    fn mac_cmd_shift_w_close_pane() {
        assert_eq!(mac_cmd_shift(egui::Key::W), Some(Action::ClosePane));
    }

    #[test]
    fn mac_cmd_shift_q_close_window() {
        assert_eq!(mac_cmd_shift(egui::Key::Q), Some(Action::CloseWindow));
    }

    #[test]
    fn mac_cmd_shift_t_new_tab() {
        assert_eq!(mac_cmd_shift(egui::Key::T), Some(Action::NewTab));
    }

    #[test]
    fn mac_cmd_shift_i_new_window() {
        assert_eq!(mac_cmd_shift(egui::Key::I), Some(Action::NewWindow));
    }

    #[test]
    fn mac_cmd_shift_c_copy() {
        assert_eq!(mac_cmd_shift(egui::Key::C), Some(Action::Copy));
    }

    #[test]
    fn mac_cmd_shift_v_paste() {
        assert_eq!(mac_cmd_shift(egui::Key::V), Some(Action::Paste));
    }

    #[test]
    fn mac_cmd_shift_f_search() {
        assert_eq!(mac_cmd_shift(egui::Key::F), Some(Action::ToggleSearch));
    }

    #[test]
    fn mac_cmd_equals_zoom_in() {
        assert_eq!(mac_cmd(egui::Key::Equals), Some(Action::ZoomIn));
    }

    #[test]
    fn mac_cmd_minus_zoom_out() {
        assert_eq!(mac_cmd(egui::Key::Minus), Some(Action::ZoomOut));
    }

    #[test]
    fn mac_cmd_0_zoom_reset() {
        assert_eq!(mac_cmd(egui::Key::Num0), Some(Action::ZoomReset));
    }

    #[test]
    fn mac_cmd_tab_focus_next() {
        assert_eq!(mac_cmd(egui::Key::Tab), Some(Action::FocusNext));
    }

    // ── Ctrl→Cmd structured conversion ───────────────────────────────

    #[test]
    fn ctrl_to_cmd_single_modifier() {
        assert_eq!(ctrl_to_cmd("Ctrl+C"), "Cmd+C");
    }

    #[test]
    fn ctrl_to_cmd_multi_modifier() {
        // The flagged failure mode: multi-modifier combos must convert the
        // Ctrl modifier while leaving every other modifier and the key intact.
        assert_eq!(ctrl_to_cmd("Ctrl+Shift+T"), "Cmd+Shift+T");
        assert_eq!(ctrl_to_cmd("Ctrl+Alt+X"), "Cmd+Alt+X");
        assert_eq!(ctrl_to_cmd("Ctrl+Shift+Alt+A"), "Cmd+Shift+Alt+A");
    }

    #[test]
    fn ctrl_to_cmd_control_spelling() {
        assert_eq!(ctrl_to_cmd("Control+Shift+T"), "Cmd+Shift+T");
    }

    #[test]
    fn ctrl_to_cmd_case_insensitive_modifier() {
        assert_eq!(ctrl_to_cmd("ctrl+shift+t"), "Cmd+shift+t");
    }

    #[test]
    fn ctrl_to_cmd_leaves_non_ctrl_combos_untouched() {
        assert_eq!(ctrl_to_cmd("Alt+Up"), "Alt+Up");
        assert_eq!(ctrl_to_cmd("F11"), "F11");
        assert_eq!(ctrl_to_cmd("Shift+Tab"), "Shift+Tab");
    }

    #[test]
    fn ctrl_to_cmd_does_not_mangle_key_token() {
        // The key is always the final token and must never be remapped, even if
        // its (hypothetical) name contained the substring "ctrl". This guards
        // against the substring-replacement bug that motivated the refactor.
        assert_eq!(ctrl_to_cmd("Shift+Ctrl"), "Shift+Ctrl");
        assert_eq!(ctrl_to_cmd("Ctrl"), "Ctrl");
    }

    #[test]
    fn ctrl_to_cmd_result_parses_to_cmd_modifier() {
        // End-to-end: a converted multi-modifier combo parses into the expected
        // structured modifiers (mac_cmd set, ctrl cleared, shift preserved).
        let (mods, key) = parse_combo(&ctrl_to_cmd("Ctrl+Shift+T")).unwrap();
        assert!(mods.mac_cmd && mods.shift && !mods.ctrl && !mods.alt);
        assert_eq!(key, egui::Key::T);
    }

    #[test]
    fn mac_multi_modifier_combo_looks_up_via_cmd() {
        // The real default Ctrl+Shift+Alt+A (HideWindow) must be reachable on
        // macOS via Cmd+Shift+Alt+A.
        let mods = egui::Modifiers {
            mac_cmd: true,
            shift: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(mac_table().lookup(egui::Key::A, mods), Some(Action::HideWindow));
    }

    #[test]
    fn mac_is_linux_with_ctrl_swapped_to_cmd() {
        let linux_bindings = linux_defaults();
        let mac_bindings = macos_defaults();
        let super_only = linux_bindings
            .iter()
            .filter(|(combo, _)| combo.starts_with("Super+"))
            .count();
        let mac_extras = 2; // Cmd+C (Copy) and Cmd+V (Paste) without Shift
        assert_eq!(linux_bindings.len() - super_only + mac_extras, mac_bindings.len());
    }

    // ── Gap inventory guardrails ──────────────────────────────────────
    // Each #[ignore] test validates that a new Action variant exists and
    // its string parses. Remove #[ignore] once implemented.

    // Gap #9: pane rotation
    #[test]
    fn action_rotate_cw() {
        assert!(Action::from_str("rotate_cw").is_some());
    }

    #[test]
    fn action_rotate_ccw() {
        assert!(Action::from_str("rotate_ccw").is_some());
    }

    // Gap #29: tab reorder by keyboard
    #[test]
    fn action_move_tab_left() {
        assert!(Action::from_str("move_tab_left").is_some());
    }

    #[test]
    fn action_move_tab_right() {
        assert!(Action::from_str("move_tab_right").is_some());
    }

    // Gap #30: direct tab switching (1-10)
    #[test]
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

    #[test]
    fn action_go_next() {
        assert!(Action::from_str("go_next").is_some());
    }

    #[test]
    fn action_go_prev() {
        assert!(Action::from_str("go_prev").is_some());
    }

    #[test]
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
        let defs = linux_defaults();
        let actions: Vec<_> = defs.iter().map(|(_, a)| a).collect();
        assert!(actions.contains(&&Action::from_str("broadcast_all").unwrap()));
    }

    // Gap #9: default bindings for rotation
    #[test]
    fn defaults_include_rotation() {
        let defs = linux_defaults();
        assert!(defs.iter().any(|(_, a)| matches!(a, Action::RotateCW)));
        assert!(defs.iter().any(|(_, a)| matches!(a, Action::RotateCCW)));
    }
}
