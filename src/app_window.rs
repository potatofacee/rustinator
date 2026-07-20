//! Per-window state split off the former `App` god-struct (WF-E Stage B).
//!
//! `AppWindow` owns everything that is one-per-terminal-window: its `GlWindow`
//! (window + surface + painter + egui context), its `TabManager`, its `Renderer`
//! (own glyph atlas + cell metrics), the per-window `FontView`, and the dialog /
//! pane-view / tab-bar UI state. Process-global state (config, bindings, prefs,
//! the `PaneId` allocator, the font anchor) lives in `AppShared` and is threaded
//! into the methods that need it. This is a pure relocation of the old `App`
//! methods (`self.user_config` -> `shared.user_config`, `self.egui_ctx` ->
//! `self.gl_window.egui_ctx`, `self.render.renderer` -> `self.renderer`); the
//! runtime behavior is unchanged.

use std::sync::{Arc, Mutex};

use crate::app_shared::{AppShared, ConfigDelta};
use crate::dialogs::{DialogAction, DialogState};
use crate::font::{self, FontContext};
use crate::gl_window::GlWindow;
use crate::groups::{self, BroadcastScope};
use crate::input;
use crate::keybindings::Action;
use crate::layout::Direction;
use crate::pane::{Pane, PaneDefaults, PaneId};
use crate::pane_ui::{self, PaneViewCtx, PaneViewState};
use crate::prefs_ui::PrefsResult;
use crate::profile::{self, defaults_from_profile, ProfileName};
use crate::renderer::Renderer;
use crate::tab_bar::{self, TabBarState, TabBarView};
use crate::tabs::{self, FocusDir, PaneFactory, TabManager};
use crate::RawTermKey;

/// Per-window font state: cell metrics, the live zoom/DPI multipliers, and the
/// shared `FontContext`. The size *anchor* (`base_size`) lives in `AppShared`
/// and is threaded in as a parameter — every method already took the family
/// string, so `base_size` rides alongside it. Renamed/relocated from the former
/// `FontState`; `base_size` is the only field lifted out (to `AppShared`).
pub(crate) struct FontView {
    pub(crate) ctx: Arc<Mutex<FontContext>>,
    pub(crate) cell_w: f32,
    pub(crate) cell_h: f32,
    pub(crate) size_override: Option<f32>,
    pub(crate) scale_factor: f32,
    /// Global font multiplier driven by scaled-zoom (Leaf 1). 1.0 = no scaling,
    /// so every non-zoom font path is a zero-behavior-change pass-through.
    pub(crate) zoom_scale: f32,
}

impl FontView {
    /// Rasterization size in physical pixels: the logical size (override or the
    /// `base_size` anchor) times the scaled-zoom multiplier times the display
    /// scale factor. With `zoom_scale == 1.0` this equals `size * scale_factor`.
    fn effective_px(&self, base_size: f32) -> f32 {
        self.size_override.unwrap_or(base_size) * self.zoom_scale * self.scale_factor
    }

    /// Rebuild the FontContext at `effective_px()`, reload the GPU renderer, swap
    /// the shared context, and mark every pane dirty. Single source of truth for
    /// the reset/apply/scale/zoom font paths.
    fn rebuild(
        &mut self,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        if let Ok(fc) = FontContext::new(font_family, self.effective_px(base_size)) {
            self.cell_w = fc.cell_width();
            self.cell_h = fc.cell_height();
            {
                let mut renderer = renderer.lock().unwrap();
                renderer.reload_font(&fc);
            }
            *self.ctx.lock().unwrap() = fc;
            for tab in &mut tab_mgr.tabs {
                for pane in tab.panes.values_mut() {
                    pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                    pane.cached = None;
                }
            }
        }
    }

    fn adjust_font_size(
        &mut self,
        delta: f32,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        let current = self.size_override.unwrap_or(base_size);
        let new_size = (current + delta).clamp(4.0, 72.0);
        self.apply_font_size(new_size, renderer, base_size, font_family, tab_mgr);
    }

    fn reset_font_size(
        &mut self,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        self.size_override = None;
        self.rebuild(renderer, base_size, font_family, tab_mgr);
    }

    fn apply_font_size(
        &mut self,
        size: f32,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        self.size_override = Some(size);
        self.rebuild(renderer, base_size, font_family, tab_mgr);
    }

    fn set_scale_factor(
        &mut self,
        scale_factor: f32,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        if (scale_factor - self.scale_factor).abs() < f32::EPSILON {
            return;
        }
        self.scale_factor = scale_factor;
        self.rebuild(renderer, base_size, font_family, tab_mgr);
    }

    /// Set the scaled-zoom font multiplier and rebuild. `scale == 1.0` returns to
    /// the unscaled font. No-op if the multiplier is unchanged.
    fn set_zoom_scale(
        &mut self,
        scale: f32,
        renderer: &Arc<Mutex<Renderer>>,
        base_size: f32,
        font_family: &str,
        tab_mgr: &mut TabManager,
    ) {
        if (scale - self.zoom_scale).abs() < f32::EPSILON {
            return;
        }
        self.zoom_scale = scale;
        self.rebuild(renderer, base_size, font_family, tab_mgr);
    }

    fn reload_font(
        &mut self,
        family: &str,
        size: f32,
        renderer: &Arc<Mutex<Renderer>>,
    ) -> Result<(), crossfont::Error> {
        let new_font = FontContext::new(family, size * self.scale_factor)?;
        let cell_w = new_font.cell_width();
        let cell_h = new_font.cell_height();
        renderer.lock().unwrap().reload_font(&new_font);
        *self.ctx.lock().unwrap() = new_font;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        Ok(())
    }
}

pub(crate) struct AppWindow {
    pub(crate) gl_window: GlWindow,
    tab_mgr: TabManager,
    renderer: Arc<Mutex<Renderer>>,
    font: FontView,
    pub(crate) dialogs: DialogState,
    pub(crate) pane_view: PaneViewState,
    /// Persistent tab-bar UI state (drag + inline-rename).
    tab_bar: TabBarState,
    current_title: String,
    pub(crate) fullscreen_pending: bool,
    pub(crate) pending_zoom_steps: i32,
    /// Set when this window regains OS focus (e.g. via Cmd+Tab). On the next
    /// logic pass, stale egui keyboard focus is cleared so terminal input flows
    /// again without requiring a click. See `logic`.
    focus_regained: bool,
    pub(crate) pending_raw_keys: Vec<RawTermKey>,
}

/// Whether `execute_pane_actions` may keep processing the action batch. Returns
/// false once the tab list is empty, so an action that emptied it (ClosePane on
/// the last tab) halts the batch before a later index-based action panics on
/// `tabs[active_tab]` (bug M1).
fn can_process_more_actions(tab_count: usize) -> bool {
    tab_count != 0
}

/// Push a profile's render defaults onto a single pane: the struct copy, the
/// PTY-shared copy, and the `clear_wipes_scrollback` atomic the PTY parser
/// reads — then mark the pane dirty and drop its cached frame. Shared by the
/// prefs-apply and live-preview paths so both keep the atomic in sync.
fn apply_pane_defaults(pane: &mut Pane, pd: PaneDefaults) {
    pane.defaults = pd;
    *pane.shared_defaults.lock().unwrap() = pd;
    pane.clear_wipes_scrollback
        .store(pd.clear_wipes_scrollback, std::sync::atomic::Ordering::Relaxed);
    pane.dirty.store(true, std::sync::atomic::Ordering::Release);
    pane.cached = None;
}

impl AppWindow {
    /// Build the primary terminal window from an already-created `GlWindow` and
    /// the shared GL context handle, spawning its first pane. The per-window half
    /// of the old `App::new`: font load (with fallback), renderer init, first-pane
    /// spawn, and startup-layout realization. `shared` supplies the config, term
    /// config, pane defaults, `PaneId` allocator, and event-loop proxy.
    pub(crate) fn new_primary(
        gl_window: GlWindow,
        gl: Arc<glow::Context>,
        shared: &mut AppShared,
        scale_factor: f32,
    ) -> Result<AppWindow, String> {
        let profile = shared.user_config.active();

        let fallback_font = if cfg!(target_os = "macos") { "Menlo" } else { "monospace" };
        let font_ctx = FontContext::new(&profile.font.family, profile.font.size * scale_factor)
            .or_else(|e| {
                eprintln!(
                    "font '{}' failed ({e}), falling back to '{fallback_font}'",
                    profile.font.family
                );
                FontContext::new(fallback_font, profile.font.size * scale_factor)
            })
            .map_err(|e| format!("failed to load any font: {e}"))?;
        let m = font_ctx.metrics;
        eprintln!(
            "font loaded: family={} size={:.1}pt advance={:.2}px line_height={:.2}px",
            profile.font.family, profile.font.size, m.average_advance, m.line_height,
        );
        let cell_w = font_ctx.cell_width();
        let cell_h = font_ctx.cell_height();

        let renderer = Renderer::new(gl, &font_ctx)
            .map_err(|e| format!("GPU renderer init failed: {e}"))?;

        let first_id: PaneId = shared.next_pane_id();
        let pane = Pane::spawn(
            first_id,
            tabs::INITIAL_COLS as usize,
            tabs::INITIAL_LINES as usize,
            cell_w,
            cell_h,
            shared.term_config.clone(),
            shared.pane_defaults,
            Some(shared.event_loop_proxy.clone()),
            None,
            shared.user_config.active_profile.clone(),
            profile::spawn_command(shared.user_config.active()),
        )
        .map_err(|e| format!("first pane: {e}"))?;

        let mut window = AppWindow {
            gl_window,
            tab_mgr: TabManager::new(pane, Arc::clone(&shared.pane_id_alloc)),
            renderer: Arc::new(Mutex::new(renderer)),
            font: FontView {
                ctx: Arc::new(Mutex::new(font_ctx)),
                cell_w,
                cell_h,
                size_override: None,
                scale_factor,
                zoom_scale: 1.0,
            },
            dialogs: DialogState::new(),
            pane_view: PaneViewState::new(),
            tab_bar: TabBarState::new(),
            current_title: "rustinator".into(),
            fullscreen_pending: false,
            pending_zoom_steps: 0,
            focus_regained: false,
            pending_raw_keys: Vec::new(),
        };
        window.apply_startup_layout(shared);
        Ok(window)
    }

    pub(crate) fn should_confirm_close(&self, shared: &AppShared) -> bool {
        shared.user_config.global.confirm_on_close && self.tab_mgr.total_alive_panes() > 1
    }

    fn update_window_title(&mut self) {
        if self.tab_mgr.tabs.is_empty() {
            return;
        }
        let tab = self.tab_mgr.active_tab();
        let focused_title = tab
            .panes
            .get(&tab.focused)
            .and_then(|p| p.title())
            .unwrap_or_default();
        let desired = if focused_title.is_empty() {
            "rustinator".to_string()
        } else {
            format!("{} — rustinator", focused_title)
        };
        if desired != self.current_title {
            self.gl_window
                .egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Title(desired.clone()));
            self.current_title = desired;
        }
    }

    fn execute_pane_actions(&mut self, shared: &mut AppShared, ctx: &egui::Context, actions: Vec<Action>) {
        let factory = shared.pane_factory(&self.font);
        for action in actions {
            // M1: a prior action in this batch (e.g. ClosePane on the last tab)
            // may have emptied the tab list; bail before an index-based action
            // (RotateCW, SetTitle, …) panics on tabs[active_tab].
            if !can_process_more_actions(self.tab_mgr.tabs.len()) {
                break;
            }
            match action {
                Action::SplitHorizontal => self.split_inheriting(shared, Direction::Horizontal),
                Action::SplitVertical => self.split_inheriting(shared, Direction::Vertical),
                Action::ClosePane => self.tab_mgr.close_focused(&self.gl_window.egui_ctx, &mut self.dialogs),
                Action::FocusNext => self.tab_mgr.cycle_focus(1),
                Action::FocusPrev => self.tab_mgr.cycle_focus(-1),
                Action::NewTab => {
                    // Honor `new_tab_after_current`: compute the insert slot via
                    // the shared pure helper (same one tab_bar::apply uses), then
                    // insert there instead of always appending.
                    let idx = tabs::tab_insert_index(
                        self.tab_mgr.active_tab,
                        shared.user_config.global.new_tab_after_current,
                        self.tab_mgr.tabs.len(),
                    );
                    self.tab_mgr.new_tab_at(idx, &factory);
                }
                Action::NextTab => self.tab_mgr.switch_tab(1),
                Action::PrevTab => self.tab_mgr.switch_tab(-1),
                Action::OpenPrefs => shared.open_prefs(),
                Action::Copy => self.tab_mgr.copy_selection(&self.gl_window.egui_ctx, shared.user_config.active().smart_copy),
                Action::Paste => self.tab_mgr.paste_from_clipboard(),
                Action::ToggleZoom => self.tab_mgr.toggle_zoom(),
                Action::ScaledZoom => self.toggle_scaled_zoom(shared),
                Action::ToggleBroadcast => self.tab_mgr.toggle_broadcast(),
                Action::GroupAll | Action::UngroupAll | Action::GroupTab | Action::UngroupTab
                | Action::UngroupWin | Action::BroadcastOff | Action::BroadcastGroup | Action::BroadcastAll
                | Action::InsertNumber | Action::InsertPadded => self.dispatch_group_action(action),
                Action::NextProfile | Action::PreviousProfile => self.dispatch_profile_action(shared, action),
                Action::ToggleSearch => self.tab_mgr.toggle_search(&mut self.dialogs),
                Action::ZoomIn => self.font.adjust_font_size(1.0, &self.renderer, shared.base_size, &shared.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::ZoomOut => self.font.adjust_font_size(-1.0, &self.renderer, shared.base_size, &shared.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::ZoomReset => self.font.reset_font_size(&self.renderer, shared.base_size, &shared.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::CloseWindow => {
                    if self.should_confirm_close(shared) {
                        self.dialogs.close_dialog_open = true;
                    } else {
                        self.dialogs.confirmed_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Action::ToggleFullscreen => self.fullscreen_pending = !self.fullscreen_pending,
                Action::ResizeLeft => self.tab_mgr.resize_split(-1, false, self.pane_view.last_root_rect),
                Action::ResizeRight => self.tab_mgr.resize_split(1, false, self.pane_view.last_root_rect),
                Action::ResizeUp => self.tab_mgr.resize_split(-1, true, self.pane_view.last_root_rect),
                Action::ResizeDown => self.tab_mgr.resize_split(1, true, self.pane_view.last_root_rect),
                Action::ResetTerminal => self.tab_mgr.reset_focused_terminal(false),
                Action::ResetClear => self.tab_mgr.reset_focused_terminal(true),
                Action::NewWindow => {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).spawn();
                    }
                }
                Action::OpenTerminalHere => self.split_here_inheriting(shared),
                Action::QuitHotkeyWindow => {
                    self.dialogs.confirmed_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Action::SwitchToTab(n) => self.tab_mgr.switch_tab_direct(n),
                Action::MoveTabLeft => self.tab_mgr.move_tab(-1),
                Action::MoveTabRight => self.tab_mgr.move_tab(1),
                Action::GoUp => self.tab_mgr.focus_direction(FocusDir::Up, self.pane_view.last_root_rect),
                Action::GoDown => self.tab_mgr.focus_direction(FocusDir::Down, self.pane_view.last_root_rect),
                Action::GoLeft => self.tab_mgr.focus_direction(FocusDir::Left, self.pane_view.last_root_rect),
                Action::GoRight => self.tab_mgr.focus_direction(FocusDir::Right, self.pane_view.last_root_rect),
                Action::GoNext => self.tab_mgr.cycle_focus(1),
                Action::GoPrev => self.tab_mgr.cycle_focus(-1),
                Action::RotateCW => self.tab_mgr.tabs[self.tab_mgr.active_tab].layout.rotate_cw(),
                Action::RotateCCW => self.tab_mgr.tabs[self.tab_mgr.active_tab].layout.rotate_ccw(),
                Action::SplitAuto => {
                    let dir = match self.pane_view.last_pane_rect {
                        Some(r) if r.width() >= r.height() => Direction::Vertical,
                        _ => Direction::Horizontal,
                    };
                    self.split_inheriting(shared, dir);
                }
                Action::ToggleScrollbar => {
                    if let Some(pane) = self.tab_mgr.active_pane_mut() {
                        pane.scrollbar_visible = !pane.scrollbar_visible;
                    }
                }
                Action::HideWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                Action::ToggleReadOnly => {
                    if let Some(pane) = self.tab_mgr.active_pane_mut() {
                        pane.read_only = !pane.read_only;
                    }
                }
                Action::SetTitle => {
                    let current = self.tab_mgr.tabs[self.tab_mgr.active_tab].custom_title
                        .clone()
                        .unwrap_or_default();
                    self.dialogs.title_dialog_buf = current;
                    self.dialogs.title_dialog_open = true;
                }
                // Gap #4: open the Layout Launcher modal (Alt+L). The dialog is
                // owned/drawn by dialogs.rs; this only flips its open flag.
                Action::LayoutLauncher => self.dialogs.layout_launcher_dialog = true,
            }
        }
    }

    /// Routes the 10 group/broadcast actions out of `execute_pane_actions`. Each
    /// arm delegates to a `groups::` orchestration fn or sets the broadcast scope
    /// — the group logic lives in `groups.rs`. `UngroupWin` clears every pane
    /// because today's single window IS all of the tabs.
    fn dispatch_group_action(&mut self, action: Action) {
        match action {
            Action::GroupAll => groups::group_all(&mut self.tab_mgr.tabs),
            Action::UngroupAll => groups::ungroup_all(&mut self.tab_mgr.tabs),
            Action::GroupTab => groups::group_active_tab(&mut self.tab_mgr.tabs, self.tab_mgr.active_tab),
            Action::UngroupTab => groups::ungroup_active_tab(&mut self.tab_mgr.tabs, self.tab_mgr.active_tab),
            Action::UngroupWin => groups::ungroup_all(&mut self.tab_mgr.tabs),
            Action::BroadcastOff => self.tab_mgr.broadcast_scope = BroadcastScope::Off,
            Action::BroadcastGroup => self.tab_mgr.broadcast_scope = BroadcastScope::Group,
            Action::BroadcastAll => self.tab_mgr.broadcast_scope = BroadcastScope::All,
            Action::InsertNumber => groups::insert_index(&self.tab_mgr, false),
            Action::InsertPadded => groups::insert_index(&self.tab_mgr, true),
            _ => {}
        }
    }

    /// Routes the two profile-cycling actions out of `execute_pane_actions`.
    /// Reads the focused pane's profile, asks `profile::next_name`/`prev_name`
    /// for the neighbouring name, then live re-styles the focused pane to it.
    fn dispatch_profile_action(&mut self, shared: &AppShared, action: Action) {
        let Some(current) = self.tab_mgr.active_pane().map(|p| p.profile.clone()) else {
            return;
        };
        let name = match action {
            Action::NextProfile => profile::next_name(&shared.user_config, &current),
            Action::PreviousProfile => profile::prev_name(&shared.user_config, &current),
            _ => return,
        };
        let pane_id = self.tab_mgr.active_tab().focused;
        self.switch_pane_profile(shared, pane_id, name);
    }

    /// Switch a live pane to a profile by NAME: re-apply that profile's colors,
    /// scrollback, and semantic-escape chars to the *running* terminal and store
    /// the new profile + resolved command — but NEVER respawn (Terminator: a
    /// switch is a re-style). Reuses the same `apply_pane_defaults` +
    /// `apply_term_config` machinery as the prefs-apply path.
    fn switch_pane_profile(&mut self, shared: &AppShared, pane_id: PaneId, name: ProfileName) {
        let resolved = profile::resolved(&shared.user_config, &name);
        let mut term_config = shared.term_config.clone();
        term_config.scrolling_history = resolved.scrolling_history;
        term_config.semantic_escape_chars = resolved.semantic_escape_chars;
        let Some(pane) = self
            .tab_mgr
            .tabs
            .iter_mut()
            .find_map(|t| t.panes.get_mut(&pane_id))
        else {
            return;
        };
        pane.profile = name;
        pane.spawn_command = resolved.command;
        apply_pane_defaults(pane, resolved.defaults);
        pane.apply_term_config(term_config);
    }

    /// Split the focused pane, inheriting ITS profile rather than the active one
    /// (Terminator "always split with profile").
    fn split_inheriting(&mut self, shared: &AppShared, dir: Direction) {
        let factory = self.inherited_factory(shared);
        self.tab_mgr.split(dir, &factory);
    }

    /// `OpenTerminalHere` form of `split_inheriting`: same inherited-profile
    /// factory, routed through `split_here` (direction picked from the pane rect).
    fn split_here_inheriting(&mut self, shared: &AppShared) {
        let factory = self.inherited_factory(shared);
        self.tab_mgr.split_here(self.pane_view.last_pane_rect, &factory);
    }

    /// Factory built for the focused pane's profile, so a split inherits the
    /// parent's profile/command/appearance. Falls back to the active-profile
    /// factory when there is no focused pane.
    fn inherited_factory(&self, shared: &AppShared) -> PaneFactory {
        match self.tab_mgr.active_pane().map(|p| p.profile.clone()) {
            Some(name) => shared.pane_factory_for(&name, &self.font),
            None => shared.pane_factory(&self.font),
        }
    }

    /// Restore a saved layout BY NAME into the active tab, reusing
    /// `TabManager::restore_layout`. No-op for an unknown name.
    fn restore_named_layout(&mut self, shared: &AppShared, name: &str) {
        let Some(saved) = shared
            .user_config
            .layouts
            .iter()
            .find(|l| l.name == name)
            .cloned()
        else {
            return;
        };
        let factory = shared.pane_factory(&self.font);
        self.tab_mgr
            .restore_layout(&saved, &factory, &shared.user_config);
    }

    /// On startup, realize `global.startup_layout` (if set) into the first tab.
    fn apply_startup_layout(&mut self, shared: &AppShared) {
        if let Some(name) = shared.user_config.global.startup_layout.clone() {
            self.restore_named_layout(shared, &name);
        }
    }

    /// Scaled zoom (Leaf 1): maximize the focused pane like ToggleZoom, but also
    /// scale the global font so the same content fills the larger area at a bigger
    /// size. Toggling off restores the unscaled font.
    fn toggle_scaled_zoom(&mut self, shared: &AppShared) {
        let family = shared.user_config.active().font.family.clone();
        if self.tab_mgr.active_tab().zoomed.is_some() {
            self.tab_mgr.toggle_zoom();
            self.font.set_zoom_scale(1.0, &self.renderer, shared.base_size, &family, &mut self.tab_mgr);
        } else {
            let factor = match (self.pane_view.last_pane_rect, self.pane_view.last_root_rect) {
                (Some(pane), Some(root)) => {
                    font::scaled_zoom_factor(pane.width(), pane.height(), root.width(), root.height())
                }
                _ => 1.0,
            };
            self.tab_mgr.toggle_zoom();
            self.font.set_zoom_scale(factor, &self.renderer, shared.base_size, &family, &mut self.tab_mgr);
        }
    }

    /// Invariant for scaled zoom: the global font multiplier is non-1.0 only while
    /// the active tab is zoomed. Resets to 1.0 once nothing is zoomed. Runs once
    /// per logic pass.
    fn sync_zoom_scale(&mut self, shared: &AppShared) {
        if self.tab_mgr.tabs.is_empty() {
            return;
        }
        if self.tab_mgr.active_tab().zoomed.is_none()
            && (self.font.zoom_scale - 1.0).abs() > f32::EPSILON
        {
            let family = shared.user_config.active().font.family.clone();
            self.font.set_zoom_scale(1.0, &self.renderer, shared.base_size, &family, &mut self.tab_mgr);
        }
    }

    /// The PER-WINDOW half of the old `apply_prefs`: reload this window's font if
    /// the config changed the family/size, then push the new pane defaults + term
    /// config onto every live pane. The base-size re-anchor (M10) is committed
    /// here on a successful reload, gated on `size_changed` — exactly as the old
    /// code updated `base_size`/`size_override` only inside the reload `Ok` arm.
    pub(crate) fn apply_config(&mut self, shared: &mut AppShared, delta: &ConfigDelta) {
        if let Some(fr) = &delta.font_reload {
            match self.font.reload_font(&fr.family, fr.size, &self.renderer) {
                Ok(()) => {
                    // M10: a prefs size change is the new zoom anchor. Update the
                    // shared anchor and drop any live Ctrl+=/Ctrl+- override so
                    // Ctrl+0 resets to the new size. Gated on a size change so a
                    // family-only change keeps the user's current zoom.
                    if fr.size_changed {
                        shared.base_size = fr.size;
                        self.font.size_override = None;
                    }
                    let cell_w = self.font.cell_w;
                    let cell_h = self.font.cell_h;
                    for tab in &mut self.tab_mgr.tabs {
                        for pane in tab.panes.values_mut() {
                            pane.force_pty_resize(cell_w, cell_h);
                        }
                    }
                }
                Err(e) => {
                    shared.prefs.status = Some(format!("Font reload failed: {e}"));
                }
            }
        }

        let pd = shared.pane_defaults;
        for tab in &mut self.tab_mgr.tabs {
            for pane in tab.panes.values_mut() {
                apply_pane_defaults(pane, pd);
                // Apply scrollback history + semantic escape chars (and the rest
                // of the term config) to the live Term, so existing panes pick up
                // the change immediately rather than only on respawn.
                pane.apply_term_config(shared.term_config.clone());
            }
        }
    }

    pub(crate) fn draw_prefs_content(&mut self, shared: &mut AppShared, ui: &mut egui::Ui) {
        let result = shared.prefs.draw(ui, &shared.bindings);
        match result {
            PrefsResult::Applied(config) => {
                let delta = shared.apply_global(config);
                self.apply_config(shared, &delta);
                match shared.user_config.save() {
                    Ok(path) => shared.prefs.status = Some(format!("Saved to {}", path.display())),
                    Err(e) => shared.prefs.status = Some(format!("Save failed: {e}")),
                }
            }
            PrefsResult::Cancelled => {}
            PrefsResult::None => {}
        }
        self.preview_draft_colors(shared);
    }

    /// Live preview: while the prefs panel is open, panes render the draft
    /// profile's colors so tweaks are visible before Apply. Pushes only on change
    /// and wakes the main window. `logic()` reverts to the saved profile when
    /// prefs closes without Apply.
    fn preview_draft_colors(&mut self, shared: &mut AppShared) {
        if !shared.prefs.open {
            // Prefs just closed via Cancel during this paint: restore now.
            if shared.preview_defaults.take().is_some() {
                let pd = defaults_from_profile(shared.user_config.active());
                self.push_pane_defaults(pd);
                shared.wake_main();
            }
            return;
        }
        let pd = defaults_from_profile(shared.prefs.draft.active());
        if shared.preview_defaults != Some(pd) {
            shared.preview_defaults = Some(pd);
            self.push_pane_defaults(pd);
            shared.wake_main();
        }
    }

    fn push_pane_defaults(&mut self, pd: PaneDefaults) {
        for tab in &mut self.tab_mgr.tabs {
            for pane in tab.panes.values_mut() {
                apply_pane_defaults(pane, pd);
            }
        }
    }

    pub(crate) fn clear_color(&self) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    pub(crate) fn notify_focus(&mut self, focused: bool) {
        if focused {
            self.focus_regained = true;
        }
        if let Some(tab) = self.tab_mgr.tabs.get(self.tab_mgr.active_tab) {
            if let Some(pane) = tab.panes.get(&tab.focused) {
                pane.send_focus_event(focused);
            }
        }
    }

    pub(crate) fn set_scale_factor(&mut self, shared: &AppShared, scale_factor: f32) {
        let family = shared.user_config.active().font.family.clone();
        self.font.set_scale_factor(scale_factor, &self.renderer, shared.base_size, &family, &mut self.tab_mgr);
    }

    pub(crate) fn logic(&mut self, shared: &mut AppShared, ctx: &egui::Context) {
        // Prefs closed (Cancel or window close) with a color preview pushed:
        // restore the saved profile's colors. After Apply the saved config
        // equals the previewed draft, so this push is a visual no-op.
        if !shared.prefs.open && shared.preview_defaults.take().is_some() {
            let pd = defaults_from_profile(shared.user_config.active());
            self.push_pane_defaults(pd);
        }
        let factory = shared.pane_factory(&self.font);
        let exit_action = shared.user_config.active().exit_action;
        self.tab_mgr.reap_exited(exit_action, &self.gl_window.egui_ctx, &mut self.dialogs, &factory);
        // Scaled zoom invariant: drop the global font multiplier once the active
        // tab is no longer zoomed.
        self.sync_zoom_scale(shared);
        // Drain OSC 52 clipboard stores stashed by PTY threads; the system
        // clipboard is only written from here on the main thread.
        for tab in &self.tab_mgr.tabs {
            for pane in tab.panes.values() {
                if let Some(text) = pane.pending_clipboard_store.lock().unwrap().take() {
                    ctx.copy_text(text);
                }
            }
        }
        // Mark which panes are visible (active tab) so their PTY threads know
        // whether to wake the event loop on output.
        let active_tab = self.tab_mgr.active_tab;
        for (idx, tab) in self.tab_mgr.tabs.iter().enumerate() {
            let visible = idx == active_tab;
            for pane in tab.panes.values() {
                pane.visible.store(visible, std::sync::atomic::Ordering::Release);
            }
        }
        if shared.user_config.active().scroll_on_output {
            if let Some(tab) = self.tab_mgr.tabs.get(self.tab_mgr.active_tab) {
                for pane in tab.panes.values() {
                    if pane.has_new_output.swap(false, std::sync::atomic::Ordering::AcqRel) {
                        pane.scroll_to_bottom();
                    }
                }
            }
        }
        // After the window regains OS focus (e.g. Cmd+Tab back in), egui can
        // still hold keyboard focus on a widget from before the switch, which
        // makes `egui_wants_keyboard_input()` true and silently drops terminal
        // keys until the user clicks. Clear that stale focus so typing resumes
        // immediately — but only when no text-input dialog is open.
        if std::mem::take(&mut self.focus_regained)
            && !self.dialogs.wants_text_input()
            && !self.tab_bar.is_renaming()
        {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        if self.tab_mgr.tabs.is_empty() || ctx.egui_wants_keyboard_input() {
            self.pending_raw_keys.clear();
        } else {
            let raw_keys = std::mem::take(&mut self.pending_raw_keys);
            let tab = self.tab_mgr.active_tab();
            let targets = tab.select_input_targets(self.tab_mgr.broadcast_scope);
            // Alt-screen passthrough is decided by the FOCUSED pane alone, not by
            // any broadcast target — see input::process_keys (bug M3).
            let focused_alt_screen = tab
                .panes
                .get(&tab.focused)
                .map(|p| p.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN))
                .unwrap_or(false);
            let scroll_on_keystroke = shared.user_config.active().scroll_on_keystroke;
            let actions = input::process_keys(ctx, &shared.bindings, raw_keys, &targets, focused_alt_screen, scroll_on_keystroke);
            self.execute_pane_actions(shared, ctx, actions);
        }
    }

    pub(crate) fn ui(&mut self, shared: &mut AppShared, ui: &mut egui::Ui) {
        let pane_count = self.tab_mgr.total_alive_panes();
        match self.dialogs.draw_close_dialog(ui.ctx(), pane_count) {
            DialogAction::ConfirmCloseAndDisable => {
                shared.user_config.global.confirm_on_close = false;
                let _ = shared.user_config.save();
            }
            DialogAction::ConfirmClose | DialogAction::CancelClose | DialogAction::None => {}
            _ => {}
        }
        match self.dialogs.draw_title_dialog(ui.ctx()) {
            DialogAction::SetTitle(title) => {
                self.tab_mgr.tabs[self.tab_mgr.active_tab].custom_title = if title.is_empty() {
                    None
                } else {
                    Some(title)
                };
            }
            DialogAction::ClearTitle => {
                self.tab_mgr.tabs[self.tab_mgr.active_tab].custom_title = None;
            }
            _ => {}
        }
        match self.dialogs.draw_layout_save_dialog(ui.ctx()) {
            DialogAction::SaveLayout(name) => {
                self.tab_mgr.save_layout(name, &mut shared.user_config);
            }
            _ => {}
        }
        // Layout Launcher (Alt+L): the dialog lists the saved layout names and
        // returns the chosen one; apply it through the same restore path as
        // startup.
        let layout_names: Vec<String> =
            shared.user_config.layouts.iter().map(|l| l.name.clone()).collect();
        match self.dialogs.draw_layout_launcher(ui.ctx(), &layout_names) {
            DialogAction::LaunchLayout(name) => self.restore_named_layout(shared, &name),
            _ => {}
        }
        // New-group naming dialog: assign the typed name to the one pane whose
        // group button opened it (Terminator's per-terminal create_group).
        match self.dialogs.draw_new_group(ui.ctx()) {
            DialogAction::NewGroup(pane, name) => {
                groups::set_pane_group(&mut self.tab_mgr.tabs, pane, name)
            }
            _ => {}
        }
        self.update_window_title();
        match self.dialogs.draw_search(ui) {
            DialogAction::SearchNext => self.tab_mgr.run_search(false, &self.dialogs),
            DialogAction::SearchPrev => self.tab_mgr.run_search(true, &self.dialogs),
            _ => {}
        }

        // Tab bar: derive a per-frame view from config + tabs, render it through
        // the configured panel (None when tab_position == Hidden), then apply the
        // resulting events to the TabManager.
        let mut tab_events: Vec<tab_bar::TabBarEvent> = Vec::new();
        let position = shared.user_config.global.tab_position;
        if let Some(panel) = tab_bar::panel_for(position, "tab_bar") {
            let view = TabBarView {
                tabs: &self.tab_mgr.tabs,
                active: self.tab_mgr.active_tab,
                position,
                homogeneous: shared.user_config.global.homogeneous,
                close_button: shared.user_config.global.close_button_on_tab,
                scroll: shared.user_config.global.scroll_tabbar,
            };
            let state = &mut self.tab_bar;
            panel.show_inside(ui, |ui| {
                tab_events = tab_bar::show(ui, state, &view);
            });
        }
        let factory = shared.pane_factory(&self.font);
        tab_bar::apply(
            tab_events,
            &mut self.tab_mgr,
            &factory,
            &shared.user_config.global,
            &self.gl_window.egui_ctx,
            &mut self.dialogs,
        );

        if !self.tab_mgr.tabs.is_empty() {
            if self.pending_zoom_steps != 0 {
                let steps = self.pending_zoom_steps;
                self.pending_zoom_steps = 0;
                let family = shared.user_config.active().font.family.clone();
                self.font.adjust_font_size(steps as f32, &self.renderer, shared.base_size, &family, &mut self.tab_mgr);
            }

            let egui_ctx = self.gl_window.egui_ctx.clone();
            // While prefs is open, panes render from the draft config so
            // non-PaneDefaults colors (focus/broadcast border) preview live
            // alongside the palette. Reverts implicitly when prefs closes.
            let view_config = if shared.prefs.open {
                &shared.prefs.draft
            } else {
                &shared.user_config
            };
            let mut pv_ctx = PaneViewCtx {
                tab_mgr: &mut self.tab_mgr,
                cell_w: self.font.cell_w,
                cell_h: self.font.cell_h,
                font: &self.font.ctx,
                renderer: &self.renderer,
                cursor_blink_epoch: shared.cursor_blink_epoch,
                user_config: view_config,
                bindings: &shared.bindings,
                egui_ctx: &egui_ctx,
                dialogs: &mut self.dialogs,
            };
            let deferred = pane_ui::draw_panes(
                &mut self.pane_view,
                &mut pv_ctx,
                ui,
            );

            self.execute_pane_actions(shared, ui.ctx(), deferred);

            if let Some(saved) = self.pane_view.layout_restore_pending.take() {
                let factory = shared.pane_factory(&self.font);
                self.tab_mgr
                    .restore_layout(&saved, &factory, &shared.user_config);
            }
            // Data-carrying profile switch staged by the right-click Profiles
            // submenu (can't be a Copy/Hash Action), applied here exactly like
            // `layout_restore_pending`. Live re-style, no respawn.
            if let Some((pane_id, name)) = self.pane_view.profile_switch_pending.take() {
                self.switch_pane_profile(shared, pane_id, name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_from_profile_maps_colors_opacity_and_empty_selection() {
        let mut profile = crate::config::Profile::default();
        // Empty selection hexes must stay None, never parsed into a bogus color.
        profile.colors.selection_background = String::new();
        profile.colors.selection_foreground = String::new();
        profile.transparency.opacity = 0.5;
        profile.clear_wipes_scrollback = true;

        let pd = defaults_from_profile(&profile);

        assert_eq!(pd.selection_bg, None, "empty selection bg hex -> None");
        assert_eq!(pd.selection_fg, None, "empty selection fg hex -> None");
        assert!((pd.bg_opacity - 0.5).abs() < f32::EPSILON, "opacity flows through");
        // ANSI blue (#3465a4) lands at palette slot 4 (normal blue).
        assert_eq!(pd.palette[4], [0x34, 0x65, 0xa4], "ANSI blue -> palette[4]");
        assert!(pd.clear_wipes_scrollback, "clear_wipes_scrollback propagates");
        // Default fg/bg hexes resolve to their RGB.
        assert_eq!(pd.fg, [0xe5, 0xe5, 0xe5], "default fg flows");
        assert_eq!(pd.bg, [0x1a, 0x1a, 0x1a], "default bg flows");
    }

    #[test]
    fn execute_pane_actions_halts_when_tabs_emptied() {
        // Bug M1: a batched [ClosePane, RotateCW] must not index tabs[active_tab]
        // after ClosePane emptied the tab list. The loop guard halts the batch
        // the moment the tab list is empty.
        assert!(can_process_more_actions(1), "one tab -> keep processing");
        assert!(can_process_more_actions(3), "many tabs -> keep processing");
        assert!(!can_process_more_actions(0), "no tabs -> halt the batch");
    }
}
