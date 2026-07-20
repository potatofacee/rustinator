# Design: Group model + broadcast-scope rework (INFRA-Group)

WF-C infra block. Status: design, no code. All line numbers verified against source on 2026-06-30.

## 1. What changes, in one paragraph

Replace the per-tab `broadcast: bool` (`tabs.rs:20`) with a single **global** 3-way
`BroadcastScope { Off, Group, All }` on `TabManager`, and give each `Pane` a named
group `group: Option<String>` (the name *is* the identity, mirroring Terminator's
`terminal.group`). All grouping/broadcast logic lands in a new leaf module
`src/groups.rs` (pure predicates + small orchestration free-functions). The per-pane
title-bar (group label + transmit/receive indicator) is extracted out of `draw_panes`
into a new `src/title_bar.rs`. `Tab::select_input_targets` gains a `scope` parameter and
keeps the read-only gate; the two existing read-only tests move to `groups.rs` against
the generalized predicate (Off == old `broadcast=false`, All == old `broadcast=true`).
Net behavior is unchanged until a group is actually formed.

## 2. Terminator behavior being mirrored (already decided — do not redesign)

Verified in `/Users/aub/code/rustinator/terminator/terminatorlib`:

- Group identity is **per-terminal**: `terminal.group` is a group NAME string or `None`
  (`terminal.py:626`); the name is the identity (`group_emit` matches `term.group == group`,
  `terminator.py:562-566`).
- Broadcast scope is a **single global** value `terminator.groupsend`, 3 states
  `groupsend_type={'all':0,'group':1,'off':2}` (`terminator.py:66`); config default
  `broadcast_default='group'` (`config.py:93`).
- Target selection `get_target_terms` (`terminator.py:610-616`): `all` -> every terminal;
  `group` -> siblings sharing the focused pane's group **only if the focused pane has a
  group**, else just the focused widget; `off` -> just focused. Focused ALWAYS receives;
  broadcast ADDS others; ungrouped panes never implicitly group.
- Group actions (`window.py`): `group_all` -> group named "All" on every terminal (842-848);
  `ungroup_all` -> all `None` (856-857); `group_tab` -> "Tab N" (N=page num, deduped) for
  the current tab's terminals (879-894); `ungroup_tab` clears the current tab (903-910);
  `ungroup_win` clears the window (876-877).
- `insert_number`/`insert_padded` (`terminal.py:2091-2095` -> `do_enumerate`
  `terminator.py:576-590`): writes each target terminal's 1-based index (among ALL terminals,
  tree order) into that terminal via `feed -> vte.feed_child` (raw bytes to child PTY);
  padded zero-pads to the width of the terminal count; targets = current broadcast targets.
- Titlebar per-pane indicator (`titlebar.py:122-183`): focused pane shows transmit + an
  `_active_broadcast_{off,group,all}` icon; a non-focused pane shows `_receive_on` when it
  would receive (all->everyone; group->same group as focused; off->nobody) else
  `_receive_off`; the group NAME label shows when set.
- Default Linux keybindings (`config.py:184-199`): `group_all <Super>g`;
  `ungroup_all <Shift><Super>g`; `group_tab <Super>t`; `ungroup_tab <Shift><Super>t`;
  `ungroup_win <Shift><Super>w`; `insert_number <Super>1`; `insert_padded <Super>0`;
  `broadcast_off/group/all` and `create_group/group_win` all `''` (UNBOUND).

On macOS rustinator strips every `Super+` default (`keybindings.rs:215`), so these are
Linux-only by default — consistent with the existing `Super+R`/`Super+Shift+R` handling, no
Cmd collision. New splits inherit the parent's group (split-to-group).

## 3. Data model

### NEW `src/groups.rs`
```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum BroadcastScope { #[default] Off, Group, All }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Indicator { Transmit(BroadcastScope), ReceiveOn, ReceiveOff }
```
Group membership is the NAME itself — **no numeric id, no registry**. The set of live group
names is DERIVED on demand by scanning panes (avoids a mutable registry that can desync;
replaces Terminator's `group_hoover` GC).

Default = `Off`. This is observationally identical to Terminator's `group` default until a
group exists (group scope with an ungrouped focused pane targets only the focused pane), and
at startup there is a single pane with no title bar (`show_title_bars = leaves.len() > 1`,
`pane_ui.rs:99`) so no indicator glyph is shown either way. This is a settled engineering
call, not an open question.

### `src/pane.rs` — `Pane` (struct at `pane.rs:762-802`)
Add `pub group: Option<String>`, initialized `None` in the `spawn` struct literal
(`pane.rs:873-896`, alongside `read_only: false` at line 892). `respawn` (`pane.rs:899-989`)
mutates `&mut self` in place and never reconstructs `Self`, so `group` survives respawn
untouched — no edit there. **Land this Pane edit together with Profile-on-spawn's
`profile`/`spawn_command` fields** (see §9 and the overview): one struct change, one
initializer change.

### `src/tabs.rs`
- `Tab` (`tabs.rs:15-22`) LOSES `broadcast: bool` (line 20) and its initializer (line 34).
- `TabManager` (`tabs.rs:182-193`) GAINS exactly ONE field `pub broadcast_scope: BroadcastScope`,
  initialized in `TabManager::new` (`tabs.rs:196-206`).

No serde anywhere on `Tab`/`Pane`/`TabManager`: all runtime-only state. Removing `tab.broadcast`
is invisible to any persisted data. `pane.group` is runtime-only here; Terminator persists
`layout['group']` (`terminal.py:1817`), so the field is *ready* for the separate
Layout-persistence gap item but that wiring is out of scope.

## 4. New modules and decomposition (NO god classes; every fn < 100 lines)

### `src/groups.rs` — focused owner of all group/broadcast LOGIC
Pure predicates (single source of truth shared by input routing AND the titlebar, so they
can never disagree). Each is < 20 lines.

```rust
// shared core: All=>true, Group=>this.is_some() && this==focused, Off=>false
fn is_receiver(scope: BroadcastScope, this_group: Option<&str>, focused_group: Option<&str>) -> bool

// input gate: !read_only && (is_focused || is_receiver(..))
pub fn pane_receives_input(scope, read_only, is_focused, this_group, focused_group) -> bool

// visual: is_focused => Transmit(scope) else ReceiveOn/Off via is_receiver
pub fn indicator(scope, is_focused, this_group, focused_group) -> Indicator

// Terminator dedup while-loop: "Tab {page}", bump page until free
pub fn next_tab_group_name(existing: &HashSet<String>, page: usize) -> String

// padded => {:0width$} where width = total.to_string().len()
pub fn enumerate_text(index_1based: usize, total: usize, padded: bool) -> String
```

Orchestration free-functions (take `&mut [Tab]` / `&TabManager`, NOT methods piled on
`TabManager`). Each < 25 lines:
```rust
pub fn group_all(tabs: &mut [Tab])              // every pane.group = Some("All")
pub fn ungroup_all(tabs: &mut [Tab])            // every pane.group = None
pub fn group_active_tab(tabs: &mut [Tab], active: usize)   // derive names -> next_tab_group_name -> assign
pub fn ungroup_active_tab(tabs: &mut [Tab], active: usize) // clear active tab's panes
pub fn insert_index(tab_mgr: &TabManager, padded: bool)    // see below
```
`insert_index`: build the global pane order by walking each `tab.layout.leaves_in_order()`
(verified `layout.rs:287`), compute `total`; for each pane in
`active_tab().select_input_targets(scope)` send `enumerate_text(idx,total,padded).into_bytes()`
via the existing `pane.send_bytes` (verified `pane.rs:1069`). Targets route through the same
gate as keystrokes — no bypass.

This accepts a benign intra-crate module cycle (`tabs` uses `groups::BroadcastScope` + the
predicate; `groups` uses `tabs::Tab`), which Rust permits within a crate. It keeps 100% of
grouping logic in `groups.rs` and `TabManager` at +1 field. A `#[cfg(test)]` module holds the
two relocated read-only tests plus new Group-scope / indicator / enumerate tests.

### `src/title_bar.rs` — focused owner of the per-pane title bar
Pulls ~60 lines of paint out of `draw_panes` so `draw_panes` SHRINKS instead of growing.
```rust
pub struct TitleBarModel {
    pub title: String, pub cols: usize, pub lines: usize,
    pub group: Option<String>, pub indicator: Indicator, pub focused: bool,
}
pub fn split_rect(pane_rect: egui::Rect) -> (egui::Rect, egui::Rect) // (title, terminal) via PANE_TITLE_HEIGHT
pub fn paint(ui: &mut egui::Ui, title_rect: egui::Rect, model: &TitleBarModel, cfg: &Config)
fn indicator_glyph(ind: Indicator, cfg: &Config) -> (egui::Color32, ...) // private mapping
```
`paint` draws bg + the existing title/dims text + the group-name label + the
transmit/receive glyph. Reuses `Profile::broadcast_border_rgb` (verified `config.rs:244`).
`TitleBarModel` is a value struct precomputed by the caller, so `paint` is a pure function of
state, free of `TabManager` borrows and testable indirectly.

### `draw_panes` decomposition (`pane_ui.rs:53-259`) — currently ~206 lines, MUST end < 100
The titlebar PAINT block (`pane_ui.rs:117-177`) moves into `title_bar::paint`. The
focus/drag INTERACTION (`126-140`) stays inline (it mutates focus/drag state); `draw_panes`
builds a cheap `TitleBarModel` and calls `title_bar::paint`. To finish landing `draw_panes`
under 100 lines, also extract the per-leaf body (the loop interior, currently `102-254`) into
a private `draw_leaf(state, ctx, ui, id, rect, show_title_bars, ...) -> ()` helper that
returns nothing but pushes into `deferred`/`pane_drop_rects` via `&mut` params. Net:
`draw_panes` becomes a thin loop (~40 lines); `draw_leaf` ~80 lines; `title_bar::paint`
~45 lines. This is a zero-behavior extraction; compile + visual-check after the move.

### `App::execute_pane_actions` (`main.rs:343-435`) — already ~92 lines
The match in `execute_pane_actions` is **exhaustive with no `_` arm**, so a new `Action`
variant is a compile error until handled — good. Adding 10 new arms inline would break the
< 100-line rule. Resolution: route all 10 group/insert variants through ONE combined match
arm:
```rust
Action::GroupAll | Action::UngroupAll | Action::GroupTab | Action::UngroupTab
| Action::UngroupWin | Action::BroadcastOff | Action::BroadcastGroup | Action::BroadcastAll
| Action::InsertNumber | Action::InsertPadded => self.dispatch_group_action(action),
```
`App::dispatch_group_action(&mut self, action: Action)` is a new ~16-line router: each arm is
one line into `groups::*` or a `self.tab_mgr.broadcast_scope = ...` assignment. `execute_pane_actions`
gains exactly ONE arm, staying under 100. `dispatch_group_action` stays under 100.

### `build_context_menu` (`pane_ui.rs:580-640`) — currently ~60 lines
Extract the broadcast section into a helper so the menu does not swell past 100 lines:
```rust
fn broadcast_and_group_menu(ui, scope: BroadcastScope, pane_id, tab, bindings, state, deferred)
```
3 scope radio-style items (Off/Group/All -> `BroadcastOff/Group/All`) + "Group with all"
(`GroupAll`) / "Ungroup" (`UngroupAll`) + a "New group..." entry that reuses the existing
title-dialog pattern (`DialogState.title_dialog_buf`, `dialogs.rs:7`, `draw_title_dialog`
`dialogs.rs:155`) via a new pending field (mirror `layout_save_buf`). The single
`ToggleBroadcast` item (`pane_ui.rs:608-613`) is REMOVED from the menu (the 3 scope items
replace it); `ToggleBroadcast` stays reachable via its `Ctrl+Shift+B` binding.

## 5. Actions + keybindings + the test-harness contract (do not miss any of these)

New `Action` variants (`keybindings.rs:11-56`): `GroupAll, UngroupAll, GroupTab, UngroupTab,
UngroupWin, BroadcastOff, BroadcastGroup, BroadcastAll, InsertNumber, InsertPadded`.

For EACH new variant the implementer MUST update ALL of:
1. `enum Action` (`keybindings.rs:11-56`).
2. `Action::from_str` (`keybindings.rs:59-116`) — strings `group_all`, `ungroup_all`,
   `group_tab`, `ungroup_tab`, `ungroup_win`, `broadcast_off`, `broadcast_group`,
   `broadcast_all`, `insert_number`, `insert_padded`.
3. `_assert_exhaustive` match (`keybindings.rs:959-1005`) — **compile error otherwise**.
4. `all_actions()` vec (`keybindings.rs:1007-1052`).
5. `execute_pane_actions` (`main.rs:343-435`) — via the single combined arm above
   (**exhaustive match, compile error otherwise**).
6. `every_action_reachable_via_binding_or_menu` (`keybindings.rs:1075-1095`): the 7
   group/insert actions get Linux default binds (covered by `bound`); the 3 unbound
   `broadcast_*` scopes must be added to `context_menu_actions()` (`keybindings.rs:1057-1073`)
   since they live in `broadcast_and_group_menu`. Also drop `ToggleBroadcast` from
   `context_menu_actions()` (it leaves the menu) — it stays covered by its `bound`
   `Ctrl+Shift+B`.

Default Linux binds to add (replace the commented stubs at `keybindings.rs:354-370`):
```
Super+G -> GroupAll        Super+Shift+G -> UngroupAll
Super+T -> GroupTab        Super+Shift+T -> UngroupTab
Super+Shift+W -> UngroupWin
Super+1 -> InsertNumber    Super+0 -> InsertPadded
```
`broadcast_off/group/all` stay UNBOUND (Terminator parity). Un-`#[ignore]`
`action_broadcast_scopes` (`keybindings.rs:903-909`); leave `defaults_include_broadcast_scopes`
(`912-918`) `#[ignore]`d (the scopes are intentionally unbound).

Collision check: `Super+T` (GroupTab) and `Super+Shift+T` (UngroupTab) only exist on Linux;
on macOS all `Super+` defaults are stripped (`keybindings.rs:212-226`), so no clash with the
existing `Ctrl/Cmd+Shift+T` NewTab.

`ToggleBroadcast` (`Ctrl+Shift+B`, `keybindings.rs:353`) is retained — see open question Q1.

## 6. Public API surface

```rust
// tabs.rs
pub(crate) fn Tab::select_input_targets(&self, scope: BroadcastScope) -> Vec<&Pane>
pub(crate) fn TabManager::toggle_broadcast(&mut self)   // Off<->All (Q1)
// pane.rs
Pane.group: Option<String>
// main.rs
fn App::dispatch_group_action(&mut self, action: Action)
// groups.rs / title_bar.rs: see §4
```

`Tab::select_input_targets` (`tabs.rs:45-53`) becomes:
```rust
pub(crate) fn select_input_targets(&self, scope: BroadcastScope) -> Vec<&Pane> {
    let focused_group = self.panes.get(&self.focused).and_then(|p| p.group.as_deref());
    self.panes.iter()
        .filter(|&(&id, pane)| groups::pane_receives_input(
            scope, pane.read_only, id == self.focused, pane.group.as_deref(), focused_group))
        .map(|(_, pane)| pane).collect()
}
```
The `pane_is_input_target` helper + its 2 tests (`tabs.rs:56-61`, `846-883`) move into
`groups.rs` as `pane_receives_input`. This preserves the comment-documented invariant at
`tabs.rs:39-44`.

## 7. The single input-routing chokepoint (composes with the existing gate)

`Tab::select_input_targets` is the ONLY input gate. All three callers route through it with
the same scope; nothing bypasses it:
- keys: `main.rs:642` (`tab.select_input_targets()` -> `input::process_keys`) becomes
  `tab.select_input_targets(self.tab_mgr.broadcast_scope)`.
- paste: `tabs.rs:669` (`paste_from_clipboard`) -> `self.active_tab().select_input_targets(self.broadcast_scope)`.
- insert-number: `groups::insert_index` uses the same `select_input_targets(scope)`.

`read_only` stays an AND filter inside `pane_receives_input` (terminal RESPONSES bypass this
and write to the PTY directly via `send_bytes`, so they stay ungated — invariant preserved).
The Off/All cases reproduce today's `broadcast=false`/`true` routing exactly, which the two
relocated tests assert.

## 8. Title-bar border + indicator wiring (`pane_ui.rs:1099-1127`)

The focus/broadcast border block currently reads `tab.broadcast` (`pane_ui.rs:1109`). Rewrite
it to use `groups::indicator(scope, focused, this_group, focused_group)` + the `read_only`
guard, mapping `Transmit(All|Group)` -> `broadcast_border_rgb`, focused-only `Transmit(Off)`
-> `focus_border_rgb`, else `None`. Compute `focused_group` once per frame in `draw_panes`
and thread it down. See Q2 for the indicator's concrete visual token.

## 9. Migration

Runtime-only; NO serde / config back-compat impact. `Tab` is not serialized, so removing
`tab.broadcast` is invisible. `Pane` is not serialized either. No new config keys: the default
scope is hardcoded `Off`; the indicator reuses the existing `colors.broadcast_border`
(`config.rs:129,244,278`). Per-state titlebar colors (transmit/receive/inactive,
gap-analysis.md:68) are a SEPARATE gap item and deliberately NOT pulled in.

**Shared-edit contract with Profile-on-spawn**: both features add a per-pane field and both
hook `split`/`split_here` (`tabs.rs:252-284`). Land the `Pane` struct edits together and set
both from the parent pane in ONE place inside `split`/`split_here`: `new.group =
parent.group.clone()` (pure copy) alongside the profile stamping. Pure-logically disjoint.

## 10. Test seams

- `groups::pane_receives_input` (pure): the two relocated read-only assertions (read_only
  excluded under All; focused-only under Off) plus new Group cases (member-of-focused-group
  receives; non-member does not; ungrouped-focused yields focused-only).
- `groups::indicator` (pure): focused => `Transmit(scope)` per scope; non-focused member
  under Group => `ReceiveOn`; non-member under Group => `ReceiveOff`; everyone under All =>
  `ReceiveOn`; everyone under Off => `ReceiveOff`. Mirrors `titlebar.py:122-183`.
- `groups::next_tab_group_name` (pure): empty set => "Tab 0"; {"Tab 0"} => "Tab 1".
- `groups::enumerate_text` (pure): (3,12,false)=>"3"; (3,12,true)=>"03"; (3,5,true)=>"3".
- keybindings: un-ignored `action_broadcast_scopes`; a new test asserts the 7 Linux default
  binds at the exact `Super+` combos.
- `Tab::select_input_targets` stays integration-testable via a constructed `Tab`, now
  parameterized by `scope`.

## 11. Ordered implementation steps (compile/test after each)

1. Create `src/groups.rs` (enums + pure fns) + `mod groups;` in `main.rs` (`1-22`). Move the
   two read-only tests in, re-expressed as `pane_receives_input(Off/All, ..)`. Add Group /
   indicator / enumerate tests. `cargo test` — fully verifiable in isolation, zero wiring.
2. Add `pub group: Option<String>` to `Pane` (`pane.rs:762`), init `None` (`~892`).
   `cargo check` — no behavior change. (Combine with Profile-on-spawn step.)
3. Rework the broadcast model: delete `Tab.broadcast` (`tabs.rs:20,34`) and
   `pane_is_input_target` + its tests (`56-61, 846-883`); add `TabManager.broadcast_scope`
   (`182-206`); change `select_input_targets` to take `scope`; update `toggle_broadcast`
   (`555-558`), `paste_from_clipboard` (`669`); fix the border block (`pane_ui.rs:1109`) and
   the context-menu read (`608`) to use scope/indicator. Thread scope at `main.rs:642`.
   `cargo test` — relocated tests guarantee the read-only gate; Off reproduces today's routing.
4. Split-to-group: in `split`/`split_here` (`tabs.rs:252-284`) copy the focused parent's
   `group` to the new pane after insert. `cargo check`.
5. Actions + dispatch: add the 10 variants + `from_str` + the SIX harness updates in §5 +
   the Linux binds; add `App::dispatch_group_action` and the single combined arm in
   `execute_pane_actions`; implement the `groups::*` orchestration fns. `cargo test`.
6. Title bar: create `src/title_bar.rs` + decompose `draw_panes` (extract `draw_leaf` +
   `title_bar::paint`); build `TitleBarModel` per leaf. `cargo check` + visual check.
7. Context menu: extract `broadcast_and_group_menu` (3 scope items + group with all / ungroup
   + New group...). `cargo check`.

## 12. Open questions (genuine user-preference forks only) — see overview for the full list

- **Q1** `Ctrl+Shift+B` (`ToggleBroadcast`) semantics now broadcast is 3-way. Terminator has
  NO toggle (it ships 3 unbound scope actions + a menu radio), so this rustinator-only key is
  unspecified by Terminator. Recommendation: keep it as an Off<->All toggle (preserves the old
  `broadcast=true == all` muscle memory) while the explicit `BroadcastOff/Group/All` cover the
  full surface.
- **Q2** the concrete transmit/receive indicator token (Terminator uses themed raster icons
  with no egui analog). Recommendation: a filled dot in `colors.broadcast_border` for
  `Transmit`, a dimmed dot for `ReceiveOn`, nothing for `ReceiveOff`, plus the group-name text
  — minimal, reuses an existing color, matches the user's "plain text over decoration" default.
