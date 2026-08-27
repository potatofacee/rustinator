//! Process-global state split off the former `App` god-struct (WF-E Stage B).
//!
//! `AppShared` holds everything that is genuinely one-per-process — the loaded
//! `Config`, the derived `BindingTable`/`TermConfig`/`PaneDefaults`, the font
//! *anchor* (`base_size`; the family is read live from `user_config`), the prefs
//! panel state, the shared `PaneId` allocator, and the event-loop proxy. Per-window
//! state lives in `AppWindow` (`app_window.rs`). This is a pure relocation: the
//! fields and the config-apply logic are moved verbatim from `App`, not redesigned.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::tty;
use winit::event_loop::EventLoopProxy;

use crate::app_window::FontView;
use crate::config::{self, Config};
use crate::keybindings::BindingTable;
use crate::pane::{PaneDefaults, PaneId};
use crate::prefs_ui::PrefsState;
use crate::profile::{self, defaults_from_profile};
use crate::tabs::{self, PaneFactory};
use crate::window;

/// What each window must re-apply after a global config change. Produced by
/// `AppShared::apply_global` (the shared half of the old `apply_prefs`) and
/// consumed by `AppWindow::apply_config` (the per-window half). Today's single
/// window consumes exactly one; the field describes *whether* a font reload is
/// needed — the pane-defaults / term-config push is unconditional, exactly as
/// the old `apply_prefs` tail always ran.
pub(crate) struct ConfigDelta {
    /// `Some` iff the font family or size changed and each window must rebuild
    /// its `FontContext` + `Renderer`. `None` leaves per-window fonts untouched.
    pub(crate) font_reload: Option<FontReload>,
}

/// The font family+size a window must reload, plus whether the *size* changed
/// (the M10 zoom-anchor reset: on a size change the live Ctrl+=/Ctrl+- override
/// is dropped and `base_size` is re-anchored — but only after the reload
/// succeeds, matching the old gated behavior).
pub(crate) struct FontReload {
    pub(crate) family: String,
    pub(crate) size: f32,
    pub(crate) size_changed: bool,
}

pub(crate) struct AppShared {
    pub(crate) user_config: Config,
    pub(crate) bindings: BindingTable,
    pub(crate) term_config: TermConfig,
    pub(crate) pane_defaults: PaneDefaults,
    /// Font size anchor (the configured/prefs size). The live per-window
    /// `size_override`/`zoom_scale`/`scale_factor` live on `FontView`; the anchor
    /// is process-global because it tracks the one shared `Config`. Re-anchored on
    /// a prefs size change (M10), gated on the per-window reload succeeding.
    pub(crate) base_size: f32,
    pub(crate) prefs: PrefsState,
    /// Last pane defaults pushed as a live preview of the prefs draft. `Some(..)`
    /// while the prefs panel previews unapplied colors; the terminal window's
    /// `logic()` restores the saved profile's colors when prefs closes without Apply.
    pub(crate) preview_defaults: Option<PaneDefaults>,
    pub(crate) hotkey_changed: bool,
    pub(crate) cursor_blink_epoch: Instant,
    pub(crate) event_loop_proxy: EventLoopProxy<window::UserEvent>,
    /// One process-wide `PaneId` source. Every new pane draws its id via
    /// `alloc_pane_id`, so two `TabManager`s that clone this `Arc` never collide.
    pub(crate) pane_id_alloc: Arc<AtomicU64>,
}

impl AppShared {
    /// Build the process-global state: load the config and derive the binding
    /// table, base term config, pane defaults, and font anchor from the active
    /// profile — the shared half of the old `App::new`. Runs `tty::setup_env`
    /// once here (before any pane spawns), exactly as `App::new` did first.
    pub(crate) fn new(event_loop_proxy: EventLoopProxy<window::UserEvent>) -> AppShared {
        tty::setup_env();

        let user_config = Config::load();
        let profile = user_config.active();
        let base_size = profile.font.size;

        let mut term_config = TermConfig::default();
        term_config.scrolling_history = profile.scrollback.effective_history();
        term_config.kitty_keyboard = true;
        term_config.semantic_escape_chars =
            config::word_chars_to_semantic_escape(&profile.word_chars);
        term_config.default_cursor_style = profile.cursor_shape.term_style();

        let pane_defaults = defaults_from_profile(profile);

        let mut bindings = BindingTable::new(user_config.global.use_linux_keybindings);
        bindings.apply_user(
            &user_config
                .keybindings
                .iter()
                .map(|b| (b.action.clone(), b.key.clone()))
                .collect::<Vec<_>>(),
        );

        // The first pane draws id 1 from this (the manager bumps to 2), matching
        // the historic hardcoded first id and the old `next_pane_id` sequence.
        let pane_id_alloc = Arc::new(AtomicU64::new(1));

        AppShared {
            user_config,
            bindings,
            term_config,
            pane_defaults,
            base_size,
            prefs: PrefsState::new(),
            preview_defaults: None,
            hotkey_changed: false,
            cursor_blink_epoch: Instant::now(),
            event_loop_proxy,
            pane_id_alloc,
        }
    }

    /// Draw the next unique `PaneId` from the process-wide allocator.
    pub(crate) fn next_pane_id(&self) -> PaneId {
        tabs::alloc_pane_id(&self.pane_id_alloc)
    }

    /// Build a `PaneFactory` for a specific profile NAME, reading cell metrics
    /// from the given per-window font. Resolves the name to its appearance +
    /// scrollback + child command via `profile::resolved`; `term_config` is
    /// cloned from the shared base so fields like `kitty_keyboard` survive.
    pub(crate) fn pane_factory_for(&self, name: &str, font: &FontView) -> PaneFactory {
        let resolved = profile::resolved(&self.user_config, name);
        let mut term_config = self.term_config.clone();
        term_config.scrolling_history = resolved.scrolling_history;
        term_config.semantic_escape_chars = resolved.semantic_escape_chars;
        term_config.default_cursor_style = resolved.cursor_style;
        PaneFactory {
            cell_w: font.cell_w,
            cell_h: font.cell_h,
            term_config,
            pane_defaults: resolved.defaults,
            event_loop_proxy: self.event_loop_proxy.clone(),
            profile: name.to_string(),
            command: resolved.command,
        }
    }

    /// Factory for the ACTIVE profile (first pane / `new_tab`). Equals
    /// `pane_factory_for(active_profile, font)`.
    pub(crate) fn pane_factory(&self, font: &FontView) -> PaneFactory {
        self.pane_factory_for(&self.user_config.active_profile, font)
    }

    /// Open the prefs panel, seeding its draft from the current config.
    pub(crate) fn open_prefs(&mut self) {
        self.prefs.open(&self.user_config);
    }

    /// Wake the main event loop so terminal windows repaint (governed).
    pub(crate) fn wake_main(&self) {
        let _ = self.event_loop_proxy.send_event(window::UserEvent::Repaint);
    }

    /// The GLOBAL half of the old `apply_prefs`: swap in the new config and
    /// re-derive bindings / term config / pane defaults, flag a hotkey-window
    /// rebuild if its config changed, and describe what each window must re-apply
    /// as a `ConfigDelta`. The per-window font reload + defaults/term push live in
    /// `AppWindow::apply_config`. The base-size re-anchor (M10) is detected here
    /// (`size_changed`) but committed there, gated on the reload succeeding —
    /// preserving the old behavior where a failed reload left the anchor unchanged.
    pub(crate) fn apply_global(&mut self, new_config: Config) -> ConfigDelta {
        let old_profile = self.user_config.active().clone();
        let old_hk = self.user_config.hotkey_window.clone();
        self.user_config = new_config;
        self.prefs.draft = self.user_config.clone();
        let profile = self.user_config.active().clone();
        let new_hk = &self.user_config.hotkey_window;
        if old_hk.enabled != new_hk.enabled
            || old_hk.hotkey != new_hk.hotkey
            || old_hk.height_percent != new_hk.height_percent
            || old_hk.hide_on_focus_loss != new_hk.hide_on_focus_loss
            || old_hk.always_on_top != new_hk.always_on_top
        {
            self.hotkey_changed = true;
        }

        self.bindings = BindingTable::new(self.user_config.global.use_linux_keybindings);
        self.bindings.apply_user(
            &self
                .user_config
                .keybindings
                .iter()
                .map(|b| (b.action.clone(), b.key.clone()))
                .collect::<Vec<_>>(),
        );

        self.pane_defaults = defaults_from_profile(&profile);
        self.term_config.scrolling_history = profile.scrollback.effective_history();
        self.term_config.semantic_escape_chars =
            config::word_chars_to_semantic_escape(&profile.word_chars);
        self.term_config.default_cursor_style = profile.cursor_shape.term_style();

        let size_changed = (profile.font.size - old_profile.font.size).abs() > 0.001;
        let font_changed = profile.font.family != old_profile.font.family || size_changed;
        let font_reload = if font_changed {
            Some(FontReload {
                family: profile.font.family.clone(),
                size: profile.font.size,
                size_changed,
            })
        } else {
            None
        };

        ConfigDelta { font_reload }
    }
}
