# Tier-1 Leaf Features — Design Contract (2026-06-30)

Facelift of Terminator. Mirror Terminator behavior/keybinds; no invented UX. No god
classes; every implied function < 100 lines and lives in a named focused helper, not
piled onto `draw_panes` / `execute_pane_actions` / `TabManager`.

Ground-truth verified against `src/*.rs` and `gap-analysis.md`. Each leaf below pins
Terminator's real behavior, the EXACT shared symbols (name / type / location) so
independent file owners stay aligned, new Action/keybind, and test seams.

---

## Leaf 1 — Scaled zoom (Gap #15, gap-analysis.md:29)

### Terminator behavior (verified)
Terminator has two distinct "fill the window with one terminal" actions:
- `toggle_zoom` (`terminal.key_toggle_zoom` -> `window.maximise` -> `window.zoom(font_scale=False)`):
  maximize the terminal, hide siblings, **font unchanged**. This is rustinator's
  existing `Action::ToggleZoom` (Ctrl+Shift+X, prefs label "Maximize pane",
  `tabs.rs::toggle_zoom` sets `tab.zoomed`).
- `scaled_zoom` (`terminal.key_scaled_zoom` -> `window.zoom(font_scale=True)`):
  maximize the terminal AND scale the font by `min(new_w/old_w, new_h/old_h)` of the
  maximized rect over the pre-zoom pane rect (>= 1.0), so the same content fills the
  larger area at a bigger font. This is the missing leaf.

### Minimal faithful behavior
`ScaledZoom` = reuse the existing maximize (`tab.zoomed = Some(focused)`, which already
hides siblings — `pane_ui.rs:66-77` renders only the zoomed leaf at `root_rect`) PLUS a
**global** font scale multiplier applied while active. rustinator has one global
`FontContext`, but only the zoomed pane is visible, so scaling the global font matches
Terminator's per-terminal scale for the active tab. Toggle off restores scale 1.0.

### Where the zoom scale lives
On `FontState` (`src/main.rs:46`) as a new `zoom_scale: f32` (default 1.0). Effective
rasterization size becomes `size_override.unwrap_or(base_size) * zoom_scale * scale_factor`.
The three existing rebuild methods (`reset_font_size`, `apply_font_size`,
`set_scale_factor`) are refactored to one private `FontState::rebuild()` so zoom folds in
once. zoom_scale defaults 1.0 => **zero behavior change** on every non-zoom path.

Invariant (single source of truth, no per-tab flag): `zoom_scale != 1.0` only while the
active tab is scaled-zoomed. `App::sync_zoom_scale()` runs once per logic pass and resets
to 1.0 whenever `active_tab.zoomed.is_none()` — this covers untoggle, ClosePane, and
focus-driven unzoom without threading resets through every clear site.

### Shared symbols
- `font::scaled_zoom_factor(pane_w: f32, pane_h: f32, full_w: f32, full_h: f32) -> f32`
  — pure: `min(full_w/pane_w, full_h/pane_h)` clamped `>= 1.0`; returns 1.0 on any
  non-positive dimension. (NEW, `src/font.rs`)
- `FontState.zoom_scale: f32`; `FontState::effective_px(&self) -> f32`;
  `FontState::set_zoom_scale(&mut self, scale, renderer, family, tab_mgr)`;
  private `FontState::rebuild(&mut self, renderer, family, tab_mgr)`. (NEW/refactor, `src/main.rs`)
- `App::toggle_scaled_zoom(&mut self)` and `App::sync_zoom_scale(&mut self)`. (NEW, `src/main.rs`)
- `Action::ScaledZoom`; from_str `"scaled_zoom"`; default binding `Ctrl+Shift+Z`
  (uncomment stub `keybindings.rs:325`). (NEW, `src/keybindings.rs`)
- Reuses existing `TabManager::toggle_zoom` (`tabs.rs:546`), `pane_view.last_pane_rect`
  + `last_root_rect` (`pane_ui.rs:24-25`).

### Keybind / Action
`Action::ScaledZoom`, default `Ctrl+Shift+Z`, prefs label "Zoom pane (scaled font)".

### Test seams
- `font::scaled_zoom_factor`: half-width pane -> 2.0; equal rects -> 1.0; zero dim -> 1.0.
- `App::sync_zoom_scale` invariant: assert documented (manual) — scale resets when no
  tab zoomed. (Pure helper is the unit-testable core.)

### Skipped (surfaced, not coded)
Switching from a scaled-zoomed tab directly to ANOTHER scaled-zoomed tab leaves the
first tab's global font scale until the next un-zoom; rare, left as a one-liner.

---

## Leaf 2 — Recursive balance (Gap #45, gap-analysis.md:30)

### Terminator behavior (verified)
Double-click a pane separator equalizes that one Paned to 50/50; **Super+double-click**
recursively redistributes every Paned in the tree to 50/50. rustinator already does the
single case: `pane_ui.rs:291-292` `if resp.double_clicked() { tab.layout.set_ratio(&div.path, 0.5) }`.
Missing: the Super+double-click recursive equalize.

### Minimal faithful behavior
On a divider double-click, if Super is held -> `tab.layout.rebalance_recursive()` (all
ratios 0.5); else keep the existing single-divider 50/50. Layout ratios change ->
next `draw_panes` re-walks rects -> `paint_pane` resizes each PTY; no extra plumbing.

### Shared symbols
- `Node::rebalance(&mut self)` — if `self` is a `Split`, set its `ratio = 0.5` (no
  recursion). Satisfies the `#[ignore]`d `rebalance_equalizes_ratios` (`layout.rs:504-508`). (NEW, `src/layout.rs`)
- `Node::rebalance_recursive(&mut self)` — set `ratio = 0.5` and recurse into both
  children. Satisfies `rebalance_recursive_equalizes_nested` (`layout.rs:511-515`). (NEW, `src/layout.rs`)
- Super detection in `handle_dividers` via `ui.input(|i| i.modifiers.mac_cmd)` —
  consistent with the existing keybinding parser mapping `super|cmd|command -> mac_cmd`
  (`keybindings.rs:390`). Reuses existing `div.path`, `tab.layout`.

### Keybind / Action
No new Action — mouse interaction only (Super + double-click on a divider handle).

### Test seams
- Un-`#[ignore]` and implement both layout tests: build `Vertical(L1 | Horizontal(L2,L3))`,
  set non-0.5 ratios, call method, assert all touched ratios == 0.5 (recursive) vs only
  root (single).

### Skipped (surfaced, not coded)
egui's `Modifiers` has no Super/logo field on Linux (only macOS `mac_cmd`), so
Super+double-click is macOS-only here — identical to the existing `Super+R` limitation.
Cross-platform fix (plumb winit `current_modifiers.state().super_key()` from
`window.rs:558` into `PaneViewCtx`) is a documented follow-up.

---

## Leaf 3 — Cell-aware keyboard resize (Gap #31, gap-analysis.md:31)

### Terminator behavior (verified)
Terminator panes use GTK geometry hints so separator positions snap to whole character
cells. rustinator's keyboard resize (`Ctrl+Shift+Arrows` ->
`Action::Resize{Left,Right,Up,Down}`) currently nudges by a fixed `±0.05` ratio
(`main.rs:379-382`), which is not cell-aligned. Make each press move the divider by
exactly one cell (one column for a vertical split, one row for a horizontal split),
snapped to the cell grid; the `set_ratio_min_cells` min-cells clamp (`layout.rs:236`)
still applies.

### Minimal faithful behavior
Reinterpret the resize delta as a signed **cell step** (±1) instead of a ratio. Inside
`resize_split` (which already locates the nearest divider of the target axis and has
`parent_rect`, `cell_w/h`, `ppp`, `min_cells` — `tabs.rs:560-619`), convert the divider's
current boundary to cells, add the step, snap back to a ratio, and clamp.

### Shared symbols
- `layout::ratio_after_cell_step(current_ratio: f32, step: i32, container_px: f32, cell_px: f32, min_cells: f32) -> f32`
  — pure free fn: `total = (container_px/cell_px)`; `cur = (current_ratio*total).round()`;
  `new = (cur + step).clamp(min_cells, total - min_cells)`; return `new/total` (guard
  `total <= 0` and `2*min_cells > total`). (NEW, `src/layout.rs`)
- `TabManager::resize_split` signature changes `delta: f32` -> `step: i32`; body calls
  `ratio_after_cell_step` then existing `set_ratio_min_cells`. (`src/tabs.rs:560`)
- Dispatch arms pass `-1 / +1` instead of `-0.05 / +0.05`. (`src/main.rs:379-382`)

### Keybind / Action
No new Action/keybind — same `Ctrl+Shift+Arrows`; only the step semantics change.

### Test seams
- `layout::ratio_after_cell_step`: container 100px / cell 10px (10 cells) at ratio 0.5,
  step +1 -> 0.6; step -1 -> 0.4; clamp at `min_cells`; degenerate `total<=0` -> input.

---

## Leaf 4 — DnD drop-zone highlight overlay (Gap #4 DnD, gap-analysis.md:32,139)

### Terminator behavior (verified)
While dragging a terminal, Terminator paints a translucent zone over the hovered target
showing exactly which half (top/bottom/left/right) the dropped terminal will occupy
(directional split-drop). rustinator's pane DnD already does the directional split-drop
(`handle_drag_drop` + `drop_zone_direction`/`drop_zone_rect`, `pane_ui.rs:642-668,1186-1225`)
and already paints a faint half-zone fill inline (`pane_ui.rs:223-236`). Terminals
composite via `egui_glow::CallbackFn` (`pane_ui.rs:818`), so an egui rect painted after
`paint_pane` is on top of the GL terminal — the overlay is visible. Gap: it is piled
inline in `draw_panes`, faint, and has no border (Terminator draws an outline).

### Minimal faithful behavior
Extract the inline block into a focused helper and bring it to Terminator parity: a
translucent accent fill over `drop_zone_rect(target, pos)` PLUS a 2px accent border, drawn
above the GL pane (unchanged z-order).

### Shared symbols
- `paint_drop_zone_overlay(painter: &egui::Painter, target_rect: egui::Rect, pos: egui::Pos2)`
  — calls existing `drop_zone_rect`, draws fill + `Stroke` border. (NEW helper, `src/pane_ui.rs`)
- Reuses existing `PaneViewState.drag_source_pane` (`pane_ui.rs:23`), `drop_zone_rect`,
  `drop_zone_direction`. The inline block at `pane_ui.rs:223-236` is replaced by a call
  to the helper.

### Keybind / Action
None — visual during an in-progress title-bar pane drag.

### Test seams
- `drop_zone_rect` / `drop_zone_direction` are already pure — add unit tests for the four
  quadrant outcomes (left/right -> Vertical; top/bottom -> Horizontal; correct half rect).

---

## Leaf 5 — External DnD / dropped files (Gap, gap-analysis.md:33,138)

### Terminator behavior (verified)
Dropping a file/URI/text from outside onto a terminal pastes its text (file path) at the
target terminal under the cursor. rustinator has NO `WindowEvent::DroppedFile` handling
(main-window match `window.rs:597-628` lacks it) and does not track the cursor position
for the main window.

### Minimal faithful behavior
winit's `DroppedFile` carries only a `PathBuf` (no coordinates), so track the last
`CursorMoved` position. On `DroppedFile`, record `(path-string, cursor-as-points)` into
shared state; on the next `draw_panes` (where pane rects are known) a focused consumer
finds the pane under the cursor and `send_paste`s the path into it. read_only panes
reject it (mirrors `paste_primary`, `tabs.rs:674-686`).

### Shared symbols
- `pane_ui::ExternalDrop { text: String, pos: egui::Pos2 }` and
  `PaneViewState.pending_external_drop: Option<ExternalDrop>` (`pane_ui.rs:22`). (NEW)
- `pane_ui::pane_at(pos: egui::Pos2, rects: &[(PaneId, egui::Rect)]) -> Option<PaneId>`
  — pure hit-test, reused by the external-drop consumer (and available to
  `handle_drag_drop`). (NEW, `src/pane_ui.rs`)
- `apply_external_drop(state: &mut PaneViewState, tab_mgr: &mut TabManager, rects: &[(PaneId, egui::Rect)])`
  — focused consumer called next to `handle_drag_drop` in `draw_panes`. (NEW, `src/pane_ui.rs`)
- `TabManager::paste_text_into_pane(&self, pane_id: PaneId, text: &str)` — `send_paste`
  if `!read_only` (mirrors `paste_primary`). (NEW, `src/tabs.rs`)
- `WinitApp.last_cursor_pos: Option<winit::dpi::PhysicalPosition<f64>>` updated on
  `WindowEvent::CursorMoved`; `WindowEvent::DroppedFile` arm converts via window
  scale_factor (`pos_points = physical / scale`) and writes `app.pane_view.pending_external_drop`.
  (NEW, `src/window.rs`)

### Keybind / Action
None — OS drag-and-drop (winit `DroppedFile`). Ctrl+right-drag is the same external-drop
path: the dropped text/path is pasted into the pane under the cursor.

### Test seams
- `pane_ui::pane_at`: pos inside a rect -> its id; pos outside all -> None; first match wins.

### Skipped (surfaced, not coded)
`HoveredFile`/`HoveredFileCancelled` hover-preview highlight is not drawn (drop still
works); add later by reusing `paint_drop_zone_overlay`. winit delivers file paths only,
not arbitrary clipboard text, so "text/URI" in practice = the dropped path string.

---

## File ownership summary

- **src/font.rs** — Leaf 1: `scaled_zoom_factor` pure helper + tests.
- **src/layout.rs** — Leaf 2: `rebalance`, `rebalance_recursive` + un-ignore 2 tests;
  Leaf 3: `ratio_after_cell_step` pure helper + tests.
- **src/tabs.rs** — Leaf 3: `resize_split` step→cell snap; Leaf 5: `paste_text_into_pane`.
- **src/pane_ui.rs** — Leaf 2: Super+double-click → `rebalance_recursive`;
  Leaf 4: `paint_drop_zone_overlay`; Leaf 5: `ExternalDrop`, `pending_external_drop`,
  `pane_at`, `apply_external_drop`.
- **src/window.rs** — Leaf 5: `last_cursor_pos` + `CursorMoved`/`DroppedFile` arms.
- **src/main.rs + src/keybindings.rs** — Leaf 1: `FontState.zoom_scale`/`effective_px`/
  `rebuild`/`set_zoom_scale`, `App::toggle_scaled_zoom`, `App::sync_zoom_scale`,
  `Action::ScaledZoom` (enum/from_str/exhaustive match `kb.rs:975`/all-actions vec
  `kb.rs:1007`/prefs label `prefs_ui.rs:534`), Ctrl+Shift+Z default, dispatch arm;
  Leaf 3: 4 resize dispatch arms pass ±1 cell step.
