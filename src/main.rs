mod app_shared;
mod app_window;
mod clipboard_read;
mod config;
mod dialogs;
mod font;
mod gl_setup;
mod gl_window;
mod groups;
mod hotkey;
mod input;
mod keybindings;
mod keyboard;
mod layout;
mod logging;
mod mouse;
mod pane;
mod pane_ui;
mod platform;
mod prefs_ui;
mod presets;
mod profile;
mod pty_event_loop;
mod reflow;
mod renderer;
mod search;
mod shell_integration;
mod tab_bar;
mod tabs;
mod term_gate;
mod term_handler;
mod title_bar;
pub mod window;

pub(crate) use input::RawTermKey;

fn main() {
    let logs_on_stderr = logging::init();

    if let Err(e) = window::run() {
        log::error!("fatal: {e}");
        if !logs_on_stderr {
            eprintln!("fatal: {e}");
        }
        std::process::exit(1);
    }
}
