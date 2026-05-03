use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::selection::SelectionType;
use egui;

use crate::config::Config;
use crate::font::FontContext;
use crate::layout::{self, Direction, LayoutTemplate};
use crate::mouse::{MouseButton, MouseKind, MouseMods};
use crate::pane::{CursorOverlay, PaneId, UrlMatch};
use crate::renderer::{BgInstance, Renderer};
use crate::tabs::{PaneAction, TabManager, PANE_GAP};

const FOCUS_BORDER: f32 = 1.0;
const PANE_TITLE_HEIGHT: f32 = 20.0;

pub(crate) struct PaneViewState {
    pub drag_source_pane: Option<PaneId>,
    pub last_pane_rect: Option<egui::Rect>,
    pub layout_restore_pending: Option<LayoutTemplate>,
}

impl PaneViewState {
    pub(crate) fn new() -> Self {
        Self {
            drag_source_pane: None,
            last_pane_rect: None,
            layout_restore_pending: None,
        }
    }
}

pub(crate) struct PaneViewCtx<'a> {
    pub tab_mgr: &'a mut TabManager,
    pub cell_w: f32,
    pub cell_h: f32,
    pub font: &'a Arc<Mutex<FontContext>>,
    pub renderer: &'a Arc<Mutex<Renderer>>,
    pub cursor_blink_epoch: Instant,
    pub user_config: &'a Config,
    pub egui_ctx: &'a egui::Context,
    pub dialogs: &'a mut crate::dialogs::DialogState,
}

pub(crate) fn draw_panes(
    state: &mut PaneViewState,
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
) -> Vec<PaneAction> {
    let root_rect = ui.available_rect_before_wrap();
    let mut leaves = Vec::new();
    let zoomed = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].zoomed;
    if let Some(zid) = zoomed {
        if ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.contains_key(&zid) {
            leaves.push((zid, root_rect));
        } else {
            ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].zoomed = None;
        }
    }
    if leaves.is_empty() {
        ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
            .layout
            .walk_rects(root_rect, PANE_GAP, &mut leaves);

        let mut dividers = Vec::new();
        ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
            .layout
            .walk_dividers(root_rect, PANE_GAP, &mut dividers);
        for div in dividers {
            let resp = ui.interact(
                div.rect,
                egui::Id::new(("divider", ctx.tab_mgr.active_tab, div.path.clone())),
                egui::Sense::click_and_drag(),
            );
            let divider_color = if resp.hovered() || resp.dragged() {
                egui::Color32::from_gray(80)
            } else {
                egui::Color32::from_gray(40)
            };
            ui.painter().rect_filled(div.rect, 0.0, divider_color);
            let cursor = match div.dir {
                layout::Direction::Horizontal => egui::CursorIcon::ResizeRow,
                layout::Direction::Vertical => egui::CursorIcon::ResizeColumn,
            };
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(cursor);
            }
            if resp.double_clicked() {
                ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
                    .layout
                    .set_ratio(&div.path, 0.5);
            } else if resp.dragged() {
                if let Some(pointer) = resp.interact_pointer_pos() {
                    let new_ratio = match div.dir {
                        layout::Direction::Vertical => {
                            (pointer.x - div.parent_rect.left()) / div.parent_rect.width()
                        }
                        layout::Direction::Horizontal => {
                            (pointer.y - div.parent_rect.top()) / div.parent_rect.height()
                        }
                    };
                    ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
                        .layout
                        .set_ratio(&div.path, new_ratio);
                }
            }
        }
    }

    let ppp = ui.ctx().pixels_per_point();
    let cell_w = ctx.cell_w;
    let cell_h = ctx.cell_h;
    let mods = ui.ctx().input(|i| i.modifiers);
    let ctrl_held = mods.ctrl;
    let shift_held = mods.shift;
    let alt_held = mods.alt;
    let mut deferred: Vec<PaneAction> = Vec::new();
    let show_title_bars = leaves.len() > 1;
    let mut pane_drop_rects: Vec<(PaneId, egui::Rect)> = Vec::new();

    {
        let elapsed_ms = ctx.cursor_blink_epoch.elapsed().as_millis() as u64;
        let next_toggle = 530 - (elapsed_ms % 530);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(next_toggle));
    }

    for (id, rect) in leaves {
        let (title_rect, terminal_rect) = if show_title_bars {
            let title = egui::Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width(), PANE_TITLE_HEIGHT),
            );
            let terminal = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + PANE_TITLE_HEIGHT),
                rect.max,
            );
            (Some(title), terminal)
        } else {
            (None, rect)
        };

        if let Some(tr) = title_rect {
            let focused = id == ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].focused;
            let bg = if focused {
                egui::Color32::from_gray(50)
            } else {
                egui::Color32::from_gray(30)
            };
            ui.painter().rect_filled(tr, 0.0, bg);

            let title_resp = ui.interact(
                tr,
                egui::Id::new(("title_bar", ctx.tab_mgr.active_tab, id)),
                egui::Sense::click_and_drag(),
            );
            if title_resp.drag_started() {
                state.drag_source_pane = Some(id);
                ctx.tab_mgr.active_tab_mut().focused = id;
            }
            if title_resp.clicked() {
                ctx.tab_mgr.active_tab_mut().focused = id;
            }
            if title_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }

            let title_text = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
                .panes
                .get(&id)
                .map(|pane| {
                    let name = pane.title().unwrap_or_default();
                    let dims = format!("{}x{}", pane.cols, pane.lines);
                    if name.is_empty() {
                        dims
                    } else {
                        format!("{name}  {dims}")
                    }
                })
                .unwrap_or_default();

            let text_color = if focused {
                egui::Color32::from_gray(220)
            } else {
                egui::Color32::from_gray(140)
            };
            let galley = ui.painter().layout_no_wrap(
                title_text,
                egui::FontId::proportional(12.0),
                text_color,
            );
            let pos = egui::Align2::CENTER_CENTER
                .anchor_size(tr.center(), galley.size());
            ui.painter().galley(pos.min, galley, text_color);
        }

        pane_drop_rects.push((id, rect));

        let response = ui.interact(
            terminal_rect,
            egui::Id::new(("pane", ctx.tab_mgr.active_tab, id)),
            egui::Sense::click_and_drag(),
        );

        if response.clicked()
            || response.secondary_clicked()
            || response.middle_clicked()
            || response.drag_started()
            || response.double_clicked()
            || response.triple_clicked()
        {
            ctx.tab_mgr.active_tab_mut().focused = id;
        }

        if response.middle_clicked() {
            ctx.tab_mgr.paste_primary(id);
        }

        let pointer = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos());
        let pointer_cell = pointer.map(|p| cell_at(p, terminal_rect, ppp, cell_w, cell_h));
        let url_at_pointer = pointer_cell.and_then(|(col, row)| {
            let frame = ctx
                .tab_mgr.tabs[ctx.tab_mgr.active_tab]
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

        let handled_by_url = if ctrl_held && response.clicked() {
            if let Some(url) = url_at_pointer.as_ref() {
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let _ = std::process::Command::new(opener).arg(&url.url).spawn();
                true
            } else {
                false
            }
        } else {
            false
        };

        let mouse_to_app = ctx
            .tab_mgr.tabs[ctx.tab_mgr.active_tab]
            .panes
            .get(&id)
            .map(|p| p.mouse_reporting())
            .unwrap_or(false)
            && !shift_held;

        let primary_down = response.is_pointer_button_down_on();

        let mem_id = egui::Id::new(("pane_sel_down", ctx.tab_mgr.active_tab, id));
        let was_down: bool = ui.ctx().data(|d| d.get_temp(mem_id).unwrap_or(false));
        ui.ctx().data_mut(|d| d.insert_temp(mem_id, primary_down));
        let just_pressed = primary_down && !was_down;

        if mouse_to_app && !handled_by_url {
            if let Some((col, row)) = pointer_cell {
                if let Some(pane) = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&id) {
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
            if let Some((col, row)) = pointer_cell {
                if let Some(pane) = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&id) {
                    if response.triple_clicked() {
                        pane.begin_selection(col, row, SelectionType::Lines);
                    } else if response.double_clicked() {
                        pane.begin_selection(col, row, SelectionType::Semantic);
                    } else if just_pressed {
                        pane.begin_selection(col, row, SelectionType::Simple);
                    } else if primary_down {
                        if let Some(p) = pointer {
                            let visible_cols = (terminal_rect.width() * ppp / cell_w).floor() as i32;
                            if p.y < terminal_rect.top() {
                                pane.selection_auto_scroll(1, visible_cols);
                            } else if p.y > terminal_rect.bottom() {
                                pane.selection_auto_scroll(-1, visible_cols);
                            } else {
                                pane.update_selection(col, row);
                            }
                        }
                    } else if response.clicked() {
                        pane.clear_selection();
                    }
                }

                let gesture_ended = (!primary_down && was_down)
                    || response.double_clicked()
                    || response.triple_clicked();
                if gesture_ended {
                    if let Some(pane) = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&id) {
                        if let Some(text) = pane.selection_text() {
                            if !text.is_empty() {
                                write_primary(&text);
                                if ctx.user_config.active().copy_on_selection {
                                    ctx.egui_ctx.copy_text(text);
                                }
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
                        if let Some(pane) = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&id) {
                            pane.scroll_by(lines);
                        }
                    }
                }
            }
        }

        let focused = id == ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].focused;
        paint_pane(ctx, ui, id, terminal_rect, focused, url_highlight);

        if state.drag_source_pane.is_some()
            && state.drag_source_pane != Some(id)
        {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if rect.contains(pos) {
                    let zone = drop_zone_rect(rect, pos);
                    ui.painter().rect_filled(
                        zone,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(0x40, 0x60, 0xc0, 0x50),
                    );
                }
            }
        }

        state.last_pane_rect = Some(terminal_rect);
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
            if ui.button("Split Horizontally").clicked() {
                deferred.push(PaneAction::SplitHorizontal);
                ui.close();
            }
            if ui.button("Split Vertically").clicked() {
                deferred.push(PaneAction::SplitVertical);
                ui.close();
            }
            if ui.button("Split Auto").clicked() {
                deferred.push(PaneAction::SplitAuto);
                ui.close();
            }
            ui.separator();
            let zoom_label = if zoomed.is_some() {
                "Restore all terminals"
            } else {
                "Maximize terminal"
            };
            if ui.button(zoom_label).clicked() {
                deferred.push(PaneAction::ToggleZoom);
                ui.close();
            }
            let read_only = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab]
                .panes.get(&id).map_or(false, |p| p.read_only);
            let ro_label = if read_only { "Disable read-only" } else { "Read-only" };
            if ui.button(ro_label).clicked() {
                deferred.push(PaneAction::ToggleReadOnly);
                ui.close();
            }
            let broadcast_label = if ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].broadcast {
                "Stop broadcasting"
            } else {
                "Broadcast input to all panes"
            };
            if ui.button(broadcast_label).clicked() {
                deferred.push(PaneAction::ToggleBroadcast);
                ui.close();
            }
            ui.separator();
            if ui.button("Set title\u{2026}").clicked() {
                deferred.push(PaneAction::SetTitle);
                ui.close();
            }
            if ui.button("Open Terminal Here").clicked() {
                deferred.push(PaneAction::OpenTerminalHere);
                ui.close();
            }
            if ui.button("Close Pane").clicked() {
                deferred.push(PaneAction::Close);
                ui.close();
            }
            ui.separator();
            ui.menu_button("Layouts", |ui| {
                if ui.button("Save current layout\u{2026}").clicked() {
                    ctx.dialogs.layout_save_buf.clear();
                    ctx.dialogs.layout_save_dialog = true;
                    ui.close();
                }
                let layouts = ctx.user_config.layouts.clone();
                if !layouts.is_empty() {
                    ui.separator();
                    for layout in &layouts {
                        if ui.button(&layout.name).clicked() {
                            state.layout_restore_pending = Some(layout.template.clone());
                            ui.close();
                        }
                    }
                }
            });
            ui.separator();
            if ui.button("New Tab").clicked() {
                deferred.push(PaneAction::NewTab);
                ui.close();
            }
            ui.separator();
            if ui.button("Preferences\u{2026}").clicked() {
                deferred.push(PaneAction::OpenPrefs);
                ui.close();
            }
        });
    }

    if state.drag_source_pane.is_some() && !ui.input(|i| i.pointer.any_down()) {
        if let Some(src) = state.drag_source_pane.take() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                for (tid, full_rect) in &pane_drop_rects {
                    if *tid != src && full_rect.contains(pos) {
                        let (dir, src_first) = drop_zone_direction(*full_rect, pos);
                        let tab = &mut ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab];
                        tab.layout.remove_leaf(src);
                        if src_first {
                            tab.layout.split_leaf(*tid, src, dir);
                            tab.layout.swap_leaves(src, *tid);
                        } else {
                            tab.layout.split_leaf(*tid, src, dir);
                        }
                        tab.focused = src;
                        break;
                    }
                }
            }
        }
    }

    deferred
}

fn paint_pane(
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
    pane_id: PaneId,
    rect: egui::Rect,
    focused: bool,
    url_highlight: Option<UrlMatch>,
) {
    let cell_w = ctx.cell_w;
    let cell_h = ctx.cell_h;
    let blink_elapsed = ctx.cursor_blink_epoch.elapsed();
    let cursor_blink_enabled = ctx.user_config.active().cursor_blink;
    let font = Arc::clone(ctx.font);
    let renderer = Arc::clone(ctx.renderer);
    let tab = ctx.tab_mgr.active_tab_mut();
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

    let should_blink = cursor_blink_enabled || frame.cursor_blink_requested;
    let blink_off = should_blink
        && focused
        && (blink_elapsed.as_millis() / 530) % 2 == 1;
    let cursor_override = match frame.cursor {
        Some(CursorOverlay::Block { col, row, color }) if !focused => {
            Some(Some(CursorOverlay::HollowBlock { col, row, color }))
        }
        Some(CursorOverlay::Block { col, row, color }) if blink_off => {
            Some(Some(CursorOverlay::HollowBlock { col, row, color }))
        }
        Some(CursorOverlay::Beam { .. } | CursorOverlay::Underline { .. }) if blink_off => {
            Some(None)
        }
        _ => None,
    };

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
                frame.default_bg[0],
                frame.default_bg[1],
                frame.default_bg[2],
                a,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
        }

        let mut bg = Vec::with_capacity(frame.cells.len());
        let mut gl_instances = Vec::with_capacity(frame.cells.len());
        for cell in &frame.cells {
            if cell.bg[..3] != frame.default_bg[..3] {
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

        let effective_cursor = cursor_override.unwrap_or(frame.cursor);
        if let Some(overlay) = effective_cursor {
            let cw = renderer.cell_w;
            let ch = renderer.cell_h;
            match overlay {
                CursorOverlay::Block { col, row, color } => {
                    bg.push(BgInstance {
                        cell: [col, row],
                        color,
                        offset_cells: [0.0, 0.0],
                        size_cells: [1.0, 1.0],
                    });
                }
                CursorOverlay::Beam { col, row, color } => {
                    let frac = (2.0 / cw).clamp(0.05, 0.3);
                    bg.push(BgInstance {
                        cell: [col, row],
                        color,
                        offset_cells: [0.0, 0.0],
                        size_cells: [frac, 1.0],
                    });
                }
                CursorOverlay::Underline { col, row, color } => {
                    let frac = (2.0 / ch).clamp(0.05, 0.3);
                    bg.push(BgInstance {
                        cell: [col, row],
                        color,
                        offset_cells: [0.0, 1.0 - frac],
                        size_cells: [1.0, frac],
                    });
                }
                CursorOverlay::HollowBlock { col, row, color } => {
                    let bw = (1.0 / cw).clamp(0.02, 0.1);
                    let bh = (1.0 / ch).clamp(0.02, 0.1);
                    // top
                    bg.push(BgInstance { cell: [col, row], color,
                        offset_cells: [0.0, 0.0], size_cells: [1.0, bh] });
                    // bottom
                    bg.push(BgInstance { cell: [col, row], color,
                        offset_cells: [0.0, 1.0 - bh], size_cells: [1.0, bh] });
                    // left
                    bg.push(BgInstance { cell: [col, row], color,
                        offset_cells: [0.0, bh], size_cells: [bw, 1.0 - 2.0 * bh] });
                    // right
                    bg.push(BgInstance { cell: [col, row], color,
                        offset_cells: [1.0 - bw, bh], size_cells: [bw, 1.0 - 2.0 * bh] });
                }
            }
        }

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

    if !focused {
        ui.painter().rect_filled(
            inner_rect,
            0.0,
            egui::Color32::from_black_alpha(50),
        );
    }

    if let Some(pane) = ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&pane_id) {
        let (offset, history, screen) = pane.scroll_info();
        if history > 0 && pane.scrollbar_visible {
            let total = history + screen;
            let sb_width = 8.0;
            let track = egui::Rect::from_min_max(
                egui::pos2(inner_rect.right() - sb_width, inner_rect.top()),
                inner_rect.right_bottom(),
            );
            let track_h = track.height();
            let thumb_frac = (screen as f32 / total as f32).clamp(0.05, 1.0);
            let thumb_h = (track_h * thumb_frac).max(16.0);
            let scrollable = track_h - thumb_h;
            let thumb_top = if history > 0 {
                track.top() + scrollable * (1.0 - offset as f32 / history as f32)
            } else {
                track.top()
            };
            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(track.left(), thumb_top),
                egui::vec2(sb_width, thumb_h),
            );

            let sb_id = egui::Id::new(("scrollbar", ctx.tab_mgr.active_tab, pane_id));
            let resp = ui.interact(track, sb_id, egui::Sense::click_and_drag());

            let hovered = resp.hovered() || resp.dragged();
            let track_color = if hovered {
                egui::Color32::from_white_alpha(20)
            } else {
                egui::Color32::TRANSPARENT
            };
            let thumb_color = if resp.dragged() {
                egui::Color32::from_white_alpha(140)
            } else if hovered {
                egui::Color32::from_white_alpha(100)
            } else {
                egui::Color32::from_white_alpha(50)
            };

            ui.painter().rect_filled(track, 0.0, track_color);
            ui.painter().rect_filled(thumb_rect, sb_width / 2.0, thumb_color);

            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let frac = ((pos.y - track.top() - thumb_h / 2.0) / scrollable)
                        .clamp(0.0, 1.0);
                    let new_offset = ((1.0 - frac) * history as f32).round() as usize;
                    pane.scroll_to_position(new_offset);
                }
            } else if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let frac = ((pos.y - track.top() - thumb_h / 2.0) / scrollable)
                        .clamp(0.0, 1.0);
                    let new_offset = ((1.0 - frac) * history as f32).round() as usize;
                    pane.scroll_to_position(new_offset);
                }
            }
        }
    }

    if focused && std::env::var("RUSTINATOR_NO_FOCUS_BORDER").is_err() {
        let color = if ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].broadcast {
            egui::Color32::from_rgb(0xc0, 0x50, 0x50)
        } else {
            egui::Color32::from_rgb(0x70, 0x70, 0xc0)
        };
        let stroke = egui::Stroke::new(FOCUS_BORDER, color);
        ui.painter()
            .rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(crate) fn read_primary() -> Option<String> {
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
pub(crate) fn read_primary() -> Option<String> {
    PRIMARY_BUFFER.lock().ok()?.clone()
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
fn write_primary(text: &str) {
    if let Ok(mut buf) = PRIMARY_BUFFER.lock() {
        *buf = Some(text.to_string());
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
static PRIMARY_BUFFER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn drop_zone_direction(rect: egui::Rect, pos: egui::Pos2) -> (Direction, bool) {
    let rx = (pos.x - rect.left()) / rect.width();
    let ry = (pos.y - rect.top()) / rect.height();
    let dist_left = rx;
    let dist_right = 1.0 - rx;
    let dist_top = ry;
    let dist_bottom = 1.0 - ry;
    let min = dist_left.min(dist_right).min(dist_top).min(dist_bottom);
    if min == dist_left {
        (Direction::Vertical, true)
    } else if min == dist_right {
        (Direction::Vertical, false)
    } else if min == dist_top {
        (Direction::Horizontal, true)
    } else {
        (Direction::Horizontal, false)
    }
}

fn drop_zone_rect(rect: egui::Rect, pos: egui::Pos2) -> egui::Rect {
    let (dir, first) = drop_zone_direction(rect, pos);
    match (dir, first) {
        (Direction::Vertical, true) => egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.center().x, rect.bottom()),
        ),
        (Direction::Vertical, false) => egui::Rect::from_min_max(
            egui::pos2(rect.center().x, rect.top()),
            rect.max,
        ),
        (Direction::Horizontal, true) => egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.right(), rect.center().y),
        ),
        (Direction::Horizontal, false) => egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.center().y),
            rect.max,
        ),
    }
}

fn cell_at(p: egui::Pos2, rect: egui::Rect, ppp: f32, cell_w: f32, cell_h: f32) -> (i32, i32) {
    let rel_x = ((p.x - rect.left()).max(0.0)) * ppp;
    let rel_y = ((p.y - rect.top()).max(0.0)) * ppp;
    let col = (rel_x / cell_w).floor() as i32;
    let row = (rel_y / cell_h).floor() as i32;
    (col.max(0), row.max(0))
}
