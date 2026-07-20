//! Tab-strip subsystem: the sole owner of the tab bar's render, hit-testing,
//! interaction->event mapping, and apply-to-`TabManager`. Lifted out of the
//! `App::ui()` god-method so the strip lives in one focused module (mirrors the
//! `pane_ui` shape).
//!
//! - `show()` paints the strip and returns the frame's `Vec<TabBarEvent>`.
//! - `apply()` replays those events through the EXISTING `TabManager` methods.
//! - `panel_for()` picks the egui panel side (`None` for `Hidden`).
//!
//! The boundary: this module reads the tabs slice + each tab's title and writes
//! the manager ONLY via `switch_tab_direct`/`close_tab`/`new_tab_at`/
//! `reorder_tab`/`set_tab_title`; it touches panes only through `Pane::title()`
//! and adds ZERO `Action` variants. `Axis` unifies the horizontal (top/bottom)
//! and vertical (left/right) strips so there is no duplicated layout logic. The
//! pure helpers `layout_slots`/`insertion_index`/`tab_title` are unit-tested
//! without egui.

use crate::config::{GlobalConfig, TabPosition};
use crate::dialogs::DialogState;
use crate::tabs::{PaneFactory, Tab, TabManager};

/// Width reserved for the trailing `+` button (matches the old `btn_width`).
const NEW_TAB_BTN_W: f32 = 24.0;
/// Smallest homogeneous tab on a shrink-to-fit strip (old `.max(40.0)`).
const MIN_TAB: f32 = 40.0;
/// Cap on a content-width tab so one long title can't dominate the strip.
const MAX_TAB: f32 = 240.0;
/// Rough glyph advance (~13pt proportional) used only to *estimate* a tab's
/// natural width in the pure `layout_slots` (the painted galley is exact).
const EST_CHAR_W: f32 = 7.0;
/// Left padding + close-button gutter folded into the width estimate.
const TAB_PADDING: f32 = 28.0;
/// Per-tab height on a vertical (left/right) strip.
const ROW_H: f32 = 24.0;

/// Persistent per-window UI state for the strip — the ONE `App` field that
/// replaces the three `App::ui()` locals. Holds the in-flight drag and the
/// inline-rename buffer. `Default`/`new` = idle (no drag, no rename).
#[derive(Default)]
pub(crate) struct TabBarState {
    drag: Option<TabDrag>,
    rename: Option<TabRename>,
}

struct TabDrag {
    from: usize,
}

struct TabRename {
    index: usize,
    buf: String,
    focus_pending: bool,
}

impl TabBarState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// True while an inline-rename `TextEdit` is active, so `App::logic` keeps
    /// egui keyboard focus on it (ORed with `DialogState::wants_text_input`).
    pub(crate) fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }
}

/// Per-frame, borrowed snapshot the App derives from config + `TabManager` and
/// hands to `show()`.
pub(crate) struct TabBarView<'a> {
    pub(crate) tabs: &'a [Tab],
    pub(crate) active: usize,
    pub(crate) position: TabPosition,
    pub(crate) homogeneous: bool,
    pub(crate) close_button: bool,
    pub(crate) scroll: bool,
}

/// The egui-free interaction vocabulary `show()` emits and `apply()` consumes,
/// so event sequences can be asserted against a `TabManager` without a UI.
/// `Detach` is the reserved drag-off seam (a drop outside the strip), not
/// emitted by this block.
pub(crate) enum TabBarEvent {
    Select(usize),
    Close(usize),
    NewTab,
    Reorder { from: usize, to: usize },
    Rename { index: usize, title: Option<String> },
    // Detach { index: usize },  // reserved for the multi-window block
}

/// Strip orientation derived from `TabPosition`: top/bottom (and hidden) lay
/// out horizontally; left/right lay out vertically.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn of(pos: TabPosition) -> Self {
        match pos {
            TabPosition::Left | TabPosition::Right => Axis::Vertical,
            _ => Axis::Horizontal,
        }
    }
}

/// Everything `paint_tab` needs about one tab for the current frame.
struct TabVisual<'a> {
    index: usize,
    title: &'a str,
    selected: bool,
    hovered: bool,
    close_button: bool,
}

/// Which egui panel side hosts the strip, pre-styled with the strip frame.
/// `None` for `Hidden` (the panel is not drawn; tabs stay reachable by
/// keyboard). The vertical sides are pinned non-resizable so the strip behaves
/// like the fixed top bar.
pub(crate) fn panel_for(pos: TabPosition, id: &str) -> Option<egui::Panel> {
    // `Id` only converts from `&'static str`; build one from the borrowed id via
    // `Id::new` (which hashes any `impl Hash`) so the param can be a plain `&str`.
    let pid = egui::Id::new(id);
    let panel = match pos {
        TabPosition::Top => egui::Panel::top(pid),
        TabPosition::Bottom => egui::Panel::bottom(pid),
        TabPosition::Left => egui::Panel::left(pid).resizable(false),
        TabPosition::Right => egui::Panel::right(pid).resizable(false),
        TabPosition::Hidden => return None,
    };
    let frame = egui::Frame::new()
        .fill(egui::Color32::from_gray(40))
        .inner_margin(2.0);
    Some(panel.frame(frame))
}

/// Render the strip and return this frame's interactions. Stays small by
/// delegating: `layout_slots` (sizes) -> `strip`/`scroll_strip` (per-tab
/// `render_tab` + the `+` button) -> `finalize_drag` (drop -> `Reorder`).
pub(crate) fn show(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    view: &TabBarView,
) -> Vec<TabBarEvent> {
    let axis = Axis::of(view.position);
    let titles: Vec<String> = view
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| tab_title(tab, i))
        .collect();
    let slots = layout_slots(&titles, available_main(ui, axis), view, axis);

    let (rects, mut events) = if view.scroll {
        scroll_strip(ui, state, view, &titles, &slots, axis)
    } else {
        strip(ui, state, view, &titles, &slots, axis)
    };

    finalize_drag(ui, state, &rects, axis, &mut events);
    events
}

/// Apply collected events to the manager via its public methods only — a flat
/// match where every arm is a single existing call. `NewTab` honors
/// `new_tab_after_current` through the shared `tabs::tab_insert_index` helper
/// (the same one the `Action::NewTab` path uses), so placement isn't duplicated.
pub(crate) fn apply(
    events: Vec<TabBarEvent>,
    tab_mgr: &mut TabManager,
    factory: &PaneFactory,
    g: &GlobalConfig,
    ctx: &egui::Context,
    dialogs: &mut DialogState,
) {
    for event in events {
        match event {
            TabBarEvent::Select(i) => tab_mgr.switch_tab_direct((i + 1) as u8),
            TabBarEvent::Close(i) => tab_mgr.close_tab(i, ctx, dialogs),
            TabBarEvent::NewTab => {
                let at = crate::tabs::tab_insert_index(
                    tab_mgr.active_tab,
                    g.new_tab_after_current,
                    tab_mgr.tabs.len(),
                );
                tab_mgr.new_tab_at(at, factory);
            }
            TabBarEvent::Reorder { from, to } => tab_mgr.reorder_tab(from, to),
            TabBarEvent::Rename { index, title } => tab_mgr.set_tab_title(index, title),
        }
    }
}

/// Main-axis space available for the strip before any tab is allocated.
fn available_main(ui: &egui::Ui, axis: Axis) -> f32 {
    match axis {
        Axis::Horizontal => ui.available_width(),
        Axis::Vertical => ui.available_height(),
    }
}

/// Lay the tabs out along `axis`, returning their screen rects plus this
/// frame's events (`+` button included). Same body for both orientations; only
/// the egui container (`horizontal` vs `vertical`) differs.
fn strip(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    view: &TabBarView,
    titles: &[String],
    slots: &[f32],
    axis: Axis,
) -> (Vec<egui::Rect>, Vec<TabBarEvent>) {
    let body = |ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        let mut events = Vec::new();
        let mut rects = Vec::with_capacity(titles.len());
        for i in 0..titles.len() {
            rects.push(render_tab(ui, state, view, titles, slots, axis, i, &mut events));
        }
        if new_tab_button(ui) {
            events.push(TabBarEvent::NewTab);
        }
        (rects, events)
    };
    match axis {
        Axis::Horizontal => ui.horizontal(body).inner,
        Axis::Vertical => ui.vertical(body).inner,
    }
}

/// `strip` wrapped in a `ScrollArea` so an overflowing bar scrolls instead of
/// shrinking (USER Q3 `scroll_tabbar`). Orientation follows `axis`.
fn scroll_strip(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    view: &TabBarView,
    titles: &[String],
    slots: &[f32],
    axis: Axis,
) -> (Vec<egui::Rect>, Vec<TabBarEvent>) {
    let area = match axis {
        Axis::Horizontal => egui::ScrollArea::horizontal(),
        Axis::Vertical => egui::ScrollArea::vertical(),
    };
    area.show(ui, |ui| strip(ui, state, view, titles, slots, axis))
        .inner
}

/// Allocate, paint, and hit-test one tab; returns its rect for drop targeting.
/// The renaming tab swaps its label for an inline `TextEdit` and senses only
/// hover so clicks reach the editor.
fn render_tab(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    view: &TabBarView,
    titles: &[String],
    slots: &[f32],
    axis: Axis,
    i: usize,
    events: &mut Vec<TabBarEvent>,
) -> egui::Rect {
    let renaming = state.rename.as_ref().is_some_and(|r| r.index == i);
    let sense = if renaming {
        egui::Sense::hover()
    } else {
        egui::Sense::click_and_drag()
    };
    let size = tab_size(ui, slots[i], axis);
    let (rect, main) = ui.allocate_exact_size(size, sense);
    if renaming {
        inline_rename(ui, state, rect, i, events);
        return rect;
    }
    main.surrender_focus();
    let visual = TabVisual {
        index: i,
        title: titles[i].as_str(),
        selected: i == view.active,
        hovered: main.hovered(),
        close_button: view.close_button,
    };
    let close = paint_tab(ui, rect, &visual, axis);
    fold_tab_events(state, &main, close.as_ref(), titles[i].as_str(), i, events);
    tab_context_menu(&main, state, titles[i].as_str(), i, events);
    rect
}

/// Size of one tab: `slot` along the main axis, full extent across it.
fn tab_size(ui: &egui::Ui, slot: f32, axis: Axis) -> egui::Vec2 {
    match axis {
        Axis::Horizontal => egui::vec2(slot, ui.available_height()),
        Axis::Vertical => egui::vec2(ui.available_width(), slot),
    }
}

/// Paint a tab's background, optional close button, and elided label into
/// `rect`. Returns the close-button response (the only sub-response paint owns;
/// the main response is produced by the caller's allocation). Ported verbatim
/// from the old `App::ui()` tab block.
fn paint_tab(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    v: &TabVisual,
    _axis: Axis,
) -> Option<egui::Response> {
    let bg = if v.selected {
        egui::Color32::from_gray(60)
    } else if v.hovered {
        egui::Color32::from_gray(50)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 2.0, bg);

    let close = if v.close_button {
        let close_rect = close_button_rect(rect);
        let resp = ui.interact(
            close_rect,
            egui::Id::new(("tab_close", v.index)),
            egui::Sense::click(),
        );
        resp.surrender_focus();
        let x_color = if resp.hovered() {
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
        Some(resp)
    } else {
        None
    };

    paint_label(ui, rect, v.title, close.as_ref());
    close
}

/// The 14px close-button rect at the tab's right edge.
fn close_button_rect(rect: egui::Rect) -> egui::Rect {
    let size = 14.0;
    egui::Rect::from_min_size(
        egui::pos2(rect.right() - size - 4.0, rect.center().y - size / 2.0),
        egui::vec2(size, size),
    )
}

/// Paint the centered title elided to the area left of the close button (or to
/// the tab's right padding when the close button is hidden).
fn paint_label(ui: &mut egui::Ui, rect: egui::Rect, title: &str, close: Option<&egui::Response>) {
    let text_color = egui::Color32::from_gray(220);
    let right = close.map_or(rect.right() - 4.0, |c| c.rect.left() - 2.0);
    let text_area = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, rect.top()),
        egui::pos2(right, rect.bottom()),
    );
    let mut job = egui::text::LayoutJob::single_section(
        title.to_string(),
        egui::text::TextFormat::simple(egui::FontId::proportional(13.0), text_color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: text_area.width().max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('\u{2026}'),
    };
    let galley = ui.painter().layout_job(job);
    let pos = egui::Align2::CENTER_CENTER
        .anchor_size(text_area.center(), galley.size())
        .intersect(text_area);
    ui.painter().galley(pos.min, galley, text_color);
}

/// The trailing `+` button; returns whether it was left-clicked this frame.
fn new_tab_button(ui: &mut egui::Ui) -> bool {
    let btn = ui.small_button("+");
    btn.surrender_focus();
    btn.clicked_by(egui::PointerButton::Primary)
}

/// Map one tab's responses to events: close-button or middle-click -> `Close`,
/// double-click -> start inline rename, drag-start -> select + begin drag,
/// plain left-click -> `Select`. A close-button click never also selects (it is
/// handled first), reproducing the old `&& !close_resp.clicked()` guard.
fn fold_tab_events(
    state: &mut TabBarState,
    main: &egui::Response,
    close: Option<&egui::Response>,
    title: &str,
    i: usize,
    events: &mut Vec<TabBarEvent>,
) {
    if close.is_some_and(|c| c.clicked_by(egui::PointerButton::Primary)) {
        events.push(TabBarEvent::Close(i));
    } else if main.double_clicked() {
        state.rename = Some(TabRename {
            index: i,
            buf: title.to_string(),
            focus_pending: true,
        });
    } else if main.clicked_by(egui::PointerButton::Middle) {
        events.push(TabBarEvent::Close(i));
    } else if main.drag_started() {
        state.drag = Some(TabDrag { from: i });
        events.push(TabBarEvent::Select(i));
    } else if main.clicked_by(egui::PointerButton::Primary) {
        events.push(TabBarEvent::Select(i));
    }
}

/// Right-click tab menu: New Tab, inline Rename, Close (the modal "Set title"
/// path is kept elsewhere — USER Q4). Mirrors the `build_context_menu` style.
fn tab_context_menu(
    main: &egui::Response,
    state: &mut TabBarState,
    title: &str,
    i: usize,
    events: &mut Vec<TabBarEvent>,
) {
    main.context_menu(|ui| {
        if ui.button("New Tab").clicked() {
            events.push(TabBarEvent::NewTab);
            ui.close();
        }
        if ui.button("Rename\u{2026}").clicked() {
            state.rename = Some(TabRename {
                index: i,
                buf: title.to_string(),
                focus_pending: true,
            });
            ui.close();
        }
        ui.separator();
        if ui.button("Close Tab").clicked() {
            events.push(TabBarEvent::Close(i));
            ui.close();
        }
    });
}

/// Draw the inline-rename editor in `rect`. Enter / focus-loss commits a
/// `Rename` (empty -> clear the custom title); Escape cancels. The buffer lives
/// in `TabBarState` so it survives across frames.
fn inline_rename(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    rect: egui::Rect,
    i: usize,
    events: &mut Vec<TabBarEvent>,
) {
    let mut commit: Option<Option<String>> = None;
    let mut cancel = false;
    if let Some(rename) = state.rename.as_mut() {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(2.0)));
        let edit = child.add(egui::TextEdit::singleline(&mut rename.buf).desired_width(f32::INFINITY));
        if rename.focus_pending {
            edit.request_focus();
            rename.focus_pending = false;
        }
        if child.input(|inp| inp.key_pressed(egui::Key::Escape)) {
            cancel = true;
        } else if edit.lost_focus() {
            let trimmed = rename.buf.trim();
            commit = Some((!trimmed.is_empty()).then(|| trimmed.to_string()));
        }
    }
    if cancel {
        state.rename = None;
    } else if let Some(title) = commit {
        events.push(TabBarEvent::Rename { index: i, title });
        state.rename = None;
    }
}

/// While a drag is in flight, paint the insertion marker; on pointer release
/// turn the drop position into a `Reorder` (a drop off the strip is a no-op —
/// the reserved Detach seam). The drop gap is converted to a destination index,
/// accounting for the source tab being removed first.
fn finalize_drag(
    ui: &mut egui::Ui,
    state: &mut TabBarState,
    rects: &[egui::Rect],
    axis: Axis,
    events: &mut Vec<TabBarEvent>,
) {
    let Some(&TabDrag { from }) = state.drag.as_ref() else {
        return;
    };
    let pointer = ui.input(|i| i.pointer.interact_pos());
    if let Some(slot) = pointer.and_then(|p| insertion_index(rects, p, axis)) {
        draw_insertion_marker(ui, rects, slot, axis);
    }
    if ui.input(|i| !i.pointer.any_down()) {
        if let Some(slot) = pointer.and_then(|p| insertion_index(rects, p, axis)) {
            // `slot` is a gap in `0..=len`; removing `from` first shifts later
            // gaps down by one, so dropping to the right of the source lands at
            // `slot - 1`.
            let to = if slot > from { slot - 1 } else { slot };
            if to != from {
                events.push(TabBarEvent::Reorder { from, to });
            }
        }
        state.drag = None;
    }
}

/// Paint the drop-insertion line at gap `slot` spanning the strip's cross axis.
fn draw_insertion_marker(ui: &mut egui::Ui, rects: &[egui::Rect], slot: usize, axis: Axis) {
    let Some(bounds) = rects.iter().copied().reduce(|a, b| a.union(b)) else {
        return;
    };
    let pos = if slot == 0 {
        match axis {
            Axis::Horizontal => rects[0].left(),
            Axis::Vertical => rects[0].top(),
        }
    } else {
        let r = rects[slot.min(rects.len()) - 1];
        match axis {
            Axis::Horizontal => r.right(),
            Axis::Vertical => r.bottom(),
        }
    };
    let stroke = egui::Stroke::new(2.0, egui::Color32::from_gray(200));
    match axis {
        Axis::Horizontal => ui.painter().vline(pos, bounds.y_range(), stroke),
        Axis::Vertical => ui.painter().hline(bounds.x_range(), pos, stroke),
    };
}

// --- pure helpers (unit-tested without egui) --------------------------------

/// Main-axis size for each tab. Horizontal + homogeneous + no-scroll reproduces
/// the old `((avail - btn)/count).max(40)`; homogeneous + scroll keeps every
/// tab at the widest natural width (overflow scrolls); non-homogeneous sizes
/// each tab to its estimated content width; a vertical strip uses fixed rows.
fn layout_slots(titles: &[String], avail_main: f32, view: &TabBarView, axis: Axis) -> Vec<f32> {
    let n = titles.len();
    if n == 0 {
        return Vec::new();
    }
    if axis == Axis::Vertical {
        return vec![ROW_H; n];
    }
    if view.homogeneous {
        if view.scroll {
            let widest = titles.iter().map(|t| content_width(t)).fold(MIN_TAB, f32::max);
            return vec![widest; n];
        }
        let w = ((avail_main - NEW_TAB_BTN_W) / n as f32).max(MIN_TAB);
        return vec![w; n];
    }
    titles.iter().map(|t| content_width(t)).collect()
}

/// Estimated natural width of a tab from its title length, clamped to the
/// strip's tab-size band.
fn content_width(title: &str) -> f32 {
    (title.chars().count() as f32 * EST_CHAR_W + TAB_PADDING).clamp(MIN_TAB, MAX_TAB)
}

/// Drop target for a drag: the gap index in `0..=len` under `pointer`, or
/// `None` when the pointer is off the strip's cross axis (the reserved Detach
/// seam). The gap is the count of tab centers the pointer has passed along the
/// main axis.
fn insertion_index(rects: &[egui::Rect], pointer: egui::Pos2, axis: Axis) -> Option<usize> {
    let bounds = rects.iter().copied().reduce(|a, b| a.union(b))?;
    let on_strip = match axis {
        Axis::Horizontal => pointer.y >= bounds.top() && pointer.y <= bounds.bottom(),
        Axis::Vertical => pointer.x >= bounds.left() && pointer.x <= bounds.right(),
    };
    if !on_strip {
        return None;
    }
    let crossed = rects
        .iter()
        .filter(|r| match axis {
            Axis::Horizontal => pointer.x > r.center().x,
            Axis::Vertical => pointer.y > r.center().y,
        })
        .count();
    Some(crossed)
}

/// Per-tab title: custom title, else the focused pane's `title()`, else a
/// 1-based `Tab N`. Extracted verbatim from the old `App::ui()` derivation.
fn tab_title(tab: &Tab, index: usize) -> String {
    tab.custom_title.clone().unwrap_or_else(|| {
        tab.panes
            .get(&tab.focused)
            .and_then(|p| p.title())
            .unwrap_or_else(|| format!("Tab {}", index + 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Node;
    use std::collections::HashMap;

    fn test_view(homogeneous: bool, scroll: bool) -> TabBarView<'static> {
        TabBarView {
            tabs: &[],
            active: 0,
            position: TabPosition::Top,
            homogeneous,
            close_button: true,
            scroll,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn layout_slots_homogeneous_divides_available_width() {
        // Three tabs share (324 - 24)/3 = 100px each (the old `App::ui` formula).
        let titles = strings(&["a", "b", "c"]);
        let slots = layout_slots(&titles, 324.0, &test_view(true, false), Axis::Horizontal);
        assert_eq!(slots, vec![100.0, 100.0, 100.0]);
    }

    #[test]
    fn layout_slots_homogeneous_floors_at_min() {
        // Width-starved -> every tab clamps up to MIN_TAB (the old `.max(40.0)`).
        let titles = strings(&["a", "b", "c"]);
        let slots = layout_slots(&titles, 24.0, &test_view(true, false), Axis::Horizontal);
        assert_eq!(slots, vec![MIN_TAB, MIN_TAB, MIN_TAB]);
    }

    #[test]
    fn layout_slots_content_width_sizes_per_title() {
        // Non-homogeneous: each tab is sized to its own estimated content width.
        let titles = strings(&["ab", "abcdef"]);
        let slots = layout_slots(&titles, 1000.0, &test_view(false, false), Axis::Horizontal);
        assert_eq!(slots, vec![content_width("ab"), content_width("abcdef")]);
        assert!(slots[1] > slots[0], "the longer title gets a wider slot");
    }

    #[test]
    fn layout_slots_scroll_keeps_equal_natural_width() {
        // Homogeneous + scroll: no shrink; all tabs take the widest natural width
        // and overflow the cramped available space.
        let titles = strings(&["x", "a-long-tab-title"]);
        let slots = layout_slots(&titles, 50.0, &test_view(true, true), Axis::Horizontal);
        let widest = content_width("a-long-tab-title");
        assert_eq!(slots, vec![widest, widest]);
        assert!(widest > 50.0, "natural width ignores the cramped available width");
    }

    #[test]
    fn layout_slots_vertical_uses_fixed_row_height() {
        let titles = strings(&["a", "b"]);
        let slots = layout_slots(&titles, 500.0, &test_view(true, false), Axis::Vertical);
        assert_eq!(slots, vec![ROW_H, ROW_H]);
    }

    #[test]
    fn layout_slots_empty_is_empty() {
        let slots = layout_slots(&[], 300.0, &test_view(true, false), Axis::Horizontal);
        assert!(slots.is_empty());
    }

    #[test]
    fn insertion_index_maps_pointer_to_gap() {
        // Three 100px tabs; centers at x = 50, 150, 250.
        let rects = vec![
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(100.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(200.0, 0.0), egui::vec2(100.0, 20.0)),
        ];
        assert_eq!(insertion_index(&rects, egui::pos2(10.0, 10.0), Axis::Horizontal), Some(0));
        assert_eq!(insertion_index(&rects, egui::pos2(120.0, 10.0), Axis::Horizontal), Some(1));
        assert_eq!(insertion_index(&rects, egui::pos2(260.0, 10.0), Axis::Horizontal), Some(3));
    }

    #[test]
    fn insertion_index_none_when_off_strip() {
        let rects = vec![egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0))];
        // Below the strip's y-band -> the reserved Detach seam.
        assert_eq!(insertion_index(&rects, egui::pos2(50.0, 80.0), Axis::Horizontal), None);
        // Empty strip -> nothing to target.
        assert_eq!(insertion_index(&[], egui::pos2(0.0, 0.0), Axis::Horizontal), None);
    }

    #[test]
    fn insertion_index_vertical_uses_y() {
        // Two stacked rows; centers at y = 10, 30.
        let rects = vec![
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(120.0, 20.0)),
        ];
        assert_eq!(insertion_index(&rects, egui::pos2(10.0, 5.0), Axis::Vertical), Some(0));
        assert_eq!(insertion_index(&rects, egui::pos2(10.0, 25.0), Axis::Vertical), Some(1));
        assert_eq!(insertion_index(&rects, egui::pos2(10.0, 35.0), Axis::Vertical), Some(2));
        // Outside the x-band -> the Detach seam.
        assert_eq!(insertion_index(&rects, egui::pos2(200.0, 25.0), Axis::Vertical), None);
    }

    #[test]
    fn tab_title_prefers_custom_then_numbered_fallback() {
        let custom = Tab {
            panes: HashMap::new(),
            layout: Node::Leaf(0),
            focused: 0,
            zoomed: None,
            custom_title: Some("Custom".to_string()),
        };
        assert_eq!(tab_title(&custom, 0), "Custom");

        // No custom title and no resolvable pane -> 1-based "Tab N".
        let fallback = Tab {
            panes: HashMap::new(),
            layout: Node::Leaf(0),
            focused: 0,
            zoomed: None,
            custom_title: None,
        };
        assert_eq!(tab_title(&fallback, 4), "Tab 5");
    }
}
