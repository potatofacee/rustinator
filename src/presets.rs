//! Built-in color presets for terminal profiles.

pub struct Preset {
    pub name: &'static str,
    pub foreground: &'static str,
    pub background: &'static str,
    pub cursor: &'static str,
}

pub const CUSTOM: &str = "Custom";

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "White on Black",
        foreground: "#e5e5e5",
        background: "#000000",
        cursor: "#e5e5e5",
    },
    Preset {
        name: "Black on White",
        foreground: "#000000",
        background: "#ffffff",
        cursor: "#000000",
    },
    Preset {
        name: "Green on Black",
        foreground: "#00cd00",
        background: "#000000",
        cursor: "#00ff00",
    },
    Preset {
        name: "Orange on Black",
        foreground: "#ffa500",
        background: "#000000",
        cursor: "#ffa500",
    },
    Preset {
        name: "Grey on Black",
        foreground: "#aaaaaa",
        background: "#000000",
        cursor: "#ffffff",
    },
    Preset {
        name: "Solarized Dark",
        foreground: "#839496",
        background: "#002b36",
        cursor: "#93a1a1",
    },
    Preset {
        name: "Solarized Light",
        foreground: "#657b83",
        background: "#fdf6e3",
        cursor: "#586e75",
    },
    Preset {
        name: "Gruvbox Dark",
        foreground: "#ebdbb2",
        background: "#282828",
        cursor: "#ebdbb2",
    },
    Preset {
        name: "Dracula",
        foreground: "#f8f8f2",
        background: "#282a36",
        cursor: "#f8f8f2",
    },
    Preset {
        name: "Nord",
        foreground: "#d8dee9",
        background: "#2e3440",
        cursor: "#d8dee9",
    },
    Preset {
        name: "Tomorrow Night",
        foreground: "#c5c8c6",
        background: "#1d1f21",
        cursor: "#c5c8c6",
    },
    Preset {
        name: "Tango",
        foreground: "#d3d7cf",
        background: "#2e3436",
        cursor: "#d3d7cf",
    },
];

/// Find a preset matching the given (fg, bg, cursor) triple, normalized hex.
pub fn match_preset(fg: &str, bg: &str, cursor: &str) -> Option<&'static str> {
    let fg_n = normalize(fg);
    let bg_n = normalize(bg);
    let cu_n = normalize(cursor);
    PRESETS.iter().find_map(|p| {
        if normalize(p.foreground) == fg_n
            && normalize(p.background) == bg_n
            && normalize(p.cursor) == cu_n
        {
            Some(p.name)
        } else {
            None
        }
    })
}

fn normalize(hex: &str) -> String {
    hex.trim().trim_start_matches('#').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_preset_finds_known() {
        let result = match_preset("#ebdbb2", "#282828", "#ebdbb2");
        assert_eq!(result, Some("Gruvbox Dark"));
    }

    #[test]
    fn match_preset_returns_none_for_unknown() {
        assert!(match_preset("#123456", "#abcdef", "#000000").is_none());
    }

    #[test]
    fn all_presets_have_valid_hex() {
        for p in PRESETS {
            assert!(p.foreground.starts_with('#'), "bad fg in {}", p.name);
            assert!(p.background.starts_with('#'), "bad bg in {}", p.name);
            assert!(p.cursor.starts_with('#'), "bad cursor in {}", p.name);
            assert_eq!(p.foreground.len(), 7, "bad fg len in {}", p.name);
            assert_eq!(p.background.len(), 7, "bad bg len in {}", p.name);
            assert_eq!(p.cursor.len(), 7, "bad cursor len in {}", p.name);
        }
    }

    // ── Gap inventory guardrails ──────────────────────────────────────

    // Terminator has Ambience and palette presets (Tango, Linux, Xterm, Rxvt)
    // that we're missing as full 16-color palette overrides.

    #[test]
    #[ignore = "gap: Ambience color scheme not yet in presets"]
    fn preset_ambience_exists() {
        assert!(PRESETS.iter().any(|p| p.name == "Ambience"));
    }

    #[test]
    #[ignore = "gap: Black on Yellow color scheme not yet in presets"]
    fn preset_black_on_yellow_exists() {
        assert!(PRESETS.iter().any(|p| p.name == "Black on Yellow"));
    }

    #[test]
    #[ignore = "gap: Gruvbox Light color scheme not yet in presets"]
    fn preset_gruvbox_light_exists() {
        assert!(PRESETS.iter().any(|p| p.name == "Gruvbox Light"));
    }

    // 16-color ANSI palette presets (Terminator has Tango/Linux/Xterm/Rxvt/etc.)
    #[test]
    #[ignore = "gap: no 16-color palette presets yet (Tango, Linux, Xterm, Rxvt)"]
    fn palette_presets_exist() {
        panic!("add optional 16-color ANSI palette to Preset struct (Tango, Linux, Xterm, Rxvt)");
    }
}
