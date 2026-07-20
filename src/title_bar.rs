//! Per-pane title bar painting.
//!
//! Extracted out of `pane_ui::draw_panes` so the paint is a pure function of a
//! precomputed value model, free of any `TabManager` borrow. The interaction
//! (click/drag to focus or start a tab-drag) stays inline in `pane_ui.rs`; this
//! module only draws.

use egui::{Align2, Color32, FontId};

use crate::config::Config;
use crate::groups::{BroadcastScope, Indicator};

/// Height (in egui points) of the per-pane title strip. Mirrors the value used
/// by `pane_ui` when carving the terminal rect.
pub const PANE_TITLE_HEIGHT: f32 = 20.0;

/// Everything `paint` needs, precomputed by the caller so paint never borrows
/// the `TabManager`.
#[derive(Clone, Debug)]
pub struct TitleBarModel {
    /// Pane title text (without dimensions); empty if unset.
    pub title: String,
    pub cols: usize,
    pub lines: usize,
    /// Group name label, shown when the pane belongs to a group.
    pub group: Option<String>,
    /// Broadcast role of this pane relative to the focused pane.
    pub indicator: Indicator,
    pub focused: bool,
}

/// Split a pane rect into its (title, terminal) sub-rects via `PANE_TITLE_HEIGHT`.
pub fn split_rect(pane_rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    let title = egui::Rect::from_min_size(
        pane_rect.min,
        egui::vec2(pane_rect.width(), PANE_TITLE_HEIGHT),
    );
    let terminal = egui::Rect::from_min_max(
        egui::pos2(pane_rect.left(), pane_rect.top() + PANE_TITLE_HEIGHT),
        pane_rect.max,
    );
    (title, terminal)
}

/// Paint the title strip: background, centered title+dimensions text, the
/// left-aligned group-name label, and the right-aligned broadcast dot.
pub fn paint(ui: &mut egui::Ui, title_rect: egui::Rect, model: &TitleBarModel, cfg: &Config) {
    let bg = if model.focused {
        Color32::from_gray(50)
    } else {
        Color32::from_gray(30)
    };
    ui.painter().rect_filled(title_rect, 0.0, bg);

    let text_color = if model.focused {
        Color32::from_gray(220)
    } else {
        Color32::from_gray(140)
    };

    // Centered: "<name>  <cols>x<lines>", or just the dims when unnamed.
    let dims = format!("{}x{}", model.cols, model.lines);
    let title_text = if model.title.is_empty() {
        dims
    } else {
        format!("{}  {}", model.title, dims)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(title_text, FontId::proportional(12.0), text_color);
    let pos = Align2::CENTER_CENTER.anchor_size(title_rect.center(), galley.size());
    ui.painter().galley(pos.min, galley, text_color);

    // Left-aligned group-name label, tinted with the group's stable color
    // (`groups::group_color`) so the header matches the pane's border.
    if let Some(name) = model.group.as_deref().filter(|n| !n.is_empty()) {
        let tint = crate::groups::group_color(name);
        let g = ui.painter().layout_no_wrap(
            name.to_string(),
            FontId::proportional(11.0),
            tint,
        );
        let anchor = egui::pos2(title_rect.left() + 6.0, title_rect.center().y);
        let gp = Align2::LEFT_CENTER.anchor_size(anchor, g.size());
        ui.painter().galley(gp.min, g, tint);
    }

    // Right-aligned broadcast dot (none when off / not a receiver).
    if let Some(color) = indicator_glyph(model.indicator, cfg) {
        let r = 4.0;
        let center = egui::pos2(title_rect.right() - r - 6.0, title_rect.center().y);
        ui.painter().circle_filled(center, r, color);
    }
}

/// Map a broadcast indicator to its dot color, reusing `broadcast_border_rgb`.
/// Transmit (group/all) is the full color; receive-on is a dimmed variant so it
/// reads as distinct; off / receive-off draw nothing.
fn indicator_glyph(ind: Indicator, cfg: &Config) -> Option<Color32> {
    let [r, g, b] = cfg.active().broadcast_border_rgb();
    let full = Color32::from_rgb(r, g, b);
    match ind {
        Indicator::Transmit(BroadcastScope::Off) => None,
        Indicator::Transmit(_) => Some(full),
        Indicator::ReceiveOn => Some(Color32::from_rgb(r / 2, g / 2, b / 2)),
        Indicator::ReceiveOff => None,
    }
}
