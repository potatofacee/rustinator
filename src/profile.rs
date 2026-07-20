//! Profile resolution and cycling.
//!
//! The focused, *pure* owner of "given a profile name, what should a pane look
//! like and what command should it spawn?". Identity is the profile **name**
//! string (mirroring `Config.active_profile` keyed against `Config.profiles`);
//! there is no numeric id. Every function here is a free function with no GL,
//! PTY, or egui dependency, so the whole module is unit-testable in isolation.
//!
//! See `20260630-design-profile-on-spawn.md` §4-§5.

use crate::config::{word_chars_to_semantic_escape, Config, Profile};
use crate::pane::PaneDefaults;

/// A profile's identity is its name, exactly like `Config.active_profile`.
pub type ProfileName = String;

/// The resolved child command for a pane, derived from a profile.
///
/// `program == None` means "platform default shell": on Linux `Pane::spawn`
/// maps it to `$SHELL`; on macOS an empty-args `None` leaves the PTY shell
/// unset so `tty::Options::default()` picks the login shell (today's behavior).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpawnCommand {
    /// `None` => `$SHELL` / platform default (preserves today's macOS path).
    pub program: Option<String>,
    /// Login shell => `["--login"]` or `[]`; custom command => `["-c", cmd]`.
    pub args: Vec<String>,
}

/// Everything a pane needs from its profile at spawn / switch time, bundled so
/// callers resolve once and apply the pieces: render `defaults` live, the term
/// bits into the alacritty `Config`, and `command` into the PTY options.
#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    /// Per-pane render defaults (colors/opacity/palette/selection).
    pub defaults: PaneDefaults,
    /// `Config.scrolling_history` for this profile's scrollback.
    pub scrolling_history: usize,
    /// `Config.semantic_escape_chars` derived from the profile's `word_chars`.
    pub semantic_escape_chars: String,
    /// The child command/shell to spawn.
    pub command: SpawnCommand,
}

/// Resolve a profile NAME to its `&Profile`. A dangling name falls back to
/// `Config::active()` (which itself falls back to `profiles[0]`), reusing the
/// existing fallback semantics so per-pane resolution and `active()` agree.
pub fn resolve<'a>(cfg: &'a Config, name: &str) -> &'a Profile {
    cfg.profiles
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| cfg.active())
}

/// Resolve a name and derive the full `ResolvedProfile` bundle.
pub fn resolved(cfg: &Config, name: &str) -> ResolvedProfile {
    let p = resolve(cfg, name);
    ResolvedProfile {
        defaults: defaults_from_profile(p),
        scrolling_history: p.scrollback.effective_history(),
        semantic_escape_chars: word_chars_to_semantic_escape(&p.word_chars),
        command: spawn_command(p),
    }
}

/// Build the child command vector for a profile.
///
/// - Custom command (`use_custom_command` and a non-blank `custom_command`):
///   run it through the shell as `["-c", cmd]` (Terminator behavior).
/// - Otherwise an interactive shell: `["--login"]` when `login_shell`, else
///   `[]`. `login_shell` defaults true, so the default profile reproduces
///   today's `--login` spawn on Linux exactly.
///
/// `program` is always `None` here; the platform default-shell nuance lives in
/// `Pane::spawn` (see [`SpawnCommand`]).
pub fn spawn_command(p: &Profile) -> SpawnCommand {
    if p.use_custom_command && !p.custom_command.trim().is_empty() {
        SpawnCommand {
            program: None,
            args: vec!["-c".into(), p.custom_command.clone()],
        }
    } else {
        let args = if p.login_shell {
            vec!["--login".into()]
        } else {
            Vec::new()
        };
        SpawnCommand { program: None, args }
    }
}

/// Next profile name in `profiles[]` order, wrapping. Single profile = no-op.
pub fn next_name(cfg: &Config, current: &str) -> ProfileName {
    cycle(cfg, current, 1)
}

/// Previous profile name in `profiles[]` order, wrapping. Single profile = no-op.
pub fn prev_name(cfg: &Config, current: &str) -> ProfileName {
    cycle(cfg, current, -1)
}

/// Step `delta` positions through `profiles[]` from `current`, wrapping. An
/// unknown `current` anchors at index 0. Empty `profiles[]` (never the case for
/// a consistent `Config`) returns `current` unchanged.
fn cycle(cfg: &Config, current: &str, delta: isize) -> ProfileName {
    let len = cfg.profiles.len();
    if len == 0 {
        return current.to_string();
    }
    let cur = cfg
        .profiles
        .iter()
        .position(|p| p.name == current)
        .unwrap_or(0);
    let next = (cur as isize + delta).rem_euclid(len as isize) as usize;
    cfg.profiles[next].name.clone()
}

/// Build the per-pane render defaults from a profile's color settings.
///
/// Relocated verbatim from `main.rs` so profile-derived render state lives with
/// the rest of profile resolution; the mapping is unchanged.
pub fn defaults_from_profile(profile: &Profile) -> PaneDefaults {
    PaneDefaults {
        fg: profile.foreground_rgb(),
        bg: profile.background_rgb(),
        cursor: profile.cursor_rgb(),
        bg_opacity: profile.transparency.opacity,
        palette: profile.palette_rgb(),
        selection_bg: profile.selection_bg_rgb(),
        selection_fg: profile.selection_fg_rgb(),
        clear_wipes_scrollback: profile.clear_wipes_scrollback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Config` with the named profiles and the given active name. Other
    /// fields take their defaults.
    fn cfg_with(names: &[&str], active: &str) -> Config {
        Config {
            active_profile: active.into(),
            profiles: names.iter().map(|n| Profile::new_named(n)).collect(),
            ..Config::default()
        }
    }

    #[test]
    fn resolve_returns_exact_name_hit() {
        let cfg = cfg_with(&["A", "B", "C"], "A");
        assert_eq!(resolve(&cfg, "B").name, "B");
        assert_eq!(resolve(&cfg, "C").name, "C");
    }

    #[test]
    fn resolve_dangling_name_falls_back_to_active() {
        // active_profile = "B"; a name matching nothing resolves to active(),
        // which is "B" (not profiles[0] = "A") — proves the active() fallback.
        let cfg = cfg_with(&["A", "B"], "B");
        assert_eq!(resolve(&cfg, "Nonexistent").name, "B");
    }

    #[test]
    fn resolved_bundles_history_command_and_term_bits() {
        let mut cfg = cfg_with(&["Default"], "Default");
        cfg.profiles[0].scrollback.history = 7777;
        cfg.profiles[0].scrollback.infinite = false;
        let r = resolved(&cfg, "Default");
        assert_eq!(r.scrolling_history, 7777);
        // Default profile => login interactive shell.
        assert_eq!(
            r.command,
            SpawnCommand { program: None, args: vec!["--login".into()] }
        );
        // defaults and semantic_escape_chars flow from the same profile.
        assert_eq!(r.defaults.fg, [0xe5, 0xe5, 0xe5]);
        assert_eq!(
            r.semantic_escape_chars,
            word_chars_to_semantic_escape(&cfg.profiles[0].word_chars)
        );
    }

    #[test]
    fn spawn_command_default_profile_is_login_shell() {
        // login_shell defaults true and there is no custom command.
        let p = Profile::new_named("X");
        assert_eq!(
            spawn_command(&p),
            SpawnCommand { program: None, args: vec!["--login".into()] }
        );
    }

    #[test]
    fn spawn_command_non_login_shell_has_no_args() {
        let mut p = Profile::new_named("X");
        p.login_shell = false;
        assert_eq!(
            spawn_command(&p),
            SpawnCommand { program: None, args: vec![] }
        );
    }

    #[test]
    fn spawn_command_custom_runs_through_shell_dash_c() {
        let mut p = Profile::new_named("X");
        p.use_custom_command = true;
        p.custom_command = "htop".into();
        assert_eq!(
            spawn_command(&p),
            SpawnCommand { program: None, args: vec!["-c".into(), "htop".into()] }
        );
    }

    #[test]
    fn spawn_command_blank_custom_falls_back_to_interactive_shell() {
        let mut p = Profile::new_named("X");
        p.use_custom_command = true;
        p.custom_command = "   ".into(); // whitespace only
        // login_shell still default true -> falls back to --login, not "-c   ".
        assert_eq!(
            spawn_command(&p),
            SpawnCommand { program: None, args: vec!["--login".into()] }
        );
    }

    #[test]
    fn next_name_wraps_around_end() {
        let cfg = cfg_with(&["A", "B", "C"], "A");
        assert_eq!(next_name(&cfg, "A"), "B");
        assert_eq!(next_name(&cfg, "C"), "A"); // wrap
    }

    #[test]
    fn prev_name_wraps_around_start() {
        let cfg = cfg_with(&["A", "B", "C"], "A");
        assert_eq!(prev_name(&cfg, "B"), "A");
        assert_eq!(prev_name(&cfg, "A"), "C"); // wrap
    }

    #[test]
    fn cycle_single_profile_is_noop() {
        let cfg = cfg_with(&["Only"], "Only");
        assert_eq!(next_name(&cfg, "Only"), "Only");
        assert_eq!(prev_name(&cfg, "Only"), "Only");
    }

    #[test]
    fn defaults_from_profile_maps_color_and_opacity() {
        let mut p = Profile::new_named("X");
        p.transparency.opacity = 0.5;
        p.clear_wipes_scrollback = true;
        p.colors.selection_background = String::new(); // empty -> None
        let pd = defaults_from_profile(&p);
        assert!((pd.bg_opacity - 0.5).abs() < f32::EPSILON);
        assert!(pd.clear_wipes_scrollback);
        assert_eq!(pd.palette[4], [0x34, 0x65, 0xa4]); // normal ANSI blue
        assert_eq!(pd.selection_bg, None);
        assert_eq!(pd.fg, [0xe5, 0xe5, 0xe5]);
        assert_eq!(pd.bg, [0x1a, 0x1a, 0x1a]);
    }
}
