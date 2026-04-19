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
