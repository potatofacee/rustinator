//! Grouping + broadcast: the focused owner of all group/broadcast LOGIC.
//!
//! Mirrors Terminator: group identity is a per-`Pane` NAME string (the name IS
//! the identity, `terminal.group`) and broadcast scope is a single global 3-way
//! value (`groupsend`). There is no numeric id and no registry — the live set of
//! group names is DERIVED on demand by scanning panes (replacing Terminator's
//! `group_hoover` GC).
//!
//! Everything here is either a pure predicate or a tiny orchestration free-fn so
//! input routing and the titlebar share ONE source of truth and can never
//! disagree. `tabs` depends on `groups` (scope + predicate) and `groups` depends
//! on `tabs::Tab` — a benign intra-crate module cycle Rust permits.

use std::collections::HashSet;

use crate::pane::PaneId;
use crate::tabs::{Tab, TabManager};

/// Global broadcast scope (mirrors Terminator's singleton `groupsend`). `Off`
/// reproduces the old `broadcast=false` routing (focused-only); `All` reproduces
/// `broadcast=true` (every non-read_only pane); `Group` targets the focused
/// pane's group siblings.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum BroadcastScope {
    #[default]
    Off,
    Group,
    All,
}

/// Per-pane broadcast classification for the titlebar dot + border. The focused
/// pane transmits (carrying the active scope); a non-focused pane either
/// receives input or does not.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Indicator {
    Transmit(BroadcastScope),
    ReceiveOn,
    ReceiveOff,
}

/// Shared receive core for a NON-focused pane. `All` => everyone; `Group` =>
/// only a pane that shares the focused pane's (existing) group — an ungrouped
/// pane never implicitly groups, so both being `None` is NOT a match; `Off` =>
/// nobody. Single source of truth for both input routing and the indicator.
fn is_receiver(scope: BroadcastScope, this_group: Option<&str>, focused_group: Option<&str>) -> bool {
    match scope {
        BroadcastScope::All => true,
        BroadcastScope::Group => this_group.is_some() && this_group == focused_group,
        BroadcastScope::Off => false,
    }
}

/// Whether a pane should receive user input (keystrokes/pastes/insert-number).
/// read_only panes never are — terminal RESPONSES bypass this and write to the
/// PTY directly via `send_bytes`, so they stay ungated. Generalizes the old
/// `pane_is_input_target`: `Off` == old `broadcast=false`, `All` == old
/// `broadcast=true`.
pub fn pane_receives_input(
    scope: BroadcastScope,
    read_only: bool,
    is_focused: bool,
    this_group: Option<&str>,
    focused_group: Option<&str>,
) -> bool {
    !read_only && (is_focused || is_receiver(scope, this_group, focused_group))
}

/// Visual classification for a pane's titlebar/border: the focused pane
/// transmits the active scope; everyone else is ReceiveOn/Off via `is_receiver`.
/// Mirrors `titlebar.py:122-183`.
pub fn indicator(
    scope: BroadcastScope,
    is_focused: bool,
    this_group: Option<&str>,
    focused_group: Option<&str>,
) -> Indicator {
    if is_focused {
        Indicator::Transmit(scope)
    } else if is_receiver(scope, this_group, focused_group) {
        Indicator::ReceiveOn
    } else {
        Indicator::ReceiveOff
    }
}

/// A stable, visually-distinct color for a group NAME, used for both the pane
/// border and the titlebar label so they agree. Group identity IS the name
/// string, so the color must depend only on the name and reproduce across runs:
/// the name is folded with FNV-1a (a fixed, toolchain-independent hash, unlike
/// `DefaultHasher`) into an index over a curated palette. The palette is
/// hand-picked to be mutually distinguishable, readable as an outline on a dark
/// terminal background, and clear of the default focus-border periwinkle so a
/// grouped pane never reads as merely focused.
pub fn group_color(name: &str) -> egui::Color32 {
    let [r, g, b] = GROUP_PALETTE[group_color_index(name)];
    egui::Color32::from_rgb(r, g, b)
}

/// Curated categorical palette (a full red->magenta hue sweep, medium-bright)
/// indexed by `group_color_index`. Stored as raw RGB triples so it stays plain,
/// inspectable, duplicate-free (by test) data independent of egui.
const GROUP_PALETTE: [[u8; 3]; 10] = [
    [0xE0, 0x6C, 0x6C], // red / coral
    [0xE0, 0x93, 0x42], // orange
    [0xD7, 0xC2, 0x4C], // gold
    [0x9F, 0xC5, 0x4E], // lime
    [0x5C, 0xC0, 0x6E], // green
    [0x3F, 0xC1, 0xA4], // teal
    [0x4C, 0xB4, 0xD6], // cyan
    [0x46, 0x8C, 0xF5], // azure
    [0xB2, 0x7C, 0xE6], // purple
    [0xE0, 0x72, 0xC4], // magenta / pink
];

/// Fold a group name into a palette index with 32-bit FNV-1a: deterministic and
/// stable across runs and toolchains; the trailing `% len` keeps it in range.
fn group_color_index(name: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for &byte in name.as_bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash % GROUP_PALETTE.len() as u32) as usize
}

/// The next free `"Tab {page}"` group name, bumping `page` until one is unused.
/// Mirrors Terminator's `group_tab` dedup while-loop (`window.py:879-894`).
pub fn next_tab_group_name(existing: &HashSet<String>, mut page: usize) -> String {
    loop {
        let name = format!("Tab {page}");
        if !existing.contains(&name) {
            return name;
        }
        page += 1;
    }
}

/// The text fed to a pane by insert-number. `padded` zero-pads to the digit
/// width of `total` (the global pane count); otherwise the bare 1-based index.
/// Mirrors `do_enumerate` (`terminator.py:576-590`) — no trailing newline.
pub fn enumerate_text(index_1based: usize, total: usize, padded: bool) -> String {
    if padded {
        let width = total.to_string().len();
        format!("{index_1based:0width$}")
    } else {
        index_1based.to_string()
    }
}

/// Group every pane in every tab under the name "All" (`group_all`,
/// `window.py:842-848`).
pub fn group_all(tabs: &mut [Tab]) {
    for tab in tabs {
        for pane in tab.panes.values_mut() {
            pane.group = Some("All".to_string());
        }
    }
}

/// Clear the group of every pane in every tab (`ungroup_all`,
/// `window.py:856-857`).
pub fn ungroup_all(tabs: &mut [Tab]) {
    for tab in tabs {
        for pane in tab.panes.values_mut() {
            pane.group = None;
        }
    }
}

/// Group the active tab's panes under a fresh `"Tab N"` name, deduped against
/// the names already live anywhere (`group_tab`, `window.py:879-894`).
pub fn group_active_tab(tabs: &mut [Tab], active: usize) {
    let existing: HashSet<String> = tabs
        .iter()
        .flat_map(|tab| tab.panes.values())
        .filter_map(|pane| pane.group.clone())
        .collect();
    let name = next_tab_group_name(&existing, active);
    if let Some(tab) = tabs.get_mut(active) {
        for pane in tab.panes.values_mut() {
            pane.group = Some(name.clone());
        }
    }
}

/// Set a single pane's group to `name`, mirroring Terminator's per-terminal
/// `create_group`/`set_group` (`terminal.py:625-637`): the typed name IS the
/// identity, applied to the one terminal whose group button was used. Scans
/// every tab for the pane id; a no-op if the pane is gone.
pub fn set_pane_group(tabs: &mut [Tab], pane: PaneId, name: String) {
    for tab in tabs.iter_mut() {
        if let Some(p) = tab.panes.get_mut(&pane) {
            p.group = Some(name);
            return;
        }
    }
}

/// Clear the group of every pane in the active tab (`ungroup_tab`,
/// `window.py:903-910`).
pub fn ungroup_active_tab(tabs: &mut [Tab], active: usize) {
    if let Some(tab) = tabs.get_mut(active) {
        for pane in tab.panes.values_mut() {
            pane.group = None;
        }
    }
}

/// Insert each broadcast target's 1-based index (among ALL panes, tree order)
/// into that pane. Targets route through the same `select_input_targets` gate as
/// keystrokes — no bypass. Mirrors `do_enumerate` (`terminator.py:576-590`).
pub fn insert_index(tab_mgr: &TabManager, padded: bool) {
    let mut order: Vec<PaneId> = Vec::new();
    for tab in &tab_mgr.tabs {
        tab.layout.leaves_in_order(&mut order);
    }
    let total = order.len();
    for pane in tab_mgr.active_tab().select_input_targets(tab_mgr.broadcast_scope) {
        if let Some(pos) = order.iter().position(|&id| id == pane.id) {
            pane.send_bytes(enumerate_text(pos + 1, total, padded).into_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- relocated from tabs.rs: the read-only gate, now under Off/All --------

    #[test]
    fn select_input_targets_broadcast_excludes_read_only() {
        // All == old broadcast=true: a read_only pane is never an input target
        // while a non-read_only pane always is — independent of which pane is
        // focused (group args are irrelevant under All).
        use BroadcastScope::All;
        assert!(
            pane_receives_input(All, false, false, None, None),
            "non-read_only unfocused pane is a broadcast target"
        );
        assert!(
            pane_receives_input(All, false, true, None, None),
            "non-read_only focused pane is a broadcast target"
        );
        assert!(
            !pane_receives_input(All, true, false, None, None),
            "read_only pane is excluded under broadcast"
        );
        assert!(
            !pane_receives_input(All, true, true, None, None),
            "read_only focused pane is excluded under broadcast"
        );
    }

    #[test]
    fn select_input_targets_focused_only_empty_when_read_only() {
        // Off == old broadcast=false: only the focused pane is a target, and only
        // when it is not read_only, so a read_only focused pane yields no targets.
        use BroadcastScope::Off;
        assert!(
            !pane_receives_input(Off, true, true, None, None),
            "read_only focused pane yields no target"
        );
        assert!(
            pane_receives_input(Off, false, true, None, None),
            "non-read_only focused pane is the sole target"
        );
        assert!(
            !pane_receives_input(Off, false, false, None, None),
            "an unfocused pane is not a target when not broadcasting"
        );
    }

    // --- new: Group-scope routing --------------------------------------------

    #[test]
    fn group_scope_routes_to_focused_group_members() {
        // Focused pane is in group "A": a non-focused sibling sharing "A"
        // receives; a different group ("B") and an ungrouped pane do not.
        use BroadcastScope::Group;
        assert!(
            pane_receives_input(Group, false, false, Some("A"), Some("A")),
            "a non-focused member of the focused group receives"
        );
        assert!(
            !pane_receives_input(Group, false, false, Some("B"), Some("A")),
            "a pane in a different group does not receive"
        );
        assert!(
            !pane_receives_input(Group, false, false, None, Some("A")),
            "an ungrouped pane does not receive under a grouped focus"
        );
        assert!(
            pane_receives_input(Group, false, true, Some("A"), Some("A")),
            "the focused pane itself is always a target"
        );
        assert!(
            !pane_receives_input(Group, true, false, Some("A"), Some("A")),
            "a read_only group member is still excluded"
        );
    }

    #[test]
    fn group_scope_ungrouped_focus_targets_focused_only() {
        // Focused pane has no group: only it receives. Ungrouped panes never
        // implicitly group, so another None pane is NOT a target.
        use BroadcastScope::Group;
        assert!(
            pane_receives_input(Group, false, true, None, None),
            "ungrouped focused pane is the sole target"
        );
        assert!(
            !pane_receives_input(Group, false, false, None, None),
            "another ungrouped pane does not implicitly join the focus"
        );
    }

    // --- new: indicator classification (mirrors titlebar.py:122-183) ----------

    #[test]
    fn indicator_focused_transmits_active_scope() {
        for scope in [BroadcastScope::Off, BroadcastScope::Group, BroadcastScope::All] {
            assert_eq!(
                indicator(scope, true, None, None),
                Indicator::Transmit(scope),
                "the focused pane transmits the active scope"
            );
        }
    }

    #[test]
    fn indicator_classifies_non_focused_receive() {
        use BroadcastScope::{All, Group, Off};
        // Group: member receives, non-member does not.
        assert_eq!(
            indicator(Group, false, Some("A"), Some("A")),
            Indicator::ReceiveOn
        );
        assert_eq!(
            indicator(Group, false, Some("B"), Some("A")),
            Indicator::ReceiveOff
        );
        // All: everyone receives; Off: nobody receives.
        assert_eq!(indicator(All, false, None, None), Indicator::ReceiveOn);
        assert_eq!(
            indicator(Off, false, Some("A"), Some("A")),
            Indicator::ReceiveOff
        );
    }

    // --- new: next_tab_group_name dedup --------------------------------------

    #[test]
    fn next_tab_group_name_dedups() {
        let mut existing = HashSet::new();
        assert_eq!(next_tab_group_name(&existing, 0), "Tab 0");
        existing.insert("Tab 0".to_string());
        assert_eq!(next_tab_group_name(&existing, 0), "Tab 1");
        // Bumps past a contiguous run of taken names.
        existing.insert("Tab 1".to_string());
        existing.insert("Tab 2".to_string());
        assert_eq!(next_tab_group_name(&existing, 0), "Tab 3");
    }

    // --- new: enumerate_text formatting --------------------------------------

    #[test]
    fn enumerate_text_pads_to_total_width() {
        assert_eq!(enumerate_text(3, 12, false), "3");
        assert_eq!(enumerate_text(3, 12, true), "03");
        assert_eq!(enumerate_text(3, 5, true), "3");
        assert_eq!(enumerate_text(7, 100, true), "007");
    }

    // --- new: group_color palette mapping ------------------------------------

    #[test]
    fn group_color_is_deterministic() {
        // The name IS the identity, so a name always maps to the same color:
        // same-group panes match at a glance, across calls and runs.
        assert_eq!(group_color("work"), group_color("work"));
        assert_eq!(group_color("logs"), group_color("logs"));
        assert_eq!(group_color(""), group_color(""));
    }

    #[test]
    fn group_color_index_stays_in_range() {
        for name in ["", "a", "work", "logs", "a long group name with spaces"] {
            assert!(group_color_index(name) < GROUP_PALETTE.len());
        }
    }

    #[test]
    fn group_palette_has_no_duplicates() {
        let unique: HashSet<[u8; 3]> = GROUP_PALETTE.iter().copied().collect();
        assert_eq!(
            unique.len(),
            GROUP_PALETTE.len(),
            "palette colors must be mutually distinct"
        );
    }

    #[test]
    fn distinct_names_get_distinct_colors() {
        // Six names chosen to land on different palette slots -> different
        // colors, so different groups are told apart.
        let names = ["work", "logs", "db", "api", "dev", "queue"];
        let slots: HashSet<usize> = names.iter().map(|n| group_color_index(n)).collect();
        assert_eq!(slots.len(), names.len(), "these groups map to distinct slots");
        assert_ne!(group_color("work"), group_color("logs"));
        assert_ne!(group_color("db"), group_color("api"));
    }
}
