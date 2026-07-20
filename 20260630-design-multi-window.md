# Design: In-process multi-window (INFRA-Multiwindow)

WF-E. The LONG POLE; recommended LAST or deferred to v2. Status: design, no code. Line numbers
verified against source on 2026-06-30.

## 1. What changes, in one paragraph

Today one `App` (`main.rs:146`) holds one `TabManager` plus that window's GL/egui/font/render
state, driven by one `WinitApp` (`window.rs:193`) that already special-cases two secondary
windows (prefs, hotkey) by `WindowId`. Split `App` into per-window state (`AppWindow`) and
process-global state (`AppShared`), and replace `WinitApp` with a thin `WindowManager` router
owning windows as a `HashMap<WindowId, AppWindow>` plus the existing single prefs and hotkey
windows. No fatter App: behavior moves OFF the App hub onto `AppWindow` (per-window) and
`AppShared` (global); the router only dispatches by id and runs window lifecycle.

GL strategy is de-risked by existing code: rustinator does NOT use multi-context GL sharing. It
uses ONE shared `PossiblyCurrentContext`, ONE shared `glow::Context` `Arc`, and ONE shared
`GlConfig`, with a per-window `Surface`. The prefs and hotkey windows already build a `GlWindow`
from the shared handles (`window.rs:43`, `window.rs:122`; `GlWindow::new` takes
`main_context: &PossiblyCurrentContext` at `gl_window.rs:28`), so single-context multi-surface is
proven on every shipped platform. Going from 2 secondary windows to N first-class terminal
windows is mechanical, not GL-theoretical.

## 2. Terminator behavior + existing rustinator binds being mirrored

`NewWindow` = `Ctrl+Shift+I` (Linux) / `Cmd+Shift+I` (mac) (`keybindings.rs:293`): a new
top-level window with one fresh terminal. `CloseWindow` = `Ctrl/Cmd+Shift+Q`
(`keybindings.rs:291`): closes only that window; the app quits only when the last window closes.
Detach: drag a tab label off the tab bar into empty space spawns a new window holding that tab
(gap-analysis.md:38; Terminator has no detach keybinding, drag-only). Multi-window layouts
(gap-analysis.md:77): a saved layout records each window's geometry + its tabs + each tab's split
tree; restoring spawns those windows. Closing the last tab of any window closes that window (the
rule today: `close_tab` sends `ViewportCommand::Close` when tabs empty, `tabs.rs:353-354`).
Confirm-on-close stays per-window (`should_confirm_close` keys off that window's
`total_alive_panes`, `main.rs:317-319`). The current `Action::NewWindow` that spawns a whole new
OS process (`main.rs:385-389`) is REPLACED by an in-process `AppWindow`.

## 3. Ground-truth facts this design rests on

- `GlState` (`gl_setup.rs:17-24`) owns `gl_context`/`gl_surface`/`gl_display`/`gl_config`/
  `window`/`gl` — the primary window's surface lives here, and the primary paints INLINE via
  `WinitApp::paint` (`window.rs:748-901`) using `self.painter`, NOT through a `GlWindow`.
- `GlWindow` (`gl_window.rs:14-20`) = window + surface + painter + egui_ctx + egui_winit; built
  from shared `gl_display`/`gl_config`/`gl`/`main_context` (`gl_window.rs:23-32`). `begin_frame`
  (`110`), `end_frame` (`119`), `destroy` (`93`), `resize` (`100`).
- `WinitApp` (`window.rs:193-227`) routes `window_event` by id: prefs (`387`), hotkey (`425`),
  main (`539`). `about_to_wait` governor (`631-705`); `effective_frame_interval` (`736-746`);
  `paint` (`748-901`); `run()` (`904-911`).
- `App` fields (`main.rs:146-171`); `FontState` (`main.rs:46-134`); `pane_factory` (`211-219`);
  `logic` (`587-654`); `ui` (`656-851`); `execute_pane_actions` (`343-435`); `apply_prefs`
  (`442-510`); `should_confirm_close` (`317-319`).
- `TabManager.next_pane_id: PaneId` (`tabs.rs:185`), init `id+1` (`201`), bumped at
  `256,275,408,737`. `Tab` (`tabs.rs:15-22`). `adjust_active_tab` (`tabs.rs:127`, already tested).
- `Renderer::paint` already takes the viewport per call (`renderer.rs` — `info.viewport_in_pixels`
  consumed in the GL callback at `pane_ui.rs:818-820`), so nothing assumes one window.
- `LayoutTemplate` per tab (`layout.rs:330`); `UserEvent` (`window.rs:19-23`).

## 4. Data model

- **`AppShared`** (NEW `src/app_shared.rs`) — process-global state moved off `App`:
  `user_config: Config`, `bindings: BindingTable`, `term_config: TermConfig`,
  `pane_defaults: PaneDefaults`, the font anchor (`base_size` + family, mutated on prefs apply at
  `main.rs:483`), `prefs: PrefsState`, `preview_defaults: Option<PaneDefaults>`,
  `hotkey_changed: bool`, `cursor_blink_epoch: Instant`, `event_loop_proxy:
  EventLoopProxy<UserEvent>`, and `pane_id_alloc: Arc<AtomicU64>` (the global `PaneId` source).
- **`AppWindow`** (NEW `src/app_window.rs`) — per-window state: `gl_window: GlWindow`,
  `tab_mgr: TabManager`, `renderer: Arc<Mutex<Renderer>>` (own glyph atlas + cell metrics),
  `font: FontView` (per-window `cell_w`/`cell_h`/`size_override`/`scale_factor` +
  `ctx: Arc<Mutex<FontContext>>`), `dialogs: DialogState`, `pane_view: PaneViewState`,
  `tab_bar: TabBarState` (from the tab-bar block), `current_title`, `fullscreen_pending`,
  `pending_zoom_steps`, `focus_regained`, `pending_raw_keys`, `current_modifiers`,
  `zoom_pixel_accumulator` (the last two moved off `WinitApp` `203-204`), and the per-window
  governor fields `repaint_pending`/`last_frame`/`last_paint_cost`/`egui_repaint_at`/`focused`.
- **`SharedGl`** (NEW, from splitting `GlState`): `gl_context: PossiblyCurrentContext`,
  `gl_display: Display`, `gl_config: GlConfig`, `gl: Arc<glow::Context>`. Outlives every window
  (it owns the one shared context).
- **`WindowManager`** (NEW `src/window_manager.rs`): `shared: AppShared`, `gl: SharedGl`,
  `windows: HashMap<WindowId, AppWindow>`, `prefs: Option<PrefsWindowState>`,
  `hotkey_window: Option<HotkeyWindowState>`, `hotkey_owner: Option<WindowId>`.
- `Renderer` stays PER-WINDOW: its atlas is keyed by `(char, FontStyle)` not size, and
  `cell_w`/`cell_h` are baked at `font*scale`, so per-window instances coexist in the one shared
  GL context (GL objects are context-scoped) and give correct per-monitor DPI + independent zoom.
- Stage F only: `WindowLayout { geometry: Option<WindowGeom>, tabs: Vec<LayoutTemplate> }` reusing
  the existing per-tab `LayoutTemplate` (`layout.rs:330`).

## 5. Decomposition (NO god classes; every fn < 100 lines)

The router is a *router + lifecycle owner*, not a behavior hub. Methods return a `WindowControl`
enum so the router (not `AppWindow`) owns lifecycle decisions:
```rust
enum WindowControl { None, CloseSelf, SpawnWindow, DetachTab(usize) }
```
- `AppWindow::paint` (replaces the 153-line inline `WinitApp::paint`, `window.rs:748-901`) splits
  into: `gl_window.begin_frame` (existing); a private `retain_terminal_tab_key(&mut raw_input)`
  for the Tab-strip workaround (`window.rs:770-772`); the `run_ui` closure calling `logic`+`ui`;
  `apply_viewport_commands(&full_output) -> WindowControl` handling Close/Title/Minimized (mirrors
  the hotkey window at `window.rs:168-174`); `gl_window.end_frame` (existing). Per-window
  `fullscreen_pending` (`window.rs:797-805`) stays here; global `hotkey_changed` (`807-841`) and
  prefs lifecycle (`886-900`) are HOISTED to the router so `paint` stays < 100.
- `WindowManager::window_event` stays thin: a pure `classify(id) -> WindowKind { Prefs, Hotkey,
  Terminal(id), Unknown }`, then delegate. Prefs/hotkey arms are the existing code moved verbatim
  (`window.rs:387-537`). The Terminal arm is one call into `AppWindow::handle_window_event ->
  WindowControl`, then the router acts.
- `App::logic` (`main.rs:587-654`, ~67 lines) -> `AppWindow::logic` almost verbatim
  (`self.user_config` -> `shared.user_config`, `self.egui_ctx` -> `self.gl_window.egui_ctx`).
- `App::execute_pane_actions` (`main.rs:343-435`) -> `AppWindow`; only two arms change: `NewWindow`
  -> return `WindowControl::SpawnWindow` (was the process spawn at `385`); `CloseWindow` ->
  `WindowControl::CloseSelf` path. The match stays one function (data, not nested logic); the
  `can_process_more_actions` guard (`main.rs:191`) is preserved.
- `apply_prefs` (`main.rs:442-510`) splits into `AppShared::apply_global` (config, bindings,
  term_config, pane_defaults, font anchor, hotkey_changed; ~60 lines, returns a `ConfigDelta` of
  what each window must re-apply) and `AppWindow::apply_config` (per-window font reload `474-498` +
  `push_pane_defaults` `556` + `apply_term_config` `507`), run by the router looping
  `windows.values_mut()`.
- `FontState` methods (`main.rs:56-133`) -> `FontView` methods taking `base_size`/`family` params
  (they already take family); each is already < 35 lines.
- `GlState::new` (`gl_setup.rs:27-91`) splits: `create_shared(event_loop)` building display/config
  + the existing `create_context_and_surface` (`gl_setup.rs:118`) for the first window, returning
  `SharedGl`, plus a wrap-into-`GlWindow` step. `pick_gl_config`/`create_display` unchanged.
- `WindowManager::about_to_wait` generalizes the governor (`window.rs:631-705`) via a pure
  `governor_next_wake(now, last_frame, interval, repaint_pending, egui_at) -> (issue: bool, next:
  Option<Instant>)` computed per window, then `min` across all windows + hotkey.
- Detach: `WindowManager::detach_tab` = `tab_mgr.take_tab(idx)` (reuses `adjust_active_tab`,
  `tabs.rs:127`, already tested) then `AppWindow::adopt_tab` then insert; if the source
  `tab_mgr.tabs` is now empty, `close_window(src)`. < 40 lines.

## 6. Public API

```rust
SharedGl::new(event_loop) -> (SharedGl, GlWindow)            // primary window + shared context
SharedGl::context(&self) -> &PossiblyCurrentContext
AppShared::new(proxy) -> AppShared
AppShared::next_pane_id(&self) -> PaneId                     // alloc.fetch_add(1)
AppShared::pane_factory(&self, font: &FontView) -> PaneFactory
AppShared::apply_global(&mut self, new: Config) -> ConfigDelta
AppWindow::new_primary(gl_window, shared: &mut AppShared, scale) -> Result<AppWindow, String>
AppWindow::spawn_fresh(event_loop, gl: &SharedGl, shared: &mut AppShared) -> Result<AppWindow, String>
AppWindow::adopt_tab(event_loop, gl, shared, tab: Tab) -> Result<AppWindow, String>
AppWindow::handle_window_event(&mut self, gl, shared, event) -> WindowControl
AppWindow::paint(&mut self, gl, shared) -> WindowControl
AppWindow::logic / ui / execute_pane_actions / schedule / apply_config
TabManager::take_tab(&mut self, idx) -> Option<Tab>
TabManager::from_tab(tab: Tab, alloc: Arc<AtomicU64>) -> TabManager
WindowManager::spawn_window / detach_tab / close_window      // close_window exits iff windows empty
impl ApplicationHandler<UserEvent> for WindowManager { resumed, user_event, window_event,
    about_to_wait, exiting }
```

## 7. PaneId uniqueness (Stage 0 prep)

Replace `TabManager.next_pane_id: PaneId` (`tabs.rs:185`, bumped at `256,275,408,737`) with
`alloc: Arc<AtomicU64>` via `fetch_add`, threaded from `AppShared` through `PaneFactory`. Two
`TabManager`s sharing one alloc never collide — required so a detached tab keeps unique ids in its
new window. `take_tab`/`from_tab` move a `Tab` (panes + layout + focus) intact between managers.

## 8. Sequencing against the other three blocks (critical)

Group, Profile-on-spawn, and Tab-bar all land against the CURRENT single-window `App`/`main.rs`
(their docs reference `App::execute_pane_actions`, `App::ui`, `Pane`, `TabManager`). Multi-window
comes AFTER and *relocates whatever exists at that point*:
- Stages A-C are zero-behavior reshapes (split `GlState`; extract `AppShared`/`AppWindow`/
  `FontView`; turn `WinitApp` into the thin router). They MOVE the already-richer App methods
  (now carrying group dispatch, profile dispatch, tab_bar field) without changing behavior.
- `TabBarState` (a single-window App field from the tab-bar block) moves onto `AppWindow`.
- `broadcast_scope` (a `TabManager` field from the group block) is naturally per-window (each
  window has its own `TabManager`). The group block's "All" scope therefore means "all panes in
  the active tab of THIS window" — consistent with single-window routing, and cross-window groups
  are explicitly out of scope (a group name in window A never reaches window B because input only
  routes to the focused tab of each window). Note for fidelity: group identity is a bare name
  string, so it does not assume a single window.
- `Pane.profile`/`spawn_command` (profile block) need no detach change — panes carry their own
  runtime state; `adopt_tab` only marks panes dirty + `cached=None` (existing pattern,
  `main.rs:75-76`) so they re-rasterize against the destination window's atlas.

## 9. Migration

Stages 0-E are RUNTIME-only: `AppWindow`/`AppShared`/`SharedGl`/`WindowManager` and the per-window
`Renderer` have no serde surface, so there is no config migration. Stage F (multi-window layouts)
is the only persistence change and is back-compatible: keep the existing `SavedLayout { name,
template }` (`config.rs:45-49`) untouched so old single-tab layouts still load; add a NEW optional
`windows: Option<Vec<WindowLayout>>` with `#[serde(default)]`. `None` => behaves exactly as today
(single-tab restore into the active window); `Some` => spawns windows. No change to the `Config`
top-level shape or to `LayoutTemplate`.

## 10. Test seams

- PaneId allocator: `Arc<AtomicU64>::fetch_add` is pure-monotonic; unit-test that two
  `TabManager`s sharing one alloc never collide and that `take_tab`/`from_tab` move panes/layout/
  focus intact (reuses the tested `adjust_active_tab`, `tabs.rs:922`).
- `classify(id, prefs_id, hotkey_id, &window_ids) -> WindowKind`: pure, fake `WindowId`s.
- `governor_next_wake(...)`: pure extraction of the existing `about_to_wait` math
  (`window.rs:657-696`); unit-test due/not-due and the min-merge.
- `should_quit(remaining_windows) -> bool`: trivial pure predicate.
- `WindowControl` is a plain enum returned by `AppWindow` methods, so detach/new/close decisions
  are assertable without a live event loop or GL context.
- `apply_global -> ConfigDelta`: pure transform (which windows need a font reload vs a defaults
  push), testable without GL.

## 11. Ordered implementation steps (verify parity after each zero-behavior stage)

- **Stage 0** (zero behavior): `TabManager.next_pane_id` -> `Arc<AtomicU64>` alloc via
  `PaneFactory` from `AppShared`; add `take_tab`/`from_tab` with unit tests. Lands alone.
- **Stage A** (zero behavior): split `GlState` into `SharedGl` + make the PRIMARY window a
  `GlWindow`; route the primary paint through `GlWindow::begin_frame`/`end_frame` +
  `apply_viewport_commands`. Riskiest extraction (primary inlines paint today). Verify launch /
  resize / title / fullscreen / close / prefs / hotkey unchanged.
- **Stage B** (zero behavior): extract `AppShared` + `AppWindow` + `FontView` from `App`;
  `windows` holds exactly one `AppWindow`. Move logic/ui/execute_pane_actions/apply_prefs per §5.
  Verify single-window parity + `cargo test`.
- **Stage C** (zero behavior): `WinitApp` -> thin `WindowManager` with `classify(id)` dispatch;
  prefs/hotkey arms verbatim; generalize `about_to_wait` via `governor_next_wake`; bind
  `hotkey_owner` to the primary. Verify parity.
- **Stage D** (new): `Action::NewWindow` -> `WindowControl::SpawnWindow` ->
  `WindowManager::spawn_window` (in-process `AppWindow`, one fresh terminal); `Action::CloseWindow`
  + `ViewportCommand::Close` -> `close_window`, which `exit()`s only when `windows` is empty.
  `UserEvent::Repaint` marks all terminal windows `repaint_pending` (per-window-id tagging is a
  deferred optimization). Verify open/close multiple windows, quit on last, PTY output repaints
  the right window.
- **Stage E** (new): `WindowManager::detach_tab` wired to the tab-bar drag-off gesture (the
  `insertion_index == None` seam from the tab-bar block). Panes keep running; mark dirty +
  `cached=None`; position the new window at the drop point (reuse `platform.rs` geometry helpers).
  Verify the source closes if it was the last tab; both windows render.
- **Stage F** (new, deferrable): multi-window layouts. Add `WindowLayout` persistence (§9);
  save = snapshot every window's geometry + tabs + templates; restore = spawn N windows. Wire the
  Layout Launcher (Alt+L, gap-analysis.md:79) later. Highest effort, lowest risk to the core; safe
  to cut from v1.

Stages A-C are zero-behavior reshapes worth landing even if D-F defer to v2.

## 12. Open questions (genuine forks) — only relevant if this block is built

- **Q5** In multi-window mode, what does the global drop-down hotkey window (Quake-style,
  `HotkeyWindowState`, `window.rs:93`) represent? It currently MIRRORS the single `App`'s
  `tab_mgr` by calling `app.logic`/`app.ui` on the same state (`window.rs:159-162`). This mirror
  model has no Terminator analog (Terminator drop-down terminals are independent), so
  mirror-of-which is undefined once there are N windows. Recommendation: mirror the
  most-recently-focused terminal window (`hotkey_owner` updated on focus) — least surprising,
  preserves the cheap existing mirror, defers the independent-Quake rework.
- **Q6** When in-process `NewWindow` (`Ctrl+Shift+I`) opens its single fresh terminal, does it
  inherit the focused window's cwd or start at the profile default / home? rustinator already
  inherits cwd for `new_tab` (`tabs.rs:406-410`) and `split` (`tabs.rs:254`); Terminator's New
  Window opens fresh at the profile default. Internal-consistency vs Terminator-fidelity genuinely
  collide and the directive does not say which principle wins. Recommendation: inherit the focused
  window's cwd to match rustinator's own new-tab/split behavior; revisit only if a user reports it
  should be fresh.
