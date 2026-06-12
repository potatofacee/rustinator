//! Forwarding `vte::ansi::Handler` wrapper around `Term`.
//!
//! The PTY parser advances into this proxy instead of `Term` directly so we
//! can interpose on escape-sequence handling. Every method of the vte 0.15.0
//! `Handler` trait (71 methods) must be forwarded explicitly: the trait
//! provides default no-op bodies, so a missing forward silently breaks the
//! terminal. Re-check this list against vte's trait when bumping the crate.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::{Term, TermMode};

use crate::pane::{indexed_default, PaneDefaults};
use crate::pty_event_loop::{EventLoopSender, Msg};
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
    ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};

pub(crate) struct TermHandlerProxy<'a, T: EventListener> {
    pub term: &'a mut Term<T>,
    pub clear_wipes_scrollback: bool,
    /// Sender into the owning PTY event loop, used to queue query replies
    /// (color queries) for write-back to the PTY.
    pub writer: &'a EventLoopSender,
    /// Snapshot of the pane's default colors, the fallback when the program
    /// has not overridden a queried color via OSC 4/10/11/12.
    pub defaults: PaneDefaults,
}

/// Widen an 8-bit channel to X11's 16-bit-per-channel form (0xab -> 0xabab),
/// the same scaling the alacritty GUI applies when answering color queries.
fn c16(x: u8) -> u16 {
    (x as u16) << 8 | x as u16
}

/// OSC color-query reply: `ESC ] {prefix} ; rgb:rrrr/gggg/bbbb {terminator}`.
pub(crate) fn color_reply(prefix: &str, [r, g, b]: [u8; 3], terminator: &str) -> String {
    format!("\x1b]{};rgb:{:04x}/{:04x}/{:04x}{}", prefix, c16(r), c16(g), c16(b), terminator)
}

/// Default color for a query index when the program has not set an override.
/// Indices follow vte's NamedColor layout: 0-255 are the indexed palette,
/// 256/257/258 are Foreground/Background/Cursor. Anything else (Dim* and
/// out-of-range) falls back to the foreground.
pub(crate) fn default_color_for_index(index: usize, defaults: &PaneDefaults) -> [u8; 3] {
    match index {
        0..=255 => indexed_default(index as u8, defaults),
        256 => defaults.fg,
        257 => defaults.bg,
        258 => defaults.cursor,
        _ => defaults.fg,
    }
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

    // Answered here instead of forwarding: Term's version only emits
    // Event::ColorRequest, but the exact answer needs the runtime override
    // table (term.colors()), which only this proxy can read — it already
    // holds &mut Term during the parse. The reply is queued on the loop's own
    // channel; the poller wakes and writes it to the PTY next iteration.
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        let rgb = self.term.colors()[index]
            .map(|c| [c.r, c.g, c.b])
            .unwrap_or_else(|| default_color_for_index(index, &self.defaults));
        let reply = color_reply(&prefix, rgb, terminator);
        let _ = self.writer.send(Msg::Input(reply.into_bytes().into()));
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

    use super::{color_reply, default_color_for_index};
    use crate::pane::PaneDefaults;

    // ---- OSC color-query replies ----

    #[test]
    fn color_reply_widens_channels_to_16_bit() {
        // 0xab -> 0xabab, per X11 rgb spec (matches alacritty GUI replies).
        assert_eq!(
            color_reply("10", [0xff, 0x80, 0x00], "\x1b\\"),
            "\x1b]10;rgb:ffff/8080/0000\x1b\\"
        );
    }

    #[test]
    fn color_reply_osc4_prefix_and_bel_terminator() {
        assert_eq!(color_reply("4;1", [0xcd, 0x00, 0x00], "\x07"), "\x1b]4;1;rgb:cdcd/0000/0000\x07");
    }

    #[test]
    fn color_reply_black_and_white() {
        assert_eq!(color_reply("11", [0x00, 0x00, 0x00], "\x07"), "\x1b]11;rgb:0000/0000/0000\x07");
        assert_eq!(color_reply("11", [0xff, 0xff, 0xff], "\x07"), "\x1b]11;rgb:ffff/ffff/ffff\x07");
    }

    #[test]
    fn default_color_index_named_special() {
        let d = PaneDefaults::default();
        // vte NamedColor: Foreground = 256, Background = 257, Cursor = 258.
        assert_eq!(default_color_for_index(256, &d), d.fg);
        assert_eq!(default_color_for_index(257, &d), d.bg);
        assert_eq!(default_color_for_index(258, &d), d.cursor);
    }

    #[test]
    fn default_color_index_palette_range() {
        let d = PaneDefaults::default();
        // 0-15 come from the profile palette.
        assert_eq!(default_color_for_index(1, &d), d.palette[1]);
        assert_eq!(default_color_for_index(15, &d), d.palette[15]);
        // 16-255 are the computed cube/grayscale ramps.
        assert_eq!(default_color_for_index(196, &d), [0xff, 0x00, 0x00]);
        assert_eq!(default_color_for_index(255, &d), [238, 238, 238]);
    }

    #[test]
    fn default_color_index_out_of_range_falls_back_to_fg() {
        let d = PaneDefaults::default();
        // Dim* names (259+) and anything unknown answer as foreground.
        assert_eq!(default_color_for_index(259, &d), d.fg);
        assert_eq!(default_color_for_index(usize::MAX, &d), d.fg);
    }

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
