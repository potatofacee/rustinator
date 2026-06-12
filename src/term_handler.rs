//! Forwarding `vte::ansi::Handler` wrapper around `Term`.
//!
//! The PTY parser advances into this proxy instead of `Term` directly so we
//! can interpose on escape-sequence handling. Every method of the vte 0.15.0
//! `Handler` trait (71 methods) must be forwarded explicitly: the trait
//! provides default no-op bodies, so a missing forward silently breaks the
//! terminal. Re-check this list against vte's trait when bumping the crate.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
    ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};

pub(crate) struct TermHandlerProxy<'a, T: EventListener> {
    pub term: &'a mut Term<T>,
    pub clear_wipes_scrollback: bool,
}

// Forwards use explicit `Handler::method(...)` syntax: `Term` has private
// inherent methods sharing names with trait methods (e.g. set_keyboard_mode),
// so method-call syntax is avoided to keep resolution unambiguous.
impl<T: EventListener> Handler for TermHandlerProxy<'_, T> {
    fn set_title(&mut self, title: Option<String>) {
        Handler::set_title(self.term, title);
    }

    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        Handler::set_cursor_style(self.term, style);
    }

    fn set_cursor_shape(&mut self, shape: CursorShape) {
        Handler::set_cursor_shape(self.term, shape);
    }

    fn input(&mut self, c: char) {
        Handler::input(self.term, c);
    }

    fn goto(&mut self, line: i32, col: usize) {
        Handler::goto(self.term, line, col);
    }

    fn goto_line(&mut self, line: i32) {
        Handler::goto_line(self.term, line);
    }

    fn goto_col(&mut self, col: usize) {
        Handler::goto_col(self.term, col);
    }

    fn insert_blank(&mut self, count: usize) {
        Handler::insert_blank(self.term, count);
    }

    fn move_up(&mut self, rows: usize) {
        Handler::move_up(self.term, rows);
    }

    fn move_down(&mut self, rows: usize) {
        Handler::move_down(self.term, rows);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        Handler::identify_terminal(self.term, intermediate);
    }

    fn device_status(&mut self, arg: usize) {
        Handler::device_status(self.term, arg);
    }

    fn move_forward(&mut self, cols: usize) {
        Handler::move_forward(self.term, cols);
    }

    fn move_backward(&mut self, cols: usize) {
        Handler::move_backward(self.term, cols);
    }

    fn move_down_and_cr(&mut self, rows: usize) {
        Handler::move_down_and_cr(self.term, rows);
    }

    fn move_up_and_cr(&mut self, rows: usize) {
        Handler::move_up_and_cr(self.term, rows);
    }

    fn put_tab(&mut self, count: u16) {
        Handler::put_tab(self.term, count);
    }

    fn backspace(&mut self) {
        Handler::backspace(self.term);
    }

    fn carriage_return(&mut self) {
        Handler::carriage_return(self.term);
    }

    fn linefeed(&mut self) {
        Handler::linefeed(self.term);
    }

    fn bell(&mut self) {
        Handler::bell(self.term);
    }

    fn substitute(&mut self) {
        Handler::substitute(self.term);
    }

    fn newline(&mut self) {
        Handler::newline(self.term);
    }

    fn set_horizontal_tabstop(&mut self) {
        Handler::set_horizontal_tabstop(self.term);
    }

    fn scroll_up(&mut self, rows: usize) {
        Handler::scroll_up(self.term, rows);
    }

    fn scroll_down(&mut self, rows: usize) {
        Handler::scroll_down(self.term, rows);
    }

    fn insert_blank_lines(&mut self, count: usize) {
        Handler::insert_blank_lines(self.term, count);
    }

    fn delete_lines(&mut self, count: usize) {
        Handler::delete_lines(self.term, count);
    }

    fn erase_chars(&mut self, count: usize) {
        Handler::erase_chars(self.term, count);
    }

    fn delete_chars(&mut self, count: usize) {
        Handler::delete_chars(self.term, count);
    }

    fn move_backward_tabs(&mut self, count: u16) {
        Handler::move_backward_tabs(self.term, count);
    }

    fn move_forward_tabs(&mut self, count: u16) {
        Handler::move_forward_tabs(self.term, count);
    }

    fn save_cursor_position(&mut self) {
        Handler::save_cursor_position(self.term);
    }

    fn restore_cursor_position(&mut self) {
        Handler::restore_cursor_position(self.term);
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        Handler::clear_line(self.term, mode);
    }

    // The one intentional difference from plain forwarding: when the pane
    // preference is set, ClearMode::All on the primary screen also wipes
    // scrollback (All scrolls the viewport into history, Saved then clears
    // it). On the alt screen history belongs to the primary screen, so the
    // sequence is forwarded untouched.
    fn clear_screen(&mut self, mode: ClearMode) {
        // matches! instead of ==: vte's ClearMode does not derive PartialEq.
        if self.clear_wipes_scrollback
            && matches!(mode, ClearMode::All)
            && !self.term.mode().contains(TermMode::ALT_SCREEN)
        {
            Handler::clear_screen(self.term, ClearMode::All);
            Handler::clear_screen(self.term, ClearMode::Saved);
        } else {
            Handler::clear_screen(self.term, mode);
        }
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        Handler::clear_tabs(self.term, mode);
    }

    fn set_tabs(&mut self, interval: u16) {
        Handler::set_tabs(self.term, interval);
    }

    fn reset_state(&mut self) {
        Handler::reset_state(self.term);
    }

    fn reverse_index(&mut self) {
        Handler::reverse_index(self.term);
    }

    fn terminal_attribute(&mut self, attr: Attr) {
        Handler::terminal_attribute(self.term, attr);
    }

    fn set_mode(&mut self, mode: Mode) {
        Handler::set_mode(self.term, mode);
    }

    fn unset_mode(&mut self, mode: Mode) {
        Handler::unset_mode(self.term, mode);
    }

    fn report_mode(&mut self, mode: Mode) {
        Handler::report_mode(self.term, mode);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        Handler::set_private_mode(self.term, mode);
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        Handler::unset_private_mode(self.term, mode);
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        Handler::report_private_mode(self.term, mode);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        Handler::set_scrolling_region(self.term, top, bottom);
    }

    fn set_keypad_application_mode(&mut self) {
        Handler::set_keypad_application_mode(self.term);
    }

    fn unset_keypad_application_mode(&mut self) {
        Handler::unset_keypad_application_mode(self.term);
    }

    fn set_active_charset(&mut self, index: CharsetIndex) {
        Handler::set_active_charset(self.term, index);
    }

    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        Handler::configure_charset(self.term, index, charset);
    }

    fn set_color(&mut self, index: usize, color: Rgb) {
        Handler::set_color(self.term, index, color);
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        Handler::dynamic_color_sequence(self.term, prefix, index, terminator);
    }

    fn reset_color(&mut self, index: usize) {
        Handler::reset_color(self.term, index);
    }

    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        Handler::clipboard_store(self.term, clipboard, base64);
    }

    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        Handler::clipboard_load(self.term, clipboard, terminator);
    }

    fn decaln(&mut self) {
        Handler::decaln(self.term);
    }

    fn push_title(&mut self) {
        Handler::push_title(self.term);
    }

    fn pop_title(&mut self) {
        Handler::pop_title(self.term);
    }

    fn text_area_size_pixels(&mut self) {
        Handler::text_area_size_pixels(self.term);
    }

    fn text_area_size_chars(&mut self) {
        Handler::text_area_size_chars(self.term);
    }

    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        Handler::set_hyperlink(self.term, hyperlink);
    }

    fn set_mouse_cursor_icon(&mut self, icon: CursorIcon) {
        Handler::set_mouse_cursor_icon(self.term, icon);
    }

    fn report_keyboard_mode(&mut self) {
        Handler::report_keyboard_mode(self.term);
    }

    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        Handler::push_keyboard_mode(self.term, mode);
    }

    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        Handler::pop_keyboard_modes(self.term, to_pop);
    }

    fn set_keyboard_mode(&mut self, mode: KeyboardModes, behavior: KeyboardModesApplyBehavior) {
        Handler::set_keyboard_mode(self.term, mode, behavior);
    }

    fn set_modify_other_keys(&mut self, mode: ModifyOtherKeys) {
        Handler::set_modify_other_keys(self.term, mode);
    }

    fn report_modify_other_keys(&mut self) {
        Handler::report_modify_other_keys(self.term);
    }

    fn set_scp(&mut self, char_path: ScpCharPath, update_mode: ScpUpdateMode) {
        Handler::set_scp(self.term, char_path, update_mode);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    /// Exact vte version this build compiled against, from Cargo.lock.
    fn locked_vte_version() -> String {
        let lock_path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock");
        let lock = fs::read_to_string(lock_path).expect("read Cargo.lock");
        let mut lines = lock.lines();
        while let Some(line) = lines.next() {
            if line.trim() == "name = \"vte\"" {
                for line in lines.by_ref() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("version = \"") {
                        return rest.trim_end_matches('"').to_string();
                    }
                    if line.starts_with("[[") {
                        break;
                    }
                }
            }
        }
        panic!("vte not found in Cargo.lock");
    }

    /// Source of vte's ansi.rs from the local cargo registry. The dep must be
    /// vendored locally for this project to have compiled, so failure to find
    /// it is an error, never a silent pass.
    fn vte_ansi_source(version: &str) -> String {
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("neither CARGO_HOME nor HOME set"))
                    .join(".cargo")
            });
        let registry = cargo_home.join("registry").join("src");
        let crate_dir = format!("vte-{version}");
        let entries = fs::read_dir(&registry)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", registry.display()));
        for entry in entries.flatten() {
            let candidate = entry.path().join(&crate_dir).join("src").join("ansi.rs");
            if candidate.is_file() {
                return fs::read_to_string(&candidate)
                    .unwrap_or_else(|e| panic!("read {}: {e}", candidate.display()));
            }
        }
        panic!(
            "vte-{version} source not found under {} -- cannot audit Handler forwarding",
            registry.display()
        );
    }

    /// Method names declared at the top level of a trait/impl block body
    /// (4-space indent; nested bodies are indented deeper).
    fn method_names(block: &str) -> BTreeSet<String> {
        block
            .lines()
            .filter_map(|line| line.strip_prefix("    fn "))
            .filter_map(|rest| rest.split('(').next())
            .map(|name| name.trim().to_string())
            .collect()
    }

    /// Block body from `start_marker` to the first column-0 closing brace.
    fn block_after<'a>(source: &'a str, start_marker: &str) -> &'a str {
        let start = source
            .find(start_marker)
            .unwrap_or_else(|| panic!("marker {start_marker:?} not found"));
        let body = &source[start..];
        let end = body.find("\n}").expect("unterminated block");
        &body[..end]
    }

    // vte's Handler trait methods have default no-op bodies, so a forward
    // missing from TermHandlerProxy compiles fine but silently disables an
    // escape sequence. Audit our impl against the trait source text.
    #[test]
    fn proxy_forwards_every_vte_handler_method() {
        let version = locked_vte_version();
        let vte_src = vte_ansi_source(&version);
        let trait_methods = method_names(block_after(&vte_src, "pub trait Handler {"));
        assert!(!trait_methods.is_empty(), "no methods extracted from vte Handler trait");

        let ours_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/term_handler.rs");
        let ours = fs::read_to_string(ours_path).expect("read term_handler.rs");
        let impl_methods = method_names(block_after(&ours, "Handler for TermHandlerProxy"));

        let missing: Vec<_> = trait_methods.difference(&impl_methods).collect();
        let extra: Vec<_> = impl_methods.difference(&trait_methods).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "vte {version} Handler trait changed; update TermHandlerProxy forwarding in \
             src/term_handler.rs. unforwarded trait methods: {missing:?}, \
             impl methods not in trait: {extra:?}"
        );
    }
}
