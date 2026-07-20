# Design: Tab-bar subsystem (INFRA-Tabbar)

WF-C infra block. Status: design, no code. Line numbers verified against source on 2026-06-30.

## 1. What changes, in one paragraph

The tab bar today is ~115 lines inline inside the App god-method `App::ui()`
(`main.rs:692-806`: an `egui::Panel::top("tab_bar")` block at `695-797` plus the locals
`clicked_tab`/`closed_tab`/`new_tab_requested` and their dispatch at `798-806`). Lift it into
`src/tab_bar.rs` mirroring the `pane_ui` shape: a persistent `TabBarState` (ONE new App field
replacing the 3 locals), a per-frame `TabBarView`, `show()` returning `Vec<TabBarEvent>` for
render + hit-test, and `apply()` that drives EXISTING `TabManager` methods. egui 0.34.1
(verified `Cargo.toml:11`) exposes a unified `egui::Panel` with `top`/`bottom` already used in
this codebase (`main.rs:695`, `dialogs.rs:58`); `left`/`right` share one render path differing
only by an `Axis`. The 4 config defaults reproduce current behavior exactly (top, homogeneous,
close-button shown, append), so extraction + config land as a zero-behavior checkpoint before
any feature is switched on. Detach-to-window is OUT (multi-window infra); the drag
drop-outside-strip branch is the exact seam a future `Detach` event hooks into. This block adds
ZERO `Action` variants (so none of the keybindings exhaustiveness/reachability harness churns).

## 2. Terminator behavior being mirrored (no invented UX)

Left-click a tab = select (today `switch_tab_direct`); double-click the label = inline rename
(in-place entry, Enter commit / Esc cancel); right-click = tab context menu; middle-click =
close that tab; drag a tab = reorder (press selects it, drop reorders, insertion marker shown);
the `+` button = new tab. Defaults mirror Terminator and equal current behavior:
`tab_position=top` (hidden = panel not drawn, tabs reachable only by keyboard),
`homogeneous=true` (equal width), `close_button_on_tab=true`, `new_tab_after_current=false`
(append). Existing keybindings unchanged: NewTab `Ctrl/Cmd+Shift+T`, NextTab `Ctrl+PageDown`,
PrevTab `Ctrl+PageUp`, MoveTabRight `Ctrl+Shift+PageDown`, MoveTabLeft `Ctrl+Shift+PageUp`,
SwitchToTab `Ctrl/Cmd+1..9` (`keybindings.rs:90-99,292,308-311`).

## 3. Ground-truth facts

- The tab strip is inline in `App::ui()` (`main.rs:692-806`); the per-tab title derivation is at
  `main.rs:706-711` (`tab.custom_title` else focused pane `title()` else `format!("Tab {}", i+1)`).
- Dispatch after the strip: `closed_tab` -> `close_tab` (`main.rs:798-799`); `clicked_tab` ->
  `switch_tab_direct` (`800-801`); `new_tab_requested` -> `new_tab` (`803-806`).
- `TabManager` methods already present: `switch_tab_direct` (`tabs.rs:443`), `close_tab`
  (`tabs.rs:339`), `new_tab` (`tabs.rs:404`), `move_tab` (`tabs.rs:458`). `Tab.custom_title`
  (`tabs.rs:21`). `total_alive_panes` (`tabs.rs:228`).
- The existing modal "Set tab title" dialog: `DialogState.title_dialog_open`/`title_dialog_buf`
  (`dialogs.rs:6-7`), `draw_title_dialog` (`dialogs.rs:155-199`), opened by `Action::SetTitle`
  (`main.rs:426-432`).
- `GlobalConfig` (`config.rs:51-57`) has `#[serde(default)]` (line 52) + a `Default` impl
  (`177-184`); `config_global_tab_position` `#[ignore]` test at `config.rs:662-666`.
- prefs: `draw_prefs_global` (`prefs_ui.rs:160-208`) is where new tab settings attach.
- egui Panel API in use: `egui::Panel::top(id).frame(..).show_inside(ui, ..)` (`main.rs:695-697`),
  `egui::Panel::bottom(..)` (`dialogs.rs:58`), `response.context_menu` (`pane_ui.rs:241`),
  `ui.close()` (`pane_ui.rs:576`).

## 4. Data model

### `src/config.rs`
```rust
#[derive(Copy, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabPosition { #[default] Top, Bottom, Left, Right, Hidden }
```
`GlobalConfig` (`config.rs:51-57`) gains (all `#[serde(default)]`):
`tab_position: TabPosition`, `homogeneous: bool` (default true), `close_button_on_tab: bool`
(default true), `new_tab_after_current: bool` (default false). Extend the `Default` impl
(`config.rs:177-184`).

### `src/tab_bar.rs`
```rust
pub struct TabBarState { drag: Option<TabDrag>, rename: Option<TabRename> }   // persistent
struct TabDrag { from: usize }
struct TabRename { index: usize, buf: String, focus_pending: bool }
enum Axis { Horizontal, Vertical }                                            // derived from TabPosition

pub struct TabBarView<'a> {                                                   // per-frame, borrowed
    pub tabs: &'a [Tab], pub active: usize, pub position: TabPosition,
    pub homogeneous: bool, pub close_button: bool, pub scroll: bool,
}
pub enum TabBarEvent {
    Select(usize), Close(usize), NewTab, Reorder { from: usize, to: usize },
    Rename { index: usize, title: Option<String> },
    // reserved seam, not emitted in this block:
    // Detach { index: usize },
}
```
Tab order stays the `Vec` index in `TabManager.tabs` (`tabs.rs:183`); no order field is added.

## 5. New module and decomposition (NO god classes; every fn < 100 lines)

`src/tab_bar.rs` is the sole owner of tab-strip render + hit-test + interaction-to-event mapping
+ apply-to-`TabManager`. It keeps `App::ui()` thin and adds ZERO match arms to
`execute_pane_actions`.

```rust
pub fn show(ui: &mut egui::Ui, state: &mut TabBarState, view: &TabBarView) -> Vec<TabBarEvent>
pub fn apply(events: Vec<TabBarEvent>, tab_mgr: &mut TabManager, factory: &PaneFactory,
             g: &GlobalConfig, ctx: &egui::Context, dialogs: &mut DialogState)
pub fn panel_for(pos: TabPosition, id: &str) -> Option<egui::Panel>   // None for Hidden
fn layout_slots(titles: &[String], avail_main: f32, view: &TabBarView, axis: Axis) -> Vec<f32>
fn insertion_index(rects: &[egui::Rect], pointer: egui::Pos2, axis: Axis) -> Option<usize>
fn tab_title(tab: &Tab, index: usize) -> String        // extracted verbatim from main.rs:706-711
fn paint_tab(ui, rect, &TabVisual, axis) -> TabResponses  // ~40 lines, ported from main.rs:713-789
```
`show()` stays < 100 lines by delegating: `layout_slots` (sizes) -> per-tab `paint_tab` (bg,
label galley, optional close button; returns sub-responses) -> fold responses into events ->
`handle_drag` -> `inline_rename` -> `tab_context_menu`. `Axis` unifies horizontal/vertical:
`layout_slots`/`insertion_index`/`tab_rect` take `axis`; only `paint_tab` differs (x vs y), so
there is no duplicated strip logic across positions. `apply()` is a flat match over 5 event
variants, each a single existing `TabManager` call — NO new arms in `execute_pane_actions`.
`new_tab_after_current` is ONE shared pure helper `tab_insert_index` used by both `apply(NewTab)`
and the existing `Action::NewTab` path — no duplicated logic.

### `src/tabs.rs` (small, zero-behavior refactors)
- Refactor `new_tab` (`tabs.rs:404-420`) into `new_tab_at(index, factory)` with a thin `new_tab`
  wrapper (`new_tab_at(self.tabs.len(), factory)` — append, signature unchanged).
- Add `reorder_tab(from, to)` and `set_tab_title(index, Option<String>)`.
- Pure helpers `remap_active_after_reorder(active, from, to) -> usize` and
  `tab_insert_index(active, len, after_current) -> usize`, unit-tested alongside the existing
  `adjust_active_tab` tests (`tabs.rs:922-932`).

### `src/main.rs`
- `mod tab_bar;` (near `main.rs:1-22`); add App field `tab_bar: TabBarState` + init in the struct
  literal (near `main.rs:308`).
- Replace `App::ui()` lines `692-806` with: derive `TabBarView` from config + tab_mgr ->
  `tab_bar::panel_for(pos, "tab_bar")` -> `.show_inside(|ui| tab_bar::show(..))` ->
  `tab_bar::apply(events, ..)`.
- The existing `Action::NewTab` arm (`main.rs:358`) computes its insert index via
  `tab_insert_index` then calls `new_tab_at` (honors `new_tab_after_current`).
- `App::logic` focus-regain guard (`main.rs:632`) also checks `tab_bar.is_renaming()` (so an
  active inline-rename TextEdit keeps egui focus), ORed with `dialogs.wants_text_input()`
  (`dialogs.rs:47`).

### `src/prefs_ui.rs`
Extend `draw_prefs_global` (`prefs_ui.rs:160-208`) with a "Tabs" group: a position `ComboBox` +
3 checkboxes (+ optional scroll checkbox per Q3). `draw_prefs_global` stays under 100 lines (it
is ~48 today); if it approaches the limit, factor the Tabs group into a `prefs_tabs_group(ui,
cfg)` helper.

## 6. Migration

Back-compat safe. `GlobalConfig` already has `#[serde(default)]` (`config.rs:52`) and a `Default`
impl; the 4 new fields get values from the extended `Default` impl, so old configs missing the
keys deserialize unchanged. `TabPosition` derives `Default = Top` with `snake_case` serde,
matching the gap test values `top/bottom/left/right/hidden` (`config.rs:665`). No
`deny_unknown_fields` on `GlobalConfig`, so older binaries reading newer configs ignore the new
keys. New keys are written on save.

## 7. Test seams

- `layout_slots(titles, avail, view, axis)`: pure — homogeneous equal-width, content-width
  sizing, vertical axis, overflow.
- `insertion_index(rects, pointer, axis)`: pure — drop target by position; `None` when outside
  (the Detach seam).
- `remap_active_after_reorder` and `tab_insert_index`: pure index math, table-tested in the
  `tabs.rs` test module alongside `adjust_active_tab` (`tabs.rs:922`).
- `tab_title(tab, index)`: pure title derivation extracted from `main.rs:706-711`.
- `config_global_tab_position`: implement the currently-`#[ignore]`d test (`config.rs:662`) to
  deserialize a TOML snippet into `TabPosition`.
- `apply()` is event-driven, so `TabBarEvent` sequences can be asserted against `TabManager`
  state without egui.

## 8. Ordered implementation steps (compile/check after each)

1. `config.rs`: add `TabPosition` + 4 `GlobalConfig` fields + `Default`; implement
   `config_global_tab_position`. `cargo test`. Defaults preserve current behavior.
2. `tabs.rs`: extract `new_tab_at` (+ thin `new_tab` wrapper), add `reorder_tab`/`set_tab_title`
   + pure helpers + unit tests. `cargo test`. `new_tab` append unchanged = zero behavior change.
3. `tab_bar.rs`: create the module skeleton (types + pure `layout_slots`/`insertion_index`/
   `tab_title`/`tab_insert_index`) with unit tests; not wired yet. `cargo check`.
4. **Wire TOP position only**: port `main.rs:695-797` verbatim into `show()`/`paint_tab`
   returning events; implement `apply()` and `panel_for(Top/Bottom)`; add `App.tab_bar` field +
   init; replace `main.rs:692-806`; add the `logic()` `is_renaming` guard. `cargo check` +
   manual verify the top bar is identical. **ZERO-BEHAVIOR-CHANGE checkpoint.**
5. Enable gated bits: middle-click close, `close_button_on_tab` gating, `homogeneous=false`
   content-width, and `+` honoring `new_tab_after_current` (also update the `Action::NewTab` arm).
6. Right-click tab context menu via `response.context_menu` (New Tab, Close, Rename/Set title),
   reusing the `build_context_menu` style.
7. Double-click inline rename: `TabRename` buffer + `TextEdit` in the tab rect; Enter / lost-focus
   commit -> `Rename` event; Esc cancels.
8. Drag-reorder: `drag_started` sets `TabDrag` + emits `Select`; on `!pointer.any_down()` compute
   `insertion_index` -> `Reorder`; draw the insertion marker. Outside-strip drop = no-op now
   (Detach seam for multi-window).
9. Scroll + vertical: wrap the strip in `ScrollArea` (horizontal or vertical by axis);
   `panel_for(Left/Right)` with `Axis::Vertical`; reuse `layout_slots`/`insertion_index(axis)`.
   Verify the egui 0.34.1 `Panel::left`/`right` orientation API at this step.
10. `prefs_ui.rs`: add the Tabs settings to `draw_prefs_global` (position combo + 3 checkboxes).

## 9. Cross-cutting notes

- `TabBarState` is a new App field now; when **multi-window** lands it moves onto `AppWindow`
  (per-window UI state) along with the other per-window App fields. No logic change.
- The drag drop-outside-strip branch (`insertion_index` returns `None`) is the single seam the
  multi-window block consumes as `TabBarEvent::Detach { index }` -> `WindowManager::detach_tab`.
  The tab-bar calls into the router, never vice versa.
- Adds NO `Action` variants, so the keybindings exhaustiveness match and reachability test
  (`keybindings.rs:959-1095`) are untouched by this block.

## 10. Open questions (genuine forks)

- **Q3** Add a 5th `GlobalConfig` knob `scroll_tabbar` (Terminator-faithful, default false) or
  auto-scroll on overflow with no new knob? Terminator implements the scrollable tab bar via a
  distinct `scroll_tabbar` toggle independent of `homogeneous`; the user's own design-defaults
  say "don't add config for flexibility." Terminator-fidelity vs the user's stated minimalism
  genuinely conflict, and Terminator cannot settle the user's preference. Recommendation: add
  `scroll_tabbar: bool` default false — faithful to Terminator and default-false equals current
  behavior.
- **Q4** Keep the existing modal "Set title" dialog (`Action::SetTitle` + `dialogs.rs:155`)
  alongside the new Terminator-style inline double-click rename, or replace it? Terminator has
  ONLY inline rename and no modal title dialog, so it gives no guidance on whether to retain
  rustinator's pre-existing modal path. Both write the same `tab.custom_title`. Recommendation:
  keep both for the first pass — lowest risk, no removed affordance; revisit if redundant.
