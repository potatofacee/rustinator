use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::LayoutTemplate;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub global: GlobalConfig,
    pub active_profile: String,
    // Field-level default (empty Vec) overrides the container `serde(default)`
    // so that a config file with no `[[profiles]]` deserializes to an empty
    // list. `migrate_legacy`/`ensure_consistent` then populate it. This lets
    // `migrate_legacy` distinguish "no profiles supplied" from "profiles
    // supplied" and avoid clobbering user `[[profiles]]` during legacy migration.
    #[serde(default)]
    pub profiles: Vec<Profile>,

    #[serde(default)]
    pub keybindings: Vec<KeyBinding>,

    #[serde(default)]
    pub layouts: Vec<SavedLayout>,

    #[serde(default)]
    pub hotkey_window: HotkeyWindowConfig,

    // ---- Legacy flat fields, for migration from pre-profile configs.
    // These are read but never written back.
    #[serde(default, skip_serializing)]
    pub font: Option<FontConfig>,
    #[serde(default, skip_serializing)]
    pub colors: Option<ColorsConfig>,
    #[serde(default, skip_serializing)]
    pub scrollback: Option<ScrollbackConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct KeyBinding {
    pub action: String,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SavedLayout {
    pub name: String,
    pub template: LayoutTemplate,
    /// Per-leaf metadata sidecar, in `leaves_in_order` order. Empty for old
    /// layouts (which had no per-pane metadata), giving identical restore.
    /// Kept as a sidecar so `LayoutTemplate::Terminal` stays a unit variant
    /// and old layouts deserialize unchanged.
    #[serde(default)]
    pub terminals: Vec<TerminalMeta>,
}

/// Per-pane metadata persisted alongside a `SavedLayout`. Every field is
/// optional so missing entries (or a missing sidecar entirely) restore to
/// today's behavior.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TerminalMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalConfig {
    pub confirm_on_close: bool,
    #[serde(default)]
    pub use_linux_keybindings: bool,
    /// Where the tab bar is drawn. `Hidden` = panel not drawn (tabs reachable
    /// only by keyboard). Defaults to `Top` to match current behavior.
    #[serde(default)]
    pub tab_position: TabPosition,
    /// Equal-width tabs. Defaults true to match current behavior.
    #[serde(default = "default_true")]
    pub homogeneous: bool,
    /// Show a close button on each tab. Defaults true to match current behavior.
    #[serde(default = "default_true")]
    pub close_button_on_tab: bool,
    /// Insert a new tab immediately after the current one rather than appending.
    #[serde(default)]
    pub new_tab_after_current: bool,
    /// Make the tab bar horizontally/vertically scrollable on overflow instead
    /// of shrinking tabs. Defaults false (current behavior); opt-in (USER Q3).
    #[serde(default)]
    pub scroll_tabbar: bool,
    /// Name of a saved layout to launch automatically on startup. `None`
    /// (default) preserves today's behavior of opening a single default tab.
    #[serde(default)]
    pub startup_layout: Option<String>,
}

/// Where the tab bar is positioned. `Hidden` suppresses the panel entirely.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
    Hidden,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub font: FontConfig,
    pub colors: ColorsConfig,
    pub scrollback: ScrollbackConfig,
    pub transparency: TransparencyConfig,
    #[serde(default)]
    pub copy_on_selection: bool,
    #[serde(default)]
    pub smart_copy: bool,
    #[serde(default = "default_true")]
    pub cursor_blink: bool,
    #[serde(default)]
    pub scroll_on_output: bool,
    #[serde(default = "default_true")]
    pub scroll_on_keystroke: bool,
    #[serde(default)]
    pub clear_wipes_scrollback: bool,
    #[serde(default = "default_word_chars")]
    pub word_chars: String,
    #[serde(default)]
    pub exit_action: ExitAction,
    /// Alpha (0-255) of the black overlay drawn over unfocused panes to dim
    /// them. Higher = darker. Defaults to 50 to match the previous hardcoded
    /// value. (Analogous to terminator's inactive_color_offset.)
    #[serde(default = "default_inactive_dim_alpha")]
    pub inactive_dim_alpha: u8,
    /// Run `custom_command` through the shell instead of an interactive shell.
    #[serde(default)]
    pub use_custom_command: bool,
    /// The command to run when `use_custom_command` is set (via `$SHELL -c`).
    #[serde(default)]
    pub custom_command: String,
    /// Spawn the shell as a login shell (passes `--login`). Defaults true so
    /// the default profile reproduces today's spawn behavior.
    #[serde(default = "default_true")]
    pub login_shell: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExitAction {
    #[default]
    Close,
    Hold,
    Restart,
}

fn default_true() -> bool {
    true
}

fn default_word_chars() -> String {
    "-A-Za-z0-9,./?%&#:_=+@~".into()
}

fn default_inactive_dim_alpha() -> u8 {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub normal: AnsiColors,
    pub bright: AnsiColors,
    /// Stroke color of the focused pane's border.
    pub focus_border: String,
    /// Border color when broadcast input is on.
    pub broadcast_border: String,
    /// Selection colors. Empty string = invert the cell's fg/bg (default).
    pub selection_background: String,
    pub selection_foreground: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AnsiColors {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ScrollbackConfig {
    pub history: usize,
    pub infinite: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TransparencyConfig {
    pub opacity: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            global: GlobalConfig::default(),
            active_profile: "Default".into(),
            profiles: vec![Profile::new_named("Default")],
            keybindings: Vec::new(),
            layouts: Vec::new(),
            hotkey_window: HotkeyWindowConfig::default(),
            font: None,
            colors: None,
            scrollback: None,
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            confirm_on_close: true,
            use_linux_keybindings: false,
            tab_position: TabPosition::Top,
            homogeneous: true,
            close_button_on_tab: true,
            new_tab_after_current: false,
            scroll_tabbar: false,
            startup_layout: None,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::new_named("Default")
    }
}

impl Profile {
    pub fn new_named(name: &str) -> Self {
        Self {
            name: name.into(),
            font: FontConfig::default(),
            colors: ColorsConfig::default(),
            scrollback: ScrollbackConfig::default(),
            transparency: TransparencyConfig::default(),
            copy_on_selection: false,
            smart_copy: false,
            cursor_blink: true,
            scroll_on_output: false,
            scroll_on_keystroke: true,
            clear_wipes_scrollback: false,
            word_chars: default_word_chars(),
            exit_action: ExitAction::Close,
            inactive_dim_alpha: default_inactive_dim_alpha(),
            use_custom_command: false,
            custom_command: String::new(),
            login_shell: true,
        }
    }

    pub fn foreground_rgb(&self) -> [u8; 3] {
        parse_hex(&self.colors.foreground).unwrap_or([0xe5, 0xe5, 0xe5])
    }

    pub fn background_rgb(&self) -> [u8; 3] {
        parse_hex(&self.colors.background).unwrap_or([0x1a, 0x1a, 0x1a])
    }

    pub fn cursor_rgb(&self) -> [u8; 3] {
        parse_hex(&self.colors.cursor).unwrap_or(self.foreground_rgb())
    }

    /// The 16 ANSI palette colors (normal 0-7, bright 8-15) as RGB, falling
    /// back per entry to the built-in table when a hex value fails to parse.
    pub fn palette_rgb(&self) -> [[u8; 3]; 16] {
        let n = &self.colors.normal;
        let b = &self.colors.bright;
        let hexes = [
            &n.black, &n.red, &n.green, &n.yellow, &n.blue, &n.magenta, &n.cyan, &n.white,
            &b.black, &b.red, &b.green, &b.yellow, &b.blue, &b.magenta, &b.cyan, &b.white,
        ];
        let mut out = [[0u8; 3]; 16];
        for (i, hex) in hexes.iter().enumerate() {
            out[i] = parse_hex(hex).unwrap_or(crate::pane::ANSI[i]);
        }
        out
    }

    pub fn focus_border_rgb(&self) -> [u8; 3] {
        parse_hex(&self.colors.focus_border).unwrap_or([0x70, 0x70, 0xc0])
    }

    pub fn broadcast_border_rgb(&self) -> [u8; 3] {
        parse_hex(&self.colors.broadcast_border).unwrap_or([0xc0, 0x50, 0x50])
    }

    /// None = invert the cell's fg/bg (default behavior).
    pub fn selection_bg_rgb(&self) -> Option<[u8; 3]> {
        parse_hex(&self.colors.selection_background)
    }

    pub fn selection_fg_rgb(&self) -> Option<[u8; 3]> {
        parse_hex(&self.colors.selection_foreground)
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        let family = if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "monospace"
        };
        Self { family: family.into(), size: 12.0 }
    }
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            foreground: "#e5e5e5".into(),
            background: "#1a1a1a".into(),
            cursor: "#e5e5e5".into(),
            normal: AnsiColors::default_normal(),
            bright: AnsiColors::default_bright(),
            focus_border: "#7070c0".into(),
            broadcast_border: "#c05050".into(),
            selection_background: String::new(),
            selection_foreground: String::new(),
        }
    }
}

impl AnsiColors {
    pub fn default_normal() -> Self {
        Self {
            black: "#000000".into(),
            red: "#cd0000".into(),
            green: "#00cd00".into(),
            yellow: "#cdcd00".into(),
            // Deliberately brighter than xterm's #0000ee: ANSI blue is the
            // default directory color in ls and must stay readable on a black
            // background (Tango blue, same as terminator).
            blue: "#3465a4".into(),
            magenta: "#cd00cd".into(),
            cyan: "#00cdcd".into(),
            white: "#e5e5e5".into(),
        }
    }

    pub fn default_bright() -> Self {
        Self {
            black: "#7f7f7f".into(),
            red: "#ff0000".into(),
            green: "#00ff00".into(),
            yellow: "#ffff00".into(),
            blue: "#5c5cff".into(),
            magenta: "#ff00ff".into(),
            cyan: "#00ffff".into(),
            white: "#ffffff".into(),
        }
    }
}

impl Default for AnsiColors {
    fn default() -> Self {
        Self::default_normal()
    }
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self { history: 10_000, infinite: false }
    }
}

impl ScrollbackConfig {
    pub fn effective_history(&self) -> usize {
        if self.infinite { 1_000_000 } else { self.history }
    }
}

impl Default for TransparencyConfig {
    fn default() -> Self {
        Self { opacity: 1.0 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HotkeyWindowConfig {
    pub enabled: bool,
    pub hotkey: String,
    pub height_percent: u32,
    pub hide_on_focus_loss: bool,
    pub always_on_top: bool,
}

impl Default for HotkeyWindowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hotkey: "Ctrl+`".into(),
            height_percent: 50,
            hide_on_focus_loss: true,
            always_on_top: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    eprintln!("loaded config from {}", path.display());
                    cfg
                }
                Err(e) => {
                    eprintln!(
                        "config parse error at {}: {}; using defaults",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!(
                    "config read error at {}: {}; using defaults",
                    path.display(),
                    e
                );
                Self::default()
            }
        };
        cfg.migrate_legacy();
        cfg.ensure_consistent();
        cfg
    }

    /// Promote legacy top-level font/colors/scrollback fields into a profile.
    ///
    /// If any legacy field is present and no `[[profiles]]` were supplied, we
    /// synthesize a single "Default" profile from the legacy fields. If the
    /// user already supplied `[[profiles]]`, those are kept and the legacy
    /// fields are simply dropped.
    fn migrate_legacy(&mut self) {
        let has_legacy = self.font.is_some() || self.colors.is_some() || self.scrollback.is_some();
        if has_legacy {
            let profile = Profile {
                name: "Default".into(),
                font: self.font.take().unwrap_or_default(),
                colors: self.colors.take().unwrap_or_default(),
                scrollback: self.scrollback.take().unwrap_or_default(),
                transparency: TransparencyConfig::default(),
                copy_on_selection: false,
                smart_copy: false,
                cursor_blink: true,
                scroll_on_output: false,
                scroll_on_keystroke: true,
                clear_wipes_scrollback: false,
                word_chars: default_word_chars(),
                exit_action: ExitAction::Close,
                inactive_dim_alpha: default_inactive_dim_alpha(),
                use_custom_command: false,
                custom_command: String::new(),
                login_shell: true,
            };
            if self.profiles.is_empty() {
                self.profiles = vec![profile];
            }
            if self.active_profile.is_empty() {
                self.active_profile = "Default".into();
            }
        }
        self.font = None;
        self.colors = None;
        self.scrollback = None;
    }

    fn ensure_consistent(&mut self) {
        if self.profiles.is_empty() {
            self.profiles.push(Profile::new_named("Default"));
        }
        if !self.profiles.iter().any(|p| p.name == self.active_profile) {
            self.active_profile = self.profiles[0].name.clone();
        }
    }

    pub fn active(&self) -> &Profile {
        self.profiles
            .iter()
            .find(|p| p.name == self.active_profile)
            .unwrap_or(&self.profiles[0])
    }

    #[allow(dead_code)]
    pub fn active_mut(&mut self) -> &mut Profile {
        let name = self.active_profile.clone();
        if let Some(idx) = self.profiles.iter().position(|p| p.name == name) {
            &mut self.profiles[idx]
        } else {
            &mut self.profiles[0]
        }
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        let path =
            config_path().ok_or_else(|| "no HOME set; cannot locate config dir".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        // Atomic write: write to a temp file in the same directory (same
        // filesystem so rename is atomic), then rename over the target. This
        // avoids leaving a truncated/corrupt config if we crash mid-write.
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("rename {} -> {}: {e}", tmp.display(), path.display())
        })?;
        Ok(path)
    }
}

pub fn format_hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn config_path() -> Option<PathBuf> {
    // Honor XDG_CONFIG_HOME first on all platforms (including macOS) so shared
    // dotfiles work; fall back to the per-platform default otherwise.
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("rustinator/config.toml"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(&home).join("Library/Application Support/rustinator/config.toml"))
    } else {
        Some(PathBuf::from(&home).join(".config/rustinator/config.toml"))
    }
}

pub fn word_chars_to_semantic_escape(word_chars: &str) -> String {
    let mut word_set = std::collections::HashSet::new();
    let chars: Vec<char> = word_chars.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let start = chars[i];
            let end = chars[i + 2];
            for c in start..=end {
                word_set.insert(c);
            }
            i += 3;
        } else {
            word_set.insert(chars[i]);
            i += 1;
        }
    }
    let candidates = ",|:\"' ()[]{}<>\t`│;!^*\\~$#@&%?/.-_=+";
    candidates.chars().filter(|c| !word_set.contains(c)).collect()
}

pub(crate) fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse_with_hash() {
        assert_eq!(parse_hex("#1a2b3c"), Some([0x1a, 0x2b, 0x3c]));
    }

    #[test]
    fn hex_parse_without_hash() {
        assert_eq!(parse_hex("abcdef"), Some([0xab, 0xcd, 0xef]));
    }

    #[test]
    fn hex_parse_rejects_wrong_length() {
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#abcdef0"), None);
    }

    #[test]
    fn hex_parse_rejects_non_hex() {
        assert_eq!(parse_hex("#xyzabc"), None);
    }

    #[test]
    fn format_hex_roundtrip() {
        let color = [0x12, 0xab, 0xef];
        let s = format_hex(color);
        assert_eq!(s, "#12abef");
        assert_eq!(parse_hex(&s), Some(color));
    }

    #[test]
    fn default_has_one_profile() {
        let c = Config::default();
        assert_eq!(c.profiles.len(), 1);
        assert_eq!(c.active().name, "Default");
        assert_eq!(c.active().foreground_rgb(), [0xe5, 0xe5, 0xe5]);
    }

    #[test]
    fn migrate_legacy_flat_config() {
        let toml_text = r##"
            [font]
            family = "Fira Code"
            size = 14.0

            [colors]
            background = "#002b36"

            [scrollback]
            history = 20000
        "##;
        let mut cfg: Config = toml::from_str(toml_text).unwrap();
        cfg.migrate_legacy();
        cfg.ensure_consistent();

        assert_eq!(cfg.profiles.len(), 1);
        let p = cfg.active();
        assert_eq!(p.name, "Default");
        assert_eq!(p.font.family, "Fira Code");
        assert_eq!(p.colors.background, "#002b36");
        assert_eq!(p.scrollback.history, 20000);
    }

    // ── Gap inventory guardrails ──────────────────────────────────────
    // Each #[ignore] test maps to a feature from terminator-gap-inventory.md.
    // Remove #[ignore] once the feature is implemented and the test passes.
    // Run `cargo test -- --ignored` to see the full checklist.

    // ── Gap inventory guardrails ──────────────────────────────────────
    // Each #[ignore] test maps to a feature from terminator-gap-inventory.md.
    // Remove #[ignore] once the feature is implemented and the test passes.
    // Run `cargo test -- --ignored` to see the full checklist.
    //
    // Tests use panic!() instead of accessing nonexistent fields so they
    // compile now but fail when run with --ignored.

    // Gap #22: custom shell command per profile
    #[test]
    fn config_profile_custom_command() {
        // Defaults: no custom command.
        let p = Profile::default();
        assert!(!p.use_custom_command);
        assert_eq!(p.custom_command, "");

        // Round-trips when set.
        let mut cfg = Config::default();
        cfg.active_mut().use_custom_command = true;
        cfg.active_mut().custom_command = "htop".into();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert!(parsed.active().use_custom_command);
        assert_eq!(parsed.active().custom_command, "htop");

        // Missing keys default for old configs.
        let toml_text = r##"
            [[profiles]]
            name = "Test"
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert!(!cfg.profiles[0].use_custom_command);
        assert_eq!(cfg.profiles[0].custom_command, "");
    }

    // Gap #24: login shell option
    #[test]
    fn config_profile_login_shell() {
        // Defaults true so the default profile reproduces today's spawn.
        assert!(Profile::default().login_shell);

        // Round-trips when cleared.
        let mut cfg = Config::default();
        cfg.active_mut().login_shell = false;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert!(!parsed.active().login_shell);

        // Missing key defaults to true for old configs.
        let toml_text = r##"
            [[profiles]]
            name = "Test"
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert!(cfg.profiles[0].login_shell);
    }

    #[test]
    fn config_profile_exit_action() {
        let p = Profile::default();
        assert_eq!(p.exit_action, ExitAction::Close);

        let toml_text = r#"
            [[profiles]]
            name = "Test"
            exit_action = "hold"
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.profiles[0].exit_action, ExitAction::Hold);
    }

    // Gap #16: bell configuration
    #[test]
    #[ignore = "gap #16: bell config not yet in Profile/Global"]
    fn config_bell_settings() {
        panic!("add visible_bell, urgent_bell, icon_bell bools to GlobalConfig");
    }

    // Gap #25: scrollbar visibility
    #[test]
    #[ignore = "gap #25: scrollbar not yet in Profile"]
    fn config_profile_scrollbar() {
        panic!("add scrollbar: String to Profile (values: left, right, hidden)");
    }

    // Gap #26: mouse autohide
    #[test]
    #[ignore = "gap #26: mouse_autohide not yet in Profile"]
    fn config_profile_mouse_autohide() {
        panic!("add mouse_autohide: bool to Profile");
    }

    // Gap #28: tab position config
    #[test]
    fn config_global_tab_position() {
        // Default preserves current behavior: top.
        assert_eq!(GlobalConfig::default().tab_position, TabPosition::Top);

        // Each snake_case value deserializes to its variant.
        for (s, want) in [
            ("top", TabPosition::Top),
            ("bottom", TabPosition::Bottom),
            ("left", TabPosition::Left),
            ("right", TabPosition::Right),
            ("hidden", TabPosition::Hidden),
        ] {
            let toml_text = format!("[global]\ntab_position = \"{s}\"\n");
            let cfg: Config = toml::from_str(&toml_text).unwrap();
            assert_eq!(cfg.global.tab_position, want, "value {s}");
        }
    }

    #[test]
    fn config_global_tab_fields_round_trip_and_default() {
        // Defaults equal current behavior.
        let g = GlobalConfig::default();
        assert_eq!(g.tab_position, TabPosition::Top);
        assert!(g.homogeneous);
        assert!(g.close_button_on_tab);
        assert!(!g.new_tab_after_current);
        assert!(!g.scroll_tabbar);

        // Non-default values round-trip through serialize -> deserialize.
        let mut cfg = Config::default();
        cfg.global.tab_position = TabPosition::Bottom;
        cfg.global.homogeneous = false;
        cfg.global.close_button_on_tab = false;
        cfg.global.new_tab_after_current = true;
        cfg.global.scroll_tabbar = true;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.global.tab_position, TabPosition::Bottom);
        assert!(!parsed.global.homogeneous);
        assert!(!parsed.global.close_button_on_tab);
        assert!(parsed.global.new_tab_after_current);
        assert!(parsed.global.scroll_tabbar);

        // Old config missing the new keys: each defaults to current behavior.
        let toml_text = r##"
            [global]
            confirm_on_close = false
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.global.tab_position, TabPosition::Top);
        assert!(cfg.global.homogeneous);
        assert!(cfg.global.close_button_on_tab);
        assert!(!cfg.global.new_tab_after_current);
        assert!(!cfg.global.scroll_tabbar);
    }

    // Gap #18: always on top
    #[test]
    #[ignore = "gap #18: always_on_top not yet in GlobalConfig"]
    fn config_global_always_on_top() {
        panic!("add always_on_top: bool to GlobalConfig");
    }

    // Gap #21: borderless window
    #[test]
    #[ignore = "gap #21: borderless not yet in GlobalConfig"]
    fn config_global_borderless() {
        panic!("add borderless: bool to GlobalConfig");
    }

    // Gap #20: hide on lose focus
    #[test]
    #[ignore = "gap #20: hide_on_lose_focus not yet in GlobalConfig"]
    fn config_global_hide_on_lose_focus() {
        panic!("add hide_on_lose_focus: bool to GlobalConfig");
    }

    // Gap #43: inactive terminal dimming
    #[test]
    #[ignore = "gap #43: inactive_color_offset not yet in Profile"]
    fn config_profile_inactive_dimming() {
        panic!("add inactive_color_offset: f32 to Profile (0.0-1.0, dims unfocused panes)");
    }

    // Gap #33: cell height/width scaling
    #[test]
    #[ignore = "gap #33: cell_height/cell_width not yet in Profile"]
    fn config_profile_cell_spacing() {
        panic!("add cell_height: f32 and cell_width: f32 to Profile");
    }

    // Gap #8: background image
    #[test]
    #[ignore = "gap #8: background image not yet in Profile"]
    fn config_profile_background_image() {
        panic!("add background_image: Option<String> and background_image_mode: Option<String> to Profile");
    }

    // Gap #14: titlebar per pane
    #[test]
    #[ignore = "gap #14: show_titlebar not yet in Profile"]
    fn config_profile_titlebar() {
        panic!("add show_titlebar: bool to Profile");
    }

    // Gap #36: disable mouse paste
    #[test]
    #[ignore = "gap #36: disable_mouse_paste not yet in Profile"]
    fn config_profile_disable_mouse_paste() {
        panic!("add disable_mouse_paste: bool to Profile");
    }

    // Gap #37: clear selection on copy
    #[test]
    #[ignore = "gap #37: clear_selection_on_copy not yet in Profile"]
    fn config_profile_clear_selection_on_copy() {
        panic!("add clear_selection_on_copy: bool to Profile");
    }

    // Gap #9 (partial): confirm_on_close as enum (always/multiple_terminals/never)
    #[test]
    #[ignore = "gap partial #9: confirm_on_close should be enum not bool"]
    fn config_global_confirm_on_close_modes() {
        panic!("change confirm_on_close from bool to enum: Always, MultipleTerminals, Never");
    }

    #[test]
    fn config_roundtrip_preserves_profiles() {
        let mut cfg = Config::default();
        cfg.active_mut().font.family = "Fira Code".into();
        cfg.active_mut().font.size = 14.5;
        cfg.active_mut().colors.background = "#002b36".into();
        cfg.active_mut().scrollback.history = 25_000;
        cfg.active_mut().transparency.opacity = 0.9;
        cfg.profiles.push(Profile::new_named("Light"));

        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.profiles.len(), 2);
        assert_eq!(parsed.active().font.family, "Fira Code");
        assert!((parsed.active().font.size - 14.5).abs() < f32::EPSILON);
        assert_eq!(parsed.active().colors.background, "#002b36");
        assert_eq!(parsed.active().scrollback.history, 25_000);
        assert!((parsed.active().transparency.opacity - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn palette_round_trips() {
        let mut cfg = Config::default();
        cfg.active_mut().colors.normal.blue = "#1122aa".into();
        cfg.active_mut().colors.bright.red = "#aa2211".into();
        cfg.active_mut().colors.focus_border = "#123456".into();
        cfg.active_mut().colors.selection_background = "#222233".into();

        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.active().colors.normal.blue, "#1122aa");
        assert_eq!(parsed.active().colors.bright.red, "#aa2211");
        assert_eq!(parsed.active().colors.focus_border, "#123456");
        assert_eq!(parsed.active().colors.selection_background, "#222233");
        assert_eq!(parsed.active().palette_rgb()[4], [0x11, 0x22, 0xaa]);
        assert_eq!(parsed.active().focus_border_rgb(), [0x12, 0x34, 0x56]);
        assert_eq!(parsed.active().selection_bg_rgb(), Some([0x22, 0x22, 0x33]));
    }

    #[test]
    fn clear_wipes_scrollback_round_trips_and_defaults_false() {
        assert!(!Profile::default().clear_wipes_scrollback);

        let mut cfg = Config::default();
        cfg.active_mut().clear_wipes_scrollback = true;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert!(parsed.active().clear_wipes_scrollback);

        let toml_text = r##"
            [[profiles]]
            name = "Test"

            [profiles.colors]
            background = "#002b36"
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert!(!cfg.profiles[0].clear_wipes_scrollback);
    }

    #[test]
    fn palette_missing_keys_take_defaults() {
        let toml_text = r##"
            [[profiles]]
            name = "Test"

            [profiles.colors]
            background = "#002b36"
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.profiles[0].colors.background, "#002b36");
        assert_eq!(cfg.profiles[0].palette_rgb()[4], [0x34, 0x65, 0xa4]);
        assert_eq!(cfg.profiles[0].selection_bg_rgb(), None);
        assert_eq!(cfg.profiles[0].focus_border_rgb(), [0x70, 0x70, 0xc0]);
    }

    // ---- word_chars_to_semantic_escape ----

    #[test]
    fn word_chars_single_chars() {
        let esc = word_chars_to_semantic_escape("_-");
        assert!(!esc.contains('_'));
        assert!(!esc.contains('-'));
        assert!(esc.contains(' '));
    }

    #[test]
    fn word_chars_range() {
        let esc = word_chars_to_semantic_escape("a-z");
        assert!(esc.contains(' '));
        assert!(esc.contains(','));
        assert!(!esc.contains('a'));
    }

    #[test]
    fn word_chars_empty() {
        let esc = word_chars_to_semantic_escape("");
        let candidates = ",|:\"' ()[]{}<>\t`│;!^*\\~$#@&%?/.-_=+";
        assert_eq!(esc, candidates);
    }

    #[test]
    fn word_chars_all_candidates_excluded() {
        // `-` at end so it's not parsed as a range operator.
        let word = "-,|:\"' ()[]{}<>\t`│;!^*\\~$#@&%?/._=+";
        let esc = word_chars_to_semantic_escape(word);
        assert!(esc.is_empty(), "remaining: {:?}", esc);
    }

    // ---- effective_history ----

    #[test]
    fn effective_history_finite() {
        let sc = ScrollbackConfig { history: 5000, infinite: false };
        assert_eq!(sc.effective_history(), 5000);
    }

    #[test]
    fn effective_history_infinite() {
        let sc = ScrollbackConfig { history: 5000, infinite: true };
        assert_eq!(sc.effective_history(), 1_000_000);
    }

    // ---- parse_hex edge cases ----

    #[test]
    fn hex_parse_whitespace_trimmed() {
        assert_eq!(parse_hex("  #abcdef  "), Some([0xab, 0xcd, 0xef]));
    }

    #[test]
    fn hex_parse_uppercase() {
        assert_eq!(parse_hex("#ABCDEF"), Some([0xab, 0xcd, 0xef]));
    }

    #[test]
    fn hex_parse_empty() {
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn ensure_consistent_repoints_active_to_first_when_name_missing() {
        let mut cfg = Config {
            active_profile: "Nonexistent".into(),
            profiles: vec![
                Profile::new_named("Solarized"),
                Profile::new_named("Light"),
            ],
            ..Config::default()
        };
        cfg.ensure_consistent();
        // active_profile named a profile that does not exist, so it should be
        // repointed to the first profile's name, not left dangling.
        assert_eq!(cfg.active_profile, "Solarized");
        assert_eq!(cfg.active().name, "Solarized");
    }

    // Regression for M12: a config with a leftover legacy field AND multiple
    // [[profiles]] must keep all parsed profiles, not collapse to one.
    #[test]
    fn migrate_legacy_keeps_existing_profiles() {
        let toml_text = r##"
            active_profile = "Two"

            [colors]
            background = "#002b36"

            [[profiles]]
            name = "One"

            [[profiles]]
            name = "Two"
        "##;
        let mut cfg: Config = toml::from_str(toml_text).unwrap();
        cfg.migrate_legacy();
        cfg.ensure_consistent();

        let names: Vec<&str> = cfg.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["One", "Two"]);
        assert_eq!(cfg.active_profile, "Two");
        // Legacy field is dropped, not merged into the user's profiles.
        assert!(cfg.colors.is_none());
    }

    // ---- SavedLayout per-pane sidecar (profile-on-spawn §8) ----

    #[test]
    fn saved_layout_terminals_sidecar_round_trips() {
        let mut cfg = Config::default();
        cfg.layouts.push(SavedLayout {
            name: "Work".into(),
            template: LayoutTemplate::Terminal,
            terminals: vec![
                TerminalMeta {
                    profile: Some("Dark".into()),
                    cwd: Some(PathBuf::from("/home/aub/src")),
                    command: Some("vim".into()),
                },
                TerminalMeta::default(),
            ],
        });

        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        let layout = &parsed.layouts[0];
        assert_eq!(layout.terminals.len(), 2);
        assert_eq!(layout.terminals[0].profile.as_deref(), Some("Dark"));
        assert_eq!(layout.terminals[0].cwd, Some(PathBuf::from("/home/aub/src")));
        assert_eq!(layout.terminals[0].command.as_deref(), Some("vim"));
        // Second entry round-trips as all-None (today's behavior for a pane
        // with no captured metadata).
        assert_eq!(layout.terminals[1].profile, None);
        assert_eq!(layout.terminals[1].cwd, None);
        assert_eq!(layout.terminals[1].command, None);
    }

    #[test]
    fn saved_layout_missing_terminals_defaults_empty() {
        // An old layout serialized before the sidecar existed: name + template
        // only, no `terminals` array.
        let toml_text = r##"
            [[layouts]]
            name = "Old"
            template = "Terminal"
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.layouts.len(), 1);
        assert_eq!(cfg.layouts[0].name, "Old");
        assert!(cfg.layouts[0].terminals.is_empty());
    }

    #[test]
    fn startup_layout_round_trips_and_defaults_none() {
        // Default config: no startup layout.
        assert_eq!(Config::default().global.startup_layout, None);
        assert_eq!(GlobalConfig::default().startup_layout, None);

        // Round-trips when set.
        let mut cfg = Config::default();
        cfg.global.startup_layout = Some("Work".into());
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.global.startup_layout.as_deref(), Some("Work"));

        // Old config missing the key defaults to None.
        let toml_text = r##"
            [global]
            confirm_on_close = false
        "##;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.global.startup_layout, None);
    }
}
