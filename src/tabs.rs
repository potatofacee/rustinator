use std::collections::HashMap;

use alacritty_terminal::term::Config as TermConfig;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::dialogs::DialogState;
use crate::layout::{self, Direction, Node};
use crate::pane::{Pane, PaneDefaults, PaneId};

pub(crate) const INITIAL_COLS: u16 = 100;
pub(crate) const INITIAL_LINES: u16 = 32;
pub(crate) const PANE_GAP: f32 = 3.0;

pub(crate) struct Tab {
    pub panes: HashMap<PaneId, Pane>,
    pub layout: Node,
    pub focused: PaneId,
    pub zoomed: Option<PaneId>,
    pub broadcast: bool,
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
            broadcast: false,
            custom_title: None,
        }
    }
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
}

pub(crate) struct TabManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub next_pane_id: PaneId,
    /// Latest known cell size in physical pixels, updated each frame by the
    /// pane view. Used to clamp keyboard split-resize to a minimum pane size.
    pub cell_w: f32,
    pub cell_h: f32,
    /// Latest pixels-per-point, used to convert point-space rects to physical
    /// pixels when clamping split sizes.
    pub ppp: f32,
}

impl TabManager {
    pub(crate) fn new(first_pane: Pane) -> Self {
        let id = first_pane.id;
        Self {
            tabs: vec![Tab::new(first_pane)],
            active_tab: 0,
            next_pane_id: id + 1,
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

    #[allow(dead_code)]
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
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let Some(pane) = self.spawn_pane(new_id, 80, 24, cwd.as_deref(), factory) else { return };
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
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let Some(pane) = self.spawn_pane(new_id, 80, 24, cwd.as_deref(), factory) else { return };
        let active = self.active_tab_mut();
        active.panes.insert(new_id, pane);
        if !active.layout.split_leaf(active.focused, new_id, dir) {
            eprintln!("split_here: focused leaf {} not found in layout", active.focused);
        }
        active.focused = new_id;
    }

    pub(crate) fn close_focused(&mut self, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        let target = self.active_tab_mut().focused;

        // Capture the pre-removal leaf order so we can pick the neighbor that
        // now occupies the closed pane's region. When a leaf is removed its
        // parent split collapses to the sibling subtree, whose leaves are
        // contiguous with `target` in in-order traversal; the immediate
        // in-order neighbor is therefore the pane that takes over the space.
        // (No sibling-lookup helper exists on the layout, so we approximate
        // with the nearest in-order neighbor, falling back to the first leaf.)
        let mut before = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut before);
        let target_idx = before.iter().position(|&id| id == target);

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

        // Prefer the neighbor that followed `target` (the sibling that took its
        // space), then the one that preceded it, then fall back to the first
        // remaining leaf.
        let new_focus = target_idx
            .and_then(|i| before.get(i + 1).copied())
            .filter(|id| leaves.contains(id))
            .or_else(|| {
                target_idx
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| before.get(i).copied())
                    .filter(|id| leaves.contains(id))
            })
            .or_else(|| leaves.first().copied());

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
        if let Some((ti, _)) = dialogs.search_pane {
            if ti == idx {
                dialogs.search_pane = None;
                dialogs.search_open = false;
            } else if ti > idx {
                dialogs.search_pane = Some((ti - 1, dialogs.search_pane.unwrap().1));
            }
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if idx < self.active_tab {
            self.active_tab -= 1;
        }
        let focused = self.tabs[self.active_tab].focused;
        if let Some(p) = self.tabs[self.active_tab].panes.get(&focused) {
            p.send_focus_event(true);
        }
    }

    pub(crate) fn close_pane(&mut self, tab_idx: usize, pane_id: PaneId, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        if tab_idx >= self.tabs.len() {
            return;
        }
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
        // Only reassign focus if the current focus died; emit a focus-in to the
        // pane that takes over so it matches close_focused / tab-switch behavior.
        if !leaves.contains(&tab.focused) {
            if let Some(&first) = leaves.first() {
                tab.focused = first;
                if let Some(p) = tab.panes.get(&first) {
                    p.send_focus_event(true);
                }
            }
        }
    }

    pub(crate) fn new_tab(&mut self, factory: &PaneFactory) {
        let focused = self.tabs[self.active_tab].focused;
        let cwd = self.tabs[self.active_tab].panes.get(&focused)
            .and_then(|p| p.cwd());
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let Some(pane) = self.spawn_pane(new_id, INITIAL_COLS as usize, INITIAL_LINES as usize, cwd.as_deref(), factory) else { return };
        if let Some(p) = self.tabs[self.active_tab].panes.get(&self.tabs[self.active_tab].focused) {
            p.send_focus_event(false);
        }
        self.tabs.push(Tab::new(pane));
        self.active_tab = self.tabs.len() - 1;
        let focused = self.tabs[self.active_tab].focused;
        if let Some(p) = self.tabs[self.active_tab].panes.get(&focused) {
            p.send_focus_event(true);
        }
    }

    pub(crate) fn switch_tab(&mut self, step: i32) {
        let n = self.tabs.len() as i32;
        if n == 0 {
            return;
        }
        let old = self.active_tab;
        let idx = old as i32;
        let new = (((idx + step) % n + n) % n) as usize;
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
        let n = self.tabs.len() as i32;
        if n < 2 { return; }
        let from = self.active_tab as i32;
        let to = ((from + delta) % n + n) % n;
        self.tabs.swap(from as usize, to as usize);
        self.active_tab = to as usize;
    }

    /// Move focus to `new_id` within `tab_idx`, emitting paired focus-out/in
    /// events when focus actually changes. No-op if it is already focused or
    /// the pane is absent.
    pub(crate) fn set_focused_pane(&mut self, tab_idx: usize, new_id: PaneId) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else { return };
        let old = tab.focused;
        if old == new_id || !tab.panes.contains_key(&new_id) {
            return;
        }
        if let Some(p) = tab.panes.get(&old) {
            p.send_focus_event(false);
        }
        tab.focused = new_id;
        if let Some(p) = tab.panes.get(&new_id) {
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
        let idx = leaves.iter().position(|&id| id == tab.focused).unwrap_or(0) as i32;
        let n = leaves.len() as i32;
        let new_idx = ((idx + step) % n + n) % n;
        let old_focused = tab.focused;
        let new_focused = leaves[new_idx as usize];
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
        let tab = self.active_tab_mut();
        tab.broadcast = !tab.broadcast;
    }

    pub(crate) fn resize_split(&mut self, delta: f32, horizontal: bool, last_pane_rect: Option<egui::Rect>) {
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
            self.tabs[self.active_tab].layout.set_ratio_min_cells(
                &path,
                current_ratio + delta,
                container_px,
                cell_px,
                min_cells,
            );
        }
    }

    pub(crate) fn reset_focused_terminal(&self, clear: bool) {
        let tab = &self.tabs[self.active_tab];
        if let Some(pane) = tab.panes.get(&tab.focused) {
            pane.send_bytes(b"\x1bc".to_vec());
            if clear {
                pane.send_bytes(b"\x1b[2J\x1b[H".to_vec());
            }
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
        let tab = &self.tabs[self.active_tab];
        if let Some(pane) = tab.panes.get(&tab.focused) {
            pane.send_paste(&text);
        }
    }

    pub(crate) fn paste_primary(&self, pane_id: PaneId) {
        let text = match crate::pane_ui::read_primary() {
            Some(t) => t,
            None => return,
        };
        if let Some(pane) = self.tabs[self.active_tab].panes.get(&pane_id) {
            pane.send_paste(&text);
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

    pub(crate) fn save_layout(&self, name: String, user_config: &mut crate::config::Config) {
        use crate::config::SavedLayout;
        let template = self.active_tab().layout.to_template();
        let layouts = &mut user_config.layouts;
        if let Some(existing) = layouts.iter_mut().find(|l| l.name == name) {
            existing.template = template;
        } else {
            layouts.push(SavedLayout { name, template });
        }
        let _ = user_config.save();
    }

    pub(crate) fn restore_layout(&mut self, template: &crate::layout::LayoutTemplate, factory: &PaneFactory) {
        let needed = template.leaf_count();
        let tab = self.active_tab_mut();
        let mut existing_ids: Vec<PaneId> = Vec::new();
        tab.layout.leaves_in_order(&mut existing_ids);

        // Track only the panes we spawn here, so a partial failure can be
        // cleaned up without touching the pre-existing layout/panes.
        let mut spawned_ids: Vec<PaneId> = Vec::new();
        while existing_ids.len() < needed {
            let new_id = self.next_pane_id;
            self.next_pane_id += 1;
            let Some(pane) = self.spawn_pane(new_id, 80, 24, None, factory) else {
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
            existing_ids.push(new_id);
            spawned_ids.push(new_id);
        }
        while existing_ids.len() > needed {
            if let Some(extra) = existing_ids.pop() {
                self.active_tab_mut().panes.remove(&extra);
            }
        }

        let mut id_iter = existing_ids.into_iter();
        self.active_tab_mut().layout = template.build(&mut id_iter);
        let mut leaves = Vec::new();
        self.active_tab_mut().layout.leaves_in_order(&mut leaves);
        if !leaves.contains(&self.active_tab_mut().focused) {
            // Fall back to an actual live leaf only; never a hardcoded id.
            if let Some(first) = leaves.first().copied() {
                self.active_tab_mut().focused = first;
            }
        }
        self.active_tab_mut().zoomed = None;
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
