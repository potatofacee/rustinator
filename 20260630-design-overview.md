# Design overview: WF-A reconcile (2026-06-30)

Reconciles the four WF-C/WF-E infra designs into ONE coherent plan. This is a FACELIFT OF
TERMINATOR: behavior, keybindings, and semantics mirror Terminator; decisions are mostly already
made ("do what Terminator does"). Only genuine user-preference forks are flagged (§5).

Per-piece docs:
- `20260630-design-group-model.md`
- `20260630-design-profile-on-spawn.md`
- `20260630-design-tab-bar.md`
- `20260630-design-multi-window.md`

All line numbers in every doc were verified against `src/*.rs` on 2026-06-30. The
`20260630-feature-worksession.md` file map was correct on the big pointers; the corrections are:
the tab-bar render is inline in `App::ui()` at `main.rs:692-806` (not a separate file); broadcast
is `Tab.broadcast: bool` at `tabs.rs:20` (per-tab today, becoming global); `LayoutTemplate` has no
per-pane metadata (`layout.rs:330`); `Pane` has no group/profile fields (`pane.rs:762-802`).

## 1. Cross-cutting resolutions (the reconcile deliverable)

### (a) Where group identity lives, and Tab vs Pane
Group identity is a **per-`Pane` name string** `group: Option<String>` — the name IS the identity
(mirrors Terminator `terminal.group`). No numeric id, no registry; the live set of names is
derived by scanning panes. Broadcast scope is a **single global** `BroadcastScope { Off, Group,
All }` field on `TabManager` (replacing the per-`Tab` `broadcast: bool`), mirroring Terminator's
singleton `terminator.groupsend`. So: identity on the Pane, scope on the TabManager — the worksession
"per-pane vs per-tab" open decision is resolved **per-pane** (Terminator-faithful). All logic lives
in a new leaf module `src/groups.rs`; `TabManager` gains exactly one field.

### (b) How broadcast-scope composes with `select_input_targets` without breaking its tests
`Tab::select_input_targets` (`tabs.rs:45-53`) gains a `scope: BroadcastScope` param and filters
via a single pure predicate `groups::pane_receives_input(scope, read_only, is_focused, this_group,
focused_group)` = `!read_only && (is_focused || is_receiver(scope, this_group, focused_group))`.
The same `is_receiver` core also drives the titlebar indicator, so routing and the visual can never
disagree. The existing `pane_is_input_target` helper and its two read-only tests
(`tabs.rs:56-61, 846-883`) MOVE into `groups.rs`, re-expressed as `pane_receives_input(Off/All, ..)`:
**Off == old `broadcast=false`** (focused-only), **All == old `broadcast=true`** (every non-read-only
pane). The read-only AND-gate is preserved exactly; terminal responses still bypass via `send_bytes`.
The single input chokepoint stays: keys (`main.rs:642`), paste (`tabs.rs:669`), and insert-number
all route through `select_input_targets(scope)`; nothing bypasses it.

### (c) Profile resolution order on spawn/respawn, and feeding the layout serializer
Profile identity is the **name string** (like `Config.active_profile`). Resolution order, owned by
the pure `src/profile.rs`:
1. Pick the name: first pane / `new_tab` -> active profile; `split`/`split_here`/`SplitAuto`/
   `OpenTerminalHere` -> the focused parent pane's profile (inherit-on-split); layout restore ->
   per-leaf sidecar profile if present; profile switch -> the chosen name.
2. `profile::resolve(cfg, name)` -> `&Profile`, dangling name -> `Config::active()`/`[0]`
   (reuses the existing fallback at `config.rs:443-448`).
3. -> `ResolvedProfile { defaults, scrolling_history, semantic_escape_chars, command }`.
4. `Pane::spawn` builds the child command from `command` (replacing the hardcoded login-shell at
   `pane.rs:838-843`) and stamps `pane.profile` + `pane.spawn_command`.
5. `respawn` reuses the stored `pane.spawn_command` (so the reaper loop needs no `Config`).

Per-pane profile feeds the layout serializer via a **sidecar** `SavedLayout.terminals:
Vec<TerminalMeta>` (leaf order, `#[serde(default)]`), NOT by changing the `LayoutTemplate::Terminal`
unit variant — changing that variant's shape would break serde back-compat for existing saved
layouts. Full layout persistence is the dependent Tier-2 feature; the infra only exposes
`Pane.profile`. Profile switching is a live re-style (no respawn), Terminator-faithful.

### (d) The real tab-bar render location and the new module boundary
The tab strip is inline in `App::ui()` at `main.rs:692-806` (a `Panel::top("tab_bar")` block plus
3 locals + dispatch). It moves wholesale into `src/tab_bar.rs`: persistent `TabBarState` (ONE App
field replacing the 3 locals), per-frame `TabBarView`, `show() -> Vec<TabBarEvent>`, and `apply()`
that drives existing `TabManager` methods. The boundary: `tab_bar` reads the tabs slice + per-tab
title and writes ONLY via `switch_tab_direct`/`close_tab`/`new_tab_at`/`reorder_tab`/`set_tab_title`;
it never touches `Pane` internals beyond `title()`. It adds ZERO `Action` variants. The Stage-1
extraction reproduces the top bar verbatim (zero-behavior checkpoint) before any feature toggles.

### (e) The multi-window `AppWindow` shape, and how detach/layout depend on it
`App` splits into `AppWindow` (per-window: gl_window, tab_mgr, renderer, font, dialogs, pane_view,
**tab_bar**, governor fields) and `AppShared` (process-global: config, bindings, term_config,
pane_defaults, font anchor, prefs, the `Arc<AtomicU64>` PaneId allocator, the event-loop proxy).
`WinitApp` -> a thin `WindowManager` router (`HashMap<WindowId, AppWindow>` + the existing
prefs/hotkey windows) that dispatches by id and owns lifecycle via a `WindowControl` enum returned
from `AppWindow` methods. ONE shared `PossiblyCurrentContext`/`GlConfig`/`glow::Context` + a
per-window `Surface` (already proven by the prefs/hotkey `GlWindow`s). Detach consumes the tab-bar's
drag-off seam (`insertion_index == None`) -> `WindowManager::detach_tab` (`take_tab`/`from_tab` +
`adopt_tab`). Multi-window layouts add an optional `SavedLayout.windows` field. Crucially, the
per-window `TabManager` means `broadcast_scope` is per-window and "All" means "all panes in the
active tab of THIS window" — consistent with today's routing; cross-window groups are out of scope.

### Shared-edit contracts (where two blocks touch the same code)
- **`Pane` struct + `spawn` initializer** (`pane.rs:762-802`, `873-896`): ONE edit adds Group's
  `group: Option<String>` AND Profile's `profile: ProfileName` + `spawn_command: SpawnCommand`.
- **`split`/`split_here` inheritance** (`tabs.rs:252-284`): ONE place sets `new.group =
  parent.group.clone()` (Group; pure copy) and `new.profile = factory.profile.clone()` +
  `new.spawn_command = factory.command.clone()` (Profile; from the parent-profile factory the App
  builds). Group needs no `Config`; Profile's command comes from the factory.
- **`execute_pane_actions`** (`main.rs:343-435`, already ~92 lines, exhaustive match, no `_` arm):
  the split arms route through `App::split_inheriting` (same arm count); the 10 group/insert
  variants collapse into ONE combined arm -> `dispatch_group_action`; the 2 profile variants into
  ONE combined arm -> `dispatch_profile_action`. Net +2 arms, stays < 100 lines.

### The keybindings test-harness contract (under-specified by the source designs — made explicit)
Every NEW `Action` variant (10 from Group, 2 from Profile; Tab-bar and Multi-window add none) MUST
update ALL of, or the build breaks or a guard test fails:
1. `enum Action` (`keybindings.rs:11-56`).
2. `Action::from_str` (`keybindings.rs:59-116`).
3. `_assert_exhaustive` match (`keybindings.rs:959-1005`) — **compile error otherwise**.
4. `all_actions()` vec (`keybindings.rs:1007-1052`).
5. `execute_pane_actions` (`main.rs:343-435`) — **exhaustive, compile error otherwise**.
6. `every_action_reachable_via_binding_or_menu` (`keybindings.rs:1075-1095`): each variant must be
   bound (in `linux_defaults`), in `context_menu_actions()` (`1057-1073`), or in the intentional
   allowlist. Group: the 7 group/insert actions are bound; the 3 unbound `broadcast_*` go in
   `context_menu_actions()`; drop `ToggleBroadcast` from `context_menu_actions()` (it leaves the
   menu but stays bound to `Ctrl+Shift+B`). Profile: `NextProfile`/`PreviousProfile` are unbound
   (Terminator parity) and switched via the Profiles submenu, so add them to the allowlist beside
   `SwitchToTab(_)`.

### The "every function < 100 lines / no god classes" constraint, concretely
- `groups.rs`, `profile.rs`, `tab_bar.rs`, `title_bar.rs`, `app_shared.rs`, `app_window.rs`,
  `window_manager.rs` are the new focused owners; new behavior lands there, NOT as more
  fields/arms on `App`/`TabManager`/`draw_panes`.
- `draw_panes` (`pane_ui.rs:53-259`, currently ~206 lines) is DECOMPOSED by the group block
  (extract `draw_leaf` + `title_bar::paint`) so the resulting `draw_panes` is < 100.
- `build_context_menu` (`pane_ui.rs:580-640`) gains helpers `broadcast_and_group_menu` (group) and
  `profiles_menu` (profile) so it stays < 100.
- `execute_pane_actions`, `apply_prefs`, `WinitApp::paint` are split as above / by the multi-window
  block. `dispatch_group_action`/`dispatch_profile_action`/`split_inheriting` are all small new fns.

## 2. Build order

```
WF-A (this design)                          done; gates everything
WF-B (Tier-1 leaves)                        independent; anytime (scaled zoom, rebalance, etc.)
WF-C infra, in this order:
  1. Group model        (groups.rs, title_bar.rs; +1 TabManager field; per-pane group)
  2. Profile-on-spawn   (profile.rs; Pane profile/command; shares the Pane + split edits)
  3. Tab-bar subsystem  (tab_bar.rs; config; zero Action churn)
WF-D (Tier-2 features)                       fan out per infra (grouping UI, profiles trio,
                                             layout persistence, the 8 tab features)
WF-E (Multi-window)                          LAST or defer to v2; relocates the richer App
```

Rationale for the WF-C internal order:
- **Group before Profile** is interchangeable, but they SHARE the `Pane` struct edit and the
  `split` inheritance point. Land Group's `group` field first (smallest, pure), then Profile's
  `profile`/`spawn_command` in the same struct/initializer/split touch. If run in parallel
  worktrees, the `Pane` struct + `split`/`split_here` are the only true collision points — assign
  ONE owner to `pane.rs` struct/spawn and to `tabs.rs:252-284` for both features.
- **Tab-bar is disjoint** from Group/Profile (touches `main.rs:692-806`, `config.rs`, new
  `tab_bar.rs`) and adds no Action variants, so it can run parallel to the other two.
- **Multi-window LAST**: its zero-behavior Stages A-C MOVE whatever exists, so it must see the final
  single-window shape (group dispatch, profile dispatch, tab_bar field) to relocate it cleanly.
  Stages D-F (the actual features) are deferrable to v2 with everything else still shipping.

## 3. New modules (all leaf/focused; none is a hub)

| Module | Owns | Replaces inline code at |
|--------|------|-------------------------|
| `src/groups.rs` | `BroadcastScope`, `Indicator`, `is_receiver`, `pane_receives_input`, `indicator`, `next_tab_group_name`, `enumerate_text`, group/ungroup/insert orchestration | `tabs.rs:56-61` predicate; broadcast logic |
| `src/title_bar.rs` | titlebar rect split, `TitleBarModel`, `paint`, indicator->glyph | `pane_ui.rs:103-177` |
| `src/profile.rs` | `ProfileName`, `SpawnCommand`, `ResolvedProfile`, `resolve`/`resolved`/`spawn_command`/`next_name`/`prev_name`, relocated `defaults_from_profile` | `main.rs:174-185`; `pane.rs:838-843` shell block |
| `src/tab_bar.rs` | `TabBarState`/`TabBarView`/`TabBarEvent`/`Axis`, `show`/`apply`/`panel_for`, pure layout/index helpers | `main.rs:692-806` |
| `src/app_shared.rs` | process-global `AppShared` + the `PaneId` allocator | (multi-window) `App` global bits |
| `src/app_window.rs` | per-window `AppWindow` + `FontView` + relocated per-window methods | (multi-window) `App` + inline `WinitApp::paint` |
| `src/window_manager.rs` | the `ApplicationHandler` router + window lifecycle | (multi-window) `WinitApp` |

## 4. Migration summary (all back-compat safe)

- Group: runtime-only; removing `Tab.broadcast` and adding `Pane.group` touch no serde.
- Profile: 3 new `Profile` fields + `SavedLayout.terminals` sidecar, all `#[serde(default)]`; old
  configs/layouts load unchanged; default-profile spawn reproduces today's command.
- Tab-bar: 4 new `GlobalConfig` fields + `TabPosition`, all `#[serde(default)]`; old configs load
  unchanged; defaults equal current behavior.
- Multi-window: Stages 0-E runtime-only; Stage F adds optional `SavedLayout.windows`
  (`#[serde(default)]`), `None` == today.

No `deny_unknown_fields` anywhere, so downgrades stay safe.

## 5. Genuine user-preference open questions

These are the ONLY forks where "do what Terminator does" does not settle it and the user's taste
decides. Everything else is resolved in the per-piece docs by Terminator fidelity.

1. **Q1 (group) — `Ctrl+Shift+B` (`ToggleBroadcast`) semantics now broadcast is 3-way.**
   Terminator has NO toggle key (it ships 3 unbound scope actions + a menu radio), so this
   rustinator-only binding is unspecified by Terminator.
   Options: (a) Off<->All toggle; (b) 3-way cycle Off->Group->All; (c) remove it, rely on the 3
   explicit scope actions.
   **Recommendation: (a)** — preserves the old `broadcast=true == all-panes` muscle memory while the
   explicit `BroadcastOff/Group/All` cover Terminator's full surface.

2. **Q2 (group) — the concrete transmit/receive titlebar indicator token.**
   Terminator answers the SEMANTICS (which state shows what) but its tokens are GTK raster icons
   with no egui analog, so the concrete glyph is unmade.
   Options: (a) filled dot in `broadcast_border` for Transmit + dimmed dot for ReceiveOn + nothing
   for ReceiveOff, plus the group-name text; (b) a short text tag (TX/RX or ALL/GRP); (c) Unicode
   glyphs.
   **Recommendation: (a)** — smallest faithful rendering, reuses the existing `broadcast_border`
   color, matches the user's "plain text over decoration" default.

3. **Q3 (tab-bar) — add a 5th config knob `scroll_tabbar`, or auto-scroll with no knob?**
   Terminator implements the scrollable tab bar via a distinct `scroll_tabbar` toggle (default
   false) independent of `homogeneous`; the user's own design-defaults say "don't add config for
   flexibility." Terminator-fidelity vs the user's stated minimalism genuinely conflict.
   Options: (a) add `scroll_tabbar: bool` default false (Terminator-exact); (b) no knob, auto-scroll
   only when `homogeneous=false` and tabs overflow; (c) no knob, always shrink to fit.
   **Recommendation: (a)** — faithful to Terminator and default-false equals current behavior; this
   is the one place to ask the user directly, since it pits their config minimalism against parity.

4. **Q4 (tab-bar) — keep the existing modal "Set title" dialog alongside the new inline
   double-click rename, or replace it?**
   Terminator has ONLY inline rename; it gives no guidance on whether to retain rustinator's
   pre-existing modal (`Action::SetTitle` + `dialogs.rs:155`). Both write `tab.custom_title`.
   Options: (a) keep both; (b) inline only (delete the modal); (c) modal only (double-click opens
   the modal).
   **Recommendation: (a)** for the first pass — lowest risk, no removed affordance; revisit if
   redundant.

5. **Q5 (multi-window, only if built) — what does the global drop-down hotkey window mirror in
   N-window mode?** It currently mirrors the single `App` (`window.rs:159-162`); this mirror model
   has no Terminator analog (Terminator drop-downs are independent).
   Options: (a) mirror the primary window; (b) mirror the most-recently-focused terminal window;
   (c) make the hotkey window independent with its own `TabManager` (Terminator-faithful, larger).
   **Recommendation: (b)** — least surprising, preserves the cheap existing mirror, defers the
   independent-Quake rework.

6. **Q6 (multi-window, only if built) — does in-process `NewWindow` inherit the focused window's
   cwd, or start at profile default / home?** rustinator's own `new_tab`/`split` inherit cwd;
   Terminator's New Window opens fresh. Internal-consistency vs Terminator-fidelity collide.
   Options: (a) inherit the focused-window cwd (consistent with rustinator); (b) profile default /
   home (Terminator-faithful); (c) a config toggle (rejected by the simplest-thing rule).
   **Recommendation: (a)** — matches rustinator's own new-tab/split UX; revisit only if a user
   reports it should be fresh.

### Resolved by fidelity (NOT open questions)
- Default broadcast scope = `Off`: observationally identical to Terminator's `group` default until
  a group exists, and at startup there is one pane and no titlebar, so no visible difference.
- Profile switch = live re-style, never a respawn (Terminator behavior).
- New tab uses the active profile; splits inherit the parent (gap-analysis.md:61); custom command
  runs via `$SHELL -c` (Terminator behavior).
- Group "All" scope = all panes in the active tab of the current window (matches today's
  single-window/active-tab input routing).

## 6. USER DECISIONS (2026-06-30) — binding for WF-C

- **Q1 (Ctrl+Shift+B):** 3-way **cycle** Off -> Group -> All -> Off. (Not the off<->all toggle.)
  The explicit `BroadcastOff/Group/All` actions still exist (unbound, menu radio).
- **Q2 (broadcast indicator):** colored **dots** in the per-pane titlebar **AND** the pane
  outline/border color reflects broadcast state — make it unmistakable. Reuse/extend the existing
  broadcast-border paint (transmit vs receive-on get distinct, obvious treatment; off = normal).
- **Q3 (scrollable tab bar):** add the `scroll_tabbar: bool` config (default **false** = current
  behavior) AND expose it as a toggle in Settings (prefs_ui Tabs group). Off by default, opt-in.
- **Q4 (tab rename):** keep **both** — inline double-click rename AND the existing modal
  "Set title..." dialog.
- **Q5 (detached-window cwd):** DEFERRED — part of multi-window (WF-E), not being built now.
