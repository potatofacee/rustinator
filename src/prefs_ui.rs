use crate::config::{self, Config};
use crate::keybindings::{Action, BindingTable};
use crate::presets;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum PrefsSection {
    Global,
    Profiles,
    Keybindings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileTab {
    General,
    Colors,
    Behavior,
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
    profile_tab: ProfileTab,
    /// Palette slot selected for inline editing (0-7 normal, 8-15 bright).
    pub palette_sel: Option<usize>,
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
            profile_tab: ProfileTab::General,
            palette_sel: None,
        }
    }

    pub(crate) fn open(&mut self, current_config: &Config) {
        self.draft = current_config.clone();
        self.status = None;
        self.open = true;
        self.palette_sel = None;
        self.selected_profile = self
            .draft
            .profiles
            .iter()
            .position(|p| p.name == self.draft.active_profile)
            .unwrap_or(0);
    }

    pub(crate) fn draw(
        &mut self,
        ui: &mut egui::Ui,
        bindings: &BindingTable,
    ) -> PrefsResult {
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
        let palette_sel = &mut self.palette_sel;
        let profile_tab = &mut self.profile_tab;
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
                        palette_sel,
                        profile_tab,
                    ),
                    PrefsSection::Keybindings => draw_prefs_keybindings(ui, bindings),
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
    palette_sel: &mut Option<usize>,
    profile_tab: &mut ProfileTab,
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

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, t) in [
                    ("General", ProfileTab::General),
                    ("Colors", ProfileTab::Colors),
                    ("Behavior", ProfileTab::Behavior),
                ] {
                    if ui.selectable_label(*profile_tab == t, label).clicked() {
                        *profile_tab = t;
                    }
                }
            });
            ui.separator();

            let selected_idx = *selected;
            let profile = &mut cfg.profiles[selected_idx];
            match *profile_tab {
                ProfileTab::General => profile_tab_general(ui, profile),
                ProfileTab::Colors => {
                    profile_tab_colors(ui, profile, palette_sel, selected_idx)
                }
                ProfileTab::Behavior => profile_tab_behavior(ui, profile),
            }
        });
    });
}

fn profile_tab_general(ui: &mut egui::Ui, profile: &mut config::Profile) {
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
}

fn profile_tab_colors(
    ui: &mut egui::Ui,
    profile: &mut config::Profile,
    palette_sel: &mut Option<usize>,
    selected: usize,
) {
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
        egui::ComboBox::from_id_salt(("preset_combo", selected))
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

    ui.add_space(4.0);
    ui.label(egui::RichText::new("ANSI palette").strong());
    ansi_palette_grid(ui, &mut profile.colors.normal, &mut profile.colors.bright, palette_sel);
    ansi_palette_picker_window(ui, &mut profile.colors.normal, &mut profile.colors.bright, palette_sel);
    ansi_palette_preview(ui, profile);
    if ui.small_button("Reset palette").clicked() {
        profile.colors.normal = config::AnsiColors::default_normal();
        profile.colors.bright = config::AnsiColors::default_bright();
    }

    ui.add_space(4.0);
    hex_color_row(ui, "Focus border", &mut profile.colors.focus_border);
    hex_color_row(ui, "Broadcast border", &mut profile.colors.broadcast_border);

    let mut custom_selection = !profile.colors.selection_background.is_empty();
    if ui.checkbox(&mut custom_selection, "Custom selection colors").changed() {
        if custom_selection {
            profile.colors.selection_background = "#4060c0".into();
            profile.colors.selection_foreground = "#ffffff".into();
        } else {
            profile.colors.selection_background = String::new();
            profile.colors.selection_foreground = String::new();
        }
    }
    if custom_selection {
        hex_color_row(ui, "Selection background", &mut profile.colors.selection_background);
        hex_color_row(ui, "Selection foreground", &mut profile.colors.selection_foreground);
    }
}

fn profile_tab_behavior(ui: &mut egui::Ui, profile: &mut config::Profile) {
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
}

fn draw_prefs_keybindings(ui: &mut egui::Ui, bindings: &BindingTable) {
    ui.heading("Keybindings");
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Key remapping is editable in the config file; this table reflects \
             your current bindings.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("keybindings").striped(true).show(ui, |ui| {
        for (action, label) in [
            (Action::SplitVertical, "Split vertically"),
            (Action::SplitHorizontal, "Split horizontally"),
            (Action::SplitAuto, "Split auto"),
            (Action::ClosePane, "Close pane"),
            (Action::NewTab, "New tab"),
            (Action::CloseWindow, "Close window"),
            (Action::NewWindow, "New window"),
            (Action::Copy, "Copy"),
            (Action::Paste, "Paste"),
            (Action::ToggleSearch, "Search"),
            (Action::FocusNext, "Cycle panes forward"),
            (Action::FocusPrev, "Cycle panes backward"),
            (Action::GoLeft, "Focus pane left"),
            (Action::GoRight, "Focus pane right"),
            (Action::GoUp, "Focus pane above"),
            (Action::GoDown, "Focus pane below"),
            (Action::ResizeLeft, "Resize pane left"),
            (Action::ResizeRight, "Resize pane right"),
            (Action::ResizeUp, "Resize pane up"),
            (Action::ResizeDown, "Resize pane down"),
            (Action::NextTab, "Next tab"),
            (Action::PrevTab, "Previous tab"),
            (Action::MoveTabLeft, "Move tab left"),
            (Action::MoveTabRight, "Move tab right"),
            (Action::ToggleZoom, "Maximize pane"),
            (Action::ToggleBroadcast, "Toggle broadcast"),
            (Action::ToggleReadOnly, "Toggle read-only"),
            (Action::ToggleScrollbar, "Toggle scrollbar"),
            (Action::ToggleFullscreen, "Fullscreen"),
            (Action::ZoomIn, "Increase font size"),
            (Action::ZoomOut, "Decrease font size"),
            (Action::ZoomReset, "Reset font size"),
            (Action::ResetTerminal, "Reset terminal"),
            (Action::ResetClear, "Reset and clear"),
            (Action::SetTitle, "Set pane title"),
            (Action::OpenTerminalHere, "Open terminal here"),
            (Action::OpenPrefs, "Open Preferences"),
        ] {
            if let Some(combo) = bindings.combo_for(action) {
                ui.label(combo);
                ui.label(label);
                ui.end_row();
            }
        }
        if let Some(combo) = bindings.combo_for(Action::SwitchToTab(1)) {
            let prefix = combo
                .strip_suffix(|c: char| c.is_ascii_digit())
                .unwrap_or(&combo);
            ui.label(format!("{combo} \u{2026} {prefix}9"));
            ui.label("Switch to tab 1-9");
            ui.end_row();
        }
        ui.label("Middle-click");
        ui.label("Paste primary selection");
        ui.end_row();
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

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

fn ansi_palette_grid(
    ui: &mut egui::Ui,
    normal: &mut config::AnsiColors,
    bright: &mut config::AnsiColors,
    sel: &mut Option<usize>,
) {
    egui::Grid::new("ansi_palette_grid").spacing([6.0, 4.0]).show(ui, |ui| {
        ui.label("");
        for name in ANSI_NAMES {
            ui.label(egui::RichText::new(name).small());
        }
        ui.end_row();
        for (row, (row_label, colors)) in [("Normal", normal), ("Bright", bright)].into_iter().enumerate() {
            ui.label(egui::RichText::new(row_label).small());
            for col in 0..8 {
                let idx = row * 8 + col;
                let hex = palette_slot(colors, col);
                let [r, g, b] = config::parse_hex(hex).unwrap_or([0, 0, 0]);
                let selected = *sel == Some(idx);
                let stroke = if selected {
                    egui::Stroke::new(2.0, egui::Color32::WHITE)
                } else {
                    egui::Stroke::new(1.0, egui::Color32::from_gray(90))
                };
                let btn = egui::Button::new("")
                    .fill(egui::Color32::from_rgb(r, g, b))
                    .stroke(stroke)
                    .min_size(egui::vec2(22.0, 18.0));
                if ui.add(btn).on_hover_text(ANSI_NAMES[col]).clicked() {
                    *sel = if selected { None } else { Some(idx) };
                }
            }
            ui.end_row();
        }
    });
}

// One row's slot by column index (0 = black .. 7 = white).
fn palette_slot(colors: &mut config::AnsiColors, col: usize) -> &mut String {
    match col {
        0 => &mut colors.black,
        1 => &mut colors.red,
        2 => &mut colors.green,
        3 => &mut colors.yellow,
        4 => &mut colors.blue,
        5 => &mut colors.magenta,
        6 => &mut colors.cyan,
        7 => &mut colors.white,
        _ => unreachable!(),
    }
}

// Picker for the selected slot in a draggable egui window (title bar + close
// button), so it can be moved off the grid and the preview strip instead of
// covering them like color_edit_button's anchored popup did. Stable id keeps
// the dragged position when switching slots.
fn ansi_palette_picker_window(
    ui: &mut egui::Ui,
    normal: &mut config::AnsiColors,
    bright: &mut config::AnsiColors,
    sel: &mut Option<usize>,
) {
    let Some(idx) = *sel else { return };
    let (row_label, colors) = if idx < 8 { ("Normal", normal) } else { ("Bright", bright) };
    let hex = palette_slot(colors, idx % 8);
    let mut open = true;
    egui::Window::new(format!("{} {}", row_label, ANSI_NAMES[idx % 8]))
        .id(egui::Id::new("palette_picker_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Hex");
                ui.add(egui::TextEdit::singleline(hex).desired_width(90.0));
            });
            let [r, g, b] = config::parse_hex(hex).unwrap_or([0, 0, 0]);
            let mut c32 = egui::Color32::from_rgb(r, g, b);
            if egui::color_picker::color_picker_color32(ui, &mut c32, egui::color_picker::Alpha::Opaque) {
                *hex = config::format_hex([c32.r(), c32.g(), c32.b()]);
            }
        });
    if !open {
        *sel = None;
    }
}

// What the palette means in practice: a sample line rendered with the draft
// colors on the profile background, using GNU ls's default color assignments.
// The real mapping comes from $LS_COLORS, but these are the out-of-the-box
// defaults virtually everyone sees.
fn ansi_palette_preview(ui: &mut egui::Ui, profile: &config::Profile) {
    let pal = profile.palette_rgb();
    let [br, bg_, bb] = profile.background_rgb();
    let [fr, fg_, fb] = profile.foreground_rgb();
    let chip = |idx: usize, text: &str| {
        let [r, g, b] = pal[idx];
        egui::RichText::new(text)
            .monospace()
            .color(egui::Color32::from_rgb(r, g, b))
    };
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(br, bg_, bb))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("file.txt")
                        .monospace()
                        .color(egui::Color32::from_rgb(fr, fg_, fb)),
                );
                ui.label(chip(4, "directory/"));
                ui.label(chip(6, "symlink@"));
                ui.label(chip(2, "executable*"));
                ui.label(chip(1, "archive.tar"));
                ui.label(chip(5, "image.png"));
            });
        });
    ui.label(
        egui::RichText::new("Preview uses ls's default color assignments; actual mapping comes from LS_COLORS.")
            .small()
            .weak(),
    );
}
