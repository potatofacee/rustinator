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
    PasteSelection,
    SendNewline,
    OpenPrefs,
    ToggleZoom,
    ScaledZoom,
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
    PageUp,
    PageDown,
    PageUpHalf,
    PageDownHalf,
    LineUp,
    LineDown,
    HideWindow,
    ToggleReadOnly,
    SetTitle,
    OpenTerminalHere,
    GroupAll,
    UngroupAll,
    GroupTab,
    UngroupTab,
    UngroupWin,
    BroadcastOff,
    BroadcastGroup,
    BroadcastAll,
    InsertNumber,
    InsertPadded,
    NextProfile,
    PreviousProfile,
    LayoutLauncher,
    DetachTab,
    EditWindowTitle,
    EditTabTitle,
    EditTerminalTitle,
    PrefsKeybindings,
    Help,
    NewTerminator,
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
            "paste_selection" => Action::PasteSelection,
            "send_newline" => Action::SendNewline,
            "open_prefs" => Action::OpenPrefs,
            "toggle_zoom" => Action::ToggleZoom,
            "scaled_zoom" => Action::ScaledZoom,
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
            "page_up" => Action::PageUp,
            "page_down" => Action::PageDown,
            "page_up_half" => Action::PageUpHalf,
            "page_down_half" => Action::PageDownHalf,
            "line_up" => Action::LineUp,
            "line_down" => Action::LineDown,
            "hide_window" => Action::HideWindow,
            "toggle_read_only" => Action::ToggleReadOnly,
            "set_title" => Action::SetTitle,
            "open_terminal_here" => Action::OpenTerminalHere,
            "group_all" => Action::GroupAll,
            "ungroup_all" => Action::UngroupAll,
            "group_tab" => Action::GroupTab,
            "ungroup_tab" => Action::UngroupTab,
            "ungroup_win" => Action::UngroupWin,
            "broadcast_off" => Action::BroadcastOff,
            "broadcast_group" => Action::BroadcastGroup,
            "broadcast_all" => Action::BroadcastAll,
            "insert_number" => Action::InsertNumber,
            "insert_padded" => Action::InsertPadded,
            "next_profile" => Action::NextProfile,
            "previous_profile" => Action::PreviousProfile,
            "layout_launcher" => Action::LayoutLauncher,
            "detach_tab" => Action::DetachTab,
            "edit_window_title" => Action::EditWindowTitle,
            "edit_tab_title" => Action::EditTabTitle,
            "edit_terminal_title" => Action::EditTerminalTitle,
            "preferences_keybindings" => Action::PrefsKeybindings,
            "help" => Action::Help,
            "new_terminator" => Action::NewTerminator,
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
            if let Ok(parsed) = parse_combo(&combo) {
                t.map.insert(parsed, action);
            }
        }
        t
    }

    /// Apply the user's `[[keybindings]]` over the defaults the way
    /// Terminator's config lays its `[keybindings]` section over its own
    /// before `Keybindings.reload` builds the lookup (keybindings.py:68-96):
    /// an action the user names loses every default chord and is bound to
    /// exactly the combos listed for it (several entries for one action are
    /// Terminator's tuple of bindings), so a rebound action's old chord is no
    /// longer live. An empty combo or "None" leaves the action unbound
    /// (keybindings.py:73-74); so does an unparseable one, which is logged —
    /// in Terminator the bad value has already replaced the default by the
    /// time it fails to parse. A chord that belongs to another action is
    /// taken over, last wins as in the loader's dict assignment (only the
    /// prefs GUI refuses duplicates), and the collision is logged. Unknown
    /// actions are logged and skipped.
    pub fn apply_user(&mut self, entries: &[(String, String)]) {
        let named: Vec<Action> = entries
            .iter()
            .filter_map(|(action_str, _)| Action::from_str(action_str))
            .collect();
        self.map.retain(|_, action| !named.contains(action));
        for (action_str, combo) in entries {
            let Some(action) = Action::from_str(action_str) else {
                eprintln!("keybinding: unknown action '{}'", action_str);
                continue;
            };
            if combo_is_unbound(combo) {
                continue;
            }
            let parsed = match parse_combo(combo) {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("keybinding: can't parse combo '{}' for {}: {}", combo, action_str, e);
                    continue;
                }
            };
            if let Some(prev) = self.map.insert(parsed, action).filter(|prev| *prev != action) {
                eprintln!("keybinding: '{}' was {:?}, now {}", combo, prev, action_str);
            }
        }
    }

    pub fn lookup(&self, key: egui::Key, modifiers: egui::Modifiers) -> Option<Action> {
        self.map.get(&(Mods::from_egui(modifiers), key)).copied()
    }

    /// Display combo bound to an action, e.g. "Ctrl+Shift+O", for menu
    /// shortcut hints. When several combos map to the action, returns the
    /// lexicographically smallest so the hint is stable across runs.
    pub fn combo_for(&self, action: Action) -> Option<String> {
        self.map
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(&(mods, key), _)| format_combo(mods, key))
            .min()
    }
}

fn format_combo(mods: Mods, key: egui::Key) -> String {
    let mut s = String::new();
    if mods.ctrl {
        s.push_str("Ctrl+");
    }
    if mods.alt {
        s.push_str("Alt+");
    }
    if mods.shift {
        s.push_str("Shift+");
    }
    if mods.mac_cmd {
        s.push_str("Cmd+");
    }
    s.push_str(key.name());
    s
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
        .map(|(combo, action)| {
            // macOS reserves Cmd+Tab / Cmd+Shift+Tab for the system app
            // switcher and intercepts them before the window sees the keys, so
            // a Cmd+Tab binding is dead. Keep Tab-key navigation on Ctrl.
            if combo_key_is(&combo, "Tab") {
                (combo, action)
            } else {
                (ctrl_to_cmd(&combo), action)
            }
        })
        .collect();
    v.push(("Cmd+C".into(), Action::Copy));
    v.push(("Cmd+V".into(), Action::Paste));
    v
}

/// True if the combo's final (key) token equals `key`, case-insensitively.
fn combo_key_is(combo: &str, key: &str) -> bool {
    combo
        .rsplit('+')
        .map(str::trim)
        .find(|p| !p.is_empty())
        .is_some_and(|k| k.eq_ignore_ascii_case(key))
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
        ("Super+I".into(), Action::NewTerminator),           // new_terminator
        ("Alt+L".into(), Action::LayoutLauncher),            // layout_launcher

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
        ("Ctrl+Shift+D".into(), Action::DetachTab),           // detach_tab
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
        ("Ctrl+Shift+Z".into(), Action::ScaledZoom),          // scaled_zoom
        ("Ctrl+Shift+Alt+A".into(), Action::HideWindow),
        // zoom_in is `<Control>plus`: the `+` keysym, wherever the layout puts
        // it — Ctrl+Shift+= on US (Shift consumed, see input::lookup_binding),
        // the unshifted `+` key on German. Ctrl+Equals is kept alongside.
        ("Ctrl+Plus".into(), Action::ZoomIn),                 // zoom_in
        ("Ctrl+Equals".into(), Action::ZoomIn),               // zoom_in (rustinator extra)
        ("Ctrl+Minus".into(), Action::ZoomOut),                // zoom_out
        ("Ctrl+0".into(), Action::ZoomReset),                  // zoom_normal
        // ("", Action::ZoomInAll),                     // zoom_in_all — not implemented
        // ("", Action::ZoomOutAll),                    // zoom_out_all — not implemented
        // ("", Action::ZoomResetAll),                  // zoom_normal_all — not implemented

        // ── Clipboard ────────────────────────────────────────────────
        ("Ctrl+Shift+C".into(), Action::Copy),                // copy
        ("Ctrl+Shift+V".into(), Action::Paste),               // paste
        // paste_selection stays UNBOUND by default (Terminator parity);
        // Shift+Insert pastes PRIMARY as VTE's own built-in (input::key_builtin).
        ("Shift+Enter".into(), Action::SendNewline),          // send_newline (<Shift>Return)

        // ── Search ───────────────────────────────────────────────────
        ("Ctrl+Shift+F".into(), Action::ToggleSearch),        // search

        // ── Terminal reset ───────────────────────────────────────────
        ("Ctrl+Shift+R".into(), Action::ResetTerminal),       // reset
        ("Ctrl+Shift+G".into(), Action::ResetClear),          // reset_clear

        // ── Scrollbar, scrolling & profiles ──────────────────────────
        ("Ctrl+Shift+S".into(), Action::ToggleScrollbar),
        // page_up / page_down / page_up_half / page_down_half / line_up /
        // line_down stay UNBOUND by default (Terminator parity); Shift+PgUp/
        // PgDn/Home/End scroll as VTE's own built-ins (input::key_builtin).
        // next_profile / previous_profile stay UNBOUND by default (Terminator
        // parity; reachable via the right-click "Profiles" radio submenu).

        // ── Grouping & broadcasting ──────────────────────────────────
        ("Ctrl+Shift+B".into(), Action::ToggleBroadcast),     // 3-way cycle Off->Group->All (rustinator-only; Terminator uses unbound scopes)
        ("Super+G".into(), Action::GroupAll),                 // group_all
        ("Super+Shift+G".into(), Action::UngroupAll),         // ungroup_all
        ("Super+T".into(), Action::GroupTab),                 // group_tab
        ("Super+Shift+T".into(), Action::UngroupTab),         // ungroup_tab
        ("Super+Shift+W".into(), Action::UngroupWin),         // ungroup_win
        // broadcast_off/group/all stay UNBOUND (Terminator parity; menu radio items)

        // ── Title editing ────────────────────────────────────────────
        ("Ctrl+Alt+W".into(), Action::EditWindowTitle),       // edit_window_title
        ("Ctrl+Alt+A".into(), Action::EditTabTitle),          // edit_tab_title
        ("Ctrl+Alt+X".into(), Action::EditTerminalTitle),     // edit_terminal_title

        // ── Terminal index insert ────────────────────────────────────
        ("Super+1".into(), Action::InsertNumber),             // insert_number
        ("Super+0".into(), Action::InsertPadded),             // insert_padded

        // ── Preferences & help ───────────────────────────────────────
        // ("", Action::OpenPrefs),                     // preferences — unbound in Terminator
        ("Ctrl+Shift+K".into(), Action::PrefsKeybindings),    // preferences_keybindings
        ("F1".into(), Action::Help),                          // help
    ]
}

/// A combo that binds nothing: Terminator's `''` / `"None"` (keybindings.py:73-74).
fn combo_is_unbound(combo: &str) -> bool {
    let combo = combo.trim();
    combo.is_empty() || combo.eq_ignore_ascii_case("none")
}

/// Parse a combo in either spelling: rustinator's `Ctrl+Shift+O` or
/// Terminator's own `<Shift><Control>o` (keybindings.py:31,98-111 — every
/// `<name>` group is a modifier, whatever is left is the keysym name), so a
/// binding copied out of a Terminator config works as written. Errors carry
/// Terminator's `KeymapError` wording.
fn parse_combo(s: &str) -> Result<(Mods, egui::Key), String> {
    let mut mods = Mods { shift: false, alt: false, ctrl: false, mac_cmd: false };
    let key_name = if s.contains('<') {
        gtk_modifiers(s, &mut mods)?
    } else {
        let mut parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        let key = parts.pop().unwrap_or_default();
        for part in parts {
            apply_modifier(part, &mut mods)?;
        }
        key.to_string()
    };
    let key_name = key_name.trim();
    if key_name.is_empty() {
        return Err("No key found".into());
    }
    parse_key(key_name)
        .map(|key| (mods, key))
        .ok_or_else(|| format!("Key '{key_name}' is unrecognised"))
}

/// Strip every `<name>` group of a GTK-style combo into `mods`, returning
/// the key name that is left over.
fn gtk_modifiers(s: &str, mods: &mut Mods) -> Result<String, String> {
    let mut key = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('<') {
        key.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after
            .find('>')
            .ok_or_else(|| format!("Unterminated modifier in '{s}'"))?;
        apply_modifier(&after[..close], mods)?;
        rest = &after[close + 1..];
    }
    key.push_str(rest);
    Ok(key)
}

/// Set the modifier a combo token names (Terminator's table, keybindings.py:35-45).
fn apply_modifier(name: &str, mods: &mut Mods) -> Result<(), String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "primary" => mods.ctrl = true,
        "shift" => mods.shift = true,
        "alt" | "meta" => mods.alt = true,
        // Super, Hyper and Mod4 are one key on a stock xkb map (Mod4 holds
        // Super_L/R and Hyper_L, and GDK raises both virtual masks for it),
        // so a Terminator `<Hyper>` or `<Mod4>` chord is the Super chord here.
        "super" | "cmd" | "command" | "hyper" | "mod4" => mods.mac_cmd = true,
        // Terminator makes Mod2 the Command key on macOS (keybindings.py:45);
        // on X11 Mod2 is NumLock, a lock state winit never reports, so such
        // a chord cannot be matched there.
        "mod2" if cfg!(target_os = "macos") => mods.mac_cmd = true,
        "mod2" => return Err("Modifier '<Mod2>' is NumLock on this platform".into()),
        other => return Err(format!("Unhandled modifier '<{other}>'")),
    }
    Ok(())
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
        "pageup" | "page_up" => Key::PageUp,
        "pagedown" | "page_down" => Key::PageDown,
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
        "plus" => Key::Plus,
        "equals" | "equal" | "=" => Key::Equals,
        "bracketleft" | "[" => Key::OpenBracket,
        "bracketright" | "]" => Key::CloseBracket,
        "apostrophe" | "'" => Key::Quote,
        "grave" | "`" => Key::Backtick,
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
        assert_eq!(parse_combo("Mod3+A"), Err("Unhandled modifier '<mod3>'".into()));
        assert_eq!(parse_combo("<Mod3>a"), Err("Unhandled modifier '<mod3>'".into()));
    }

    #[test]
    fn unknown_key_rejected() {
        assert_eq!(parse_combo("Ctrl+UnknownKey"), Err("Key 'UnknownKey' is unrecognised".into()));
        assert_eq!(parse_combo(""), Err("No key found".into()));
        assert_eq!(parse_combo("<Control>"), Err("No key found".into()));
    }

    #[test]
    fn linux_defaults_all_parse() {
        for (combo, _) in linux_defaults() {
            assert!(parse_combo(&combo).is_ok(), "failed: {combo}");
        }
    }

    #[test]
    fn macos_defaults_all_parse() {
        for (combo, _) in macos_defaults() {
            assert!(parse_combo(&combo).is_ok(), "failed: {combo}");
        }
    }

    // plugins-29: Terminator's own `<Modifier>keyname` spelling parses to the
    // same chord as the `+` form, keysym names included.
    #[test]
    fn parse_gtk_style_combo() {
        assert_eq!(parse_combo("<Shift><Control>t"), parse_combo("Ctrl+Shift+T"));
        assert_eq!(parse_combo("<Control>Page_Down"), parse_combo("Ctrl+PageDown"));
        assert_eq!(parse_combo("<Shift>Return"), parse_combo("Shift+Enter"));
        assert_eq!(parse_combo("<Control>plus"), parse_combo("Ctrl+Plus"));
        assert_eq!(parse_combo("<Alt>Up"), parse_combo("Alt+Up"));
        assert_eq!(parse_combo("<Shift><Control><Alt>a"), parse_combo("Ctrl+Shift+Alt+A"));
        assert_eq!(parse_combo("<Super>r"), parse_combo("Super+R"));
        assert_eq!(parse_combo("<Control>equal"), parse_combo("Ctrl+Equals"));
        assert_eq!(parse_combo("<Control>bracketleft"), parse_combo("Ctrl+["));
        // Modifiers are case-insensitive, as Terminator lowercases them.
        assert_eq!(parse_combo("<CONTROL><shift>Tab"), parse_combo("Ctrl+Shift+Tab"));
        // Terminator strips the groups wherever they sit.
        assert_eq!(parse_combo("t<Control>"), parse_combo("Ctrl+T"));
        assert_eq!(parse_combo("<Control"), Err("Unterminated modifier in '<Control'".into()));
    }

    /// Terminator's non-empty default chords (config.py:126-215), verbatim.
    fn terminator_default_combos() -> Vec<&'static str> {
        vec![
            "<Control>plus", "<Control>minus", "<Control>0", "<Shift><Control>t",
            "<Control>Tab", "<Shift><Control>Tab", "<Shift><Control>n",
            "<Shift><Control>p", "<Alt>Up", "<Alt>Down", "<Alt>Left", "<Alt>Right",
            "<Super>r", "<Super><Shift>r", "<Shift><Control>a", "<Shift><Control>o",
            "<Shift><Control>e", "<Shift><Control>w", "<Shift><Control>c",
            "<Shift><Control>v", "<Shift>Return", "<Shift><Control>s",
            "<Shift><Control>f", "<Shift><Control>q", "<Shift><Control>Up",
            "<Shift><Control>Down", "<Shift><Control>Left", "<Shift><Control>Right",
            "<Shift><Control>Page_Down", "<Shift><Control>Page_Up",
            "<Shift><Control>x", "<Shift><Control>z", "<Control>Page_Down",
            "<Control>Page_Up", "F11", "<Shift><Control>r", "<Shift><Control>g",
            "<Shift><Control><Alt>a", "<Super>g", "<Shift><Super>g",
            "<Shift><Super>w", "<Super>t", "<Shift><Super>t", "<Shift><Control>i",
            "<Super>i", "<Super>1", "<Super>0", "<Control><Alt>w", "<Control><Alt>a",
            "<Control><Alt>x", "<Alt>l", "<Control><Shift>k", "F1",
            "<Shift><Control>d",
        ]
    }

    #[test]
    fn terminator_default_combos_all_parse() {
        for combo in terminator_default_combos() {
            assert!(parse_combo(combo).is_ok(), "failed: {combo}");
        }
    }

    // plugins-30: Terminator's whole modifier table (keybindings.py:35-45).
    #[test]
    fn terminator_modifier_names() {
        let ctrl = Mods { ctrl: true, shift: false, alt: false, mac_cmd: false };
        let sup = Mods { ctrl: false, shift: false, alt: false, mac_cmd: true };
        assert_eq!(parse_combo("<Primary>a"), Ok((ctrl, egui::Key::A)));
        assert_eq!(parse_combo("<Control>a"), Ok((ctrl, egui::Key::A)));
        assert_eq!(parse_combo("<Hyper>a"), Ok((sup, egui::Key::A)));
        assert_eq!(parse_combo("<Mod4>a"), Ok((sup, egui::Key::A)));
        assert_eq!(parse_combo("Hyper+A"), Ok((sup, egui::Key::A)));
        if cfg!(target_os = "macos") {
            // Terminator's darwin remap: Mod2 is the Command key.
            assert_eq!(parse_combo("<Mod2>a"), Ok((sup, egui::Key::A)));
        } else {
            // NumLock cannot be part of a chord here.
            assert!(parse_combo("<Mod2>a").is_err());
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
        // R-086: the default chord is gone with it, as Terminator's config
        // replaces the action's value rather than adding to it.
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::O, ctrl_shift), None);
    }

    #[test]
    fn user_empty_or_none_combo_unbinds() {
        // R-086: '' and "None" are Terminator's spellings for "no chord"
        // (keybindings.py:73-74); the default chord no longer fires.
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        for combo in ["", "None", "none", "  "] {
            let mut table = BindingTable::new(true);
            table.apply_user(&[("copy".into(), combo.into())]);
            assert_eq!(table.lookup(egui::Key::C, ctrl_shift), None, "combo {combo:?}");
            assert_eq!(table.combo_for(Action::Copy), None, "combo {combo:?}");
        }
    }

    #[test]
    fn user_unparseable_combo_unbinds() {
        // In Terminator the bad value has replaced the default before it
        // fails to parse, so the action ends up unbound rather than on a
        // chord the user never wrote.
        let mut table = BindingTable::new(true);
        table.apply_user(&[("copy".into(), "Ctrl+Shift+Bogus".into())]);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::C, ctrl_shift), None);
        // An unknown action touches nothing.
        let mut table = BindingTable::new(true);
        table.apply_user(&[("no_such_action".into(), "Ctrl+Shift+C".into())]);
        assert_eq!(table.lookup(egui::Key::C, ctrl_shift), Some(Action::Copy));
    }

    #[test]
    fn user_lists_several_chords_for_one_action() {
        // Terminator's tuple of bindings: every entry for the action stays.
        let mut table = BindingTable::new(true);
        table.apply_user(&[
            ("copy".into(), "Ctrl+Alt+C".into()),
            ("copy".into(), "<Shift><Control>y".into()),
        ]);
        let ctrl_alt = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::C, ctrl_alt), Some(Action::Copy));
        assert_eq!(table.lookup(egui::Key::Y, ctrl_shift), Some(Action::Copy));
        assert_eq!(table.lookup(egui::Key::C, ctrl_shift), None);
    }

    #[test]
    fn user_chord_collision_last_wins() {
        // Rebinding onto another action's default chord takes it over, as the
        // loader's dict assignment does (keybindings.py:94-95).
        let mut table = BindingTable::new(true);
        table.apply_user(&[("new_tab".into(), "Ctrl+Shift+O".into())]);
        let ctrl_shift = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::O, ctrl_shift), Some(Action::NewTab));
        assert_eq!(table.lookup(egui::Key::T, ctrl_shift), None);
        assert_eq!(table.combo_for(Action::SplitHorizontal), None);
    }

    #[test]
    fn binding_shift_enter_send_newline() {
        // R-087: Terminator's `<Shift>Return` -> send_newline.
        let table = BindingTable::new(true);
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::Enter, shift), Some(Action::SendNewline));
        assert_eq!(Action::from_str("send_newline"), Some(Action::SendNewline));
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
    fn binding_ctrl_plus_zoom_in() {
        // Terminator's `<Control>plus` (config.py:131): the `+` keysym with
        // Shift consumed, which is how Ctrl+Shift+= on US reaches this entry
        // (input::lookup_binding). A literal Ctrl+Shift+Equals entry no longer
        // exists on its own.
        assert_eq!(linux_ctrl_only(egui::Key::Plus), Some(Action::ZoomIn));
        assert_eq!(linux_ctrl_shift(egui::Key::Equals), None);
    }

    #[test]
    fn parse_plus_key() {
        let (mods, key) = parse_combo("Ctrl+Plus").unwrap();
        assert!(mods.ctrl && !mods.shift);
        assert_eq!(key, egui::Key::Plus);
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

    // R-013: every Terminator default chord has an action, so none of them can
    // fall through to the shell as a control byte (Ctrl+Shift+D was EOF).
    #[test]
    fn binding_ctrl_shift_d_detach_tab() {
        assert_eq!(linux_ctrl_shift(egui::Key::D), Some(Action::DetachTab));
    }

    #[test]
    fn binding_ctrl_shift_k_prefs_keybindings() {
        assert_eq!(linux_ctrl_shift(egui::Key::K), Some(Action::PrefsKeybindings));
    }

    #[test]
    fn binding_f1_help() {
        assert_eq!(linux_no_mods(egui::Key::F1), Some(Action::Help));
    }

    fn linux_ctrl_alt(key: egui::Key) -> Option<Action> {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
        table.lookup(key, mods)
    }

    #[test]
    fn binding_ctrl_alt_title_editors() {
        assert_eq!(linux_ctrl_alt(egui::Key::W), Some(Action::EditWindowTitle));
        assert_eq!(linux_ctrl_alt(egui::Key::A), Some(Action::EditTabTitle));
        assert_eq!(linux_ctrl_alt(egui::Key::X), Some(Action::EditTerminalTitle));
    }

    #[test]
    fn binding_super_i_new_terminator() {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { mac_cmd: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::I, mods), Some(Action::NewTerminator));
    }

    #[test]
    fn terminator_default_actions_parse_by_name() {
        for (name, action) in [
            ("detach_tab", Action::DetachTab),
            ("edit_window_title", Action::EditWindowTitle),
            ("edit_tab_title", Action::EditTabTitle),
            ("edit_terminal_title", Action::EditTerminalTitle),
            ("preferences_keybindings", Action::PrefsKeybindings),
            ("help", Action::Help),
            ("new_terminator", Action::NewTerminator),
        ] {
            assert_eq!(Action::from_str(name), Some(action), "{name}");
        }
    }

    // ── macOS per-binding coverage ──────────────────────────────────

    fn mac_table() -> BindingTable {
        let mut t = BindingTable { map: HashMap::new() };
        for (combo, action) in macos_defaults() {
            if let Ok(parsed) = parse_combo(&combo) {
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

    fn mac_ctrl(key: egui::Key) -> Option<Action> {
        let mods = egui::Modifiers { ctrl: true, ..Default::default() };
        mac_table().lookup(key, mods)
    }

    fn mac_ctrl_shift(key: egui::Key) -> Option<Action> {
        let mods = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        mac_table().lookup(key, mods)
    }

    #[test]
    fn mac_tab_navigation_stays_on_ctrl() {
        // macOS reserves Cmd+Tab / Cmd+Shift+Tab for the app switcher, so Tab
        // navigation must remain on Ctrl rather than being remapped to Cmd.
        assert_eq!(mac_ctrl(egui::Key::Tab), Some(Action::FocusNext));
        assert_eq!(mac_ctrl_shift(egui::Key::Tab), Some(Action::FocusPrev));
        assert_eq!(mac_cmd(egui::Key::Tab), None);
        assert_eq!(mac_cmd_shift(egui::Key::Tab), None);
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
    fn action_scaled_zoom() {
        assert_eq!(Action::from_str("scaled_zoom"), Some(Action::ScaledZoom));
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

    // R-009 / R-035: Terminator's scroll actions (terminal.py:2284-2300) and
    // `paste_selection` parse from their config names and are unbound by
    // default (config.py:154,158-163), so a user binding is the only way in.
    #[test]
    fn scroll_and_paste_selection_actions_parse_and_start_unbound() {
        let table = BindingTable::new(true);
        for (name, action) in [
            ("page_up", Action::PageUp),
            ("page_down", Action::PageDown),
            ("page_up_half", Action::PageUpHalf),
            ("page_down_half", Action::PageDownHalf),
            ("line_up", Action::LineUp),
            ("line_down", Action::LineDown),
            ("paste_selection", Action::PasteSelection),
        ] {
            assert_eq!(Action::from_str(name), Some(action), "{name}");
            assert_eq!(table.combo_for(action), None, "{name}");
        }
        let mut table = BindingTable::new(true);
        table.apply_user(&[("page_up".into(), "Shift+PageUp".into())]);
        assert_eq!(table.lookup(egui::Key::PageUp, egui::Modifiers::SHIFT), Some(Action::PageUp));
    }

    // Profile cycling: both parse from their config strings. Unbound by default
    // (Terminator parity), so there is no default-binding assertion.
    #[test]
    fn action_next_previous_profile() {
        assert_eq!(Action::from_str("next_profile"), Some(Action::NextProfile));
        assert_eq!(
            Action::from_str("previous_profile"),
            Some(Action::PreviousProfile)
        );
    }

    // Gap #4: Layout Launcher (Terminator's Alt+L). Parses from its config
    // string and is bound to Alt+L in the Linux defaults.
    #[test]
    fn action_layout_launcher() {
        assert_eq!(
            Action::from_str("layout_launcher"),
            Some(Action::LayoutLauncher)
        );
    }

    #[test]
    fn binding_alt_l_layout_launcher() {
        let table = BindingTable::new(true);
        let mods = egui::Modifiers { alt: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::L, mods), Some(Action::LayoutLauncher));
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
    fn action_broadcast_scopes() {
        assert!(Action::from_str("broadcast_all").is_some());
        assert!(Action::from_str("broadcast_group").is_some());
        assert!(Action::from_str("broadcast_off").is_some());
    }

    // Gap #13: the 7 group/insert actions get Linux default binds at the exact
    // Super+ combos Terminator uses (config.py:184-199). Super maps to mac_cmd
    // in our Mods, so look them up with mac_cmd set under force_linux=true.
    #[test]
    fn defaults_include_grouping_super_binds() {
        let table = BindingTable::new(true);
        let cmd = egui::Modifiers { mac_cmd: true, ..Default::default() };
        let cmd_shift = egui::Modifiers { mac_cmd: true, shift: true, ..Default::default() };
        assert_eq!(table.lookup(egui::Key::G, cmd), Some(Action::GroupAll));
        assert_eq!(table.lookup(egui::Key::G, cmd_shift), Some(Action::UngroupAll));
        assert_eq!(table.lookup(egui::Key::T, cmd), Some(Action::GroupTab));
        assert_eq!(table.lookup(egui::Key::T, cmd_shift), Some(Action::UngroupTab));
        assert_eq!(table.lookup(egui::Key::W, cmd_shift), Some(Action::UngroupWin));
        assert_eq!(table.lookup(egui::Key::Num1, cmd), Some(Action::InsertNumber));
        assert_eq!(table.lookup(egui::Key::Num0, cmd), Some(Action::InsertPadded));
    }

    // The 3 broadcast scopes stay UNBOUND by default (Terminator parity; menu
    // radio items only).
    #[test]
    fn broadcast_scopes_unbound_by_default() {
        let bound: Vec<Action> = linux_defaults().into_iter().map(|(_, a)| a).collect();
        assert!(!bound.contains(&Action::BroadcastOff));
        assert!(!bound.contains(&Action::BroadcastGroup));
        assert!(!bound.contains(&Action::BroadcastAll));
    }

    // Gap #13: default bindings for broadcast scopes
    #[test]
    #[ignore = "gap #13: broadcast scopes are intentionally unbound (Terminator parity)"]
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

    #[test]
    fn combo_for_reverse_lookup() {
        let table = BindingTable::new(true);
        assert_eq!(
            table.combo_for(Action::SplitHorizontal).as_deref(),
            Some("Ctrl+Shift+O")
        );
        // User override is reflected in the hint — and is the only chord left
        // for the action (R-086).
        let mut table = BindingTable::new(true);
        table.apply_user(&[("split_horizontal".into(), "Ctrl+Alt+H".into())]);
        assert_eq!(
            table.combo_for(Action::SplitHorizontal).as_deref(),
            Some("Ctrl+Alt+H")
        );
    }

    // ── Dispatch-coverage guard ───────────────────────────────────────
    // Every dispatchable Action must be reachable from either a default key
    // binding or the right-click context menu. This guards the planned
    // action-dispatch unification: if a future change drops both the binding
    // and the menu entry for an Action, this test fails.

    /// Every `Action` variant, exactly once (parameterized `SwitchToTab` has a
    /// single representative). The exhaustiveness guard below makes adding a new
    /// variant a compile error until it is listed here, so the list cannot
    /// silently fall out of sync with the enum.
    fn all_actions() -> Vec<Action> {
        // Compile-time exhaustiveness guard: a new Action variant breaks this
        // match and forces an update to the list below.
        fn _assert_exhaustive(a: Action) {
            match a {
                Action::SplitHorizontal
                | Action::SplitVertical
                | Action::ClosePane
                | Action::NewTab
                | Action::NextTab
                | Action::PrevTab
                | Action::FocusNext
                | Action::FocusPrev
                | Action::Copy
                | Action::Paste
                | Action::PasteSelection
                | Action::SendNewline
                | Action::OpenPrefs
                | Action::ToggleZoom
                | Action::ScaledZoom
                | Action::ToggleBroadcast
                | Action::ToggleSearch
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ZoomReset
                | Action::CloseWindow
                | Action::ToggleFullscreen
                | Action::ResizeLeft
                | Action::ResizeRight
                | Action::ResizeUp
                | Action::ResizeDown
                | Action::ResetTerminal
                | Action::ResetClear
                | Action::NewWindow
                | Action::QuitHotkeyWindow
                | Action::MoveTabLeft
                | Action::MoveTabRight
                | Action::SwitchToTab(_)
                | Action::GoUp
                | Action::GoDown
                | Action::GoLeft
                | Action::GoRight
                | Action::GoNext
                | Action::GoPrev
                | Action::RotateCW
                | Action::RotateCCW
                | Action::SplitAuto
                | Action::ToggleScrollbar
                | Action::PageUp
                | Action::PageDown
                | Action::PageUpHalf
                | Action::PageDownHalf
                | Action::LineUp
                | Action::LineDown
                | Action::HideWindow
                | Action::ToggleReadOnly
                | Action::SetTitle
                | Action::OpenTerminalHere
                | Action::GroupAll
                | Action::UngroupAll
                | Action::GroupTab
                | Action::UngroupTab
                | Action::UngroupWin
                | Action::BroadcastOff
                | Action::BroadcastGroup
                | Action::BroadcastAll
                | Action::InsertNumber
                | Action::InsertPadded
                | Action::NextProfile
                | Action::PreviousProfile
                | Action::LayoutLauncher
                | Action::DetachTab
                | Action::EditWindowTitle
                | Action::EditTabTitle
                | Action::EditTerminalTitle
                | Action::PrefsKeybindings
                | Action::Help
                | Action::NewTerminator => {}
            }
        }
        vec![
            Action::SplitHorizontal,
            Action::SplitVertical,
            Action::ClosePane,
            Action::NewTab,
            Action::NextTab,
            Action::PrevTab,
            Action::FocusNext,
            Action::FocusPrev,
            Action::Copy,
            Action::Paste,
            Action::PasteSelection,
            Action::SendNewline,
            Action::OpenPrefs,
            Action::ToggleZoom,
            Action::ScaledZoom,
            Action::ToggleBroadcast,
            Action::ToggleSearch,
            Action::ZoomIn,
            Action::ZoomOut,
            Action::ZoomReset,
            Action::CloseWindow,
            Action::ToggleFullscreen,
            Action::ResizeLeft,
            Action::ResizeRight,
            Action::ResizeUp,
            Action::ResizeDown,
            Action::ResetTerminal,
            Action::ResetClear,
            Action::NewWindow,
            Action::QuitHotkeyWindow,
            Action::MoveTabLeft,
            Action::MoveTabRight,
            Action::SwitchToTab(1),
            Action::GoUp,
            Action::GoDown,
            Action::GoLeft,
            Action::GoRight,
            Action::GoNext,
            Action::GoPrev,
            Action::RotateCW,
            Action::RotateCCW,
            Action::SplitAuto,
            Action::ToggleScrollbar,
            Action::PageUp,
            Action::PageDown,
            Action::PageUpHalf,
            Action::PageDownHalf,
            Action::LineUp,
            Action::LineDown,
            Action::HideWindow,
            Action::ToggleReadOnly,
            Action::SetTitle,
            Action::OpenTerminalHere,
            Action::GroupAll,
            Action::UngroupAll,
            Action::GroupTab,
            Action::UngroupTab,
            Action::UngroupWin,
            Action::BroadcastOff,
            Action::BroadcastGroup,
            Action::BroadcastAll,
            Action::InsertNumber,
            Action::InsertPadded,
            Action::NextProfile,
            Action::PreviousProfile,
            Action::LayoutLauncher,
            Action::DetachTab,
            Action::EditWindowTitle,
            Action::EditTabTitle,
            Action::EditTerminalTitle,
            Action::PrefsKeybindings,
            Action::Help,
            Action::NewTerminator,
        ]
    }

    /// Actions reachable from the right-click context menu. Mirrors the
    /// `action_menu_item` entries in `pane_ui::build_context_menu` (the menu's
    /// Copy is a plain copy that bypasses `Action::Copy`, so it is not listed).
    fn context_menu_actions() -> Vec<Action> {
        vec![
            Action::Paste,
            Action::SplitHorizontal,
            Action::SplitVertical,
            Action::SplitAuto,
            Action::ToggleZoom,
            Action::ToggleReadOnly,
            // The 3 broadcast scopes replace the old single ToggleBroadcast item
            // in the per-pane context menu (`broadcast_and_group_menu`).
            // ToggleBroadcast leaves the menu but stays bound to Ctrl+Shift+B.
            Action::BroadcastOff,
            Action::BroadcastGroup,
            Action::BroadcastAll,
            Action::SetTitle,
            Action::OpenTerminalHere,
            Action::ClosePane,
            Action::NewTab,
            Action::OpenPrefs,
        ]
    }

    #[test]
    fn every_action_reachable_via_binding_or_menu() {
        use std::collections::HashSet;
        let bound: HashSet<Action> = linux_defaults().into_iter().map(|(_, a)| a).collect();
        let menu: HashSet<Action> = context_menu_actions().into_iter().collect();
        for action in all_actions() {
            // Explicit allowlist of intentionally unbound, non-menu actions:
            //  - SwitchToTab(n): unbound by default (Terminator parity);
            //    reachable only via user keybindings.
            //  - QuitHotkeyWindow: dispatched by the dedicated hotkey window,
            //    not the main binding table or pane menu.
            //  - NextProfile/PreviousProfile: unbound by default (Terminator
            //    parity); switched via the right-click "Profiles" submenu, not
            //    these Actions.
            //  - PasteSelection and the six scroll actions: unbound by default
            //    (Terminator parity); Shift+Insert and Shift+PgUp/PgDn/Home/End
            //    reach the same effects as VTE's built-ins (input::key_builtin).
            if matches!(
                action,
                Action::SwitchToTab(_)
                    | Action::QuitHotkeyWindow
                    | Action::NextProfile
                    | Action::PreviousProfile
                    | Action::PasteSelection
                    | Action::PageUp
                    | Action::PageDown
                    | Action::PageUpHalf
                    | Action::PageDownHalf
                    | Action::LineUp
                    | Action::LineDown
            ) {
                continue;
            }
            assert!(
                bound.contains(&action) || menu.contains(&action),
                "Action {action:?} is unreachable: not in default linux bindings, \
                 not in the context menu, and not in the intentional allowlist"
            );
        }
    }
}
