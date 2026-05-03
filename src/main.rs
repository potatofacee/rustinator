mod config;
mod dialogs;
mod font;
mod hotkey;
mod input;
mod keybindings;
mod keyboard;
mod layout;
mod mouse;
mod pane;
mod pane_ui;
mod prefs_ui;
mod presets;
mod renderer;
mod shell_integration;
mod tabs;
pub mod window;

use crate::dialogs::{DialogAction, DialogState};
use crate::pane_ui::{PaneViewCtx, PaneViewState};
use crate::prefs_ui::{PrefsResult, PrefsState};
use crate::tabs::{FocusDir, PaneAction, PaneFactory, TabManager};

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::tty;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::config::Config;
use crate::font::FontContext;
use crate::keybindings::BindingTable;
use crate::layout::Direction;
use crate::pane::{Pane, PaneDefaults, PaneId};
use crate::renderer::Renderer;

pub(crate) use input::RawTermKey;

pub(crate) struct App {
    pub(crate) tab_mgr: TabManager,
    egui_ctx: egui::Context,
    event_loop_proxy: EventLoopProxy<window::UserEvent>,
    font: Arc<Mutex<FontContext>>,
    renderer: Arc<Mutex<Renderer>>,
    cell_w: f32,
    cell_h: f32,
    term_config: TermConfig,
    pane_defaults: PaneDefaults,
    pub(crate) user_config: Config,
    pub(crate) prefs: PrefsState,
    current_title: String,
    pub(crate) dialogs: DialogState,
    bindings: BindingTable,
    pub(crate) pane_view: PaneViewState,
    base_font_size: f32,
    font_size_override: Option<f32>,
    scale_factor: f32,
    pub(crate) fullscreen_pending: bool,
    pub(crate) hotkey_changed: bool,
    pub(crate) pending_zoom_steps: i32,
    cursor_blink_epoch: Instant,
    pub(crate) pending_raw_keys: Vec<RawTermKey>,
}

impl App {
    fn pane_factory(&self) -> PaneFactory {
        PaneFactory {
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            egui_ctx: self.egui_ctx.clone(),
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

        let pane_defaults = PaneDefaults {
            fg: profile.foreground_rgb(),
            bg: profile.background_rgb(),
            cursor: profile.cursor_rgb(),
            bg_opacity: profile.transparency.opacity,
        };

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
            egui_ctx.clone(),
            term_config.clone(),
            pane_defaults,
            Some(event_loop_proxy.clone()),
            None,
        ).map_err(|e| format!("first pane: {e}"))?;

        Ok(Self {
            tab_mgr: TabManager::new(pane),
            egui_ctx,
            event_loop_proxy,
            font: Arc::new(Mutex::new(font_ctx)),
            renderer: Arc::new(Mutex::new(renderer)),
            cell_w,
            cell_h,
            term_config,
            pane_defaults,
            user_config: user_config.clone(),
            prefs: PrefsState::new(),
            current_title: "rustinator".into(),
            dialogs: DialogState::new(),
            bindings,
            pane_view: PaneViewState::new(),
            base_font_size,
            font_size_override: None,
            scale_factor,
            fullscreen_pending: false,
            hotkey_changed: false,
            pending_zoom_steps: 0,
            cursor_blink_epoch: Instant::now(),
            pending_raw_keys: Vec::new(),
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

    fn execute_pane_actions(&mut self, ctx: &egui::Context, actions: Vec<PaneAction>) {
        let factory = self.pane_factory();
        for action in actions {
            match action {
                PaneAction::SplitHorizontal => self.tab_mgr.split(Direction::Horizontal, &factory),
                PaneAction::SplitVertical => self.tab_mgr.split(Direction::Vertical, &factory),
                PaneAction::Close => self.tab_mgr.close_focused(&self.egui_ctx, &mut self.dialogs),
                PaneAction::FocusNext => self.tab_mgr.cycle_focus(1),
                PaneAction::FocusPrev => self.tab_mgr.cycle_focus(-1),
                PaneAction::NewTab => self.tab_mgr.new_tab(&factory),
                PaneAction::NextTab => self.tab_mgr.switch_tab(1),
                PaneAction::PrevTab => self.tab_mgr.switch_tab(-1),
                PaneAction::OpenPrefs => self.open_prefs(),
                PaneAction::Copy => self.tab_mgr.copy_selection(&self.egui_ctx, self.user_config.active().smart_copy),
                PaneAction::Paste => self.tab_mgr.paste_from_clipboard(),
                PaneAction::ToggleZoom => self.tab_mgr.toggle_zoom(),
                PaneAction::ToggleBroadcast => self.tab_mgr.toggle_broadcast(),
                PaneAction::ToggleSearch => self.tab_mgr.toggle_search(&mut self.dialogs),
                PaneAction::ZoomIn => self.adjust_font_size(1.0),
                PaneAction::ZoomOut => self.adjust_font_size(-1.0),
                PaneAction::ZoomReset => self.reset_font_size(),
                PaneAction::CloseWindow => {
                    if self.should_confirm_close() {
                        self.dialogs.close_dialog_open = true;
                    } else {
                        self.dialogs.confirmed_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                PaneAction::ToggleFullscreen => self.fullscreen_pending = !self.fullscreen_pending,
                PaneAction::ResizeLeft => self.tab_mgr.resize_split(-0.05, false, self.pane_view.last_pane_rect),
                PaneAction::ResizeRight => self.tab_mgr.resize_split(0.05, false, self.pane_view.last_pane_rect),
                PaneAction::ResizeUp => self.tab_mgr.resize_split(-0.05, true, self.pane_view.last_pane_rect),
                PaneAction::ResizeDown => self.tab_mgr.resize_split(0.05, true, self.pane_view.last_pane_rect),
                PaneAction::ResetTerminal => self.tab_mgr.reset_focused_terminal(false),
                PaneAction::ResetClear => self.tab_mgr.reset_focused_terminal(true),
                PaneAction::NewWindow => { let _ = std::process::Command::new(std::env::current_exe().unwrap_or_default()).spawn(); }
                PaneAction::OpenTerminalHere => self.tab_mgr.split_here(self.pane_view.last_pane_rect, &factory),
                PaneAction::QuitHotkeyWindow => {
                    self.dialogs.confirmed_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                PaneAction::SwitchToTab(n) => self.tab_mgr.switch_tab_direct(n),
                PaneAction::MoveTabLeft => self.tab_mgr.move_tab(-1),
                PaneAction::MoveTabRight => self.tab_mgr.move_tab(1),
                PaneAction::GoUp => self.tab_mgr.focus_direction(FocusDir::Up, self.pane_view.last_pane_rect),
                PaneAction::GoDown => self.tab_mgr.focus_direction(FocusDir::Down, self.pane_view.last_pane_rect),
                PaneAction::GoLeft => self.tab_mgr.focus_direction(FocusDir::Left, self.pane_view.last_pane_rect),
                PaneAction::GoRight => self.tab_mgr.focus_direction(FocusDir::Right, self.pane_view.last_pane_rect),
                PaneAction::GoNext => self.tab_mgr.cycle_focus(1),
                PaneAction::GoPrev => self.tab_mgr.cycle_focus(-1),
                PaneAction::RotateCW => self.tab_mgr.tabs[self.tab_mgr.active_tab].layout.rotate_cw(),
                PaneAction::RotateCCW => self.tab_mgr.tabs[self.tab_mgr.active_tab].layout.rotate_ccw(),
                PaneAction::SplitAuto => {
                    let dir = match self.pane_view.last_pane_rect {
                        Some(r) if r.width() >= r.height() => Direction::Vertical,
                        _ => Direction::Horizontal,
                    };
                    self.tab_mgr.split(dir, &factory);
                }
                PaneAction::ToggleScrollbar => {
                    if let Some(pane) = self.tab_mgr.active_pane_mut() {
                        pane.scrollbar_visible = !pane.scrollbar_visible;
                    }
                }
                PaneAction::HideWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                PaneAction::ToggleReadOnly | PaneAction::SetTitle => {}
            }
        }
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let current = self.font_size_override.unwrap_or(self.base_font_size);
        let new_size = (current + delta).clamp(4.0, 72.0);
        self.apply_font_size(new_size);
    }

    fn reset_font_size(&mut self) {
        self.font_size_override = None;
        self.apply_font_size(self.base_font_size);
    }

    fn apply_font_size(&mut self, size: f32) {
        self.font_size_override = Some(size);
        let profile = self.user_config.active();
        if let Ok(fc) = FontContext::new(&profile.font.family, size * self.scale_factor) {
            self.cell_w = fc.cell_width();
            self.cell_h = fc.cell_height();
            {
                let mut renderer = self.renderer.lock().unwrap();
                renderer.reload_font(&fc);
            }
            *self.font.lock().unwrap() = fc;
            for tab in &mut self.tab_mgr.tabs {
                for pane in tab.panes.values() {
                    pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                    pane.cached.as_ref();
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

        self.bindings = BindingTable::new(self.user_config.global.use_linux_keybindings);
        self.bindings.apply_user(
            &self
                .user_config
                .keybindings
                .iter()
                .map(|b| (b.action.clone(), b.key.clone()))
                .collect::<Vec<_>>(),
        );

        self.pane_defaults = PaneDefaults {
            fg: profile.foreground_rgb(),
            bg: profile.background_rgb(),
            cursor: profile.cursor_rgb(),
            bg_opacity: profile.transparency.opacity,
        };
        self.term_config.scrolling_history = profile.scrollback.effective_history();
        self.term_config.semantic_escape_chars = config::word_chars_to_semantic_escape(&profile.word_chars);

        let font_changed = profile.font.family != old_profile.font.family
            || (profile.font.size - old_profile.font.size).abs() > 0.001;
        if font_changed {
            match self.reload_font(&profile.font.family, profile.font.size) {
                Ok(()) => {}
                Err(e) => {
                    self.prefs.status = Some(format!("Font reload failed: {e}"));
                }
            }
        }

        for tab in &mut self.tab_mgr.tabs {
            for pane in tab.panes.values_mut() {
                pane.defaults = self.pane_defaults;
                pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                pane.cached = None;
                if font_changed {
                    pane.cols = 0;
                    pane.lines = 0;
                }
            }
        }
    }

    fn reload_font(&mut self, family: &str, size: f32) -> Result<(), crossfont::Error> {
        let new_font = FontContext::new(family, size * self.scale_factor)?;
        let cell_w = new_font.cell_width();
        let cell_h = new_font.cell_height();
        self.renderer.lock().unwrap().reload_font(&new_font);
        *self.font.lock().unwrap() = new_font;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        Ok(())
    }

    pub(crate) fn draw_prefs_content(&mut self, ui: &mut egui::Ui) {
        let result = self.prefs.draw(ui);
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
    }

}

impl App {
    pub(crate) fn clear_color(&self) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    pub(crate) fn notify_focus(&self, focused: bool) {
        if let Some(tab) = self.tab_mgr.tabs.get(self.tab_mgr.active_tab) {
            for pane in tab.panes.values() {
                pane.send_focus_event(focused);
            }
        }
    }

    pub(crate) fn logic(&mut self, ctx: &egui::Context) {
        let factory = self.pane_factory();
        let exit_action = self.user_config.active().exit_action;
        self.tab_mgr.reap_exited(exit_action, &self.egui_ctx, &mut self.dialogs, &factory);
        if self.user_config.active().scroll_on_output {
            if let Some(tab) = self.tab_mgr.tabs.get(self.tab_mgr.active_tab) {
                for pane in tab.panes.values() {
                    if pane.dirty.load(std::sync::atomic::Ordering::Relaxed) {
                        pane.scroll_to_bottom();
                    }
                }
            }
        }
        if self.tab_mgr.tabs.is_empty() || ctx.egui_wants_keyboard_input() {
            self.pending_raw_keys.clear();
        } else {
            let raw_keys = std::mem::take(&mut self.pending_raw_keys);
            let tab = self.tab_mgr.active_tab();
            let targets: Vec<&Pane> = if tab.broadcast {
                tab.panes.values().filter(|p| !p.read_only).collect()
            } else {
                tab.panes.get(&tab.focused).into_iter().filter(|p| !p.read_only).collect()
            };
            let scroll_on_keystroke = self.user_config.active().scroll_on_keystroke;
            let actions = input::process_keys(ctx, &self.bindings, raw_keys, &targets, scroll_on_keystroke);
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
                    if response.clicked_by(egui::PointerButton::Primary) {
                        clicked_tab = Some(i);
                    }

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
            self.tab_mgr.active_tab = i;
        }
        if new_tab_requested {
            let factory = self.pane_factory();
            self.tab_mgr.new_tab(&factory);
        }

        if !self.tab_mgr.tabs.is_empty() {
            if self.pending_zoom_steps != 0 {
                let steps = self.pending_zoom_steps;
                self.pending_zoom_steps = 0;
                self.adjust_font_size(steps as f32);
            }

            let egui_ctx = self.egui_ctx.clone();
            let mut pv_ctx = PaneViewCtx {
                tab_mgr: &mut self.tab_mgr,
                cell_w: self.cell_w,
                cell_h: self.cell_h,
                font: &self.font,
                renderer: &self.renderer,
                cursor_blink_epoch: self.cursor_blink_epoch,
                user_config: &self.user_config,
                egui_ctx: &egui_ctx,
                dialogs: &mut self.dialogs,
            };
            let deferred = pane_ui::draw_panes(
                &mut self.pane_view,
                &mut pv_ctx,
                ui,
            );

            let factory = self.pane_factory();
            for action in deferred {
                match action {
                    PaneAction::SplitHorizontal => self.tab_mgr.split(Direction::Horizontal, &factory),
                    PaneAction::SplitVertical => self.tab_mgr.split(Direction::Vertical, &factory),
                    PaneAction::SplitAuto => {
                        let dir = match self.pane_view.last_pane_rect {
                            Some(r) if r.width() >= r.height() => Direction::Vertical,
                            _ => Direction::Horizontal,
                        };
                        self.tab_mgr.split(dir, &factory);
                    }
                    PaneAction::Close => self.tab_mgr.close_focused(&self.egui_ctx, &mut self.dialogs),
                    PaneAction::NewTab => self.tab_mgr.new_tab(&factory),
                    PaneAction::OpenPrefs => self.open_prefs(),
                    PaneAction::Copy => self.tab_mgr.copy_selection(&self.egui_ctx, self.user_config.active().smart_copy),
                    PaneAction::Paste => self.tab_mgr.paste_from_clipboard(),
                    PaneAction::ToggleReadOnly => {
                        if let Some(pane) = self.tab_mgr.active_pane_mut() {
                            pane.read_only = !pane.read_only;
                        }
                    }
                    PaneAction::SetTitle => {
                        let current = self.tab_mgr.tabs[self.tab_mgr.active_tab].custom_title
                            .clone()
                            .unwrap_or_default();
                        self.dialogs.title_dialog_buf = current;
                        self.dialogs.title_dialog_open = true;
                    }
                    PaneAction::ToggleZoom => self.tab_mgr.toggle_zoom(),
                    PaneAction::ToggleBroadcast => self.tab_mgr.toggle_broadcast(),
                    PaneAction::OpenTerminalHere => self.tab_mgr.split_here(self.pane_view.last_pane_rect, &factory),
                    _ => {}
                }
            }

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
