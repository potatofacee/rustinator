mod config;
mod font;
mod keybindings;
mod keyboard;
mod layout;
mod mouse;
mod pane;
mod presets;
mod renderer;
pub mod window;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alacritty_terminal::selection::SelectionType;

use crate::mouse::{MouseButton, MouseKind, MouseMods};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::tty;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::config::Config;
use crate::font::FontContext;
use crate::keybindings::{Action, BindingTable};
use crate::layout::{Direction, Node};
use crate::pane::{CursorOverlay, Pane, PaneDefaults, PaneId, UrlMatch};
use crate::renderer::{BgInstance, Renderer};

const INITIAL_COLS: u16 = 100;
const INITIAL_LINES: u16 = 32;
const PANE_GAP: f32 = 2.0;
const FOCUS_BORDER: f32 = 1.0;

struct Tab {
    panes: HashMap<PaneId, Pane>,
    layout: Node,
    focused: PaneId,
    zoomed: Option<PaneId>,
    broadcast: bool,
}

impl Tab {
    fn new(first_pane: Pane) -> Self {
        let id = first_pane.id;
        let mut panes = HashMap::new();
        panes.insert(id, first_pane);
        Self {
            panes,
            layout: Node::Leaf(id),
            focused: id,
            zoomed: None,
            broadcast: false,
        }
    }
}

pub(crate) struct App {
    tabs: Vec<Tab>,
    active_tab: usize,
    next_pane_id: PaneId,
    egui_ctx: egui::Context,
    event_loop_proxy: EventLoopProxy<window::UserEvent>,
    font: Arc<Mutex<FontContext>>,
    renderer: Arc<Mutex<Renderer>>,
    cell_w: f32,
    cell_h: f32,
    term_config: TermConfig,
    pane_defaults: PaneDefaults,
    user_config: Config,
    prefs_open: bool,
    prefs_draft: Config,
    prefs_status: Option<String>,
    prefs_section: PrefsSection,
    prefs_selected_profile: usize,
    current_title: String,
    pub(crate) close_dialog_open: bool,
    pub(crate) confirmed_close: bool,
    search_open: bool,
    search_query: String,
    search_pane: Option<(usize, PaneId)>,
    search_focus_pending: bool,
    bindings: BindingTable,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum PrefsSection {
    Global,
    Profiles,
    Keybindings,
}

enum PaneAction {
    SplitHorizontal,
    SplitVertical,
    Close,
    FocusNext,
    FocusPrev,
    NewTab,
    NextTab,
    PrevTab,
    OpenPrefs,
    Copy,
    Paste,
    ToggleZoom,
    ToggleBroadcast,
    ToggleSearch,
}

impl App {
    pub(crate) fn new(
        gl: Arc<glow::Context>,
        egui_ctx: egui::Context,
        event_loop_proxy: EventLoopProxy<window::UserEvent>,
    ) -> Self {
        tty::setup_env();

        let user_config = Config::load();
        let profile = user_config.active();

        let font_ctx = FontContext::new(&profile.font.family, profile.font.size)
            .expect("failed to load font");
        let m = font_ctx.metrics;
        eprintln!(
            "font loaded: family={} size={:.1}pt advance={:.2}px line_height={:.2}px",
            profile.font.family, profile.font.size, m.average_advance, m.line_height,
        );
        let cell_w = font_ctx.cell_width();
        let cell_h = font_ctx.cell_height();

        let renderer = Renderer::new(gl, &font_ctx);

        let mut term_config = TermConfig::default();
        term_config.scrolling_history = profile.scrollback.history;
        term_config.kitty_keyboard = true;

        let pane_defaults = PaneDefaults {
            fg: profile.foreground_rgb(),
            bg: profile.background_rgb(),
            cursor: profile.cursor_rgb(),
            bg_opacity: profile.transparency.opacity,
        };

        let mut bindings = BindingTable::new();
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
            INITIAL_COLS as usize,
            INITIAL_LINES as usize,
            cell_w,
            cell_h,
            egui_ctx.clone(),
            term_config.clone(),
            pane_defaults,
            Some(event_loop_proxy.clone()),
        );

        Self {
            tabs: vec![Tab::new(pane)],
            active_tab: 0,
            next_pane_id: first_id + 1,
            egui_ctx,
            event_loop_proxy,
            font: Arc::new(Mutex::new(font_ctx)),
            renderer: Arc::new(Mutex::new(renderer)),
            cell_w,
            cell_h,
            term_config,
            pane_defaults,
            user_config: user_config.clone(),
            prefs_open: false,
            prefs_draft: user_config,
            prefs_status: None,
            prefs_section: PrefsSection::Profiles,
            prefs_selected_profile: 0,
            current_title: "rustinator".into(),
            close_dialog_open: false,
            confirmed_close: false,
            search_open: false,
            search_query: String::new(),
            search_pane: None,
            search_focus_pending: false,
            bindings,
        }
    }

    fn total_alive_panes(&self) -> usize {
        self.tabs.iter().map(|t| t.panes.len()).sum()
    }

    pub(crate) fn should_confirm_close(&self) -> bool {
        self.user_config.global.confirm_on_close && self.total_alive_panes() > 1
    }

    fn draw_search(&mut self, ui: &mut egui::Ui) {
        if !self.search_open {
            return;
        }
        let mut close = false;
        let mut find_next = false;
        let mut find_prev = false;
        egui::Panel::bottom("search_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find:");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .desired_width(280.0)
                        .hint_text("search scrollback..."),
                );
                if self.search_focus_pending {
                    edit.request_focus();
                    self.search_focus_pending = false;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if ui.input(|i| i.modifiers.shift) {
                        find_prev = true;
                    } else {
                        find_next = true;
                    }
                }
                if ui.button("Prev").clicked() {
                    find_prev = true;
                }
                if ui.button("Next").clicked() {
                    find_next = true;
                }
                if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
        if find_next {
            self.run_search(false);
            self.search_focus_pending = true;
        }
        if find_prev {
            self.run_search(true);
            self.search_focus_pending = true;
        }
        if close {
            self.search_open = false;
        }
    }

    fn draw_close_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_dialog_open {
            return;
        }
        let mut keep_open = true;
        let mut cancel = false;
        let mut confirm = false;
        let pane_count = self.total_alive_panes();
        egui::Window::new("Confirm close")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} panes open. Really close the window?",
                    pane_count
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Close").clicked() {
                        confirm = true;
                    }
                });
            });
        if cancel || !keep_open {
            self.close_dialog_open = false;
        }
        if confirm {
            self.confirmed_close = true;
            self.close_dialog_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn update_window_title(&mut self) {
        let tab = &self.tabs[self.active_tab];
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

    fn spawn_pane(&mut self, id: PaneId, cols: usize, lines: usize) -> Pane {
        Pane::spawn(
            id,
            cols,
            lines,
            self.cell_w,
            self.cell_h,
            self.egui_ctx.clone(),
            self.term_config.clone(),
            self.pane_defaults,
            Some(self.event_loop_proxy.clone()),
        )
    }

    fn active(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    fn consume_pane_actions(&mut self, ctx: &egui::Context) {
        let mut actions: Vec<PaneAction> = Vec::new();
        let bindings = &self.bindings;

        ctx.input_mut(|i| {
            i.events.retain(|ev| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if let Some(action) = bindings.lookup(*key, *modifiers) {
                        actions.push(action_to_pane_action(action));
                        return false;
                    }
                }
                true
            });
        });

        for action in actions {
            match action {
                PaneAction::SplitHorizontal => self.split(Direction::Horizontal),
                PaneAction::SplitVertical => self.split(Direction::Vertical),
                PaneAction::Close => self.close_focused(),
                PaneAction::FocusNext => self.cycle_focus(1),
                PaneAction::FocusPrev => self.cycle_focus(-1),
                PaneAction::NewTab => self.new_tab(),
                PaneAction::NextTab => self.switch_tab(1),
                PaneAction::PrevTab => self.switch_tab(-1),
                PaneAction::OpenPrefs => self.open_prefs(),
                PaneAction::Copy => self.copy_selection(),
                PaneAction::Paste => self.paste_from_clipboard(),
                PaneAction::ToggleZoom => self.toggle_zoom(),
                PaneAction::ToggleBroadcast => self.toggle_broadcast(),
                PaneAction::ToggleSearch => self.toggle_search(),
            }
        }
    }

    fn toggle_search(&mut self) {
        if self.search_open {
            self.search_open = false;
            return;
        }
        self.search_open = true;
        self.search_focus_pending = true;
        let tab_idx = self.active_tab;
        let focused = self.tabs[tab_idx].focused;
        self.search_pane = Some((tab_idx, focused));
    }

    fn run_search(&mut self, backward: bool) {
        let Some((tab_idx, pane_id)) = self.search_pane else { return };
        if tab_idx >= self.tabs.len() {
            return;
        }
        let Some(pane) = self.tabs[tab_idx].panes.get(&pane_id) else { return };
        pane.search(&self.search_query, backward);
    }

    fn toggle_broadcast(&mut self) {
        let tab = self.active();
        tab.broadcast = !tab.broadcast;
    }

    fn toggle_zoom(&mut self) {
        let tab = self.active();
        if tab.zoomed.is_some() {
            tab.zoomed = None;
        } else {
            tab.zoomed = Some(tab.focused);
        }
    }

    fn copy_selection(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let Some(pane) = tab.panes.get(&tab.focused) else {
            return;
        };
        if let Some(text) = pane.selection_text() {
            if !text.is_empty() {
                self.egui_ctx.copy_text(text);
            }
        }
    }

    fn paste_from_clipboard(&mut self) {
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("clipboard read failed: {e}");
                return;
            }
        };
        let tab = &self.tabs[self.active_tab];
        if let Some(pane) = tab.panes.get(&tab.focused) {
            pane.send_paste(&text);
        }
    }

    fn paste_primary(&self, pane_id: PaneId) {
        let text = match read_primary() {
            Some(t) => t,
            None => return,
        };
        if let Some(pane) = self.tabs[self.active_tab].panes.get(&pane_id) {
            pane.send_paste(&text);
        }
    }

    fn open_prefs(&mut self) {
        self.prefs_draft = self.user_config.clone();
        self.prefs_status = None;
        self.prefs_open = true;
        self.prefs_selected_profile = self
            .prefs_draft
            .profiles
            .iter()
            .position(|p| p.name == self.prefs_draft.active_profile)
            .unwrap_or(0);
    }

    fn apply_prefs(&mut self) {
        let old_profile = self.user_config.active().clone();
        self.user_config = self.prefs_draft.clone();
        let profile = self.user_config.active().clone();

        // Rebuild keybinding table from the new config.
        self.bindings = BindingTable::new();
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
        self.term_config.scrolling_history = profile.scrollback.history;

        let font_changed = profile.font.family != old_profile.font.family
            || (profile.font.size - old_profile.font.size).abs() > 0.001;
        if font_changed {
            match self.reload_font(&profile.font.family, profile.font.size) {
                Ok(()) => {}
                Err(e) => {
                    self.prefs_status = Some(format!("Font reload failed: {e}"));
                }
            }
        }

        for tab in &mut self.tabs {
            for pane in tab.panes.values_mut() {
                pane.defaults = self.pane_defaults;
                pane.dirty.store(true, std::sync::atomic::Ordering::Release);
                pane.cached = None;
                if font_changed {
                    // Force re-resize at the new cell metrics on next paint.
                    pane.cols = 0;
                    pane.lines = 0;
                }
            }
        }
    }

    fn reload_font(&mut self, family: &str, size: f32) -> Result<(), crossfont::Error> {
        let new_font = FontContext::new(family, size)?;
        let cell_w = new_font.cell_width();
        let cell_h = new_font.cell_height();
        self.renderer.lock().unwrap().reload_font(&new_font);
        *self.font.lock().unwrap() = new_font;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        Ok(())
    }

    fn draw_prefs(&mut self, ctx: &egui::Context) {
        if !self.prefs_open {
            return;
        }

        let mut save_now = false;
        let mut cancel_now = false;

        egui::Window::new("Rustinator — Preferences")
            .open(&mut self.prefs_open)
            .resizable(true)
            .default_size([640.0, 460.0])
            .min_size([520.0, 360.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_min_width(120.0);
                            ui.heading("Prefs");
                            ui.add_space(6.0);
                            for (label, section) in [
                                ("Global", PrefsSection::Global),
                                ("Profiles", PrefsSection::Profiles),
                                ("Keybindings", PrefsSection::Keybindings),
                            ] {
                                let selected = self.prefs_section == section;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.prefs_section = section;
                                }
                            }
                        });

                        ui.separator();

                        ui.vertical(|ui| match self.prefs_section {
                            PrefsSection::Global => draw_prefs_global(ui, &mut self.prefs_draft),
                            PrefsSection::Profiles => draw_prefs_profiles(
                                ui,
                                &mut self.prefs_draft,
                                &mut self.prefs_selected_profile,
                            ),
                            PrefsSection::Keybindings => draw_prefs_keybindings(ui),
                        });
                    });
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save_now = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_now = true;
                    }
                    if let Some(status) = &self.prefs_status {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(status).color(egui::Color32::LIGHT_YELLOW),
                        );
                    }
                });
            });

        if save_now {
            self.apply_prefs();
            match self.user_config.save() {
                Ok(path) => self.prefs_status = Some(format!("Saved to {}", path.display())),
                Err(e) => self.prefs_status = Some(format!("Save failed: {e}")),
            }
        }
        if cancel_now {
            self.prefs_open = false;
        }
    }

    fn split(&mut self, dir: Direction) {
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let pane = self.spawn_pane(new_id, 80, 24);
        let active = self.active();
        active.panes.insert(new_id, pane);
        if !active.layout.split_leaf(active.focused, new_id, dir) {
            eprintln!("split: focused leaf {} not found in layout", active.focused);
        }
        active.focused = new_id;
    }

    fn close_focused(&mut self) {
        let target = self.active().focused;
        let result = self.active().layout.remove_leaf(target);
        self.active().panes.remove(&target);
        if self.active().zoomed == Some(target) {
            self.active().zoomed = None;
        }
        if matches!(result, crate::layout::RemoveResult::NotFound) {
            return;
        }

        // Any leaves remaining in this tab?
        let mut leaves = Vec::new();
        self.active().layout.leaves_in_order(&mut leaves);
        if let Some(first) = leaves.first() {
            self.active().focused = *first;
            return;
        }

        // Tab is empty — remove it.
        self.tabs.remove(self.active_tab);
        if self.tabs.is_empty() {
            self.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    fn cycle_focus(&mut self, step: i32) {
        let tab = self.active();
        let mut leaves = Vec::new();
        tab.layout.leaves_in_order(&mut leaves);
        if leaves.is_empty() {
            return;
        }
        let idx = leaves.iter().position(|&id| id == tab.focused).unwrap_or(0) as i32;
        let n = leaves.len() as i32;
        let new_idx = ((idx + step) % n + n) % n;
        tab.focused = leaves[new_idx as usize];
    }

    fn new_tab(&mut self) {
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let pane = self.spawn_pane(new_id, INITIAL_COLS as usize, INITIAL_LINES as usize);
        self.tabs.push(Tab::new(pane));
        self.active_tab = self.tabs.len() - 1;
    }

    fn switch_tab(&mut self, step: i32) {
        let n = self.tabs.len() as i32;
        if n == 0 {
            return;
        }
        let idx = self.active_tab as i32;
        self.active_tab = (((idx + step) % n + n) % n) as usize;
    }

    fn forward_input(&self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        let tab = &self.tabs[self.active_tab];
        let targets: Vec<&Pane> = if tab.broadcast {
            tab.panes.values().collect()
        } else {
            tab.panes.get(&tab.focused).into_iter().collect()
        };
        if targets.is_empty() {
            return;
        }
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    for pane in &targets {
                        if !pane.text_is_suppressed() {
                            pane.send_bytes(text.clone().into_bytes());
                        }
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    for pane in &targets {
                        pane.send_key(key, modifiers, key_to_bytes);
                    }
                }
                egui::Event::Paste(text) => {
                    for pane in &targets {
                        pane.send_paste(&text);
                    }
                }
                _ => {}
            }
        }
    }

    fn paint_pane(
        &mut self,
        ui: &mut egui::Ui,
        pane_id: PaneId,
        rect: egui::Rect,
        focused: bool,
        url_highlight: Option<UrlMatch>,
    ) {
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let font = Arc::clone(&self.font);
        let renderer = Arc::clone(&self.renderer);
        let tab = self.active();
        let Some(pane) = tab.panes.get_mut(&pane_id) else {
            return;
        };

        let ppp = ui.ctx().pixels_per_point();
        let inner_rect = if focused {
            rect.shrink(FOCUS_BORDER)
        } else {
            rect
        };
        let width_px = inner_rect.width() * ppp;
        let height_px = inner_rect.height() * ppp;
        let new_cols = ((width_px / cell_w).floor() as usize).max(1);
        let new_lines = ((height_px / cell_h).floor() as usize).max(1);
        pane.resize(new_cols, new_lines, cell_w, cell_h);

        let frame = pane.frame();

        let url_underline = url_highlight;
        let url_color: [f32; 4] = {
            let [r, g, b] = pane.defaults.fg;
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        };
        let cb = egui_glow::CallbackFn::new(move |info, painter| {
            let vp = info.viewport_in_pixels();
            let viewport_px = (vp.width_px as f32, vp.height_px as f32);

            let mut font = font.lock().unwrap();
            let mut renderer = renderer.lock().unwrap();

            unsafe {
                use glow::HasContext as _;
                let gl = painter.gl();
                gl.scissor(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
                let a = frame.default_bg[3];
                gl.clear_color(
                    frame.default_bg[0] * a,
                    frame.default_bg[1] * a,
                    frame.default_bg[2] * a,
                    a,
                );
                gl.clear(glow::COLOR_BUFFER_BIT);
            }

            let mut bg = Vec::with_capacity(frame.cells.len());
            let mut gl_instances = Vec::with_capacity(frame.cells.len());
            for cell in &frame.cells {
                if cell.bg != frame.default_bg {
                    bg.push(BgInstance::full(cell.col, cell.row, cell.bg));
                }
                if cell.c != ' ' && cell.c != '\0' {
                    if let Some(gi) = renderer.build_glyph_instance(
                        cell.c, cell.style, cell.col, cell.row, cell.fg, &mut font,
                    ) {
                        gl_instances.push(gi);
                    }
                }
            }

            // Beam / underline cursor overlays. Dimensions chosen so 2px is the
            // visible minimum but the overlay scales with larger cells.
            if let Some(overlay) = frame.cursor {
                let cw = renderer.cell_w;
                let ch = renderer.cell_h;
                let inst = match overlay {
                    CursorOverlay::Beam { col, row, color } => {
                        let frac = (2.0 / cw).clamp(0.05, 0.3);
                        BgInstance {
                            cell: [col, row],
                            color,
                            offset_cells: [0.0, 0.0],
                            size_cells: [frac, 1.0],
                        }
                    }
                    CursorOverlay::Underline { col, row, color } => {
                        let frac = (2.0 / ch).clamp(0.05, 0.3);
                        BgInstance {
                            cell: [col, row],
                            color,
                            offset_cells: [0.0, 1.0 - frac],
                            size_cells: [1.0, frac],
                        }
                    }
                };
                bg.push(inst);
            }

            // URL underline on Ctrl-hover.
            if let Some(url) = &url_underline {
                let ch = renderer.cell_h;
                let frac = (1.5 / ch).clamp(0.04, 0.2);
                let width_cells = (url.end_col - url.start_col) as f32;
                bg.push(BgInstance {
                    cell: [url.start_col, url.row],
                    color: url_color,
                    offset_cells: [0.0, 1.0 - frac],
                    size_cells: [width_cells, frac],
                });
            }

            renderer.paint(viewport_px, &bg, &gl_instances);
        });

        ui.painter().add(egui::PaintCallback {
            rect: inner_rect,
            callback: Arc::new(cb),
        });

        if focused && std::env::var("RUSTINATOR_NO_FOCUS_BORDER").is_err() {
            let color = if self.tabs[self.active_tab].broadcast {
                egui::Color32::from_rgb(0xc0, 0x50, 0x50)
            } else {
                egui::Color32::from_rgb(0x70, 0x70, 0xc0)
            };
            let stroke = egui::Stroke::new(FOCUS_BORDER, color);
            ui.painter()
                .rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

impl App {
    /// Reap panes whose child process has exited. Runs once per frame.
    fn reap_exited(&mut self) {
        use std::sync::atomic::Ordering;
        // Collect (tab_idx, pane_id) to close.
        let mut to_close: Vec<(usize, PaneId)> = Vec::new();
        for (t, tab) in self.tabs.iter().enumerate() {
            for (&id, pane) in &tab.panes {
                if pane.exited.load(Ordering::Acquire) {
                    to_close.push((t, id));
                }
            }
        }
        if to_close.is_empty() {
            return;
        }
        for (t, id) in to_close {
            self.close_pane(t, id);
        }
    }

    fn close_pane(&mut self, tab_idx: usize, pane_id: PaneId) {
        if tab_idx >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[tab_idx];
        let result = tab.layout.remove_leaf(pane_id);
        tab.panes.remove(&pane_id);
        if matches!(result, crate::layout::RemoveResult::NotFound) {
            return;
        }
        let mut leaves = Vec::new();
        tab.layout.leaves_in_order(&mut leaves);
        if let Some(first) = leaves.first() {
            if !leaves.contains(&tab.focused) {
                tab.focused = *first;
            }
            return;
        }
        // Tab empty — remove it.
        self.tabs.remove(tab_idx);
        if self.tabs.is_empty() {
            self.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if tab_idx < self.active_tab {
            self.active_tab -= 1;
        }
    }
}

impl App {
    pub(crate) fn clear_color(&self) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    pub(crate) fn logic(&mut self, ctx: &egui::Context) {
        self.reap_exited();
        self.consume_pane_actions(ctx);
        self.forward_input(ctx);
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        self.draw_close_dialog(ui.ctx());
        self.update_window_title();
        self.draw_prefs(ui.ctx());
        self.draw_search(ui);

        let mut clicked_tab: Option<usize> = None;
        let mut new_tab_requested = false;
        egui::Panel::top("tab_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, _) in self.tabs.iter().enumerate() {
                    let label = format!("Tab {}", i + 1);
                    if ui.selectable_label(i == self.active_tab, label).clicked() {
                        clicked_tab = Some(i);
                    }
                }
                if ui.small_button("+").clicked() {
                    new_tab_requested = true;
                }
            });
        });
        if let Some(i) = clicked_tab {
            self.active_tab = i;
        }
        if new_tab_requested {
            self.new_tab();
        }

        self.draw_panes(ui);
    }
}

impl App {
    fn draw_panes(&mut self, ui: &mut egui::Ui) {
        let root_rect = ui.available_rect_before_wrap();
        let mut leaves = Vec::new();
        let zoomed = self.tabs[self.active_tab].zoomed;
        if let Some(zid) = zoomed {
            if self.tabs[self.active_tab].panes.contains_key(&zid) {
                leaves.push((zid, root_rect));
            } else {
                self.tabs[self.active_tab].zoomed = None;
            }
        }
        if leaves.is_empty() {
            self.tabs[self.active_tab]
                .layout
                .walk_rects(root_rect, PANE_GAP, &mut leaves);

            // Divider drag handles (not shown when a pane is zoomed).
            let mut dividers = Vec::new();
            self.tabs[self.active_tab]
                .layout
                .walk_dividers(root_rect, PANE_GAP, &mut dividers);
            for div in dividers {
                let resp = ui.interact(
                    div.rect,
                    egui::Id::new(("divider", self.active_tab, div.path.clone())),
                    egui::Sense::drag(),
                );
                let cursor = match div.dir {
                    layout::Direction::Horizontal => egui::CursorIcon::ResizeRow,
                    layout::Direction::Vertical => egui::CursorIcon::ResizeColumn,
                };
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.dragged() {
                    if let Some(pointer) = resp.interact_pointer_pos() {
                        let new_ratio = match div.dir {
                            layout::Direction::Vertical => {
                                (pointer.x - div.parent_rect.left()) / div.parent_rect.width()
                            }
                            layout::Direction::Horizontal => {
                                (pointer.y - div.parent_rect.top()) / div.parent_rect.height()
                            }
                        };
                        self.tabs[self.active_tab]
                            .layout
                            .set_ratio(&div.path, new_ratio);
                    }
                }
            }
        }

        let ppp = ui.ctx().pixels_per_point();
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let mods = ui.ctx().input(|i| i.modifiers);
        let ctrl_held = mods.ctrl;
        let shift_held = mods.shift;
        let alt_held = mods.alt;
        let mut deferred: Vec<PaneAction> = Vec::new();

        for (id, rect) in leaves {
            let response = ui.interact(
                rect,
                egui::Id::new(("pane", self.active_tab, id)),
                egui::Sense::click_and_drag(),
            );

            // Any interaction focuses the pane.
            if response.clicked()
                || response.secondary_clicked()
                || response.middle_clicked()
                || response.drag_started()
                || response.double_clicked()
                || response.triple_clicked()
            {
                self.active().focused = id;
            }

            // Middle-click pastes the X11 primary selection.
            if response.middle_clicked() {
                self.paste_primary(id);
            }

            // Compute pointer cell + URL under pointer (for Ctrl-hover underline + Ctrl-click open).
            let pointer = response
                .interact_pointer_pos()
                .or_else(|| response.hover_pos());
            let pointer_cell = pointer.map(|p| cell_at(p, rect, ppp, cell_w, cell_h));
            let url_at_pointer = pointer_cell.and_then(|(col, row)| {
                let frame = self
                    .tabs[self.active_tab]
                    .panes
                    .get(&id)?
                    .cached
                    .as_ref()?
                    .clone();
                frame
                    .urls
                    .iter()
                    .find(|u| u.row == row && col >= u.start_col && col < u.end_col)
                    .cloned()
            });
            let url_highlight = if ctrl_held && response.hovered() {
                url_at_pointer.clone()
            } else {
                None
            };

            // Ctrl+click opens a URL if one is under the pointer. Skip selection handling in that case.
            let handled_by_url = if ctrl_held && response.clicked() {
                if let Some(url) = url_at_pointer.as_ref() {
                    let _ = std::process::Command::new("xdg-open").arg(&url.url).spawn();
                    true
                } else {
                    false
                }
            } else {
                false
            };

            // Determine whether this pane's program wants the mouse. Shift acts as an
            // escape hatch — hold shift to force rustinator's selection/scroll behavior.
            let mouse_to_app = self
                .tabs[self.active_tab]
                .panes
                .get(&id)
                .map(|p| p.mouse_reporting())
                .unwrap_or(false)
                && !shift_held;

            if mouse_to_app && !handled_by_url {
                if let Some((col, row)) = pointer_cell {
                    if let Some(pane) = self.tabs[self.active_tab].panes.get(&id) {
                        let mm = MouseMods { shift: shift_held, alt: alt_held, ctrl: ctrl_held };
                        let send_click = |btn| {
                            pane.send_mouse(MouseKind::Press, btn, col, row, mm);
                            pane.send_mouse(MouseKind::Release, btn, col, row, mm);
                        };
                        if response.clicked() {
                            send_click(MouseButton::Left);
                        }
                        if response.secondary_clicked() {
                            send_click(MouseButton::Right);
                        }
                        if response.middle_clicked() {
                            send_click(MouseButton::Middle);
                        }
                        if response.hovered() {
                            let scroll_y = ui.ctx().input(|i| i.smooth_scroll_delta.y);
                            if scroll_y.abs() > 1.0 {
                                let steps = (scroll_y.abs() / cell_h.max(1.0)).ceil() as i32;
                                let btn = if scroll_y > 0.0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
                                for _ in 0..steps.min(8) {
                                    pane.send_mouse(MouseKind::Press, btn, col, row, mm);
                                }
                            }
                        }
                    }
                }
            } else if !handled_by_url {
                // Normal rustinator behavior: selection + scroll.
                if let Some((col, row)) = pointer_cell {
                    if let Some(pane) = self.tabs[self.active_tab].panes.get(&id) {
                        if response.triple_clicked() {
                            pane.begin_selection(col, row, SelectionType::Lines);
                        } else if response.double_clicked() {
                            pane.begin_selection(col, row, SelectionType::Semantic);
                        } else if response.drag_started() {
                            pane.begin_selection(col, row, SelectionType::Simple);
                        } else if response.dragged() {
                            pane.update_selection(col, row);
                        } else if response.clicked() {
                            pane.clear_selection();
                        }
                    }

                    let gesture_ended = response.drag_stopped()
                        || response.double_clicked()
                        || response.triple_clicked();
                    if gesture_ended {
                        if let Some(pane) = self.tabs[self.active_tab].panes.get(&id) {
                            if let Some(text) = pane.selection_text() {
                                if !text.is_empty() {
                                    write_primary(&text);
                                }
                            }
                        }
                    }
                }

                if response.hovered() {
                    let scroll_y = ui.ctx().input(|i| i.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.5 {
                        let lines = (scroll_y * ppp / cell_h).round() as i32;
                        if lines != 0 {
                            if let Some(pane) = self.tabs[self.active_tab].panes.get(&id) {
                                pane.scroll_by(lines);
                            }
                        }
                    }
                }
            }

            let focused = id == self.tabs[self.active_tab].focused;
            self.paint_pane(ui, id, rect, focused, url_highlight);

            response.context_menu(|ui| {
                if ui.button("Copy").clicked() {
                    deferred.push(PaneAction::Copy);
                    ui.close();
                }
                if ui.button("Paste").clicked() {
                    deferred.push(PaneAction::Paste);
                    ui.close();
                }
                ui.separator();
                let zoom_label = if zoomed.is_some() {
                    "Unzoom pane"
                } else {
                    "Zoom pane"
                };
                if ui.button(zoom_label).clicked() {
                    deferred.push(PaneAction::ToggleZoom);
                    ui.close();
                }
                let broadcast_label = if self.tabs[self.active_tab].broadcast {
                    "Stop broadcasting"
                } else {
                    "Broadcast input to all panes"
                };
                if ui.button(broadcast_label).clicked() {
                    deferred.push(PaneAction::ToggleBroadcast);
                    ui.close();
                }
                ui.separator();
                if ui.button("Split Horizontally").clicked() {
                    deferred.push(PaneAction::SplitHorizontal);
                    ui.close();
                }
                if ui.button("Split Vertically").clicked() {
                    deferred.push(PaneAction::SplitVertical);
                    ui.close();
                }
                if ui.button("Close Pane").clicked() {
                    deferred.push(PaneAction::Close);
                    ui.close();
                }
                ui.separator();
                if ui.button("New Tab").clicked() {
                    deferred.push(PaneAction::NewTab);
                    ui.close();
                }
                ui.separator();
                if ui.button("Preferences…").clicked() {
                    deferred.push(PaneAction::OpenPrefs);
                    ui.close();
                }
            });
        }

        for action in deferred {
            match action {
                PaneAction::SplitHorizontal => self.split(Direction::Horizontal),
                PaneAction::SplitVertical => self.split(Direction::Vertical),
                PaneAction::Close => self.close_focused(),
                PaneAction::NewTab => self.new_tab(),
                PaneAction::OpenPrefs => self.open_prefs(),
                PaneAction::Copy => self.copy_selection(),
                PaneAction::Paste => self.paste_from_clipboard(),
                _ => {}
            }
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn read_primary() -> Option<String> {
    use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
    let mut c = Clipboard::new().ok()?;
    c.get().clipboard(LinuxClipboardKind::Primary).text().ok()
}
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn read_primary() -> Option<String> {
    None
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn write_primary(text: &str) {
    use arboard::{Clipboard, LinuxClipboardKind, SetExtLinux};
    if let Ok(mut c) = Clipboard::new() {
        let _ = c
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text.to_string());
    }
}
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn write_primary(_: &str) {}

fn cell_at(p: egui::Pos2, rect: egui::Rect, ppp: f32, cell_w: f32, cell_h: f32) -> (i32, i32) {
    let rel_x = ((p.x - rect.left()).max(0.0)) * ppp;
    let rel_y = ((p.y - rect.top()).max(0.0)) * ppp;
    let col = (rel_x / cell_w).floor() as i32;
    let row = (rel_y / cell_h).floor() as i32;
    (col.max(0), row.max(0))
}

fn action_to_pane_action(a: Action) -> PaneAction {
    match a {
        Action::SplitHorizontal => PaneAction::SplitHorizontal,
        Action::SplitVertical => PaneAction::SplitVertical,
        Action::ClosePane => PaneAction::Close,
        Action::NewTab => PaneAction::NewTab,
        Action::NextTab => PaneAction::NextTab,
        Action::PrevTab => PaneAction::PrevTab,
        Action::FocusNext => PaneAction::FocusNext,
        Action::FocusPrev => PaneAction::FocusPrev,
        Action::Copy => PaneAction::Copy,
        Action::Paste => PaneAction::Paste,
        Action::OpenPrefs => PaneAction::OpenPrefs,
        Action::ToggleZoom => PaneAction::ToggleZoom,
        Action::ToggleBroadcast => PaneAction::ToggleBroadcast,
        Action::ToggleSearch => PaneAction::ToggleSearch,
    }
}

fn draw_prefs_global(ui: &mut egui::Ui, cfg: &mut Config) {
    ui.heading("Global");
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Active profile");
        let active = cfg.active_profile.clone();
        egui::ComboBox::from_id_salt("active_profile")
            .selected_text(&active)
            .show_ui(ui, |ui| {
                let names: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
                for name in names {
                    if ui
                        .selectable_label(name == active, name.as_str())
                        .clicked()
                    {
                        cfg.active_profile = name;
                    }
                }
            });
    });
    ui.checkbox(
        &mut cfg.global.confirm_on_close,
        "Confirm before closing a window with multiple panes",
    );
}

fn draw_prefs_profiles(
    ui: &mut egui::Ui,
    cfg: &mut Config,
    selected: &mut usize,
) {
    ui.heading("Profiles");
    ui.add_space(6.0);

    ui.horizontal_top(|ui| {
        // Profile list.
        ui.vertical(|ui| {
            ui.set_min_width(140.0);
            let names: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
            for (i, name) in names.iter().enumerate() {
                if ui.selectable_label(i == *selected, name).clicked() {
                    *selected = i;
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.small_button("+").on_hover_text("Add profile").clicked() {
                    let mut new_name = "New Profile".to_string();
                    let mut n = 1;
                    while cfg.profiles.iter().any(|p| p.name == new_name) {
                        n += 1;
                        new_name = format!("New Profile {n}");
                    }
                    cfg.profiles.push(crate::config::Profile::new_named(&new_name));
                    *selected = cfg.profiles.len() - 1;
                }
                let can_delete = cfg.profiles.len() > 1;
                if ui
                    .add_enabled(can_delete, egui::Button::new("–").small())
                    .on_hover_text("Delete selected profile")
                    .clicked()
                {
                    let removed = cfg.profiles.remove(*selected);
                    if cfg.active_profile == removed.name {
                        cfg.active_profile = cfg.profiles[0].name.clone();
                    }
                    if *selected >= cfg.profiles.len() {
                        *selected = cfg.profiles.len() - 1;
                    }
                }
            });
        });

        ui.separator();

        // Editor for the selected profile.
        ui.vertical(|ui| {
            if *selected >= cfg.profiles.len() {
                *selected = 0;
            }
            let profile = &mut cfg.profiles[*selected];

            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut profile.name);
            });

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Font").strong());
            ui.horizontal(|ui| {
                ui.label("Family");
                ui.text_edit_singleline(&mut profile.font.family);
            });
            ui.horizontal(|ui| {
                ui.label("Size");
                ui.add(
                    egui::DragValue::new(&mut profile.font.size)
                        .range(6.0..=48.0)
                        .speed(0.1)
                        .suffix(" pt"),
                );
            });
            ui.label(
                egui::RichText::new("Font changes apply on Save.")
                    .small()
                    .weak(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Colors").strong());
            let current = presets::match_preset(
                &profile.colors.foreground,
                &profile.colors.background,
                &profile.colors.cursor,
            )
            .unwrap_or(presets::CUSTOM);
            ui.horizontal(|ui| {
                ui.label("Preset");
                egui::ComboBox::from_id_salt(("preset_combo", *selected))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(current == presets::CUSTOM, presets::CUSTOM).clicked() {
                            // "Custom" keeps the existing values.
                        }
                        for preset in presets::PRESETS {
                            if ui
                                .selectable_label(current == preset.name, preset.name)
                                .clicked()
                            {
                                profile.colors.foreground = preset.foreground.to_string();
                                profile.colors.background = preset.background.to_string();
                                profile.colors.cursor = preset.cursor.to_string();
                            }
                        }
                    });
            });
            hex_color_row(ui, "Foreground", &mut profile.colors.foreground);
            hex_color_row(ui, "Background", &mut profile.colors.background);
            hex_color_row(ui, "Cursor", &mut profile.colors.cursor);

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Scrolling").strong());
            ui.horizontal(|ui| {
                ui.label("History (lines)");
                ui.add(
                    egui::DragValue::new(&mut profile.scrollback.history)
                        .range(100..=1_000_000)
                        .speed(100.0),
                );
            });

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Transparency").strong());
            ui.horizontal(|ui| {
                ui.label("Opacity");
                ui.add(
                    egui::Slider::new(&mut profile.transparency.opacity, 0.3..=1.0)
                        .fixed_decimals(2),
                );
            });
            ui.label(
                egui::RichText::new(
                    "Requires a running compositor (e.g. picom, mutter, kwin). \
                     Has no effect if your window manager does not support \
                     composited transparency.",
                )
                .small()
                .weak(),
            );
        });
    });
}

fn draw_prefs_keybindings(ui: &mut egui::Ui) {
    ui.heading("Keybindings");
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Key remapping is not editable yet.")
            .small()
            .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("keybindings").striped(true).show(ui, |ui| {
        for (combo, action) in [
            ("Ctrl+Shift+E", "Split vertically"),
            ("Ctrl+Shift+O", "Split horizontally"),
            ("Ctrl+Shift+W", "Close pane"),
            ("Ctrl+Shift+T", "New tab"),
            ("Ctrl+Shift+C / Ctrl+Shift+V", "Copy / Paste"),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "Cycle panes forward / backward"),
            ("Alt+Arrow", "Focus adjacent pane"),
            ("Ctrl+PageUp / Ctrl+PageDown", "Previous / next tab"),
            ("Ctrl+,", "Open Preferences"),
            ("Middle-click", "Paste primary selection"),
        ] {
            ui.label(combo);
            ui.label(action);
            ui.end_row();
        }
    });
}

fn hex_color_row(ui: &mut egui::Ui, label: &str, hex: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut rgb = parse_hex_rgb(hex).unwrap_or([0, 0, 0]);
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *hex = config::format_hex(rgb);
        }
        ui.add(egui::TextEdit::singleline(hex).desired_width(90.0));
    });
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

fn key_to_bytes(key: egui::Key, mods: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key;

    if mods.ctrl && !mods.shift && !mods.alt {
        if let Some(b) = ctrl_byte(key) {
            return Some(vec![b]);
        }
    }

    let seq: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Delete => b"\x1b[3~",
        _ => return None,
    };
    Some(seq.to_vec())
}

fn ctrl_byte(key: egui::Key) -> Option<u8> {
    use egui::Key;
    let c = match key {
        Key::A => b'a',
        Key::B => b'b',
        Key::C => b'c',
        Key::D => b'd',
        Key::E => b'e',
        Key::F => b'f',
        Key::G => b'g',
        Key::H => b'h',
        Key::I => b'i',
        Key::J => b'j',
        Key::K => b'k',
        Key::L => b'l',
        Key::M => b'm',
        Key::N => b'n',
        Key::O => b'o',
        Key::P => b'p',
        Key::Q => b'q',
        Key::R => b'r',
        Key::S => b's',
        Key::T => b't',
        Key::U => b'u',
        Key::V => b'v',
        Key::W => b'w',
        Key::X => b'x',
        Key::Y => b'y',
        Key::Z => b'z',
        _ => return None,
    };
    Some(c - b'a' + 1)
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
