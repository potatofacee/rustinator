mod app_shared;
mod app_window;
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
mod mouse;
mod pane;
mod pane_ui;
mod platform;
mod prefs_ui;
mod presets;
mod profile;
mod pty_event_loop;
mod renderer;
mod shell_integration;
mod tab_bar;
mod tabs;
mod term_handler;
mod title_bar;
pub mod window;

pub(crate) use input::RawTermKey;

fn main() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    if let Err(e) = window::run() {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}
