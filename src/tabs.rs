use std::collections::HashMap;

use alacritty_terminal::term::Config as TermConfig;
use egui;
use winit::event_loop::EventLoopProxy;

use crate::dialogs::DialogState;
use crate::keybindings::Action;
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

pub(crate) enum PaneAction {
    SplitHorizontal,
    SplitVertical,
    SplitAuto,
    Close,
    FocusNext,
    FocusPrev,
    NewTab,
    NextTab,
    PrevTab,
    OpenPrefs,
    Copy,
    Paste,
    ToggleZoom,
    ToggleBroadcast,
    ToggleSearch,
    ToggleReadOnly,
    SetTitle,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    CloseWindow,
    ToggleFullscreen,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    ResetTerminal,
    ResetClear,
    NewWindow,
    OpenTerminalHere,
    QuitHotkeyWindow,
    MoveTabLeft,
    MoveTabRight,
    SwitchToTab(u8),
    GoUp,
    GoDown,
    GoLeft,
    GoRight,
    GoNext,
    GoPrev,
    RotateCW,
    RotateCCW,
    ToggleScrollbar,
    HideWindow,
}

pub(crate) enum FocusDir {
    Up,
    Down,
    Left,
    Right,
}

pub(crate) fn action_to_pane_action(a: Action) -> PaneAction {
    match a {
        Action::SplitHorizontal => PaneAction::SplitHorizontal,
        Action::SplitVertical => PaneAction::SplitVertical,
        Action::ClosePane => PaneAction::Close,
        Action::NewTab => PaneAction::NewTab,
        Action::NextTab => PaneAction::NextTab,
        Action::PrevTab => PaneAction::PrevTab,
        Action::FocusNext => PaneAction::FocusNext,
        Action::FocusPrev => PaneAction::FocusPrev,
        Action::Copy => PaneAction::Copy,
        Action::Paste => PaneAction::Paste,
        Action::OpenPrefs => PaneAction::OpenPrefs,
        Action::ToggleZoom => PaneAction::ToggleZoom,
        Action::ToggleBroadcast => PaneAction::ToggleBroadcast,
        Action::ToggleSearch => PaneAction::ToggleSearch,
        Action::ZoomIn => PaneAction::ZoomIn,
        Action::ZoomOut => PaneAction::ZoomOut,
        Action::ZoomReset => PaneAction::ZoomReset,
        Action::CloseWindow => PaneAction::CloseWindow,
        Action::ToggleFullscreen => PaneAction::ToggleFullscreen,
        Action::ResizeLeft => PaneAction::ResizeLeft,
        Action::ResizeRight => PaneAction::ResizeRight,
        Action::ResizeUp => PaneAction::ResizeUp,
        Action::ResizeDown => PaneAction::ResizeDown,
        Action::ResetTerminal => PaneAction::ResetTerminal,
        Action::ResetClear => PaneAction::ResetClear,
        Action::NewWindow => PaneAction::NewWindow,
        Action::QuitHotkeyWindow => PaneAction::QuitHotkeyWindow,
        Action::MoveTabLeft => PaneAction::MoveTabLeft,
        Action::MoveTabRight => PaneAction::MoveTabRight,
        Action::SwitchToTab(n) => PaneAction::SwitchToTab(n),
        Action::GoUp => PaneAction::GoUp,
        Action::GoDown => PaneAction::GoDown,
        Action::GoLeft => PaneAction::GoLeft,
        Action::GoRight => PaneAction::GoRight,
        Action::GoNext => PaneAction::GoNext,
        Action::GoPrev => PaneAction::GoPrev,
        Action::RotateCW => PaneAction::RotateCW,
        Action::RotateCCW => PaneAction::RotateCCW,
        Action::SplitAuto => PaneAction::SplitAuto,
        Action::ToggleScrollbar => PaneAction::ToggleScrollbar,
        Action::HideWindow => PaneAction::HideWindow,
    }
}

pub(crate) struct PaneFactory {
    pub cell_w: f32,
    pub cell_h: f32,
    pub egui_ctx: egui::Context,
    pub term_config: TermConfig,
    pub pane_defaults: PaneDefaults,
    pub event_loop_proxy: EventLoopProxy<crate::window::UserEvent>,
}

pub(crate) struct TabManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub next_pane_id: PaneId,
}

impl TabManager {
    pub(crate) fn new(first_pane: Pane) -> Self {
        let id = first_pane.id;
        Self {
            tabs: vec![Tab::new(first_pane)],
            active_tab: 0,
            next_pane_id: id + 1,
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
            factory.egui_ctx.clone(),
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
        if let Some(first) = leaves.first() {
            self.active_tab_mut().focused = *first;
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
    }

    pub(crate) fn close_pane(&mut self, tab_idx: usize, pane_id: PaneId, egui_ctx: &egui::Context, dialogs: &mut DialogState) {
        if tab_idx >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[tab_idx];
        let result = tab.layout.remove_leaf(pane_id);
        tab.panes.remove(&pane_id);
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
        if let Some(first) = leaves.first() {
            if !leaves.contains(&tab.focused) {
                tab.focused = *first;
            }
        }
    }

    pub(crate) fn new_tab(&mut self, factory: &PaneFactory) {
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let Some(pane) = self.spawn_pane(new_id, INITIAL_COLS as usize, INITIAL_LINES as usize, None, factory) else { return };
        self.tabs.push(Tab::new(pane));
        self.active_tab = self.tabs.len() - 1;
    }

    pub(crate) fn switch_tab(&mut self, step: i32) {
        let n = self.tabs.len() as i32;
        if n == 0 {
            return;
        }
        let idx = self.active_tab as i32;
        self.active_tab = (((idx + step) % n + n) % n) as usize;
    }

    pub(crate) fn switch_tab_direct(&mut self, tab_number: u8) {
        let idx = (tab_number as usize).saturating_sub(1);
        if idx < self.tabs.len() {
            self.active_tab = idx;
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
        tab.focused = leaves[new_idx as usize];
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
            self.tabs[self.active_tab].focused = *id;
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
            self.tabs[self.active_tab]
                .layout
                .set_ratio(&path, current_ratio + delta);
        }
    }

    pub(crate) fn reset_focused_terminal(&self, clear: bool) {
        let tab = &self.tabs[self.active_tab];
        if let Some(pane) = tab.panes.get(&tab.focused) {
            if clear {
                pane.send_bytes(b"\x1b[2J\x1b[H".to_vec());
            }
            pane.send_bytes(b"\x1bc".to_vec());
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

        while existing_ids.len() < needed {
            let new_id = self.next_pane_id;
            self.next_pane_id += 1;
            let Some(pane) = self.spawn_pane(new_id, 80, 24, None, factory) else { break };
            self.active_tab_mut().panes.insert(new_id, pane);
            existing_ids.push(new_id);
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
            self.active_tab_mut().focused = leaves.first().copied().unwrap_or(1);
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
                                    pane.exited.store(false, Ordering::Release);
                                }
                            }
                            break;
                        }
                        ExitAction::Restart => {
                            let (cols, lines) = match self.tabs.get(t)
                                .and_then(|tab| tab.panes.get(&id))
                            {
                                Some(pane) => (pane.cols, pane.lines),
                                None => break,
                            };
                            let new_id = self.next_pane_id;
                            self.next_pane_id += 1;
                            if let Some(new_pane) = self.spawn_pane(new_id, cols, lines, None, factory) {
                                let tab = &mut self.tabs[t];
                                tab.layout.swap_leaves(id, new_id);
                                tab.panes.remove(&id);
                                tab.panes.insert(new_id, new_pane);
                                if tab.focused == id {
                                    tab.focused = new_id;
                                }
                            } else {
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
