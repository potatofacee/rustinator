use crate::config::{self, Config};
use crate::presets;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum PrefsSection {
    Global,
    Profiles,
    Keybindings,
}

pub(crate) enum PrefsResult {
    None,
    Applied(Config),
    Cancelled,
}

pub(crate) struct PrefsState {
    pub open: bool,
    pub draft: Config,
    pub status: Option<String>,
    section: PrefsSection,
    selected_profile: usize,
}

impl PrefsState {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            // The draft is always overwritten by open() before the panel is
            // shown, so avoid an unnecessary disk read here.
            draft: Config::default(),
            status: None,
            section: PrefsSection::Global,
            selected_profile: 0,
        }
    }

    pub(crate) fn open(&mut self, current_config: &Config) {
        self.draft = current_config.clone();
        self.status = None;
        self.open = true;
        self.selected_profile = self
            .draft
            .profiles
            .iter()
            .position(|p| p.name == self.draft.active_profile)
            .unwrap_or(0);
    }

    pub(crate) fn draw(&mut self, ui: &mut egui::Ui) -> PrefsResult {
        let bottom_h = 40.0;
        let (top_rect, bottom_rect) = {
            let full = ui.available_rect_before_wrap();
            let split_y = (full.bottom() - bottom_h).max(full.top());
            (
                egui::Rect::from_min_max(full.min, egui::pos2(full.right(), split_y)),
                egui::Rect::from_min_max(egui::pos2(full.left(), split_y), full.max),
            )
        };

        let mut save_now = false;
        let mut cancel_now = false;

        let mut top_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(top_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        let section = &mut self.section;
        let draft = &mut self.draft;
        let selected_profile = &mut self.selected_profile;
        egui::ScrollArea::vertical().show(&mut top_ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(120.0);
                    ui.heading("Prefs");
                    ui.add_space(6.0);
                    for (label, s) in [
                        ("Global", PrefsSection::Global),
                        ("Profiles", PrefsSection::Profiles),
                        ("Keybindings", PrefsSection::Keybindings),
                    ] {
                        let selected = *section == s;
                        if ui.selectable_label(selected, label).clicked() {
                            *section = s;
                        }
                    }
                });

                ui.separator();

                ui.vertical(|ui| match *section {
                    PrefsSection::Global => draw_prefs_global(ui, draft),
                    PrefsSection::Profiles => draw_prefs_profiles(
                        ui,
                        draft,
                        selected_profile,
                    ),
                    PrefsSection::Keybindings => draw_prefs_keybindings(ui),
                });
            });
        });

        let mut bottom_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bottom_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        bottom_ui.separator();
        bottom_ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                save_now = true;
            }
            if ui.button("Cancel").clicked() {
                cancel_now = true;
            }
            if let Some(status) = &self.status {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(status).color(egui::Color32::LIGHT_YELLOW),
                );
            }
        });

        if save_now {
            let config = self.draft.clone();
            return PrefsResult::Applied(config);
        }
        if cancel_now {
            self.open = false;
            return PrefsResult::Cancelled;
        }

        PrefsResult::None
    }
}

fn draw_prefs_global(ui: &mut egui::Ui, cfg: &mut Config) {
    ui.heading("Global");
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Active profile");
        let active = cfg.active_profile.clone();
        egui::ComboBox::from_id_salt("active_profile")
            .selected_text(&active)
            .show_ui(ui, |ui| {
                let names: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
                for name in names {
                    if ui
                        .selectable_label(name == active, name.as_str())
                        .clicked()
                    {
                        cfg.active_profile = name;
                    }
                }
            });
    });
    ui.checkbox(
        &mut cfg.global.confirm_on_close,
        "Confirm before closing a window with multiple panes",
    );

    ui.add_space(16.0);
    ui.heading("Hotkey Window");
    ui.add_space(6.0);
    ui.checkbox(&mut cfg.hotkey_window.enabled, "Enable hotkey window");
    if cfg.hotkey_window.enabled {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Hotkey");
            ui.text_edit_singleline(&mut cfg.hotkey_window.hotkey);
        });
        ui.horizontal(|ui| {
            ui.label("Height (% of screen)");
            ui.add(egui::Slider::new(&mut cfg.hotkey_window.height_percent, 10..=100).suffix("%"));
        });
        ui.checkbox(&mut cfg.hotkey_window.hide_on_focus_loss, "Hide when focus is lost");
        ui.checkbox(&mut cfg.hotkey_window.always_on_top, "Always on top");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("macOS: grant Accessibility permission if prompted.")
                .small()
                .weak(),
        );
    }
}

fn draw_prefs_profiles(
    ui: &mut egui::Ui,
    cfg: &mut Config,
    selected: &mut usize,
) {
    ui.heading("Profiles");
    ui.add_space(6.0);

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_min_width(140.0);
            let names: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
            for (i, name) in names.iter().enumerate() {
                if ui.selectable_label(i == *selected, name).clicked() {
                    *selected = i;
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.small_button("+").on_hover_text("Add profile").clicked() {
                    let mut new_name = "New Profile".to_string();
                    let mut n = 1;
                    while cfg.profiles.iter().any(|p| p.name == new_name) {
                        n += 1;
                        new_name = format!("New Profile {n}");
                    }
                    cfg.profiles.push(crate::config::Profile::new_named(&new_name));
                    *selected = cfg.profiles.len() - 1;
                }
                let can_delete = cfg.profiles.len() > 1;
                if ui
                    .add_enabled(can_delete, egui::Button::new("\u{2013}").small())
                    .on_hover_text("Delete selected profile")
                    .clicked()
                {
                    let removed = cfg.profiles.remove(*selected);
                    if cfg.active_profile == removed.name {
                        cfg.active_profile = cfg.profiles[0].name.clone();
                    }
                    if *selected >= cfg.profiles.len() {
                        *selected = cfg.profiles.len() - 1;
                    }
                }
            });
        });

        ui.separator();

        ui.vertical(|ui| {
            if *selected >= cfg.profiles.len() {
                *selected = 0;
            }
            let old_name = cfg.profiles[*selected].name.clone();
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut cfg.profiles[*selected].name);
            });
            // Keep active_profile in sync when the active profile is renamed,
            // otherwise active() would silently fall through to profiles[0].
            let new_name = cfg.profiles[*selected].name.clone();
            if new_name != old_name && cfg.active_profile == old_name {
                cfg.active_profile = new_name;
            }

            let profile = &mut cfg.profiles[*selected];

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Font").strong());
            ui.horizontal(|ui| {
                ui.label("Family");
                ui.text_edit_singleline(&mut profile.font.family);
            });
            ui.horizontal(|ui| {
                ui.label("Size");
                ui.add(
                    egui::DragValue::new(&mut profile.font.size)
                        .range(6.0..=48.0)
                        .speed(0.1)
                        .suffix(" pt"),
                );
            });
            ui.label(
                egui::RichText::new("Font changes apply on Save.")
                    .small()
                    .weak(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Colors").strong());
            let current = presets::match_preset(
                &profile.colors.foreground,
                &profile.colors.background,
                &profile.colors.cursor,
            )
            .unwrap_or(presets::CUSTOM);
            ui.horizontal(|ui| {
                ui.label("Preset");
                egui::ComboBox::from_id_salt(("preset_combo", *selected))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        // "Custom" is a state indicator, not a selectable
                        // option: it has no preset colors to apply.
                        ui.label(presets::CUSTOM);
                        for preset in presets::PRESETS {
                            if ui
                                .selectable_label(current == preset.name, preset.name)
                                .clicked()
                            {
                                profile.colors.foreground = preset.foreground.to_string();
                                profile.colors.background = preset.background.to_string();
                                profile.colors.cursor = preset.cursor.to_string();
                            }
                        }
                    });
            });
            hex_color_row(ui, "Foreground", &mut profile.colors.foreground);
            hex_color_row(ui, "Background", &mut profile.colors.background);
            hex_color_row(ui, "Cursor", &mut profile.colors.cursor);

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Scrolling").strong());
            ui.checkbox(&mut profile.scrollback.infinite, "Infinite scrollback");
            if !profile.scrollback.infinite {
                ui.horizontal(|ui| {
                    ui.label("History (lines)");
                    ui.add(
                        egui::DragValue::new(&mut profile.scrollback.history)
                            .range(100..=1_000_000)
                            .speed(100.0),
                    );
                });
            }

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Transparency").strong());
            ui.horizontal(|ui| {
                ui.label("Opacity");
                ui.add(
                    egui::Slider::new(&mut profile.transparency.opacity, 0.3..=1.0)
                        .fixed_decimals(2),
                );
            });
            ui.label(
                egui::RichText::new(
                    "Requires a running compositor (e.g. picom, mutter, kwin). \
                     Has no effect if your window manager does not support \
                     composited transparency.",
                )
                .small()
                .weak(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Cursor").strong());
            ui.checkbox(&mut profile.cursor_blink, "Cursor blink");

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Clipboard").strong());
            ui.checkbox(&mut profile.copy_on_selection, "Copy on selection");
            ui.checkbox(&mut profile.smart_copy, "Smart copy (Ctrl+Shift+C sends Ctrl+C when no selection)");

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Scroll behavior").strong());
            ui.checkbox(&mut profile.scroll_on_output, "Scroll on output");
            ui.checkbox(&mut profile.scroll_on_keystroke, "Scroll on keystroke");

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Shell").strong());
            ui.horizontal(|ui| {
                ui.label("When shell exits");
                egui::ComboBox::from_id_salt("exit_action")
                    .selected_text(match profile.exit_action {
                        config::ExitAction::Close => "Close pane",
                        config::ExitAction::Hold => "Hold open",
                        config::ExitAction::Restart => "Restart shell",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut profile.exit_action, config::ExitAction::Close, "Close pane");
                        ui.selectable_value(&mut profile.exit_action, config::ExitAction::Hold, "Hold open");
                        ui.selectable_value(&mut profile.exit_action, config::ExitAction::Restart, "Restart shell");
                    });
            });

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Selection").strong());
            ui.horizontal(|ui| {
                ui.label("Word characters");
                ui.text_edit_singleline(&mut profile.word_chars);
            });
        });
    });
}

fn draw_prefs_keybindings(ui: &mut egui::Ui) {
    ui.heading("Keybindings");
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Key remapping is not editable yet.")
            .small()
            .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("keybindings").striped(true).show(ui, |ui| {
        for (combo, action) in [
            ("Ctrl+Shift+E", "Split vertically"),
            ("Ctrl+Shift+O", "Split horizontally"),
            ("Ctrl+Shift+W", "Close pane"),
            ("Ctrl+Shift+T", "New tab"),
            ("Ctrl+Shift+C / Ctrl+Shift+V", "Copy / Paste"),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "Cycle panes forward / backward"),
            ("Alt+Arrow", "Focus adjacent pane"),
            ("Ctrl+PageUp / Ctrl+PageDown", "Previous / next tab"),
            ("Ctrl+,", "Open Preferences"),
            ("Middle-click", "Paste primary selection"),
        ] {
            ui.label(combo);
            ui.label(action);
            ui.end_row();
        }
    });
}

fn hex_color_row(ui: &mut egui::Ui, label: &str, hex: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut rgb = config::parse_hex(hex).unwrap_or([0, 0, 0]);
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *hex = config::format_hex(rgb);
        }
        ui.add(egui::TextEdit::singleline(hex).desired_width(90.0));
    });
}
