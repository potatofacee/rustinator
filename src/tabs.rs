use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use alacritty_terminal::term::Config as TermConfig;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::config::{Config, SavedLayout, TerminalMeta};
use crate::dialogs::DialogState;
use crate::groups::{self, BroadcastScope};
use crate::layout::{self, Direction, Node};
use crate::pane::{Pane, PaneDefaults, PaneId};
use crate::profile::{self, ProfileName, SpawnCommand};

pub(crate) const INITIAL_COLS: u16 = 100;
pub(crate) const INITIAL_LINES: u16 = 32;
pub(crate) const PANE_GAP: f32 = 3.0;

pub(crate) struct Tab {
    pub panes: HashMap<PaneId, Pane>,
    pub layout: Node,
    pub focused: PaneId,
    pub zoomed: Option<PaneId>,
    pub custom_title: Option<String>,
}

impl Tab {
    pub(crate) fn new(first_pane: Pane) -> Self {
        let id = first_pane.id;
        let mut panes = HashMap::new();
        panes.insert(id, first_pane);
        Self {
            panes,
            layout: Node::Leaf(id),
            focused: id,
            zoomed: None,
            custom_title: None,
        }
    }

    /// Panes that should receive user input (keystrokes and pastes). `scope`
    /// selects the fan-out: `Off` targets only the focused pane; `All` targets
    /// every non-read_only pane; `Group` targets the focused pane plus every
    /// pane sharing its group (an ungrouped focused pane collapses to
    /// focused-only). The read_only AND-gate holds in every scope: read_only
    /// panes are never input targets — terminal RESPONSES (OSC replies, focus
    /// events, DA) bypass this and write to the PTY directly via `send_bytes`,
    /// so they stay ungated. The per-pane decision lives in the shared predicate
    /// `groups::pane_receives_input`, the single source of truth it shares with
    /// the title-bar indicator.
    pub(crate) fn select_input_targets(&self, scope: BroadcastScope) -> Vec<&Pane> {
        let focused_group = self
            .panes
            .get(&self.focused)
            .and_then(|p| p.group.as_deref());
        self.panes
            .iter()
            .filter(|&(&id, pane)| {
                groups::pane_receives_input(
                    scope,
                    pane.read_only,
                    id == self.focused,
                    pane.group.as_deref(),
                    focused_group,
                )
            })
            .map(|(_, pane)| pane)
            .collect()
    }

    /// Per-leaf metadata for a layout save, in `leaves_in_order` order — one
    /// `TerminalMeta` per leaf so the sidecar stays index-aligned with the
    /// template's leaves (a missing pane, which should not happen, yields a
    /// default entry rather than a shifted Vec). Implements the save side of the
    /// §8 `SavedLayout.terminals` contract.
    fn terminal_metas(&self) -> Vec<TerminalMeta> {
        let mut leaves = Vec::new();
        self.layout.leaves_in_order(&mut leaves);
        leaves
            .iter()
            .map(|id| {
                self.panes
                    .get(id)
                    .map(|p| terminal_meta(&p.profile, p.cwd(), &p.spawn_command))
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// Pick the pane that should receive focus after the leaf at `target_idx` (its
/// position in the pre-removal in-order `before` list) is removed. Prefers the
/// in-order successor, then the predecessor, then the first surviving leaf. A
/// candidate is only chosen when it survives in `leaves`. This is the generic
/// neighbor heuristic; `close_focused` overrides it for a right-child leaf,
/// whose left sibling (the predecessor) is the pane that expands into the rect.
fn pick_new_focus(before: &[PaneId], target_idx: Option<usize>, leaves: &[PaneId]) -> Option<PaneId> {
    target_idx
        .and_then(|i| before.get(i + 1).copied())
        .filter(|id| leaves.contains(id))
        .or_else(|| {
            target_idx
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| before.get(i).copied())
                .filter(|id| leaves.contains(id))
        })
        .or_else(|| leaves.first().copied())
}

/// Whether `target` is the right child of its immediate parent split. `None`
/// when `target` is the root leaf or absent. Mirrors which sibling
/// `Node::remove_leaf` promotes: a right child is replaced by its left sibling
/// subtree (whose rightmost leaf is the in-order predecessor), a left child by
/// its right sibling subtree (whose leftmost leaf is the in-order successor).
fn leaf_is_right_child(node: &Node, target: PaneId) -> Option<bool> {
    match node {
        Node::Leaf(_) => None,
        Node::Split { left, right, .. } => {
            if matches!(left.as_ref(), Node::Leaf(id) if *id == target) {
                Some(false)
            } else if matches!(right.as_ref(), Node::Leaf(id) if *id == target) {
                Some(true)
            } else {
                leaf_is_right_child(left, target).or_else(|| leaf_is_right_child(right, target))
            }
        }
    }
}

/// Decide the focus handoff when moving focus from `old` to `new`. Returns
/// `Some((old, new))` — focus-out `old`, focus-in `new` — only when focus
/// actually moves to a different, present pane; `None` for a no-op (already
/// focused, or the target is absent).
fn focus_transition(old: PaneId, new: PaneId, new_present: bool) -> Option<(PaneId, PaneId)> {
    if old != new && new_present {
        Some((old, new))
    } else {
        None
    }
}

/// Wrap `current + step` into `0..n` with correct negative-step handling.
/// `n` must be greater than 0. Shared by tab switching, focus cycling, and
/// tab moves.
fn wrap_index(current: usize, step: i32, n: usize) -> usize {
    let n_i = n as i32;
    let idx = current as i32;
    (((idx + step) % n_i + n_i) % n_i) as usize
}

/// New active-tab index after the tab at `closed_idx` is removed, given the
/// post-removal tab count `new_len` (which must be >= 1). Clamps the active
/// index back onto the list when it ran off the end, and shifts it down by one
/// when a lower-indexed tab was closed.
fn adjust_active_tab(active: usize, closed_idx: usize, new_len: usize) -> usize {
    if active >= new_len {
        new_len - 1
    } else if closed_idx < active {
        active - 1
    } else {
        active
    }
}

/// New active-tab index after the tab at `from` is moved to `to` — i.e. removed
/// at `from`, then re-inserted at `to` (mirrors `reorder_tab`'s
/// `Vec::remove`+`Vec::insert`). The active tab follows the move: if the active
/// tab IS the moved one it lands at `to`; otherwise it shifts down by the
/// removal at `from` (when it sat after `from`) and up by the re-insertion at
/// `to` (when its post-removal index is at or past `to`).
fn remap_active_after_reorder(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        return to;
    }
    let after_remove = if active > from { active - 1 } else { active };
    if after_remove >= to {
        after_remove + 1
    } else {
        after_remove
    }
}

/// Index at which a brand-new tab is inserted into `tabs`. With
/// `new_tab_after_current` the tab lands directly after the active tab
/// (`active + 1`, Terminator's `new_tab_after_current=true`); otherwise it is
/// appended at the end (`len`, the default). Clamped to `0..=len` so the result
/// is always a valid `Vec::insert` position.
pub(crate) fn tab_insert_index(active: usize, new_tab_after_current: bool, len: usize) -> usize {
    if new_tab_after_current {
        (active + 1).min(len)
    } else {
        len
    }
}

/// Reindex the search-pane anchor when the tab at `closed_idx` is removed.
/// Returns the updated anchor plus whether the search panel should close
/// (true only when the search pane lived in the closed tab).
fn adjust_search_pane(
    search_pane: Option<(usize, PaneId)>,
    closed_idx: usize,
) -> (Option<(usize, PaneId)>, bool) {
    match search_pane {
        Some((ti, _)) if ti == closed_idx => (None, true),
        Some((ti, pid)) if ti > closed_idx => (Some((ti - 1, pid)), false),
        other => (other, false),
    }
}

/// Reconcile the `existing` pane ids against the `needed` leaf count of a
/// restored layout. Returns `(kept, spawn_count, removed)`: the existing ids to
/// keep (the first `needed` when over-supplied, else all of them), how many new
/// panes must be spawned and appended, and the surplus ids to drop (in original
/// order). Exactly one of `spawn_count`/`removed` is non-empty.
fn reconcile_ids(existing: Vec<PaneId>, needed: usize) -> (Vec<PaneId>, usize, Vec<PaneId>) {
    if existing.len() <= needed {
        let spawn = needed - existing.len();
        (existing, spawn, Vec::new())
    } else {
        let removed = existing[needed..].to_vec();
        let kept = existing[..needed].to_vec();
        (kept, 0, removed)
    }
}

/// The custom command string of a spawn command, if it is a `["-c", cmd]` shell
/// invocation; `None` for an interactive shell (`[]` / `["--login"]`). Drives
/// the layout-save side of the §8 sidecar: `TerminalMeta.command` records a
/// command only when the pane was spawned with a custom one.
fn custom_command_arg(command: &SpawnCommand) -> Option<String> {
    match command.args.as_slice() {
        [flag, cmd] if flag.as_str() == "-c" => Some(cmd.clone()),
        _ => None,
    }
}

/// Build the per-leaf `TerminalMeta` saved for a pane: its profile name always,
/// cwd when known, and the command only when it is a custom (`-c`) invocation.
fn terminal_meta(profile: &str, cwd: Option<PathBuf>, command: &SpawnCommand) -> TerminalMeta {
    TerminalMeta {
        profile: Some(profile.to_string()),
        cwd,
        command: custom_command_arg(command),
    }
}

/// Profile NAME to spawn a restored leaf with: the sidecar entry's profile when
/// present, else `fallback` (the active-profile factory's name — the back-compat
/// path for a missing/short sidecar).
fn leaf_profile_name(meta: Option<&TerminalMeta>, fallback: &str) -> ProfileName {
    meta.and_then(|m| m.profile.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// The non-blank custom command recorded in a sidecar entry, if any. The stored
/// string is returned verbatim (only all-whitespace is rejected) so a restore
/// round-trips the exact command the pane was running.
fn custom_command_text(meta: &TerminalMeta) -> Option<String> {
    meta.command.as_ref().filter(|c| !c.trim().is_empty()).cloned()
}

/// Spawn command for a restored leaf. A non-blank saved custom command wins (run
/// through the shell as `["-c", cmd]`); otherwise the saved profile is resolved
/// against `cfg`, honoring that profile's login/custom-shell rules even when it
/// is not the active one; with neither, `fallback` (the active-profile factory's
/// command) is used — the back-compat path for a missing/short sidecar.
fn leaf_command(meta: Option<&TerminalMeta>, cfg: &Config, fallback: &SpawnCommand) -> SpawnCommand {
    let Some(meta) = meta else {
        return fallback.clone();
    };
    if let Some(cmd) = custom_command_text(meta) {
        return SpawnCommand {
            program: None,
            args: vec!["-c".into(), cmd],
        };
    }
    match meta.profile.as_deref() {
        Some(name) => profile::spawn_command(profile::resolve(cfg, name)),
        None => fallback.clone(),
    }
}

/// Next broadcast scope in the `Ctrl+Shift+B` (`ToggleBroadcast`) cycle:
/// `Off -> Group -> All -> Off` (USER DECISION Q1). The explicit
/// `BroadcastOff/Group/All` actions set a scope directly; this drives only the
/// single cycling toggle.
fn next_broadcast_scope(scope: BroadcastScope) -> BroadcastScope {
    match scope {
        BroadcastScope::Off => BroadcastScope::Group,
        BroadcastScope::Group => BroadcastScope::All,
        BroadcastScope::All => BroadcastScope::Off,
    }
}

/// Allocate the next `PaneId` from a shared atomic source. `fetch_add(1)`
/// returns the prior counter value and advances it, so the ids handed out are
/// strictly increasing and unique even when several `TabManager`s clone the same
/// `Arc<AtomicU64>` — a detached window keeps drawing unique ids from the one
/// process-wide source. Starting the counter at 1 reproduces the historic
/// first-pane id and `next_pane_id` sequence exactly. `Relaxed` suffices: only
/// the uniqueness/monotonicity of the returned value matters, not ordering
/// against other memory (id allocation only ever runs on the UI thread).
pub(crate) fn alloc_pane_id(alloc: &AtomicU64) -> PaneId {
    alloc.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub(crate) enum FocusDir {
    Up,
    Down,
    Left,
    Right,
}

pub(crate) struct PaneFactory {
    pub cell_w: f32,
    pub cell_h: f32,
    pub term_config: TermConfig,
    pub pane_defaults: PaneDefaults,
    pub event_loop_proxy: EventLoopProxy<crate::window::UserEvent>,
    /// The profile NAME panes built by this factory spawn with. The App resolves
    /// the name (active profile for `new_tab`/first pane; the focused parent
    /// pane's profile for `split`/`split_here`/`SplitAuto`/`OpenTerminalHere`)
    /// before constructing the factory, so this carries the inherit-on-split
    /// decision into `spawn_pane`. Profile resolution itself lives in
    /// `profile::` — the factory only transports the result.
    pub profile: ProfileName,
    /// The resolved spawn command for `profile`, derived by
    /// `profile::resolved`/`spawn_command` (custom command via `$SHELL -c`, or
    /// the interactive login/non-login shell). Stamped onto each spawned pane so
    /// `respawn` can reuse it without re-resolving against `Config`.
    pub command: SpawnCommand,
}

pub(crate) struct TabManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Shared, process-wide source of unique `PaneId`s, replacing the old local
    /// `next_pane_id` counter. Every new pane draws its id via `alloc_pane_id`,
    /// so two `TabManager`s that clone this `Arc` (e.g. a detached window) never
    /// collide. Created once in `App::new` and handed to the manager.
    pub alloc: Arc<AtomicU64>,
    /// Global broadcast scope (mirrors Terminator's singleton `groupsend`):
    /// `Off` routes input to the focused pane only, `Group` to the focused
    /// pane's group, `All` to every pane. Default `Off`. Cycled by
    /// `toggle_broadcast` (Ctrl+Shift+B); set directly by the
    /// BroadcastOff/Group/All actions.
    pub broadcast_scope: BroadcastScope,
    /// Latest known cell size in physical pixels, updated each frame by the
    /// pane view. Used to clamp keyboard split-resize to a minimum pane size.
    pub cell_w: f32,
    pub cell_h: f32,
    /// Latest pixels-per-point, used to convert point-space rects to physical
    /// pixels when clamping split sizes.
    pub ppp: f32,
}

impl TabManager {
    pub(crate) fn new(first_pane: Pane, alloc: Arc<AtomicU64>) -> Self {
        Self {
            tabs: vec![Tab::new(first_pane)],
            active_tab: 0,
            alloc,
            broadcast_scope: BroadcastScope::Off,
            cell_w: 0.0,
            cell_h: 0.0,
            ppp: 1.0,
        }
    }

    pub(crate) fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub(crate) fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub(crate) fn active_pane(&self) -> Option<&Pane> {
        let tab = self.active_tab();
        tab.panes.get(&tab.focused)
    }

    pub(crate) fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        let tab = self.active_tab_mut();
        let focused = tab.focused;
        tab.panes.get_mut(&focused)
    }

    pub(crate) fn total_alive_panes(&self) -> usize {
        self.tabs.iter().map(|t| t.panes.len()).sum()
    }

    fn spawn_pane(&mut self, id: PaneId, cols: usize, lines: usize, cwd: Option<&std::path::Path>, factory: &PaneFactory) -> Option<Pane> {
        // The active-profile shorthand used by split / split_here / new_tab:
        // profile and command come from the factory the App built for the
        // parent / active profile.
        self.spawn_pane_with(id, cols, lines, cwd, factory.profile.clone(), factory.command.clone(), factory)
    }

    /// Spawn a pane with an explicit `profile` + `command`, used by
    /// `restore_layout` to honor a saved layout's per-leaf sidecar. The cell
    /// sizes, `term_config`, `pane_defaults`, and event proxy always come from
    /// `factory`; only the profile, command, and cwd vary per leaf (the params
    /// the §8 contract carries through the sidecar).
    fn spawn_pane_with(
        &mut self,
        id: PaneId,
        cols: usize,
        lines: usize,
        cwd: Option<&std::path::Path>,
        profile: ProfileName,
        command: SpawnCommand,
        factory: &PaneFactory,
    ) -> Option<Pane> {
        match Pane::spawn(
            id,
            cols,
            lines,
            factory.cell_w,
            factory.cell_h,
            factory.term_config.clone(),
            factory.pane_defaults,
            Some(factory.event_loop_proxy.clone()),
            cwd,
            profile,
            command,
        ) {
            Ok(pane) => Some(pane),
            Err(e) => {
                eprintln!("failed to spawn pane: {e}");
                None
            }
        }
    }

    pub(crate) fn split(&mut self, dir: Direction, factory: &PaneFactory) {
        let focused = self.tabs[self.active_tab].focused;
        let cwd = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.cwd());
        // Inherit-on-split: the new pane stays in the focused parent's group so
        // a split stays within the same broadcast group (mirrors Terminator).
        // Its profile + resolved command are inherited too, but via `factory`:
        // the App builds this factory for the parent pane's profile, and
        // `spawn_pane` stamps `factory.profile`/`factory.command` onto the new
        // pane — so profile inheritance needs no per-pane copy here.
        let group = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.group.clone());
        let new_id = alloc_pane_id(&self.alloc);
        let Some(mut pane) = self.spawn_pane(new_id, 80, 24, cwd.as_deref(), factory) else { return };
        pane.group = group;
        let active = self.active_tab_mut();
        active.panes.insert(new_id, pane);
        if !active.layout.split_leaf(active.focused, new_id, dir) {
            eprintln!("split: focused leaf {} not found in layout", active.focused);
        }
        active.focused = new_id;
    }

    pub(crate) fn split_here(&mut self, last_pane_rect: Option<egui::Rect>, factory: &PaneFactory) {
        let dir = match last_pane_rect {
            Some(r) if r.width() >= r.height() => Direction::Vertical,
            _ => Direction::Horizontal,
        };
        let focused = self.tabs[self.active_tab].focused;
        let cwd = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.cwd());
        // Inherit-on-split: the new pane stays in the focused parent's group so
        // a split stays within the same broadcast group (mirrors Terminator).
        // Its profile + resolved command are inherited via `factory` (built by
        // the App for the parent pane's profile and stamped in `spawn_pane`), so
        // profile inheritance needs no per-pane copy here.
        let group = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.group.clone());
        let new_id = alloc_pane_id(&self.alloc);
        let Some(mut pane) = self.spawn_pane(new_id, 80, 24, cwd.as_deref(), factory) else { return };
        pane.group = group;
        let active = self.active_tab_mut();
        active.panes.insert(new_id, pane);
        if !active.layout.split_leaf(active.focused, new_id, dir) {
            eprintln!("split_here: focused leaf {} not found in layout", active.focused);
        }
        active.focused = new_id;
    }

    pub(crate) fn close_focused(&mut self, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        let target = self.active_tab_mut().focused;

        // Capture the pre-removal leaf order and which side of its parent split
        // `target` sits on, so we can focus the pane that actually expands into
        // the freed rect. `remove_leaf` collapses the parent onto the sibling
        // subtree: a left-child leaf is replaced by its right sibling (the
        // in-order successor), a right-child leaf by its left sibling (the
        // in-order predecessor). We mirror that promotion below.
        let mut before = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut before);
        let target_idx = before.iter().position(|&id| id == target);
        let removed_right_child = leaf_is_right_child(&self.active_tab().layout, target);

        let result = self.active_tab_mut().layout.remove_leaf(target);
        self.active_tab_mut().panes.remove(&target);
        if self.active_tab_mut().zoomed == Some(target) {
            self.active_tab_mut().zoomed = None;
        }
        if matches!(result, crate::layout::RemoveResult::NotFound) {
            return;
        }

        if matches!(result, crate::layout::RemoveResult::Empty) {
            self.close_tab(self.active_tab, egui_ctx, dialogs);
            return;
        }

        let mut leaves = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut leaves);

        // A right-child leaf is replaced by its left sibling's rightmost leaf
        // (the in-order predecessor); otherwise the successor-first heuristic is
        // correct (left child) and a safe default. The predecessor is guaranteed
        // to survive here, so the fallback is only defensive.
        let new_focus = if removed_right_child == Some(true) {
            target_idx
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| before.get(i).copied())
                .filter(|id| leaves.contains(id))
                .or_else(|| pick_new_focus(&before, target_idx, &leaves))
        } else {
            pick_new_focus(&before, target_idx, &leaves)
        };

        if let Some(id) = new_focus {
            self.active_tab_mut().focused = id;
            if let Some(p) = self.active_tab_mut().panes.get(&id) {
                p.send_focus_event(true);
            }
        }
    }

    pub(crate) fn close_tab(&mut self, idx: usize, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        if idx >= self.tabs.len() {
            return;
        }
        // Closing the active tab is the only case that moves focus onto a
        // different pane; closing a background tab leaves the viewed pane
        // focused (its index may shift, but its focus state does not change).
        let closing_active = idx == self.active_tab;
        let (search_pane, close_search) = adjust_search_pane(dialogs.search_pane, idx);
        dialogs.search_pane = search_pane;
        if close_search {
            dialogs.search_open = false;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.active_tab = adjust_active_tab(self.active_tab, idx, self.tabs.len());
        // Only emit focus-in when the active tab actually changed; re-sending it
        // to the still-focused active pane would be an unpaired \x1b[I.
        if closing_active {
            let focused = self.tabs[self.active_tab].focused;
            if let Some(p) = self.tabs[self.active_tab].panes.get(&focused) {
                p.send_focus_event(true);
            }
        }
    }

    pub(crate) fn close_pane(&mut self, tab_idx: usize, pane_id: PaneId, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        if tab_idx >= self.tabs.len() {
            return;
        }
        let is_active_tab = tab_idx == self.active_tab;
        let tab = &mut self.tabs[tab_idx];
        let result = tab.layout.remove_leaf(pane_id);
        tab.panes.remove(&pane_id);
        if tab.zoomed == Some(pane_id) {
            tab.zoomed = None;
        }
        if matches!(result, crate::layout::RemoveResult::NotFound) {
            return;
        }
        if matches!(result, crate::layout::RemoveResult::Empty) {
            self.close_tab(tab_idx, egui_ctx, dialogs);
            return;
        }
        let tab = &mut self.tabs[tab_idx];
        let mut leaves = Vec::new();
        tab.layout.leaves_in_order(&mut leaves);
        // Only reassign focus if the current focus died. Emit the focus-in only
        // when this is the active tab; a background tab's pane must not receive a
        // spurious \x1b[I (it gets one when the user actually switches to it).
        if !leaves.contains(&tab.focused) {
            if let Some(&first) = leaves.first() {
                tab.focused = first;
                if is_active_tab {
                    if let Some(p) = tab.panes.get(&first) {
                        p.send_focus_event(true);
                    }
                }
            }
        }
    }

    /// Spawn a new tab and insert it at `index` (clamped to `0..=len`), then
    /// make it active. Inherits the cwd of the currently-focused pane and emits
    /// the paired focus-out (old active pane) / focus-in (new tab's pane) so the
    /// PTY focus state stays consistent. Pass `self.tabs.len()` as `index` to
    /// append.
    pub(crate) fn new_tab_at(&mut self, index: usize, factory: &PaneFactory) {
        let focused = self.tabs[self.active_tab].focused;
        let cwd = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.cwd());
        let new_id = alloc_pane_id(&self.alloc);
        let Some(pane) = self.spawn_pane(new_id, INITIAL_COLS as usize, INITIAL_LINES as usize, cwd.as_deref(), factory) else { return };
        if let Some(p) = self.tabs[self.active_tab].panes.get(&focused) {
            p.send_focus_event(false);
        }
        let index = index.min(self.tabs.len());
        self.tabs.insert(index, Tab::new(pane));
        self.active_tab = index;
        let focused = self.tabs[self.active_tab].focused;
        if let Some(p) = self.tabs[self.active_tab].panes.get(&focused) {
            p.send_focus_event(true);
        }
    }

    /// Move the tab at `from` to `to` (remove+insert), keeping the active tab
    /// pointed at the same logical tab via `remap_active_after_reorder`. No-op
    /// for out-of-range indices or a same-slot move. Focus is untouched: the
    /// active tab's focused pane is unchanged, only its index shifts.
    pub(crate) fn reorder_tab(&mut self, from: usize, to: usize) {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = remap_active_after_reorder(self.active_tab, from, to);
    }

    /// Set (or clear, with `None`) the custom title of the tab at `index`.
    /// Out-of-range indices are ignored. Shared sink for the inline tab-bar
    /// rename and the modal "Set title" dialog (both write `custom_title`).
    pub(crate) fn set_tab_title(&mut self, index: usize, title: Option<String>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.custom_title = title;
        }
    }

    pub(crate) fn switch_tab(&mut self, step: i32) {
        let n = self.tabs.len();
        if n == 0 {
            return;
        }
        let old = self.active_tab;
        let new = wrap_index(old, step, n);
        if old == new {
            return;
        }
        let old_focused = self.tabs[old].focused;
        if let Some(pane) = self.tabs[old].panes.get(&old_focused) {
            pane.send_focus_event(false);
        }
        self.active_tab = new;
        let focused = self.tabs[new].focused;
        if let Some(pane) = self.tabs[new].panes.get(&focused) {
            pane.send_focus_event(true);
        }
    }

    pub(crate) fn switch_tab_direct(&mut self, tab_number: u8) {
        let idx = (tab_number as usize).saturating_sub(1);
        if idx < self.tabs.len() && idx != self.active_tab {
            let old_focused = self.tabs[self.active_tab].focused;
            if let Some(pane) = self.tabs[self.active_tab].panes.get(&old_focused) {
                pane.send_focus_event(false);
            }
            self.active_tab = idx;
            let focused = self.tabs[idx].focused;
            if let Some(pane) = self.tabs[idx].panes.get(&focused) {
                pane.send_focus_event(true);
            }
        }
    }

    pub(crate) fn move_tab(&mut self, delta: i32) {
        let n = self.tabs.len();
        if n < 2 { return; }
        let from = self.active_tab;
        let to = wrap_index(from, delta, n);
        self.tabs.swap(from, to);
        self.active_tab = to;
    }

    /// Move focus to `new_id` within `tab_idx`, emitting paired focus-out/in
    /// events when focus actually changes. No-op if it is already focused or
    /// the pane is absent.
    pub(crate) fn set_focused_pane(&mut self, tab_idx: usize, new_id: PaneId) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else { return };
        let present = tab.panes.contains_key(&new_id);
        let Some((old, new)) = focus_transition(tab.focused, new_id, present) else { return };
        if let Some(p) = tab.panes.get(&old) {
            p.send_focus_event(false);
        }
        tab.focused = new;
        if let Some(p) = tab.panes.get(&new) {
            p.send_focus_event(true);
        }
    }

    pub(crate) fn cycle_focus(&mut self, step: i32) {
        let tab = self.active_tab_mut();
        let mut leaves = Vec::new();
        tab.layout.leaves_in_order(&mut leaves);
        if leaves.is_empty() {
            return;
        }
        let idx = leaves.iter().position(|&id| id == tab.focused).unwrap_or(0);
        let new_idx = wrap_index(idx, step, leaves.len());
        let old_focused = tab.focused;
        let new_focused = leaves[new_idx];
        if old_focused != new_focused {
            if let Some(p) = tab.panes.get(&old_focused) {
                p.send_focus_event(false);
            }
            tab.focused = new_focused;
            if let Some(p) = tab.panes.get(&new_focused) {
                p.send_focus_event(true);
            }
        }
    }

    pub(crate) fn focus_direction(&mut self, dir: FocusDir, last_pane_rect: Option<egui::Rect>) {
        let tab = &self.tabs[self.active_tab];
        let root_rect = last_pane_rect.unwrap_or(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        ));
        let mut rects = Vec::new();
        tab.layout.walk_rects(root_rect, PANE_GAP, &mut rects);

        let focused_rect = rects.iter().find(|(id, _)| *id == tab.focused).map(|(_, r)| *r);
        let Some(fr) = focused_rect else { return };

        let best = rects.iter()
            .filter(|(id, _)| *id != tab.focused)
            .filter(|(_, r)| match dir {
                FocusDir::Up => r.center().y < fr.center().y,
                FocusDir::Down => r.center().y > fr.center().y,
                FocusDir::Left => r.center().x < fr.center().x,
                FocusDir::Right => r.center().x > fr.center().x,
            })
            .min_by_key(|(_, r)| {
                let dx = r.center().x - fr.center().x;
                let dy = r.center().y - fr.center().y;
                ((dx * dx + dy * dy) * 1000.0) as i64
            });

        if let Some((id, _)) = best {
            let old_focused = self.tabs[self.active_tab].focused;
            let new_focused = *id;
            if old_focused != new_focused {
                if let Some(p) = self.tabs[self.active_tab].panes.get(&old_focused) {
                    p.send_focus_event(false);
                }
                self.tabs[self.active_tab].focused = new_focused;
                if let Some(p) = self.tabs[self.active_tab].panes.get(&new_focused) {
                    p.send_focus_event(true);
                }
            }
        }
    }

    pub(crate) fn toggle_zoom(&mut self) {
        let tab = self.active_tab_mut();
        if tab.zoomed.is_some() {
            tab.zoomed = None;
        } else {
            tab.zoomed = Some(tab.focused);
        }
    }

    pub(crate) fn toggle_broadcast(&mut self) {
        self.broadcast_scope = next_broadcast_scope(self.broadcast_scope);
    }

    pub(crate) fn resize_split(&mut self, step: i32, horizontal: bool, last_pane_rect: Option<egui::Rect>) {
        let tab = &self.tabs[self.active_tab];
        let focused = tab.focused;
        let root_rect = last_pane_rect.unwrap_or(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        ));
        let mut dividers = Vec::new();
        tab.layout.walk_dividers(root_rect, PANE_GAP, &mut dividers);

        let mut rects = Vec::new();
        tab.layout.walk_rects(root_rect, PANE_GAP, &mut rects);
        let focused_rect = rects.iter().find(|(id, _)| *id == focused).map(|(_, r)| *r);
        let Some(fr) = focused_rect else { return };

        let target_dir = if horizontal {
            layout::Direction::Horizontal
        } else {
            layout::Direction::Vertical
        };

        let best = dividers
            .iter()
            .filter(|d| d.dir == target_dir)
            .min_by_key(|d| {
                let dist = if horizontal {
                    ((d.rect.center().y - fr.center().y).abs() * 100.0) as i32
                } else {
                    ((d.rect.center().x - fr.center().x).abs() * 100.0) as i32
                };
                dist
            });

        if let Some(div) = best {
            let path = div.path.clone();
            let parent = div.parent_rect;
            let current_ratio = match target_dir {
                layout::Direction::Horizontal => {
                    (div.rect.center().y - parent.top()) / parent.height()
                }
                layout::Direction::Vertical => {
                    (div.rect.center().x - parent.left()) / parent.width()
                }
            };
            // Mirror the drag-site clamp: keep both children >= 3 cols (vertical
            // split) / 1 row (horizontal split). parent rect is in points; cell
            // sizes are physical px, so scale by ppp.
            let (container_px, cell_px, min_cells) = match target_dir {
                layout::Direction::Vertical => (parent.width() * self.ppp, self.cell_w, 3.0),
                layout::Direction::Horizontal => (parent.height() * self.ppp, self.cell_h, 1.0),
            };
            // Move the divider by whole character cells: snap the boundary to the
            // grid, then re-apply the min-cells clamp so both children stay legal.
            let snapped =
                layout::ratio_after_cell_step(current_ratio, step, container_px, cell_px, min_cells);
            self.tabs[self.active_tab].layout.set_ratio_min_cells(
                &path,
                snapped,
                container_px,
                cell_px,
                min_cells,
            );
        }
    }

    pub(crate) fn reset_focused_terminal(&mut self, clear: bool) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let tab = &mut self.tabs[self.active_tab];
        if let Some(pane) = tab.panes.get_mut(&tab.focused) {
            {
                let mut term = pane.terminal.lock();
                // reset_state already blanks both grids and wipes scrollback
                // (Grid::reset calls clear_history), so the clear variant's
                // extra calls are belt-and-braces against semantics changes.
                Handler::reset_state(&mut *term);
                if clear {
                    Handler::clear_screen(&mut *term, ClearMode::All);
                    Handler::clear_screen(&mut *term, ClearMode::Saved);
                }
            }
            pane.dirty.store(true, std::sync::atomic::Ordering::Release);
            pane.cached = None;
        }
    }

    pub(crate) fn copy_selection(&self, egui_ctx: &egui::Context, smart_copy: bool) {
        let tab = &self.tabs[self.active_tab];
        let Some(pane) = tab.panes.get(&tab.focused) else {
            return;
        };
        match pane.selection_text() {
            Some(text) if !text.is_empty() => {
                egui_ctx.copy_text(text);
            }
            _ => {
                if smart_copy {
                    pane.send_bytes(vec![0x03]);
                }
            }
        }
    }

    pub(crate) fn paste_from_clipboard(&self) {
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("clipboard read failed: {e}");
                return;
            }
        };
        // Route through the shared input-target selector under the current
        // broadcast scope: `Off` -> focused pane only, `Group` -> the focused
        // pane's group, `All` -> every non-read_only pane. read_only panes never
        // receive pasted input.
        for pane in self.active_tab().select_input_targets(self.broadcast_scope) {
            pane.send_paste(&text);
        }
    }

    pub(crate) fn paste_primary(&self, pane_id: PaneId) {
        let text = match crate::pane_ui::read_primary() {
            Some(t) => t,
            None => return,
        };
        // Middle-click targets one specific pane (not a broadcast fan-out), but
        // a read_only pane must still reject pasted input.
        if let Some(pane) = self.tabs[self.active_tab].panes.get(&pane_id) {
            if !pane.read_only {
                pane.send_paste(&text);
            }
        }
    }

    pub(crate) fn paste_text_into_pane(&self, pane_id: PaneId, text: &str) {
        // External drop targets one specific pane (the one under the cursor); a
        // read_only pane must still reject the dropped text.
        if let Some(pane) = self.tabs[self.active_tab].panes.get(&pane_id) {
            if !pane.read_only {
                pane.send_paste(text);
            }
        }
    }

    pub(crate) fn toggle_search(&mut self, dialogs: &mut DialogState) {
        if dialogs.search_open {
            dialogs.search_open = false;
            return;
        }
        dialogs.search_open = true;
        dialogs.search_focus_pending = true;
        let tab_idx = self.active_tab;
        let focused = self.tabs[tab_idx].focused;
        dialogs.search_pane = Some((tab_idx, focused));
    }

    pub(crate) fn run_search(&self, backward: bool, dialogs: &DialogState) {
        let Some((tab_idx, pane_id)) = dialogs.search_pane else { return };
        if tab_idx >= self.tabs.len() {
            return;
        }
        let Some(pane) = self.tabs[tab_idx].panes.get(&pane_id) else { return };
        pane.search(&dialogs.search_query, backward);
    }

    pub(crate) fn save_layout(&self, name: String, user_config: &mut Config) {
        // Capture the structure (template) plus the per-leaf sidecar (profile /
        // cwd / custom command, in leaf order) so a restore can respawn each leaf
        // with its own profile and working directory (§8 sidecar contract).
        let tab = self.active_tab();
        let template = tab.layout.to_template();
        let terminals = tab.terminal_metas();
        let layouts = &mut user_config.layouts;
        if let Some(existing) = layouts.iter_mut().find(|l| l.name == name) {
            existing.template = template;
            existing.terminals = terminals;
        } else {
            layouts.push(SavedLayout { name, template, terminals });
        }
        let _ = user_config.save();
    }

    pub(crate) fn restore_layout(&mut self, saved: &SavedLayout, factory: &PaneFactory, cfg: &Config) {
        let needed = saved.template.leaf_count();
        let terminals = &saved.terminals;
        let mut existing_ids: Vec<PaneId> = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut existing_ids);

        let (mut ids, spawn_count, removed) = reconcile_ids(existing_ids, needed);

        // Drop the panes the template no longer needs.
        for id in removed {
            self.active_tab_mut().panes.remove(&id);
        }

        // Spawn the shortfall, tracking only what we create so a partial failure
        // can be rolled back without touching the pre-existing layout/panes.
        // `build`/`leaves_in_order` share the same left-to-right traversal, so
        // the j-th spawned pane lands at leaf `kept_len + j` and consumes the
        // sidecar entry at that index — restoring its saved cwd/profile/command.
        // Reused (kept) panes fill the lower leaves and keep their running
        // shells, so the sidecar applies only to panes we actually spawn (§8).
        // An empty/short sidecar falls back to the active-profile factory, giving
        // old (pre-sidecar) layouts an identical restore.
        let kept_len = ids.len();
        let mut spawned_ids: Vec<PaneId> = Vec::new();
        for j in 0..spawn_count {
            let meta = terminals.get(kept_len + j);
            let cwd = meta.and_then(|m| m.cwd.clone());
            let profile = leaf_profile_name(meta, &factory.profile);
            let command = leaf_command(meta, cfg, &factory.command);
            let new_id = alloc_pane_id(&self.alloc);
            let Some(pane) = self.spawn_pane_with(new_id, 80, 24, cwd.as_deref(), profile, command, factory) else {
                // Couldn't produce enough panes for the template. Abort the
                // restore rather than building a layout with orphan Leaf(0)
                // leaves: roll back the panes we spawned and keep the existing
                // layout, focus, and zoom untouched.
                for id in spawned_ids {
                    self.active_tab_mut().panes.remove(&id);
                }
                return;
            };
            self.active_tab_mut().panes.insert(new_id, pane);
            spawned_ids.push(new_id);
        }
        ids.extend(spawned_ids);

        let mut id_iter = ids.into_iter();
        self.active_tab_mut().layout = saved.template.build(&mut id_iter);

        let mut leaves = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut leaves);
        let tab = self.active_tab_mut();
        if !leaves.contains(&tab.focused) {
            if let Some(&first) = leaves.first() {
                // L5: pair the handoff — focus-out the trimmed pane (a no-op
                // once it has been removed) and focus-in its replacement — so a
                // restore never silently reassigns focus.
                let old = tab.focused;
                if let Some(p) = tab.panes.get(&old) {
                    p.send_focus_event(false);
                }
                tab.focused = first;
                if let Some(p) = tab.panes.get(&first) {
                    p.send_focus_event(true);
                }
            }
        }
        tab.zoomed = None;
    }

    pub(crate) fn reap_exited(
        &mut self,
        exit_action: crate::config::ExitAction,
        egui_ctx: &egui::Context,
        dialogs: &mut DialogState,
        factory: &PaneFactory,
    ) {
        use crate::config::ExitAction;
        use std::sync::atomic::Ordering;
        loop {
            let mut found = None;
            'outer: for (t, tab) in self.tabs.iter().enumerate() {
                for (&id, pane) in &tab.panes {
                    if pane.exited.load(Ordering::Acquire) {
                        found = Some((t, id));
                        break 'outer;
                    }
                }
            }
            match found {
                Some((t, id)) => {
                    match exit_action {
                        ExitAction::Close => self.close_pane(t, id, egui_ctx, dialogs),
                        ExitAction::Hold => {
                            if let Some(tab) = self.tabs.get_mut(t) {
                                if let Some(pane) = tab.panes.get(&id) {
                                    // Consume the transient Exit event so the
                                    // reaper loop doesn't re-find this pane, and
                                    // mark it persistently dead: its PTY channel
                                    // is closed, so input must be a no-op rather
                                    // than silently dropped. The pane stays in
                                    // the layout as a held husk until closed.
                                    pane.exited.store(false, Ordering::Release);
                                    pane.mark_dead();
                                }
                            }
                        }
                        ExitAction::Restart => {
                            let tab = match self.tabs.get_mut(t) {
                                Some(tab) => tab,
                                None => break,
                            };
                            let pane = match tab.panes.get_mut(&id) {
                                Some(p) => p,
                                None => break,
                            };
                            if pane.respawn(
                                factory.cell_w,
                                factory.cell_h,
                                factory.term_config.clone(),
                                Some(factory.event_loop_proxy.clone()),
                            ).is_err() {
                                self.close_pane(t, id, egui_ctx, dialogs);
                            }
                        }
                    }
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_input_targets_broadcast_excludes_read_only() {
        // `All` (scope=All) reproduces the old broadcast=true routing that
        // `select_input_targets` relies on: a read_only pane is never an input
        // target while a non-read_only pane always is — independent of which
        // pane is focused. The decision now lives in the shared
        // `groups::pane_receives_input` predicate (group args unused under All).
        use BroadcastScope::All;
        assert!(
            groups::pane_receives_input(All, false, false, None, None),
            "non-read_only unfocused pane is a broadcast target"
        );
        assert!(
            groups::pane_receives_input(All, false, true, None, None),
            "non-read_only focused pane is a broadcast target"
        );
        assert!(
            !groups::pane_receives_input(All, true, false, None, None),
            "read_only pane is excluded under broadcast"
        );
        assert!(
            !groups::pane_receives_input(All, true, true, None, None),
            "read_only focused pane is excluded under broadcast"
        );
    }

    #[test]
    fn select_input_targets_focused_only_empty_when_read_only() {
        // `Off` (scope=Off) reproduces the old broadcast=false routing: only the
        // focused pane is a target, and only when it is not read_only, so a
        // read_only focused pane yields no targets.
        use BroadcastScope::Off;
        assert!(
            !groups::pane_receives_input(Off, true, true, None, None),
            "read_only focused pane yields no target"
        );
        assert!(
            groups::pane_receives_input(Off, false, true, None, None),
            "non-read_only focused pane is the sole target"
        );
        assert!(
            !groups::pane_receives_input(Off, false, false, None, None),
            "an unfocused pane is not a target when not broadcasting"
        );
    }

    #[test]
    fn toggle_broadcast_cycles_off_group_all() {
        // USER DECISION Q1: Ctrl+Shift+B cycles Off -> Group -> All -> Off, so
        // three steps from Off return to Off.
        use BroadcastScope::{All, Group, Off};
        assert_eq!(next_broadcast_scope(Off), Group);
        assert_eq!(next_broadcast_scope(Group), All);
        assert_eq!(next_broadcast_scope(All), Off);
    }

    #[test]
    fn pick_new_focus_prefers_successor_then_predecessor_then_first() {
        // Successor survives -> pick it.
        assert_eq!(pick_new_focus(&[1, 2, 3], Some(0), &[2, 3]), Some(2));
        // No successor (target was last) -> fall back to the predecessor.
        assert_eq!(pick_new_focus(&[1, 2, 3], Some(2), &[1, 2]), Some(2));
        // No target index -> first surviving leaf.
        assert_eq!(pick_new_focus(&[1, 2, 3], None, &[1, 2]), Some(1));
        // Nothing survives -> None.
        assert_eq!(pick_new_focus(&[1, 2, 3], Some(1), &[]), None);
        // Documents M5: this pure heuristic always prefers the successor, so it
        // returns 3 here. The fix lives in `close_focused`, which consults
        // `leaf_is_right_child` and overrides this with the predecessor when the
        // closed leaf was a right child; the helper itself stays successor-first.
        assert_eq!(pick_new_focus(&[1, 2, 3], Some(1), &[1, 3]), Some(3));
    }

    #[test]
    fn focus_transition_guards_noop_and_pairs() {
        // Already focused -> no events.
        assert_eq!(focus_transition(5, 5, true), None);
        // Target absent -> no events.
        assert_eq!(focus_transition(5, 6, false), None);
        // Distinct, present target -> exactly one focus-out/in pair.
        assert_eq!(focus_transition(5, 6, true), Some((5, 6)));
    }

    #[test]
    fn wrap_index_wraps_and_noops() {
        assert_eq!(wrap_index(2, 1, 3), 0); // forward wrap past the end
        assert_eq!(wrap_index(0, -1, 3), 2); // backward wrap past the start
        assert_eq!(wrap_index(0, 5, 3), 2); // large positive step
        assert_eq!(wrap_index(0, -4, 3), 2); // large negative step
        assert_eq!(wrap_index(0, 1, 1), 0); // single element stays put
        assert_eq!(wrap_index(2, 3, 3), 2); // full cycle returns to the start
    }

    #[test]
    fn adjust_active_tab_fixup() {
        // Active index ran off the shortened list -> clamp to the new last tab.
        assert_eq!(adjust_active_tab(2, 0, 2), 1);
        // A lower-indexed tab closed (active still in range) -> shift down one.
        assert_eq!(adjust_active_tab(2, 0, 3), 1);
        // Active was the last tab and was itself closed -> clamp to the new end.
        assert_eq!(adjust_active_tab(3, 3, 3), 2);
        // A higher-indexed tab closed -> active index unchanged.
        assert_eq!(adjust_active_tab(1, 2, 3), 1);
    }

    #[test]
    fn adjust_search_pane_reindexes_and_clears() {
        // The search pane's own tab was closed -> drop the anchor and close search.
        assert_eq!(adjust_search_pane(Some((1, 7)), 1), (None, true));
        // A lower-indexed tab closed -> shift the anchor's tab index down.
        assert_eq!(adjust_search_pane(Some((2, 7)), 0), (Some((1, 7)), false));
        // A higher-indexed tab closed -> anchor unchanged.
        assert_eq!(adjust_search_pane(Some((0, 7)), 2), (Some((0, 7)), false));
        // No active search -> nothing to adjust.
        assert_eq!(adjust_search_pane(None, 1), (None, false));
    }

    #[test]
    fn reconcile_ids_under_over_and_equal() {
        // Fewer existing panes than needed: keep them all, report how many to
        // spawn, remove nothing.
        assert_eq!(reconcile_ids(vec![1, 2], 4), (vec![1, 2], 2, vec![]));
        // More existing panes than needed: keep the first `needed`, spawn none,
        // report the tail as removed in original order.
        assert_eq!(reconcile_ids(vec![1, 2, 3, 4], 2), (vec![1, 2], 0, vec![3, 4]));
        // Exactly enough: keep all, spawn none, remove none.
        assert_eq!(reconcile_ids(vec![1, 2, 3], 3), (vec![1, 2, 3], 0, vec![]));
    }

    #[test]
    fn remap_active_after_reorder_follows_moved_tab_and_neighbors() {
        // [A,B,C,D] move from=0 to=2 -> [B,C,A,D].
        assert_eq!(remap_active_after_reorder(0, 0, 2), 2); // moved tab A follows to `to`
        assert_eq!(remap_active_after_reorder(1, 0, 2), 0); // B shifts down past the removal
        assert_eq!(remap_active_after_reorder(2, 0, 2), 1); // C shifts down past the removal
        assert_eq!(remap_active_after_reorder(3, 0, 2), 3); // D lands past the insertion, unchanged
        // [A,B,C,D] move from=3 to=1 -> [A,D,B,C].
        assert_eq!(remap_active_after_reorder(3, 3, 1), 1); // moved tab D follows to `to`
        assert_eq!(remap_active_after_reorder(0, 3, 1), 0); // A before the insertion, unchanged
        assert_eq!(remap_active_after_reorder(1, 3, 1), 2); // B shifts up past the insertion
        assert_eq!(remap_active_after_reorder(2, 3, 1), 3); // C shifts up past the insertion
        // A same-slot move (from == to) leaves a neighbor put.
        assert_eq!(remap_active_after_reorder(2, 1, 1), 2);
    }

    #[test]
    fn tab_insert_index_after_current_vs_append() {
        // Append (default): always the end, independent of the active index.
        assert_eq!(tab_insert_index(0, false, 3), 3);
        assert_eq!(tab_insert_index(2, false, 3), 3);
        // After-current: directly after the active tab.
        assert_eq!(tab_insert_index(0, true, 3), 1);
        assert_eq!(tab_insert_index(1, true, 3), 2);
        // After-current with the active tab last clamps to the end (== append).
        assert_eq!(tab_insert_index(2, true, 3), 3);
    }

    fn login() -> SpawnCommand {
        SpawnCommand { program: None, args: vec!["--login".into()] }
    }

    fn custom(cmd: &str) -> SpawnCommand {
        SpawnCommand { program: None, args: vec!["-c".into(), cmd.into()] }
    }

    #[test]
    fn custom_command_arg_extracts_only_dash_c() {
        // Interactive shells (login or bare) record no command on save.
        assert_eq!(custom_command_arg(&login()), None);
        assert_eq!(custom_command_arg(&SpawnCommand { program: None, args: vec![] }), None);
        // A `-c <cmd>` invocation yields the custom command string.
        assert_eq!(custom_command_arg(&custom("htop")), Some("htop".to_string()));
        // A lone `-c` (malformed, wrong arity) is not treated as custom.
        assert_eq!(
            custom_command_arg(&SpawnCommand { program: None, args: vec!["-c".into()] }),
            None
        );
    }

    #[test]
    fn terminal_meta_records_profile_cwd_and_only_custom_command() {
        // Interactive shell: profile + cwd saved, command omitted.
        let m = terminal_meta("dark", Some(PathBuf::from("/tmp/x")), &login());
        assert_eq!(m.profile.as_deref(), Some("dark"));
        assert_eq!(m.cwd, Some(PathBuf::from("/tmp/x")));
        assert_eq!(m.command, None);
        // Custom command: recorded; absent cwd stays None.
        let m = terminal_meta("default", None, &custom("vim"));
        assert_eq!(m.profile.as_deref(), Some("default"));
        assert_eq!(m.cwd, None);
        assert_eq!(m.command.as_deref(), Some("vim"));
    }

    #[test]
    fn leaf_profile_name_prefers_sidecar_then_fallback() {
        let with = TerminalMeta { profile: Some("dark".into()), ..Default::default() };
        assert_eq!(leaf_profile_name(Some(&with), "active"), "dark");
        // Missing entry (short/empty sidecar) -> fallback (back-compat).
        assert_eq!(leaf_profile_name(None, "active"), "active");
        // Entry present but no profile -> fallback.
        assert_eq!(leaf_profile_name(Some(&TerminalMeta::default()), "active"), "active");
    }

    #[test]
    fn custom_command_text_filters_blank() {
        assert_eq!(custom_command_text(&TerminalMeta::default()), None);
        assert_eq!(
            custom_command_text(&TerminalMeta { command: Some(String::new()), ..Default::default() }),
            None
        );
        assert_eq!(
            custom_command_text(&TerminalMeta { command: Some("   ".into()), ..Default::default() }),
            None
        );
        assert_eq!(
            custom_command_text(&TerminalMeta { command: Some("htop".into()), ..Default::default() }),
            Some("htop".to_string())
        );
    }

    #[test]
    fn leaf_command_custom_then_profile_then_fallback() {
        use crate::config::Profile;
        // Sentinel fallback (the active-profile factory's command) — distinct
        // from any resolved command so the assertions prove which branch ran.
        let fallback = SpawnCommand { program: None, args: vec!["FALLBACK".into()] };
        // A non-login profile in the config: resolving it gives empty args, so a
        // result of `[]` proves we honored the SAVED profile, not the fallback.
        let mut plain = Profile::new_named("plain");
        plain.login_shell = false;
        let cfg = Config {
            active_profile: "plain".into(),
            profiles: vec![plain],
            ..Config::default()
        };

        // Missing entry -> fallback (back-compat path for old layouts).
        assert_eq!(leaf_command(None, &cfg, &fallback), fallback);
        // Saved custom command wins, run through `-c`.
        let m = TerminalMeta { command: Some("htop".into()), ..Default::default() };
        assert_eq!(leaf_command(Some(&m), &cfg, &fallback), custom("htop"));
        // Saved profile, no custom command -> resolve that profile's command
        // (login_shell=false => no args), NOT the fallback.
        let m = TerminalMeta { profile: Some("plain".into()), ..Default::default() };
        assert_eq!(
            leaf_command(Some(&m), &cfg, &fallback),
            SpawnCommand { program: None, args: vec![] }
        );
        // Entry with neither profile nor command -> fallback.
        assert_eq!(leaf_command(Some(&TerminalMeta::default()), &cfg, &fallback), fallback);
    }

    #[test]
    fn alloc_pane_id_strictly_increasing_and_unique() {
        // The shared allocator hands out strictly increasing, unique ids: each
        // call returns the prior counter value and advances it. Starting at 1
        // reproduces today's first-pane id (1) and the old `next_pane_id`
        // sequence (2, 3, 4, ...) exactly.
        let alloc = Arc::new(AtomicU64::new(1));
        let ids: Vec<PaneId> = (0..5).map(|_| alloc_pane_id(&alloc)).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5], "ids start at 1 and step by 1");
        assert!(
            ids.windows(2).all(|w| w[1] > w[0]),
            "ids are strictly increasing, hence unique"
        );
    }

    #[test]
    fn shared_alloc_never_collides_across_two_managers() {
        // Two TabManagers that clone the SAME Arc<AtomicU64> (the detach case: a
        // tab moves into a second window whose manager shares the one
        // process-wide source) must never hand out a colliding PaneId. Each clone
        // stands in for a manager's `alloc` field; we drive the shared source
        // directly because building a real TabManager needs a live PTY-backed
        // Pane. Allocations are interleaved between the two clones.
        let shared = Arc::new(AtomicU64::new(1));
        let mgr_a = Arc::clone(&shared);
        let mgr_b = Arc::clone(&shared);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            assert!(seen.insert(alloc_pane_id(&mgr_a)), "manager A produced a duplicate id");
            assert!(seen.insert(alloc_pane_id(&mgr_b)), "manager B produced a duplicate id");
        }
        assert_eq!(seen.len(), 100, "100 interleaved allocations are all unique");
    }
}
