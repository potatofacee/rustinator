use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::LayoutTemplate;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub global: GlobalConfig,
    pub active_profile: String,
    pub profiles: Vec<Profile>,

    #[serde(default)]
    pub keybindings: Vec<KeyBinding>,

    #[serde(default)]
    pub layouts: Vec<SavedLayout>,

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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalConfig {
    pub confirm_on_close: bool,
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
    #[serde(default = "default_word_chars")]
    pub word_chars: String,
}

fn default_true() -> bool {
    true
}

fn default_word_chars() -> String {
    "-A-Za-z0-9,./?%&#:_=+@~".into()
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
            font: None,
            colors: None,
            scrollback: None,
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self { confirm_on_close: true }
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
            word_chars: default_word_chars(),
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
        }
    }
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self { history: 10_000, infinite: false }
    }
}

impl ScrollbackConfig {
    pub fn effective_history(&self) -> usize {
        if self.infinite { 100_000_000 } else { self.history }
    }
}

impl Default for TransparencyConfig {
    fn default() -> Self {
        Self { opacity: 1.0 }
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
    /// If any legacy field is present we rebuild the profiles list from it,
    /// overriding the serde-supplied default profile.
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
                word_chars: default_word_chars(),
            };
            self.profiles = vec![profile];
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
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path)
    }
}

pub fn format_hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(&home).join("Library/Application Support/rustinator/config.toml"))
    } else if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        Some(PathBuf::from(x).join("rustinator/config.toml"))
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

fn parse_hex(s: &str) -> Option<[u8; 3]> {
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
    #[ignore = "gap #22: custom_command not yet in Profile"]
    fn config_profile_custom_command() {
        panic!("add use_custom_command: bool and custom_command: Option<String> to Profile");
    }

    // Gap #24: login shell option
    #[test]
    #[ignore = "gap #24: login_shell not yet in Profile"]
    fn config_profile_login_shell() {
        panic!("add login_shell: bool to Profile");
    }

    // Gap #23: exit action (close/hold/restart)
    #[test]
    #[ignore = "gap #23: exit_action not yet in Profile"]
    fn config_profile_exit_action() {
        panic!("add exit_action: String to Profile (values: close, hold, restart)");
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
    #[ignore = "gap #28: tab_position not yet in GlobalConfig"]
    fn config_global_tab_position() {
        panic!("add tab_position: String to GlobalConfig (values: top, bottom, left, right, hidden)");
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
}
