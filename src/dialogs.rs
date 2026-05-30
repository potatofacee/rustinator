use crate::pane::PaneId;

pub(crate) struct DialogState {
    pub close_dialog_open: bool,
    pub confirmed_close: bool,
    pub title_dialog_open: bool,
    pub title_dialog_buf: String,
    pub layout_save_dialog: bool,
    pub layout_save_buf: String,
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
            search_open: false,
            search_query: String::new(),
            search_pane: None,
            search_focus_pending: false,
        }
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
}
