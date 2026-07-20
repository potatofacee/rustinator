use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use egui;

use crate::config::{Config, SavedLayout};
use crate::font::FontContext;
use crate::layout::{self, Direction};
use crate::mouse::{should_report_motion, MouseButton, MouseKind, MouseMods};
use crate::pane::{CursorOverlay, PaneId, Underline, UrlMatch};
use crate::profile::ProfileName;
use crate::renderer::{BgInstance, Renderer};
use crate::keybindings::{Action, BindingTable};
use crate::tabs::{TabManager, PANE_GAP};
use crate::groups::{self, BroadcastScope, Indicator};
use crate::title_bar::{self, TitleBarModel};

const FOCUS_BORDER: f32 = 1.0;
const CURSOR_BLINK_INTERVAL_MS: u128 = 530;

/// A drag-and-drop payload originating outside the app (an OS file drop). winit's
/// `DroppedFile` carries only a path and no coordinates, so window.rs records the
/// path text plus the last cursor position (converted to egui points) here; a
/// `draw_panes` consumer resolves the target pane once pane rects are known.
pub(crate) struct ExternalDrop {
    pub text: String,
    pub pos: egui::Pos2,
}

pub(crate) struct PaneViewState {
    pub drag_source_pane: Option<PaneId>,
    pub last_pane_rect: Option<egui::Rect>,
    pub last_root_rect: Option<egui::Rect>,
    pub layout_restore_pending: Option<SavedLayout>,
    pub pending_external_drop: Option<ExternalDrop>,
    /// A profile switch selected from the right-click "Profiles" submenu. The
    /// choice carries data (which pane, which profile name), so it can't be an
    /// `Action` (that enum is Copy/Hash); it is staged here and applied by the
    /// App after `draw_panes`, exactly like `layout_restore_pending`.
    pub profile_switch_pending: Option<(PaneId, ProfileName)>,
}

impl PaneViewState {
    pub(crate) fn new() -> Self {
        Self {
            drag_source_pane: None,
            last_pane_rect: None,
            last_root_rect: None,
            layout_restore_pending: None,
            pending_external_drop: None,
            profile_switch_pending: None,
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
    pub bindings: &'a BindingTable,
    pub egui_ctx: &'a egui::Context,
    pub dialogs: &'a mut crate::dialogs::DialogState,
}

pub(crate) fn draw_panes(
    state: &mut PaneViewState,
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
) -> Vec<Action> {
    let root_rect = ui.available_rect_before_wrap();
    state.last_root_rect = Some(root_rect);
    // Keep cell metrics on the tab manager fresh so keyboard split-resize can
    // enforce a minimum pane size (see TabManager::resize_split).
    ctx.tab_mgr.cell_w = ctx.cell_w;
    ctx.tab_mgr.cell_h = ctx.cell_h;
    ctx.tab_mgr.ppp = ui.ctx().pixels_per_point();
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

        let ppp = ui.ctx().pixels_per_point();
        handle_dividers(
            ctx.tab_mgr.active_tab,
            &mut ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab],
            root_rect,
            ui,
            ctx.cell_w,
            ctx.cell_h,
            ppp,
        );
    }

    let mut deferred: Vec<Action> = Vec::new();
    let show_title_bars = leaves.len() > 1;
    let mut pane_drop_rects: Vec<(PaneId, egui::Rect)> = Vec::new();

    // Group identity of the focused pane, computed once per frame and threaded
    // into each leaf so the broadcast indicator (titlebar dots + pane outline)
    // can decide receiver status without re-deriving it per pane.
    let active = ctx.tab_mgr.active_tab;
    let focused_id = ctx.tab_mgr.tabs[active].focused;
    let focused_group: Option<String> = ctx.tab_mgr.tabs[active]
        .panes
        .get(&focused_id)
        .and_then(|p| p.group.clone());

    for (id, rect) in leaves {
        draw_leaf(
            state,
            ctx,
            ui,
            id,
            rect,
            show_title_bars,
            zoomed,
            focused_group.as_deref(),
            &mut deferred,
            &mut pane_drop_rects,
        );
    }

    handle_drag_drop(state, &mut ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab], &pane_drop_rects, ui);
    apply_external_drop(state, ctx.tab_mgr, &pane_drop_rects);

    deferred
}

/// Render one leaf pane: its optional title bar (interaction inline, paint
/// delegated to `title_bar::paint`), terminal surface, mouse handling, drop-zone
/// overlay, and context menu. Pushes into `deferred`/`pane_drop_rects` via the
/// `&mut` params so `draw_panes` stays a thin loop. A zero-behavior extraction of
/// the old per-leaf loop body plus the broadcast-scope indicator wiring.
fn draw_leaf(
    state: &mut PaneViewState,
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
    id: PaneId,
    rect: egui::Rect,
    show_title_bars: bool,
    zoomed: Option<PaneId>,
    focused_group: Option<&str>,
    deferred: &mut Vec<Action>,
    pane_drop_rects: &mut Vec<(PaneId, egui::Rect)>,
) {
    let ppp = ui.ctx().pixels_per_point();
    let cell_w = ctx.cell_w;
    let cell_h = ctx.cell_h;
    let mods = ui.ctx().input(|i| i.modifiers);
    let (ctrl_held, shift_held, alt_held) = (mods.ctrl, mods.shift, mods.alt);

    let (title_rect, terminal_rect) = if show_title_bars {
        let (title, terminal) = title_bar::split_rect(rect);
        (Some(title), terminal)
    } else {
        (None, rect)
    };

    if let Some(tr) = title_rect {
        draw_title_bar(state, ctx, ui, id, tr, terminal_rect, focused_group);
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
        let active = ctx.tab_mgr.active_tab;
        ctx.tab_mgr.set_focused_pane(active, id);
    }

    if response.middle_clicked() {
        ctx.tab_mgr.paste_primary(id);
    }

    let (handled_by_url, url_highlight) = handle_pane_mouse(
        id,
        ctx.tab_mgr.active_tab,
        &mut ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab],
        &response,
        terminal_rect,
        ui,
        cell_w,
        cell_h,
        ppp,
        ctrl_held,
        shift_held,
        alt_held,
        ctx.user_config,
        ctx.egui_ctx,
    );
    let _ = handled_by_url;

    let focused = id == ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].focused;
    paint_pane(ctx, ui, id, terminal_rect, focused, focused_group, url_highlight);

    maybe_paint_drop_zone(state, ui, id, rect);

    if focused {
        state.last_pane_rect = Some(terminal_rect);
    }

    let scope = ctx.tab_mgr.broadcast_scope;
    response.context_menu(|ui| {
        build_context_menu(
            ui,
            id,
            &ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab],
            zoomed,
            scope,
            ctx.user_config,
            ctx.bindings,
            ctx.dialogs,
            state,
            deferred,
        );
    });
}

/// Per-pane title bar. The interaction (click to focus, drag to rearrange) stays
/// here because it mutates manager state; the visual (background, centered
/// title/dimensions, the group-name label, and the broadcast transmit/receive
/// dot) is delegated to `title_bar::paint` from a precomputed `TitleBarModel`.
fn draw_title_bar(
    state: &mut PaneViewState,
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
    id: PaneId,
    title_rect: egui::Rect,
    terminal_rect: egui::Rect,
    focused_group: Option<&str>,
) {
    let focused = id == ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].focused;

    let title_resp = ui.interact(
        title_rect,
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

    // Dimensions from this frame's terminal_rect (same formula as paint_pane:
    // inner_rect = shrink by FOCUS_BORDER, then pixel-space ÷ cell size) so the
    // bar never shows stale values from a previous frame.
    let ppp = ui.ctx().pixels_per_point();
    let inner = terminal_rect.shrink(FOCUS_BORDER);
    let cols = ((inner.width() * ppp / ctx.cell_w).floor() as usize).max(1);
    let lines = ((inner.height() * ppp / ctx.cell_h).floor() as usize).max(1);
    let scope = ctx.tab_mgr.broadcast_scope;
    let model = title_bar_model(
        ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab].panes.get(&id),
        scope,
        focused,
        cols,
        lines,
        focused_group,
    );
    title_bar::paint(ui, title_rect, &model, ctx.user_config);
}

/// Build the value model `title_bar::paint` consumes from a pane (the title is
/// the raw name; paint joins it with the dimensions) and the broadcast indicator
/// derived from `groups::indicator`. A missing pane yields an empty model.
fn title_bar_model(
    pane: Option<&crate::pane::Pane>,
    scope: BroadcastScope,
    focused: bool,
    cols: usize,
    lines: usize,
    focused_group: Option<&str>,
) -> TitleBarModel {
    let this_group = pane.and_then(|p| p.group.as_deref());
    TitleBarModel {
        title: pane.and_then(|p| p.title()).unwrap_or_default(),
        cols,
        lines,
        group: pane.and_then(|p| p.group.clone()),
        indicator: groups::indicator(scope, focused, this_group, focused_group),
        focused,
    }
}

/// Paint the directional drop-zone overlay on this pane while another pane's
/// title bar is being dragged over it (no-op otherwise).
fn maybe_paint_drop_zone(
    state: &PaneViewState,
    ui: &egui::Ui,
    id: PaneId,
    rect: egui::Rect,
) {
    if state.drag_source_pane.is_some() && state.drag_source_pane != Some(id) {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            if rect.contains(pos) {
                paint_drop_zone_overlay(ui.painter(), rect, pos);
            }
        }
    }
}

fn handle_dividers(
    tab_idx: usize,
    tab: &mut crate::tabs::Tab,
    root_rect: egui::Rect,
    ui: &mut egui::Ui,
    cell_w: f32,
    cell_h: f32,
    ppp: f32,
) {
    let mut dividers = Vec::new();
    tab.layout.walk_dividers(root_rect, PANE_GAP, &mut dividers);
    for div in dividers {
        let resp = ui.interact(
            div.rect,
            egui::Id::new(("divider", tab_idx, div.path.clone())),
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
            // Super+double-click recursively equalizes every split in the tree
            // to 50/50 (Terminator); a plain double-click equalizes only this
            // divider. egui exposes Super only as mac_cmd, so the recursive case
            // is macOS-only here (same limit as the existing Super+R binding).
            if ui.input(|i| i.modifiers.mac_cmd) {
                tab.layout.rebalance_recursive();
            } else {
                tab.layout.set_ratio(&div.path, 0.5);
            }
        } else if resp.dragged() {
            if let Some(pointer) = resp.interact_pointer_pos() {
                // Minimum pane size: 3 columns wide for a vertical (side-by-
                // side) split, 1 row tall for a horizontal (stacked) split.
                // Container extent and cell size must be in the same units;
                // rects are in points, cell_w/cell_h in physical px, so scale
                // the container extent by ppp.
                let (new_ratio, container_px, cell_px, min_cells) = match div.dir {
                    layout::Direction::Vertical => (
                        (pointer.x - div.parent_rect.left()) / div.parent_rect.width(),
                        div.parent_rect.width() * ppp,
                        cell_w,
                        3.0,
                    ),
                    layout::Direction::Horizontal => (
                        (pointer.y - div.parent_rect.top()) / div.parent_rect.height(),
                        div.parent_rect.height() * ppp,
                        cell_h,
                        1.0,
                    ),
                };
                tab.layout.set_ratio_min_cells(
                    &div.path,
                    new_ratio,
                    container_px,
                    cell_px,
                    min_cells,
                );
            }
        }
    }
}

fn handle_pane_mouse(
    pane_id: PaneId,
    tab_idx: usize,
    tab: &mut crate::tabs::Tab,
    response: &egui::Response,
    terminal_rect: egui::Rect,
    ui: &mut egui::Ui,
    cell_w: f32,
    cell_h: f32,
    ppp: f32,
    ctrl_held: bool,
    shift_held: bool,
    alt_held: bool,
    user_config: &Config,
    egui_ctx: &egui::Context,
) -> (bool, Option<UrlMatch>) {
    let pointer = response
        .interact_pointer_pos()
        .or_else(|| response.hover_pos());
    let inner_rect = terminal_rect.shrink(FOCUS_BORDER);
    // Pixel->cell from fixed cell width; then resolve a click on the right half
    // of a double-width glyph (its WIDE_CHAR_SPACER column) back to the base
    // column, matching alacritty's whole-glyph hit semantics. All downstream
    // uses (selection, mouse-to-app, URL hit-test) get the authoritative column.
    let pointer_cell = pointer.map(|p| {
        let (col, row) = cell_at(p, inner_rect, ppp, cell_w, cell_h);
        let col = tab
            .panes
            .get(&pane_id)
            .and_then(|p| p.cached.as_ref())
            .map(|frame| crate::pane::resolve_wide_click_col(&frame.cells, row, col))
            .unwrap_or(col);
        (col, row)
    });
    let pointer_side = pointer.map(|p| cell_side(p, inner_rect, ppp, cell_w));
    // URL hit-testing only matters while Ctrl is held (hover highlight + click).
    // Scan on demand for the pointer's row instead of every frame snapshot.
    let url_at_pointer = if ctrl_held {
        pointer_cell.and_then(|(col, row)| {
            let frame = tab.panes.get(&pane_id)?.cached.as_ref()?;
            url_at_click(&frame.cells, row, col)
        })
    } else {
        None
    };
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

    let mouse_to_app = tab
        .panes
        .get(&pane_id)
        .map(|p| p.mouse_reporting())
        .unwrap_or(false)
        && !shift_held;

    // `is_pointer_button_down_on()` is true for ANY button, so a right/middle
    // press would otherwise be treated as a selection gesture and clear it.
    // Gate on the primary (left) button actually being held.
    let primary_down = response.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_down());

    let mem_id = egui::Id::new(("pane_sel_down", tab_idx, pane_id));
    let was_down: bool = ui.ctx().data(|d| d.get_temp(mem_id).unwrap_or(false));
    ui.ctx().data_mut(|d| d.insert_temp(mem_id, primary_down));
    let just_pressed = primary_down && !was_down;

    if mouse_to_app && !handled_by_url {
        if let Some((col, row)) = pointer_cell {
            if let Some(pane) = tab.panes.get(&pane_id) {
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

                // Cell-motion / drag reporting (modes 1002/1003). The pointer moves
                // at frame rate, but a terminal only cares about cell transitions, so
                // we coalesce by reporting only when the resolved cell changes from the
                // last one reported for this pane (tracked in egui temp data).
                if response.hovered() {
                    let (held_button, button_held) = ui.input(|i| {
                        if i.pointer.primary_down() {
                            (MouseButton::Left, true)
                        } else if i.pointer.secondary_down() {
                            (MouseButton::Right, true)
                        } else if i.pointer.middle_down() {
                            (MouseButton::Middle, true)
                        } else {
                            (MouseButton::Left, false)
                        }
                    });
                    let motion_id = egui::Id::new(("pane_motion_cell", tab_idx, pane_id));
                    let last_cell: Option<(i32, i32)> =
                        ui.ctx().data(|d| d.get_temp(motion_id));
                    let (should_send, new_last) =
                        motion_report(pane.term_mode(), button_held, last_cell, (col, row));
                    if should_send {
                        pane.send_mouse(MouseKind::Motion, held_button, col, row, mm);
                    }
                    if new_last != last_cell {
                        ui.ctx().data_mut(|d| d.insert_temp(motion_id, (col, row)));
                    }
                }
            }
        }
    } else if !handled_by_url {
        if let Some((col, row)) = pointer_cell {
            if let Some(pane) = tab.panes.get(&pane_id) {
                let side = pointer_side.unwrap_or(Side::Left);
                if response.triple_clicked() {
                    pane.begin_selection(col, row, side, SelectionType::Lines);
                } else if response.double_clicked() {
                    pane.begin_selection(col, row, side, SelectionType::Semantic);
                } else if just_pressed {
                    pane.begin_selection(col, row, side, SelectionType::Simple);
                } else if primary_down {
                    if let Some(p) = pointer {
                        let visible_cols = (terminal_rect.width() * ppp / cell_w).floor() as i32;
                        // Auto-scroll when the drag reaches the top/bottom edge.
                        // A maximized window's bottom edge coincides with the
                        // screen edge, where the OS clamps the cursor so it can
                        // never travel *past* the rect — so an edge band (>=/<=)
                        // is used instead of a strict-outside test, else downward
                        // auto-scroll never fires. When already at the scroll
                        // limit, selection_auto_scroll returns false and we do a
                        // normal in-bounds update so the edge line still selects.
                        const EDGE: f32 = 6.0;
                        let scrolled = if p.y <= terminal_rect.top() + EDGE {
                            pane.selection_auto_scroll(1, visible_cols)
                        } else if p.y >= terminal_rect.bottom() - EDGE {
                            pane.selection_auto_scroll(-1, visible_cols)
                        } else {
                            false
                        };
                        if !scrolled {
                            pane.update_selection(col, row, side);
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
                if let Some(pane) = tab.panes.get(&pane_id) {
                    if let Some(text) = pane.selection_text() {
                        if !text.is_empty() {
                            write_primary(&text);
                            if user_config.active().copy_on_selection {
                                egui_ctx.copy_text(text);
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
                    if let Some(pane) = tab.panes.get(&pane_id) {
                        let mode = pane.mode();
                        // Alternate scroll (xterm mode 1007): on the alt screen an
                        // app like vim/less/man has no scrollback, so a wheel tick
                        // is translated into arrow-key presses that scroll it.
                        if mode.contains(TermMode::ALT_SCREEN)
                            && mode.contains(TermMode::ALTERNATE_SCROLL)
                        {
                            // APP_CURSOR (DECCKM): SS3 (ESC O A/B) vs CSI (ESC [ A/B).
                            let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                                (true, false) => b"\x1b[A",
                                (true, true) => b"\x1bOA",
                                (false, false) => b"\x1b[B",
                                (false, true) => b"\x1bOB",
                            };
                            // Read-only gate: a wheel tick here is translated
                            // into user keystrokes sent to the app, so it must
                            // honor read-only (unlike terminal-response
                            // send_bytes paths, which must never be suppressed).
                            if !pane.read_only {
                                for _ in 0..lines.abs().min(8) {
                                    pane.send_bytes(seq.to_vec());
                                }
                            }
                        } else {
                            pane.scroll_by(lines);
                        }
                    }
                }
            }
        }
    }

    (handled_by_url, url_highlight)
}

/// Menu entry that fires an action, showing the bound key combo (if any)
/// right-aligned the way native menus do.
fn action_menu_item(
    ui: &mut egui::Ui,
    bindings: &BindingTable,
    label: &str,
    action: Action,
    deferred: &mut Vec<Action>,
) {
    let mut btn = egui::Button::new(label);
    if let Some(combo) = bindings.combo_for(action) {
        btn = btn.shortcut_text(combo);
    }
    if ui.add(btn).clicked() {
        deferred.push(action);
        ui.close();
    }
}

fn build_context_menu(
    ui: &mut egui::Ui,
    pane_id: PaneId,
    tab: &crate::tabs::Tab,
    zoomed: Option<PaneId>,
    scope: BroadcastScope,
    user_config: &Config,
    bindings: &BindingTable,
    dialogs: &mut crate::dialogs::DialogState,
    state: &mut PaneViewState,
    deferred: &mut Vec<Action>,
) {
    action_menu_item(ui, bindings, "Copy", Action::Copy, deferred);
    action_menu_item(ui, bindings, "Paste", Action::Paste, deferred);
    ui.separator();
    action_menu_item(ui, bindings, "Split Horizontally", Action::SplitHorizontal, deferred);
    action_menu_item(ui, bindings, "Split Vertically", Action::SplitVertical, deferred);
    action_menu_item(ui, bindings, "Split Auto", Action::SplitAuto, deferred);
    ui.separator();
    let zoom_label = if zoomed.is_some() {
        "Restore all terminals"
    } else {
        "Maximize terminal"
    };
    action_menu_item(ui, bindings, zoom_label, Action::ToggleZoom, deferred);
    let read_only = tab
        .panes.get(&pane_id).map_or(false, |p| p.read_only);
    let ro_label = if read_only { "Disable read-only" } else { "Read-only" };
    action_menu_item(ui, bindings, ro_label, Action::ToggleReadOnly, deferred);
    broadcast_and_group_menu(ui, pane_id, scope, bindings, dialogs, deferred);
    ui.separator();
    action_menu_item(ui, bindings, "Set title\u{2026}", Action::SetTitle, deferred);
    action_menu_item(ui, bindings, "Open Terminal Here", Action::OpenTerminalHere, deferred);
    action_menu_item(ui, bindings, "Close Pane", Action::ClosePane, deferred);
    ui.separator();
    let current_profile = tab
        .panes
        .get(&pane_id)
        .map(|p| p.profile.clone())
        .unwrap_or_default();
    profiles_menu(ui, pane_id, &current_profile, user_config, state);
    ui.menu_button("Layouts", |ui| {
        if ui.button("Save current layout\u{2026}").clicked() {
            dialogs.layout_save_buf.clear();
            dialogs.layout_save_dialog = true;
            ui.close();
        }
        let layouts = user_config.layouts.clone();
        if !layouts.is_empty() {
            ui.separator();
            for layout in &layouts {
                if ui.button(&layout.name).clicked() {
                    state.layout_restore_pending = Some(layout.clone());
                    ui.close();
                }
            }
        }
    });
    ui.separator();
    action_menu_item(ui, bindings, "New Tab", Action::NewTab, deferred);
    ui.separator();
    action_menu_item(ui, bindings, "Preferences\u{2026}", Action::OpenPrefs, deferred);
}

/// Broadcast scope (Off/Group/All, shown radio-style with the active scope
/// selected) plus the group set/clear entries. Replaces the old single broadcast
/// toggle now that broadcast is a 3-way global scope and panes carry named
/// groups. The explicit `BroadcastOff/Group/All` actions are unbound on Linux,
/// so this menu is their reachable surface; `ToggleBroadcast` (Ctrl+Shift+B)
/// keeps cycling the scope and is no longer a menu item.
///
/// "New group\u{2026}" opens the new-group naming dialog (mirroring the
/// "Save current layout\u{2026}" / title-dialog pattern): it is data-carrying
/// (the focused pane seeds the dialog), so unlike the broadcast scopes it can't
/// fire a Copy/Hash `Action`. It just stages `DialogState`; `main.rs` reads the
/// typed name on confirm and applies it to that pane via `groups::set_pane_group`. The old
/// `GroupAll`/`GroupTab` placeholder entries are dropped from the menu; both stay
/// reachable via their Linux default binds (Super+G / Super+T). "Ungroup" clears
/// the group via `UngroupAll`.
fn broadcast_and_group_menu(
    ui: &mut egui::Ui,
    pane_id: PaneId,
    scope: BroadcastScope,
    bindings: &BindingTable,
    dialogs: &mut crate::dialogs::DialogState,
    deferred: &mut Vec<Action>,
) {
    if ui.radio(scope == BroadcastScope::Off, "Broadcast off").clicked() {
        deferred.push(Action::BroadcastOff);
        ui.close();
    }
    if ui.radio(scope == BroadcastScope::Group, "Broadcast to group").clicked() {
        deferred.push(Action::BroadcastGroup);
        ui.close();
    }
    if ui.radio(scope == BroadcastScope::All, "Broadcast to all").clicked() {
        deferred.push(Action::BroadcastAll);
        ui.close();
    }
    ui.separator();
    if ui.button("New group\u{2026}").clicked() {
        dialogs.new_group_buf.clear();
        dialogs.new_group_pane = Some(pane_id);
        dialogs.new_group_dialog = true;
        ui.close();
    }
    action_menu_item(ui, bindings, "Ungroup", Action::UngroupAll, deferred);
}

/// The right-click "Profiles" submenu: a radio list of the configured profile
/// names with the clicked pane's current profile checked. Mirrors Terminator's
/// per-terminal Profiles radio submenu. Selecting a name is data-carrying (which
/// pane, which profile), so unlike the other entries it can't fire an `Action`
/// (that enum is Copy/Hash); it stages `state.profile_switch_pending`, which the
/// App applies after `draw_panes` as a live re-style (no respawn). Factored out
/// so `build_context_menu` stays under 100 lines.
fn profiles_menu(
    ui: &mut egui::Ui,
    pane_id: PaneId,
    current: &str,
    user_config: &Config,
    state: &mut PaneViewState,
) {
    ui.menu_button("Profiles", |ui| {
        for profile in &user_config.profiles {
            if ui.radio(profile.name == current, &profile.name).clicked() {
                state.profile_switch_pending = Some((pane_id, profile.name.clone()));
                ui.close();
            }
        }
    });
}

fn handle_drag_drop(
    state: &mut PaneViewState,
    tab: &mut crate::tabs::Tab,
    pane_drop_rects: &[(PaneId, egui::Rect)],
    ui: &mut egui::Ui,
) {
    if state.drag_source_pane.is_some() && !ui.input(|i| i.pointer.any_down()) {
        if let Some(src) = state.drag_source_pane.take() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                for (tid, full_rect) in pane_drop_rects {
                    if *tid != src && full_rect.contains(pos) {
                        let (dir, src_first) = drop_zone_direction(*full_rect, pos);
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
}

/// Paint the directional drop-zone indicator while a pane title-bar drag is in
/// flight: a translucent accent fill over the half the dropped pane will occupy
/// plus a 2px accent outline. Terminator draws a bordered zone (not just a
/// fill), and the egui overlay composites above the GL terminal (drawn via
/// `egui_glow::CallbackFn`), so the indicator is visible on top of the pane.
fn paint_drop_zone_overlay(painter: &egui::Painter, target_rect: egui::Rect, pos: egui::Pos2) {
    let zone = drop_zone_rect(target_rect, pos);
    let accent = egui::Color32::from_rgb(0x40, 0x60, 0xc0);
    let fill = egui::Color32::from_rgba_unmultiplied(0x40, 0x60, 0xc0, 0x50);
    painter.rect_filled(zone, 0.0, fill);
    painter.rect_stroke(
        zone,
        0.0,
        egui::Stroke::new(2.0, accent),
        egui::StrokeKind::Inside,
    );
}

/// Pure hit-test: the id of the first rect that contains `pos`, else None.
/// Shared by the external-drop consumer and available to the pane drag-drop
/// path (first match wins, mirroring `handle_drag_drop`'s rect scan).
fn pane_at(pos: egui::Pos2, rects: &[(PaneId, egui::Rect)]) -> Option<PaneId> {
    rects
        .iter()
        .find(|(_, rect)| rect.contains(pos))
        .map(|(id, _)| *id)
}

/// Consume a pending external drop (a file path dropped onto the window from the
/// OS). window.rs records the drop without pane context; here, where pane rects
/// are known, resolve the pane under the drop point and paste the path into it.
/// Read-only panes reject the paste inside `paste_text_into_pane`.
fn apply_external_drop(
    state: &mut PaneViewState,
    tab_mgr: &mut TabManager,
    rects: &[(PaneId, egui::Rect)],
) {
    if let Some(drop) = state.pending_external_drop.take() {
        if let Some(id) = pane_at(drop.pos, rects) {
            tab_mgr.paste_text_into_pane(id, &drop.text);
        }
    }
}

fn paint_scrollbar(
    ui: &mut egui::Ui,
    tab_idx: usize,
    tab: &crate::tabs::Tab,
    pane_id: PaneId,
    inner_rect: egui::Rect,
) {
    if let Some(pane) = tab.panes.get(&pane_id) {
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

            let sb_id = egui::Id::new(("scrollbar", tab_idx, pane_id));
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
}

fn paint_pane(
    ctx: &mut PaneViewCtx<'_>,
    ui: &mut egui::Ui,
    pane_id: PaneId,
    rect: egui::Rect,
    focused: bool,
    focused_group: Option<&str>,
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
    let inner_rect = rect.shrink(FOCUS_BORDER);
    let width_px = inner_rect.width() * ppp;
    let height_px = inner_rect.height() * ppp;
    let new_cols = ((width_px / cell_w).floor() as usize).max(1);
    let new_lines = ((height_px / cell_h).floor() as usize).max(1);
    pane.resize(new_cols, new_lines, cell_w, cell_h);

    let frame = pane.frame();

    let should_blink = cursor_blink_enabled || frame.cursor_blink_requested;
    let blink_off = should_blink
        && focused
        && (blink_elapsed.as_millis() / CURSOR_BLINK_INTERVAL_MS) % 2 == 1;

    if focused && should_blink && frame.cursor.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(CURSOR_BLINK_INTERVAL_MS as u64));
    }

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
    // Explicit OSC-8 links get a blue underline to distinguish them from
    // heuristic matches (which use the terminal's default fg color).
    let url_color: [f32; 4] = if let Some(ref u) = url_underline {
        if u.is_hyperlink {
            [0.0, 0.5, 1.0, 1.0] // bright blue for explicit hyperlinks
        } else {
            let [r, g, b] = pane.defaults.fg;
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        }
    } else {
        let [r, g, b] = pane.defaults.fg;
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
    };
    // The GL viewport is inset by FOCUS_BORDER; for a focused pane that ring is
    // covered by the focus stroke, but for an unfocused pane nothing painted it
    // and the window background showed through as a 1px gap. Fill the pane with
    // the terminal's own default bg (same color+opacity the GL quad uses) so
    // the ring matches the terminal regardless of focus.
    let bg_fill = {
        let [r, g, b, a] = frame.default_bg;
        egui::Color32::from_rgba_unmultiplied(
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
            (a * 255.0).round() as u8,
        )
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
            // NOTE: We intentionally do NOT call gl.clear() here. Clearing the
            // viewport before drawing content causes visible flicker in full-
            // screen terminal applications (vi, htop, less) because the cleared
            // frame can briefly appear on-screen before content is painted.
            // Instead, we draw the default background as a full-viewport quad
            // below, which is rendered atomically with the rest of the content.
        }

        // Compute how many cells cover the viewport so we can draw a single
        // background quad that fills the entire pane without needing gl.clear.
        let cols_cover = (vp.width_px as f32 / renderer.cell_w).ceil() as i32;
        let rows_cover = (vp.height_px as f32 / renderer.cell_h).ceil() as i32;

        let mut bg = Vec::with_capacity(frame.cells.len() + 1);
        // Full-pane default-bg quad (replaces gl.clear).
        bg.push(BgInstance {
            cell: [0, 0],
            color: frame.default_bg,
            offset_cells: [0.0, 0.0],
            size_cells: [cols_cover as f32, rows_cover as f32],
        });
        for cell in &frame.cells {
            if cell.bg[..3] != frame.default_bg[..3] {
                bg.push(BgInstance::full(cell.col, cell.row, cell.bg));
            }
        }

        let effective_cursor = cursor_override.unwrap_or(frame.cursor);

        // A solid (filled) block cursor is painted below as a cursor-colored
        // full-cell quad. To keep the glyph under it readable (as alacritty /
        // iTerm2 do), invert that one cell's glyph: render it in the cell's
        // BACKGROUND color so it reads against the cursor block. `effective_
        // cursor` is only a Block here when focused and not blink-off
        // (unfocused / blink-off were already downgraded to HollowBlock), so
        // this never affects beam / underline / hollow / unfocused cursors.
        let invert_cell = match effective_cursor {
            Some(CursorOverlay::Block { col, row, .. }) => Some((col, row)),
            _ => None,
        };

        // Build glyph instances. If the atlas fills mid-build it is reset,
        // zeroing the UVs of every glyph already recorded this frame. Detect
        // that via the atlas generation and rebuild against the fresh atlas. A
        // single pane's glyph set always fits, so this retries at most once.
        // Bound the loop regardless so a pathological case can never spin
        // forever; on exhaustion proceed with whatever was built last.
        const MAX_ATLAS_RETRIES: u32 = 4;
        let gl_instances = {
            let mut built = Vec::new();
            for _ in 0..MAX_ATLAS_RETRIES {
                let atlas_gen = renderer.atlas_generation();
                let mut gl_instances = Vec::with_capacity(frame.cells.len());
                for cell in &frame.cells {
                    if cell.c != ' ' && cell.c != '\0' {
                        let glyph_fg = if invert_cell == Some((cell.col, cell.row)) {
                            cell.bg
                        } else {
                            cell.fg
                        };
                        if let Some(gi) = renderer.build_glyph_instance(
                            cell.c, cell.style, cell.col, cell.row, glyph_fg, cell.hidden, &mut font,
                        ) {
                            gl_instances.push(gi);
                        }
                        // Composite overlay (Approach A): stack each extra
                        // codepoint's glyph over the same cell origin. Combining
                        // marks are zero-advance by definition, so this is
                        // approximately correct for accents. crossfont can't
                        // shape clusters, so emoji ZWJ sequences render as base
                        // + visible joiners rather than one merged glyph -- this
                        // stops the silent data drop, not full emoji shaping.
                        // build_glyph_instance returns None for empty glyphs, so
                        // bare combining chars that don't rasterize are skipped.
                        for &zw in &cell.zerowidth {
                            if let Some(gi) = renderer.build_glyph_instance(
                                zw, cell.style, cell.col, cell.row, glyph_fg, cell.hidden, &mut font,
                            ) {
                                gl_instances.push(gi);
                            }
                        }
                    }
                }
                built = gl_instances;
                if renderer.atlas_generation() == atlas_gen {
                    break;
                }
            }
            built
        };
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

        // SGR underline + strikeout decorations. Positions are expressed as
        // fractions of the cell (BgInstance is cell-relative), matching the
        // url-underline / underline-cursor paths; no font metrics needed.
        // HIDDEN cells already have fg == bg from the snapshot, so their
        // decoration color is invisible (nothing to special-case here).
        {
            let ch = renderer.cell_h;
            // Single-line thickness ~1.5px, clamped, in cell-height units.
            let t = (1.5 / ch).clamp(0.04, 0.2);
            // Underline sits near the descender; strikeout near mid-cell.
            let u_y = 1.0 - t - 0.06;
            let s_y = 0.5 - t * 0.5;
            for cell in &frame.cells {
                let col = cell.col;
                let row = cell.row;
                let uc = cell.underline_color;
                // A double-width (CJK) base cell spans two columns; its blank
                // spacer carries no flags, so the decoration must cover both.
                let w = if cell.wide { 2.0 } else { 1.0 };
                match cell.underline {
                    Underline::None => {}
                    Underline::Single => {
                        bg.push(BgInstance {
                            cell: [col, row],
                            color: uc,
                            offset_cells: [0.0, u_y],
                            size_cells: [w, t],
                        });
                    }
                    Underline::Double => {
                        // Two thinner lines straddling the single-line position.
                        let tt = t * 0.6;
                        bg.push(BgInstance {
                            cell: [col, row],
                            color: uc,
                            offset_cells: [0.0, u_y - tt],
                            size_cells: [w, tt],
                        });
                        bg.push(BgInstance {
                            cell: [col, row],
                            color: uc,
                            offset_cells: [0.0, u_y + tt],
                            size_cells: [w, tt],
                        });
                    }
                    Underline::Dotted | Underline::Dashed => {
                        // Stipple: short rects across the cell width. Dotted uses
                        // a 1/4-cell period; dashed a 1/2-cell period with a
                        // longer on-segment. No shader change.
                        let (period, on): (f32, f32) = match cell.underline {
                            Underline::Dotted => (0.25, 0.5),
                            _ => (0.5, 0.6),
                        };
                        let seg = period * on;
                        let mut x = 0.0;
                        while x < w {
                            bg.push(BgInstance {
                                cell: [col, row],
                                color: uc,
                                offset_cells: [x, u_y],
                                size_cells: [seg.min(w - x), t],
                            });
                            x += period;
                        }
                    }
                    Underline::Curly => {
                        // Approximation: a small triangle-wave of short rects at
                        // alternating heights, visibly distinct from a flat
                        // single line without a dedicated shader.
                        let step: f32 = 0.25;
                        let amp = t * 1.5;
                        let mut x = 0.0;
                        let mut up = false;
                        while x < w {
                            let y = if up { u_y - amp } else { u_y + amp };
                            bg.push(BgInstance {
                                cell: [col, row],
                                color: uc,
                                offset_cells: [x, y],
                                size_cells: [step.min(w - x), t],
                            });
                            x += step;
                            up = !up;
                        }
                    }
                }
                if cell.strikeout {
                    bg.push(BgInstance {
                        cell: [col, row],
                        color: cell.fg,
                        offset_cells: [0.0, s_y],
                        size_cells: [w, t],
                    });
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

    // Paint the bg behind the GL viewport so the FOCUS_BORDER inset ring is
    // filled with the terminal bg (the GL callback covers inner_rect on top).
    ui.painter().rect_filled(rect, 0.0, bg_fill);
    ui.painter().add(egui::PaintCallback {
        rect: inner_rect,
        callback: Arc::new(cb),
    });

    if !focused {
        let dim_alpha = ctx.user_config.active().inactive_dim_alpha;
        ui.painter().rect_filled(
            rect,
            0.0,
            egui::Color32::from_black_alpha(dim_alpha),
        );
    }

    paint_scrollbar(ui, ctx.tab_mgr.active_tab, &ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab], pane_id, inner_rect);

    static SHOW_FOCUS_BORDER: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("RUSTINATOR_NO_FOCUS_BORDER").is_err()
    });

    if *SHOW_FOCUS_BORDER {
        // The pane outline reflects broadcast state (Q2): the focused pane
        // transmits, group/all receivers get a distinct dimmed ring, and an
        // off / ungrouped pane keeps the normal focus border. read_only panes
        // never receive input, so they only ever show the focus ring -- matching
        // the previous `broadcast && !read_only` behavior.
        let profile = ctx.user_config.active();
        let scope = ctx.tab_mgr.broadcast_scope;
        let tab = &ctx.tab_mgr.tabs[ctx.tab_mgr.active_tab];
        let pane = tab.panes.get(&pane_id);
        let read_only = pane.is_some_and(|p| p.read_only);
        let this_group = pane.and_then(|p| p.group.as_deref());
        let indicator = groups::indicator(scope, focused, this_group, focused_group);
        if let Some(color) = pane_border_color(profile, indicator, read_only, focused, this_group) {
            let stroke = egui::Stroke::new(FOCUS_BORDER, color);
            ui.painter()
                .rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

/// Outline color for a pane. A grouped pane's border encodes group IDENTITY --
/// its stable `groups::group_color` -- so same-group panes match at a glance;
/// this replaces the broadcast-state color on the border (the transmit/receive
/// STATE stays shown by the titlebar dot). Ungrouped panes keep the broadcast
/// behavior: transmit and receive-on get distinct treatment -- full vs
/// half-intensity `broadcast_border`, matching the titlebar dots in
/// `title_bar::indicator_glyph` -- while an off / non-receiving pane uses the
/// normal focus ring (focused) or nothing. The read_only guard is unchanged and
/// takes precedence: a read_only pane (grouped or not) never receives input, so
/// it shows only the focus ring when focused, reproducing the old
/// `broadcast && !read_only` gate exactly.
fn pane_border_color(
    profile: &crate::config::Profile,
    indicator: Indicator,
    read_only: bool,
    focused: bool,
    group: Option<&str>,
) -> Option<egui::Color32> {
    let focus = || {
        let [r, g, b] = profile.focus_border_rgb();
        egui::Color32::from_rgb(r, g, b)
    };
    if read_only {
        return focused.then(focus);
    }
    if let Some(name) = group {
        return Some(groups::group_color(name));
    }
    let [r, g, b] = profile.broadcast_border_rgb();
    match indicator {
        Indicator::Transmit(BroadcastScope::Off) => Some(focus()),
        Indicator::Transmit(_) => Some(egui::Color32::from_rgb(r, g, b)),
        Indicator::ReceiveOn => Some(egui::Color32::from_rgb(r / 2, g / 2, b / 2)),
        Indicator::ReceiveOff => None,
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

/// Which half of the cell the cursor sits in. Selection includes the anchor
/// cell only when the cursor is on its right half, matching alacritty/vte.
fn cell_side(p: egui::Pos2, rect: egui::Rect, ppp: f32, cell_w: f32) -> Side {
    let rel_x = ((p.x - rect.left()).max(0.0)) * ppp;
    if rel_x % cell_w >= cell_w / 2.0 {
        Side::Right
    } else {
        Side::Left
    }
}

/// Mouse-motion dedupe: given the cell last reported for this pane and the cell
/// the pointer now resolves to, decide whether to forward a motion report (per
/// the term's mouse mode and whether a button is held) and what the new
/// last-reported cell should be. A report is only emitted on a cell transition,
/// so per-pixel moves within one cell are suppressed. Lifted out of the
/// per-frame motion block unchanged so the suppression is unit-testable.
fn motion_report(
    term_mode: TermMode,
    button_held: bool,
    last_cell: Option<(i32, i32)>,
    cell: (i32, i32),
) -> (bool, Option<(i32, i32)>) {
    let cell_changed = last_cell != Some(cell);
    let should_send = should_report_motion(term_mode, button_held, cell_changed);
    let new_last = if cell_changed { Some(cell) } else { last_cell };
    (should_send, new_last)
}

/// Resolve a raw pixel-derived click column to the wide-glyph base column and
/// then hit-test for a URL at that cell. Composing the spacer resolution with
/// the scan keeps a click on the right half of a double-width glyph (its
/// WIDE_CHAR_SPACER column) from missing a URL anchored on the base column.
fn url_at_click(cells: &[crate::pane::CellSnapshot], row: i32, col: i32) -> Option<UrlMatch> {
    let col = crate::pane::resolve_wide_click_col(cells, row, col);
    crate::pane::scan_url_at(cells, row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontStyle;
    use crate::pane::CellSnapshot;

    fn rect_origin() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 1000.0))
    }

    #[test]
    fn cell_at_maps_clamps_and_ppp_scales() {
        let rect = rect_origin();
        // Interior: with cell 10x20, pos(25,30) -> col 2 (25/10), row 1 (30/20).
        assert_eq!(cell_at(egui::pos2(25.0, 30.0), rect, 1.0, 10.0, 20.0), (2, 1));
        // Above-and-left of the rect clamps to the origin cell.
        assert_eq!(cell_at(egui::pos2(-5.0, -5.0), rect, 1.0, 10.0, 20.0), (0, 0));
        // pixels_per_point scales the effective coordinate: with cell_w=20,
        // pos.x=15 is in col 0 at ppp=1 but col 1 at ppp=2 (15*2/20 = 1.5).
        assert_eq!(cell_at(egui::pos2(15.0, 0.0), rect, 1.0, 20.0, 20.0), (0, 0));
        assert_eq!(cell_at(egui::pos2(15.0, 0.0), rect, 2.0, 20.0, 20.0), (1, 0));
    }

    #[test]
    fn cell_side_half_cell_threshold_inclusive_right() {
        let rect = rect_origin();
        // Within the first cell (cell_w=10): just under the half-cell is Left,
        // exactly the half-cell is Right (the threshold is inclusive `>=`).
        assert_eq!(cell_side(egui::pos2(4.9, 0.0), rect, 1.0, 10.0), Side::Left);
        assert_eq!(cell_side(egui::pos2(5.0, 0.0), rect, 1.0, 10.0), Side::Right);
        // The threshold is per-cell (modulo cell_w): the same boundary recurs in
        // the second cell at 14.9 / 15.0.
        assert_eq!(cell_side(egui::pos2(14.9, 0.0), rect, 1.0, 10.0), Side::Left);
        assert_eq!(cell_side(egui::pos2(15.0, 0.0), rect, 1.0, 10.0), Side::Right);
    }

    #[test]
    fn motion_report_suppresses_same_cell_reports_changed() {
        let drag = TermMode::MOUSE_DRAG;
        // Same cell with a button held: suppressed, last cell unchanged.
        assert_eq!(
            motion_report(drag, true, Some((5, 5)), (5, 5)),
            (false, Some((5, 5)))
        );
        // Changed cell with a button held: reported, last cell advanced.
        assert_eq!(
            motion_report(drag, true, Some((5, 5)), (6, 5)),
            (true, Some((6, 5)))
        );
        // No button held in drag mode: records the new cell but stays silent.
        assert_eq!(
            motion_report(drag, false, Some((5, 5)), (6, 5)),
            (false, Some((6, 5)))
        );
    }

    fn cell(col: i32, c: char, wide: bool, hyperlink: Option<&str>) -> CellSnapshot {
        CellSnapshot {
            col,
            row: 0,
            c,
            fg: [1.0; 4],
            bg: [0.0, 0.0, 0.0, 1.0],
            style: FontStyle::Regular,
            underline: Underline::None,
            underline_color: [1.0; 4],
            strikeout: false,
            wide,
            hidden: false,
            zerowidth: Vec::new(),
            hyperlink: hyperlink.map(str::to_string),
        }
    }

    #[test]
    fn url_at_click_resolves_wide_spacer_then_scans_hyperlink() {
        let uri = "https://example.com/foo";
        // A double-width glyph carrying an OSC-8 hyperlink at col 0, with its
        // blank WIDE_CHAR_SPACER at col 1 (no hyperlink of its own).
        let cells = vec![cell(0, '中', true, Some(uri)), cell(1, ' ', false, None)];
        // Clicking the spacer column directly finds nothing: the spacer has no
        // hyperlink and ' ' is not heuristically a URL.
        assert!(crate::pane::scan_url_at(&cells, 0, 1).is_none());
        // url_at_click first resolves the spacer back to the wide base column,
        // so the OSC-8 hyperlink is found.
        let m = url_at_click(&cells, 0, 1).expect("spacer should resolve to hyperlink");
        assert_eq!(m.url, uri);
        assert!(m.is_hyperlink);
        assert_eq!(m.start_col, 0);
    }

    fn unit_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0))
    }

    #[test]
    fn drop_zone_direction_picks_nearest_edge() {
        let rect = unit_rect();
        // Nearest the left edge -> vertical split, dropped pane first (left).
        assert_eq!(
            drop_zone_direction(rect, egui::pos2(10.0, 50.0)),
            (Direction::Vertical, true)
        );
        // Nearest the right edge -> vertical split, dropped pane second (right).
        assert_eq!(
            drop_zone_direction(rect, egui::pos2(90.0, 50.0)),
            (Direction::Vertical, false)
        );
        // Nearest the top edge -> horizontal split, dropped pane first (top).
        assert_eq!(
            drop_zone_direction(rect, egui::pos2(50.0, 10.0)),
            (Direction::Horizontal, true)
        );
        // Nearest the bottom edge -> horizontal split, dropped pane second (bottom).
        assert_eq!(
            drop_zone_direction(rect, egui::pos2(50.0, 90.0)),
            (Direction::Horizontal, false)
        );
    }

    #[test]
    fn drop_zone_rect_is_the_correct_half() {
        let rect = unit_rect();
        // Left edge -> left half.
        assert_eq!(
            drop_zone_rect(rect, egui::pos2(10.0, 50.0)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(50.0, 100.0))
        );
        // Right edge -> right half.
        assert_eq!(
            drop_zone_rect(rect, egui::pos2(90.0, 50.0)),
            egui::Rect::from_min_max(egui::pos2(50.0, 0.0), egui::pos2(100.0, 100.0))
        );
        // Top edge -> top half.
        assert_eq!(
            drop_zone_rect(rect, egui::pos2(50.0, 10.0)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0))
        );
        // Bottom edge -> bottom half.
        assert_eq!(
            drop_zone_rect(rect, egui::pos2(50.0, 90.0)),
            egui::Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(100.0, 100.0))
        );
    }

    #[test]
    fn pane_at_first_containing_rect_wins() {
        let a = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let b = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(100.0, 100.0));
        let rects: Vec<(PaneId, egui::Rect)> = vec![(7, a), (9, b)];
        // A point inside the first rect resolves to its id.
        assert_eq!(pane_at(egui::pos2(50.0, 50.0), &rects), Some(7));
        // A point inside the second rect resolves to its id.
        assert_eq!(pane_at(egui::pos2(150.0, 50.0), &rects), Some(9));
        // A point outside every rect resolves to None.
        assert_eq!(pane_at(egui::pos2(500.0, 500.0), &rects), None);
        // When rects overlap, the first listed match wins.
        let overlap: Vec<(PaneId, egui::Rect)> = vec![(1, a), (2, a)];
        assert_eq!(pane_at(egui::pos2(50.0, 50.0), &overlap), Some(1));
    }

    #[test]
    fn pane_border_color_distinguishes_transmit_receive_and_readonly() {
        use crate::config::Profile;
        let p = Profile::default();
        let focus = {
            let [r, g, b] = p.focus_border_rgb();
            egui::Color32::from_rgb(r, g, b)
        };
        let [br, bg, bb] = p.broadcast_border_rgb();
        let transmit = egui::Color32::from_rgb(br, bg, bb);
        let receive = egui::Color32::from_rgb(br / 2, bg / 2, bb / 2);

        // Ungrouped panes (group = None) keep the broadcast behavior.
        // Off + focused => the normal focus ring (Transmit(Off)).
        assert_eq!(
            pane_border_color(&p, Indicator::Transmit(BroadcastScope::Off), false, true, None),
            Some(focus)
        );
        // A focused transmitter under Group/All gets the full broadcast color.
        assert_eq!(
            pane_border_color(&p, Indicator::Transmit(BroadcastScope::All), false, true, None),
            Some(transmit)
        );
        assert_eq!(
            pane_border_color(&p, Indicator::Transmit(BroadcastScope::Group), false, true, None),
            Some(transmit)
        );
        // A receiver gets a distinct, dimmed broadcast ring.
        assert_eq!(
            pane_border_color(&p, Indicator::ReceiveOn, false, false, None),
            Some(receive)
        );
        assert_ne!(transmit, receive);
        // Not focused and not receiving => no ring.
        assert_eq!(
            pane_border_color(&p, Indicator::ReceiveOff, false, false, None),
            None
        );
        // read_only never shows the broadcast/receive ring even when it would
        // otherwise receive: only the focus ring when focused, else nothing.
        assert_eq!(pane_border_color(&p, Indicator::ReceiveOn, true, false, None), None);
        assert_eq!(
            pane_border_color(&p, Indicator::Transmit(BroadcastScope::All), true, true, None),
            Some(focus)
        );
    }

    #[test]
    fn pane_border_color_uses_group_color_for_grouped_panes() {
        use crate::config::Profile;
        let p = Profile::default();
        let work = crate::groups::group_color("work");
        // A grouped, non-read_only pane shows its stable group color regardless
        // of broadcast state or focus -- the group identity replaces the
        // broadcast-state border (state stays on the titlebar dot).
        assert_eq!(
            pane_border_color(&p, Indicator::ReceiveOff, false, false, Some("work")),
            Some(work),
            "an unfocused grouped pane is tagged with its group color"
        );
        assert_eq!(
            pane_border_color(&p, Indicator::Transmit(BroadcastScope::Off), false, true, Some("work")),
            Some(work),
            "a focused grouped pane shows the group color, not the focus ring"
        );
        // Different groups -> different colors, so members are told apart.
        assert_ne!(
            pane_border_color(&p, Indicator::ReceiveOff, false, false, Some("work")),
            pane_border_color(&p, Indicator::ReceiveOff, false, false, Some("logs")),
        );
        // The read_only guard still wins: a read_only grouped pane never shows
        // the group color -- only the focus ring when focused, else nothing.
        let focus = {
            let [r, g, b] = p.focus_border_rgb();
            egui::Color32::from_rgb(r, g, b)
        };
        assert_eq!(
            pane_border_color(&p, Indicator::ReceiveOff, true, true, Some("work")),
            Some(focus),
            "read_only grouped + focused => focus ring (guard preserved)"
        );
        assert_eq!(
            pane_border_color(&p, Indicator::ReceiveOff, true, false, Some("work")),
            None,
            "read_only grouped + unfocused => no ring (guard preserved)"
        );
    }
}
