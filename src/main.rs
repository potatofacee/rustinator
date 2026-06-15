mod config;
mod dialogs;
mod font;
mod gl_setup;
mod gl_window;
mod hotkey;
mod input;
mod keybindings;
mod keyboard;
mod layout;
mod mouse;
mod pane;
mod pane_ui;
mod platform;
mod prefs_ui;
mod presets;
mod pty_event_loop;
mod renderer;
mod shell_integration;
mod tabs;
mod term_handler;
pub mod window;

use crate::dialogs::{DialogAction, DialogState};
use crate::pane_ui::{PaneViewCtx, PaneViewState};
use crate::prefs_ui::{PrefsResult, PrefsState};
use crate::tabs::{FocusDir, PaneFactory, TabManager};

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::tty;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::config::Config;
use crate::font::FontContext;
use crate::keybindings::{Action, BindingTable};
use crate::layout::Direction;
use crate::pane::{Pane, PaneDefaults, PaneId};
use crate::renderer::Renderer;

pub(crate) use input::RawTermKey;

pub(crate) struct FontState {
    pub(crate) ctx: Arc<Mutex<FontContext>>,
    pub(crate) cell_w: f32,
    pub(crate) cell_h: f32,
    pub(crate) base_size: f32,
    pub(crate) size_override: Option<f32>,
    pub(crate) scale_factor: f32,
}

impl FontState {
    fn adjust_font_size(&mut self, delta: f32, renderer: &Arc<Mutex<Renderer>>, font_family: &str, tab_mgr: &mut TabManager) {
        let current = self.size_override.unwrap_or(self.base_size);
        let new_size = (current + delta).clamp(4.0, 72.0);
        self.apply_font_size(new_size, renderer, font_family, tab_mgr);
    }

    fn reset_font_size(&mut self, renderer: &Arc<Mutex<Renderer>>, font_family: &str, tab_mgr: &mut TabManager) {
        self.size_override = None;
        let size = self.base_size;
        if let Ok(fc) = FontContext::new(font_family, size * self.scale_factor) {
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

    fn apply_font_size(&mut self, size: f32, renderer: &Arc<Mutex<Renderer>>, font_family: &str, tab_mgr: &mut TabManager) {
        self.size_override = Some(size);
        if let Ok(fc) = FontContext::new(font_family, size * self.scale_factor) {
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

    fn set_scale_factor(&mut self, scale_factor: f32, renderer: &Arc<Mutex<Renderer>>, font_family: &str, tab_mgr: &mut TabManager) {
        if (scale_factor - self.scale_factor).abs() < f32::EPSILON {
            return;
        }
        self.scale_factor = scale_factor;
        let size = self.size_override.unwrap_or(self.base_size);
        if let Ok(fc) = FontContext::new(font_family, size * self.scale_factor) {
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

    fn reload_font(&mut self, family: &str, size: f32, renderer: &Arc<Mutex<Renderer>>) -> Result<(), crossfont::Error> {
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

pub(crate) struct RendererState {
    pub(crate) renderer: Arc<Mutex<Renderer>>,
}

pub(crate) struct InputState {
    pub(crate) bindings: BindingTable,
    pub(crate) pending_raw_keys: Vec<RawTermKey>,
    pub(crate) cursor_blink_epoch: Instant,
}

pub(crate) struct App {
    pub(crate) tab_mgr: TabManager,
    egui_ctx: egui::Context,
    event_loop_proxy: EventLoopProxy<window::UserEvent>,
    pub(crate) font: FontState,
    pub(crate) render: RendererState,
    pub(crate) input: InputState,
    term_config: TermConfig,
    pane_defaults: PaneDefaults,
    pub(crate) user_config: Config,
    pub(crate) prefs: PrefsState,
    pub(crate) dialogs: DialogState,
    pub(crate) pane_view: PaneViewState,
    current_title: String,
    pub(crate) fullscreen_pending: bool,
    pub(crate) hotkey_changed: bool,
    pub(crate) pending_zoom_steps: i32,
    /// Last pane defaults pushed as a live preview of the prefs draft.
    /// Some(..) while the prefs panel has previewed unapplied colors; logic()
    /// restores the saved profile's colors when prefs closes without Apply.
    preview_defaults: Option<PaneDefaults>,
    /// Set when the main window regains OS focus (e.g. via Cmd+Tab). On the next
    /// logic pass, stale egui keyboard focus is cleared so terminal input flows
    /// again without requiring a click. See `logic`.
    focus_regained: bool,
}

/// Build the per-pane render defaults from a profile's color settings.
fn defaults_from_profile(profile: &config::Profile) -> PaneDefaults {
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

impl App {
    fn pane_factory(&self) -> PaneFactory {
        PaneFactory {
            cell_w: self.font.cell_w,
            cell_h: self.font.cell_h,
            term_config: self.term_config.clone(),
            pane_defaults: self.pane_defaults,
            event_loop_proxy: self.event_loop_proxy.clone(),
        }
    }
}

impl App {
    pub(crate) fn new(
        gl: Arc<glow::Context>,
        egui_ctx: egui::Context,
        event_loop_proxy: EventLoopProxy<window::UserEvent>,
        scale_factor: f32,
    ) -> Result<Self, String> {
        tty::setup_env();

        let user_config = Config::load();
        let profile = user_config.active();
        let base_font_size = profile.font.size;

        let fallback_font = if cfg!(target_os = "macos") { "Menlo" } else { "monospace" };
        let font_ctx = FontContext::new(&profile.font.family, profile.font.size * scale_factor)
            .or_else(|e| {
                eprintln!("font '{}' failed ({e}), falling back to '{fallback_font}'", profile.font.family);
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

        let mut term_config = TermConfig::default();
        term_config.scrolling_history = profile.scrollback.effective_history();
        term_config.kitty_keyboard = true;
        term_config.semantic_escape_chars = config::word_chars_to_semantic_escape(&profile.word_chars);

        let pane_defaults = defaults_from_profile(profile);

        let mut bindings = BindingTable::new(user_config.global.use_linux_keybindings);
        bindings.apply_user(
            &user_config
                .keybindings
                .iter()
                .map(|b| (b.action.clone(), b.key.clone()))
                .collect::<Vec<_>>(),
        );

        let first_id: PaneId = 1;
        let pane = Pane::spawn(
            first_id,
            tabs::INITIAL_COLS as usize,
            tabs::INITIAL_LINES as usize,
            cell_w,
            cell_h,
            term_config.clone(),
            pane_defaults,
            Some(event_loop_proxy.clone()),
            None,
        ).map_err(|e| format!("first pane: {e}"))?;

        Ok(Self {
            tab_mgr: TabManager::new(pane),
            egui_ctx,
            event_loop_proxy,
            font: FontState {
                ctx: Arc::new(Mutex::new(font_ctx)),
                cell_w,
                cell_h,
                base_size: base_font_size,
                size_override: None,
                scale_factor,
            },
            render: RendererState {
                renderer: Arc::new(Mutex::new(renderer)),
            },
            input: InputState {
                bindings,
                pending_raw_keys: Vec::new(),
                cursor_blink_epoch: Instant::now(),
            },
            term_config,
            pane_defaults,
            user_config: user_config.clone(),
            prefs: PrefsState::new(),
            current_title: "rustinator".into(),
            dialogs: DialogState::new(),
            pane_view: PaneViewState::new(),
            fullscreen_pending: false,
            hotkey_changed: false,
            pending_zoom_steps: 0,
            preview_defaults: None,
            focus_regained: false,
        })
    }

    pub(crate) fn should_confirm_close(&self) -> bool {
        self.user_config.global.confirm_on_close && self.tab_mgr.total_alive_panes() > 1
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
            self.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Title(desired.clone()));
            self.current_title = desired;
        }
    }

    fn execute_pane_actions(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        let factory = self.pane_factory();
        for action in actions {
            match action {
                Action::SplitHorizontal => self.tab_mgr.split(Direction::Horizontal, &factory),
                Action::SplitVertical => self.tab_mgr.split(Direction::Vertical, &factory),
                Action::ClosePane => self.tab_mgr.close_focused(&self.egui_ctx, &mut self.dialogs),
                Action::FocusNext => self.tab_mgr.cycle_focus(1),
                Action::FocusPrev => self.tab_mgr.cycle_focus(-1),
                Action::NewTab => self.tab_mgr.new_tab(&factory),
                Action::NextTab => self.tab_mgr.switch_tab(1),
                Action::PrevTab => self.tab_mgr.switch_tab(-1),
                Action::OpenPrefs => self.open_prefs(),
                Action::Copy => self.tab_mgr.copy_selection(&self.egui_ctx, self.user_config.active().smart_copy),
                Action::Paste => self.tab_mgr.paste_from_clipboard(),
                Action::ToggleZoom => self.tab_mgr.toggle_zoom(),
                Action::ToggleBroadcast => self.tab_mgr.toggle_broadcast(),
                Action::ToggleSearch => self.tab_mgr.toggle_search(&mut self.dialogs),
                Action::ZoomIn => self.font.adjust_font_size(1.0, &self.render.renderer, &self.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::ZoomOut => self.font.adjust_font_size(-1.0, &self.render.renderer, &self.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::ZoomReset => self.font.reset_font_size(&self.render.renderer, &self.user_config.active().font.family.clone(), &mut self.tab_mgr),
                Action::CloseWindow => {
                    if self.should_confirm_close() {
                        self.dialogs.close_dialog_open = true;
                    } else {
                        self.dialogs.confirmed_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Action::ToggleFullscreen => self.fullscreen_pending = !self.fullscreen_pending,
                Action::ResizeLeft => self.tab_mgr.resize_split(-0.05, false, self.pane_view.last_root_rect),
                Action::ResizeRight => self.tab_mgr.resize_split(0.05, false, self.pane_view.last_root_rect),
                Action::ResizeUp => self.tab_mgr.resize_split(-0.05, true, self.pane_view.last_root_rect),
                Action::ResizeDown => self.tab_mgr.resize_split(0.05, true, self.pane_view.last_root_rect),
                Action::ResetTerminal => self.tab_mgr.reset_focused_terminal(false),
                Action::ResetClear => self.tab_mgr.reset_focused_terminal(true),
                Action::NewWindow => {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).spawn();
                    }
                }
                Action::OpenTerminalHere => self.tab_mgr.split_here(self.pane_view.last_pane_rect, &factory),
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
                    self.tab_mgr.split(dir, &factory);
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
            }
        }
    }


    fn open_prefs(&mut self) {
        self.prefs.open(&self.user_config);
    }

    fn apply_prefs(&mut self, new_config: Config) {
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

        self.input.bindings = BindingTable::new(self.user_config.global.use_linux_keybindings);
        self.input.bindings.apply_user(
            &self
                .user_config
                .keybindings
                .iter()
                .map(|b| (b.action.clone(), b.key.clone()))
                .collect::<Vec<_>>(),
        );

        self.pane_defaults = defaults_from_profile(&profile);
        self.term_config.scrolling_history = profile.scrollback.effective_history();
        self.term_config.semantic_escape_chars = config::word_chars_to_semantic_escape(&profile.word_chars);

        let font_changed = profile.font.family != old_profile.font.family
            || (profile.font.size - old_profile.font.size).abs() > 0.001;
        if font_changed {
            match self.font.reload_font(&profile.font.family, profile.font.size, &self.render.renderer) {
                Ok(()) => {
                    let cell_w = self.font.cell_w;
                    let cell_h = self.font.cell_h;
                    for tab in &mut self.tab_mgr.tabs {
                        for pane in tab.panes.values_mut() {
                            pane.force_pty_resize(cell_w, cell_h);
                        }
                    }
                }
                Err(e) => {
                    self.prefs.status = Some(format!("Font reload failed: {e}"));
                }
            }
        }

        for tab in &mut self.tab_mgr.tabs {
            for pane in tab.panes.values_mut() {
                pane.defaults = self.pane_defaults;
                *pane.shared_defaults.lock().unwrap() = self.pane_defaults;
                // Apply scrollback history + semantic escape chars (and the
                // rest of the term config) to the live Term, so existing panes
                // pick up the change immediately rather than only on respawn.
                pane.apply_term_config(self.term_config.clone());
                pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                pane.cached = None;
            }
        }
    }

    pub(crate) fn draw_prefs_content(&mut self, ui: &mut egui::Ui) {
        let result = self.prefs.draw(ui, &self.input.bindings);
        match result {
            PrefsResult::Applied(config) => {
                self.apply_prefs(config);
                match self.user_config.save() {
                    Ok(path) => self.prefs.status = Some(format!("Saved to {}", path.display())),
                    Err(e) => self.prefs.status = Some(format!("Save failed: {e}")),
                }
            }
            PrefsResult::Cancelled => {}
            PrefsResult::None => {}
        }
        self.preview_draft_colors();
    }

    /// Live preview: while the prefs panel is open, panes render the draft
    /// profile's colors so tweaks are visible in the real terminal before
    /// Apply. Runs during the prefs window's paint; pushes only on change and
    /// wakes the main window so it repaints with the new values. logic()
    /// reverts to the saved profile when prefs closes without Apply.
    fn preview_draft_colors(&mut self) {
        if !self.prefs.open {
            // Prefs just closed via Cancel during this paint: restore now.
            if self.preview_defaults.take().is_some() {
                let pd = defaults_from_profile(self.user_config.active());
                self.push_pane_defaults(pd);
                self.wake_main();
            }
            return;
        }
        let pd = defaults_from_profile(self.prefs.draft.active());
        if self.preview_defaults != Some(pd) {
            self.preview_defaults = Some(pd);
            self.push_pane_defaults(pd);
            self.wake_main();
        }
    }

    /// Wake the main event loop so the main window repaints (governed).
    pub(crate) fn wake_main(&self) {
        let _ = self.event_loop_proxy.send_event(window::UserEvent::Repaint);
    }

    fn push_pane_defaults(&mut self, pd: PaneDefaults) {
        for tab in &mut self.tab_mgr.tabs {
            for pane in tab.panes.values_mut() {
                pane.defaults = pd;
                *pane.shared_defaults.lock().unwrap() = pd;
                pane.clear_wipes_scrollback
                    .store(pd.clear_wipes_scrollback, std::sync::atomic::Ordering::Relaxed);
                pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                pane.cached = None;
            }
        }
    }

}

impl App {
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

    pub(crate) fn set_scale_factor(&mut self, scale_factor: f32) {
        let family = self.user_config.active().font.family.clone();
        self.font.set_scale_factor(scale_factor, &self.render.renderer, &family, &mut self.tab_mgr);
    }

    pub(crate) fn logic(&mut self, ctx: &egui::Context) {
        // Prefs closed (Cancel or window close) with a color preview pushed:
        // restore the saved profile's colors. After Apply the saved config
        // equals the previewed draft, so this push is a visual no-op.
        if !self.prefs.open && self.preview_defaults.take().is_some() {
            let pd = defaults_from_profile(self.user_config.active());
            self.push_pane_defaults(pd);
        }
        let factory = self.pane_factory();
        let exit_action = self.user_config.active().exit_action;
        self.tab_mgr.reap_exited(exit_action, &self.egui_ctx, &mut self.dialogs, &factory);
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
        // whether to wake the event loop on output. Background panes stay dirty
        // but don't keep the loop hot.
        let active_tab = self.tab_mgr.active_tab;
        for (idx, tab) in self.tab_mgr.tabs.iter().enumerate() {
            let visible = idx == active_tab;
            for pane in tab.panes.values() {
                pane.visible.store(visible, std::sync::atomic::Ordering::Release);
            }
        }
        if self.user_config.active().scroll_on_output {
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
        // immediately — but only when no text-input dialog is open, so a focused
        // search/title field keeps its focus.
        if std::mem::take(&mut self.focus_regained) && !self.dialogs.wants_text_input() {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        if self.tab_mgr.tabs.is_empty() || ctx.egui_wants_keyboard_input() {
            self.input.pending_raw_keys.clear();
        } else {
            let raw_keys = std::mem::take(&mut self.input.pending_raw_keys);
            let tab = self.tab_mgr.active_tab();
            let targets: Vec<&Pane> = if tab.broadcast {
                tab.panes.values().filter(|p| !p.read_only).collect()
            } else {
                tab.panes.get(&tab.focused).into_iter().filter(|p| !p.read_only).collect()
            };
            let scroll_on_keystroke = self.user_config.active().scroll_on_keystroke;
            let actions = input::process_keys(ctx, &self.input.bindings, raw_keys, &targets, scroll_on_keystroke);
            self.execute_pane_actions(ctx, actions);
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        let pane_count = self.tab_mgr.total_alive_panes();
        match self.dialogs.draw_close_dialog(ui.ctx(), pane_count) {
            DialogAction::ConfirmCloseAndDisable => {
                self.user_config.global.confirm_on_close = false;
                let _ = self.user_config.save();
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
                self.tab_mgr.save_layout(name, &mut self.user_config);
            }
            _ => {}
        }
        self.update_window_title();
        match self.dialogs.draw_search(ui) {
            DialogAction::SearchNext => self.tab_mgr.run_search(false, &self.dialogs),
            DialogAction::SearchPrev => self.tab_mgr.run_search(true, &self.dialogs),
            _ => {}
        }

        let mut clicked_tab: Option<usize> = None;
        let mut closed_tab: Option<usize> = None;
        let mut new_tab_requested = false;
        egui::Panel::top("tab_bar")
            .frame(egui::Frame::new().fill(egui::Color32::from_gray(40)).inner_margin(2.0))
            .show_inside(ui, |ui| {
            let avail = ui.available_width();
            let btn_width = 24.0;
            let tab_count = self.tab_mgr.tabs.len().max(1) as f32;
            let tab_width = ((avail - btn_width) / tab_count).max(40.0);

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, tab) in self.tab_mgr.tabs.iter().enumerate() {
                    let title = tab.custom_title.clone().unwrap_or_else(|| {
                        tab.panes
                            .get(&tab.focused)
                            .and_then(|p| p.title())
                            .unwrap_or_else(|| format!("Tab {}", i + 1))
                    });

                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(tab_width, ui.available_height()),
                        egui::Sense::click(),
                    );
                    response.surrender_focus();

                    let selected = i == self.tab_mgr.active_tab;
                    let bg = if selected {
                        egui::Color32::from_gray(60)
                    } else if response.hovered() {
                        egui::Color32::from_gray(50)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 2.0, bg);

                    let text_color = egui::Color32::from_gray(220);
                    let close_size = 14.0;
                    let close_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.right() - close_size - 4.0,
                                   rect.center().y - close_size / 2.0),
                        egui::vec2(close_size, close_size),
                    );
                    let close_resp = ui.interact(
                        close_rect,
                        egui::Id::new(("tab_close", i)),
                        egui::Sense::click(),
                    );
                    close_resp.surrender_focus();
                    if close_resp.clicked_by(egui::PointerButton::Primary) {
                        closed_tab = Some(i);
                    }
                    if response.clicked_by(egui::PointerButton::Primary) && !close_resp.clicked() {
                        clicked_tab = Some(i);
                    }
                    let x_color = if close_resp.hovered() {
                        egui::Color32::from_gray(255)
                    } else {
                        egui::Color32::from_gray(140)
                    };
                    ui.painter().text(
                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "\u{00d7}",
                        egui::FontId::proportional(14.0),
                        x_color,
                    );

                    let text_area = egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 4.0, rect.top()),
                        egui::pos2(close_rect.left() - 2.0, rect.bottom()),
                    );
                    let galley = ui.painter().layout_no_wrap(
                        title,
                        egui::FontId::proportional(13.0),
                        text_color,
                    );
                    let text_pos = egui::Align2::CENTER_CENTER
                        .anchor_size(text_area.center(), galley.size());
                    let text_pos = text_pos.intersect(text_area);
                    ui.painter().galley(
                        text_pos.min,
                        galley,
                        text_color,
                    );
                }
                let plus_btn = ui.small_button("+");
                plus_btn.surrender_focus();
                if plus_btn.clicked_by(egui::PointerButton::Primary) {
                    new_tab_requested = true;
                }
            });
        });
        if let Some(i) = closed_tab {
            self.tab_mgr.close_tab(i, &self.egui_ctx, &mut self.dialogs);
        } else if let Some(i) = clicked_tab {
            self.tab_mgr.switch_tab_direct((i + 1) as u8);
        }
        if new_tab_requested {
            let factory = self.pane_factory();
            self.tab_mgr.new_tab(&factory);
        }

        if !self.tab_mgr.tabs.is_empty() {
            if self.pending_zoom_steps != 0 {
                let steps = self.pending_zoom_steps;
                self.pending_zoom_steps = 0;
                let family = self.user_config.active().font.family.clone();
                self.font.adjust_font_size(steps as f32, &self.render.renderer, &family, &mut self.tab_mgr);
            }

            let egui_ctx = self.egui_ctx.clone();
            // While prefs is open, panes render from the draft config so
            // non-PaneDefaults colors (focus/broadcast border) preview live
            // alongside the palette. Reverts implicitly when prefs closes.
            let view_config = if self.prefs.open {
                &self.prefs.draft
            } else {
                &self.user_config
            };
            let mut pv_ctx = PaneViewCtx {
                tab_mgr: &mut self.tab_mgr,
                cell_w: self.font.cell_w,
                cell_h: self.font.cell_h,
                font: &self.font.ctx,
                renderer: &self.render.renderer,
                cursor_blink_epoch: self.input.cursor_blink_epoch,
                user_config: view_config,
                bindings: &self.input.bindings,
                egui_ctx: &egui_ctx,
                dialogs: &mut self.dialogs,
            };
            let deferred = pane_ui::draw_panes(
                &mut self.pane_view,
                &mut pv_ctx,
                ui,
            );

            self.execute_pane_actions(ui.ctx(), deferred);

            if let Some(template) = self.pane_view.layout_restore_pending.take() {
                let factory = self.pane_factory();
                self.tab_mgr.restore_layout(&template, &factory);
            }
        }
    }
}

fn main() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    if let Err(e) = window::run() {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}
