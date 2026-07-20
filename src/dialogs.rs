use crate::pane::PaneId;

pub(crate) struct DialogState {
    pub close_dialog_open: bool,
    pub confirmed_close: bool,
    pub title_dialog_open: bool,
    pub title_dialog_buf: String,
    pub layout_save_dialog: bool,
    pub layout_save_buf: String,
    pub layout_launcher_dialog: bool,
    pub layout_launcher_selected: usize,
    pub new_group_dialog: bool,
    pub new_group_buf: String,
    pub new_group_pane: Option<PaneId>,
    pub search_open: bool,
    pub search_query: String,
    pub search_pane: Option<(usize, PaneId)>,
    pub search_focus_pending: bool,
}

pub(crate) enum DialogAction {
    None,
    ConfirmClose,
    ConfirmCloseAndDisable,
    CancelClose,
    SetTitle(String),
    ClearTitle,
    SaveLayout(String),
    LaunchLayout(String),
    NewGroup(PaneId, String),
    SearchNext,
    SearchPrev,
    CloseSearch,
}

impl DialogState {
    pub(crate) fn new() -> Self {
        Self {
            close_dialog_open: false,
            confirmed_close: false,
            title_dialog_open: false,
            title_dialog_buf: String::new(),
            layout_save_dialog: false,
            layout_save_buf: String::new(),
            layout_launcher_dialog: false,
            layout_launcher_selected: 0,
            new_group_dialog: false,
            new_group_buf: String::new(),
            new_group_pane: None,
            search_open: false,
            search_query: String::new(),
            search_pane: None,
            search_focus_pending: false,
        }
    }

    /// True when a dialog with a focused text field is open, so the main
    /// window must not steal egui keyboard focus away from it. The layout
    /// launcher is a picker (no text field), so it is intentionally excluded.
    pub(crate) fn wants_text_input(&self) -> bool {
        self.search_open
            || self.title_dialog_open
            || self.layout_save_dialog
            || self.new_group_dialog
    }

    pub(crate) fn draw_search(&mut self, ui: &mut egui::Ui) -> DialogAction {
        if !self.search_open {
            return DialogAction::None;
        }
        let mut close = false;
        let mut find_next = false;
        let mut find_prev = false;
        egui::Panel::bottom("search_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find:");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .desired_width(280.0)
                        .hint_text("search scrollback..."),
                );
                if self.search_focus_pending {
                    edit.request_focus();
                    self.search_focus_pending = false;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if ui.input(|i| i.modifiers.shift) {
                        find_prev = true;
                    } else {
                        find_next = true;
                    }
                } else if edit.changed() {
                    // Search-as-you-type: re-run the search whenever the query
                    // text is edited. `changed()` only fires on actual edits, so
                    // we never re-search when the text is unchanged.
                    find_next = true;
                }
                if ui.button("Prev").clicked() {
                    find_prev = true;
                }
                if ui.button("Next").clicked() {
                    find_next = true;
                }
                if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
        if find_next {
            self.search_focus_pending = true;
            return DialogAction::SearchNext;
        }
        if find_prev {
            self.search_focus_pending = true;
            return DialogAction::SearchPrev;
        }
        if close {
            self.search_open = false;
            return DialogAction::CloseSearch;
        }
        DialogAction::None
    }

    pub(crate) fn draw_close_dialog(&mut self, ctx: &egui::Context, pane_count: usize) -> DialogAction {
        if !self.close_dialog_open {
            return DialogAction::None;
        }
        let mut keep_open = true;
        let mut cancel = false;
        let mut confirm = false;
        let mut dont_ask = false;
        egui::Window::new("Confirm close")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} panes open. Really close the window?",
                    pane_count
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Close").clicked() {
                        confirm = true;
                    }
                    if ui.button("Close and don't ask again").clicked() {
                        confirm = true;
                        dont_ask = true;
                    }
                });
            });
        if cancel || !keep_open {
            self.close_dialog_open = false;
            return DialogAction::CancelClose;
        }
        if confirm {
            self.confirmed_close = true;
            self.close_dialog_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            if dont_ask {
                return DialogAction::ConfirmCloseAndDisable;
            }
            return DialogAction::ConfirmClose;
        }
        DialogAction::None
    }

    pub(crate) fn draw_title_dialog(&mut self, ctx: &egui::Context) -> DialogAction {
        if !self.title_dialog_open {
            return DialogAction::None;
        }
        let mut keep_open = true;
        let mut apply = false;
        let mut clear = false;
        egui::Window::new("Set tab title")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Title:");
                    let resp = ui.text_edit_singleline(&mut self.title_dialog_buf);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply = true;
                    }
                    resp.request_focus();
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Clear").clicked() {
                        clear = true;
                    }
                });
            });
        if clear {
            self.title_dialog_open = false;
            return DialogAction::ClearTitle;
        }
        if !keep_open {
            self.title_dialog_open = false;
            return DialogAction::None;
        }
        if apply {
            let title = self.title_dialog_buf.trim().to_string();
            self.title_dialog_open = false;
            return DialogAction::SetTitle(title);
        }
        DialogAction::None
    }

    pub(crate) fn draw_layout_save_dialog(&mut self, ctx: &egui::Context) -> DialogAction {
        if !self.layout_save_dialog {
            return DialogAction::None;
        }
        let mut keep_open = true;
        let mut save = false;
        egui::Window::new("Save layout")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let resp = ui.text_edit_singleline(&mut self.layout_save_buf);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        save = true;
                    }
                    resp.request_focus();
                });
                ui.add_space(4.0);
                if ui.button("Save").clicked() {
                    save = true;
                }
            });
        if !keep_open {
            self.layout_save_dialog = false;
            return DialogAction::None;
        }
        if save && !self.layout_save_buf.trim().is_empty() {
            let name = self.layout_save_buf.trim().to_string();
            self.layout_save_dialog = false;
            return DialogAction::SaveLayout(name);
        }
        DialogAction::None
    }

    /// Layout Launcher (Alt+L): pick one of the saved layouts (`names`, in
    /// `user_config.layouts` order) and launch it. A modal picker, not a new
    /// window. Returns `LaunchLayout(name)` on Launch/double-click, else `None`.
    pub(crate) fn draw_layout_launcher(
        &mut self,
        ctx: &egui::Context,
        names: &[String],
    ) -> DialogAction {
        if !self.layout_launcher_dialog {
            return DialogAction::None;
        }
        if self.layout_launcher_selected >= names.len() {
            self.layout_launcher_selected = 0;
        }
        let mut keep_open = true;
        let mut launch = false;
        let mut cancel = false;
        egui::Window::new("Launch layout")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                if names.is_empty() {
                    ui.label("No saved layouts.");
                } else {
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            let selected = i == self.layout_launcher_selected;
                            let resp = ui.selectable_label(selected, name);
                            if resp.clicked() {
                                self.layout_launcher_selected = i;
                            }
                            if resp.double_clicked() {
                                self.layout_launcher_selected = i;
                                launch = true;
                            }
                        }
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!names.is_empty(), egui::Button::new("Launch"))
                        .clicked()
                    {
                        launch = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        if cancel || !keep_open {
            self.layout_launcher_dialog = false;
            self.layout_launcher_selected = 0;
            return DialogAction::None;
        }
        if launch {
            if let Some(name) = names.get(self.layout_launcher_selected) {
                let name = name.clone();
                self.layout_launcher_dialog = false;
                self.layout_launcher_selected = 0;
                return DialogAction::LaunchLayout(name);
            }
        }
        DialogAction::None
    }

    /// New Group dialog: name a group for `new_group_pane`. The opener prefills
    /// `new_group_buf` with a suggestion (e.g. the next "N" name) before setting
    /// `new_group_dialog`, mirroring how `draw_title_dialog` is seeded. Returns
    /// `NewGroup(pane, name)` on OK/Enter with a non-empty name, else `None`.
    pub(crate) fn draw_new_group(&mut self, ctx: &egui::Context) -> DialogAction {
        if !self.new_group_dialog {
            return DialogAction::None;
        }
        let mut keep_open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("New group")
            .open(&mut keep_open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Group:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.new_group_buf)
                            .hint_text("group name"),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply = true;
                    }
                    resp.request_focus();
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        if cancel || !keep_open {
            self.new_group_dialog = false;
            self.new_group_pane = None;
            return DialogAction::None;
        }
        if apply {
            let name = self.new_group_buf.trim().to_string();
            if !name.is_empty() {
                if let Some(pane) = self.new_group_pane.take() {
                    self.new_group_dialog = false;
                    return DialogAction::NewGroup(pane, name);
                }
            }
        }
        DialogAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_added_dialogs_closed() {
        let d = DialogState::new();
        assert!(!d.layout_launcher_dialog);
        assert_eq!(d.layout_launcher_selected, 0);
        assert!(!d.new_group_dialog);
        assert!(d.new_group_buf.is_empty());
        assert!(d.new_group_pane.is_none());
    }

    #[test]
    fn new_group_dialog_claims_text_input_focus() {
        let mut d = DialogState::new();
        assert!(!d.wants_text_input());
        d.new_group_dialog = true;
        assert!(
            d.wants_text_input(),
            "new-group has a text field, so it must hold egui keyboard focus"
        );
    }

    #[test]
    fn layout_launcher_does_not_claim_text_input_focus() {
        // The launcher is a picker, not a text field; claiming text focus would
        // wrongly suppress terminal keys for a dialog with no text widget.
        let mut d = DialogState::new();
        d.layout_launcher_dialog = true;
        assert!(!d.wants_text_input());
    }

    #[test]
    fn launcher_returns_none_while_closed() {
        let mut d = DialogState::new();
        let ctx = egui::Context::default();
        // Closed by default: must early-return without drawing.
        assert!(matches!(
            d.draw_layout_launcher(&ctx, &["dev".to_string()]),
            DialogAction::None
        ));
    }

    #[test]
    fn new_group_returns_none_while_closed() {
        let mut d = DialogState::new();
        let ctx = egui::Context::default();
        assert!(matches!(d.draw_new_group(&ctx), DialogAction::None));
    }
}
