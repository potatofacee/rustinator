use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction as GridDir, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use egui;

use crate::font::FontStyle;
use crate::keyboard;
use crate::mouse::{self, MouseButton, MouseKind, MouseMods};

pub type PaneId = u64;

// Xterm-ish ANSI palette (NamedColor::Black..BrightWhite = 0..15).
const ANSI: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

#[derive(Debug, Clone, Copy)]
pub struct PaneDefaults {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    /// 0.0..=1.0; alpha to apply to the *default* bg only. Non-default cell
    /// backgrounds stay fully opaque.
    pub bg_opacity: f32,
}

impl Default for PaneDefaults {
    fn default() -> Self {
        Self {
            fg: [0xe5, 0xe5, 0xe5],
            bg: [0x1a, 0x1a, 0x1a],
            cursor: [0xe5, 0xe5, 0xe5],
            bg_opacity: 1.0,
        }
    }
}

fn rgb_to_f32(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

fn named_default(n: NamedColor, defaults: &PaneDefaults) -> [u8; 3] {
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            defaults.fg
        }
        NamedColor::Background => defaults.bg,
        NamedColor::Cursor => defaults.cursor,
        _ => {
            let idx = n as usize;
            if idx < 16 { ANSI[idx] } else { defaults.fg }
        }
    }
}

fn indexed_default(i: u8) -> [u8; 3] {
    let i = i as usize;
    if i < 16 {
        ANSI[i]
    } else if i < 232 {
        let n = i - 16;
        let steps = [0, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
        [steps[n / 36], steps[(n / 6) % 6], steps[n % 6]]
    } else {
        let g = 8 + (i - 232) * 10;
        [g as u8, g as u8, g as u8]
    }
}

fn resolve_color(
    color: Color,
    palette: &alacritty_terminal::term::color::Colors,
    is_fg: bool,
    defaults: &PaneDefaults,
) -> [f32; 4] {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(n) => palette[n].unwrap_or_else(|| {
            let [r, g, b] = if matches!(n, NamedColor::Background)
                || (!is_fg && matches!(n, NamedColor::Foreground))
            {
                defaults.bg
            } else {
                named_default(n, defaults)
            };
            Rgb { r, g, b }
        }),
        Color::Indexed(i) => palette[i as usize].unwrap_or_else(|| {
            let [r, g, b] = indexed_default(i);
            Rgb { r, g, b }
        }),
    };
    rgb_to_f32(rgb.r, rgb.g, rgb.b)
}

#[derive(Clone)]
pub struct EventProxy {
    pub ctx: egui::Context,
    pub dirty: Arc<AtomicBool>,
    pub exited: Arc<AtomicBool>,
    pub title: Arc<Mutex<Option<String>>>,
    pub winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
}

impl EventProxy {
    pub fn new(
        ctx: egui::Context,
        winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
    ) -> Self {
        Self {
            ctx,
            dirty: Arc::new(AtomicBool::new(true)),
            exited: Arc::new(AtomicBool::new(false)),
            title: Arc::new(Mutex::new(None)),
            winit_proxy,
        }
    }

    fn wake(&self) {
        self.ctx.request_repaint();
        if let Some(proxy) = &self.winit_proxy {
            let _ = proxy.send_event(crate::window::UserEvent::Repaint);
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::Bell | Event::MouseCursorDirty => {
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            Event::Title(t) => {
                *self.title.lock().unwrap() = Some(t);
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            Event::ResetTitle => {
                *self.title.lock().unwrap() = None;
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            Event::Exit => {
                self.exited.store(true, Ordering::Release);
                self.wake();
            }
            Event::ChildExit(_) => {
                self.wake();
            }
            _ => {}
        }
    }
}

struct TermDims {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Frame {
    pub cells: Vec<CellSnapshot>,
    pub default_bg: [f32; 4],
    pub cursor: Option<CursorOverlay>,
    pub urls: Vec<UrlMatch>,
}

#[derive(Debug, Clone)]
pub struct UrlMatch {
    pub row: i32,
    pub start_col: i32,
    pub end_col: i32, // exclusive
    pub url: String,
}

#[derive(Debug, Clone, Copy)]
pub enum CursorOverlay {
    Beam { col: i32, row: i32, color: [f32; 4] },
    Underline { col: i32, row: i32, color: [f32; 4] },
    HollowBlock { col: i32, row: i32, color: [f32; 4] },
}

pub struct CellSnapshot {
    pub col: i32,
    pub row: i32,
    pub c: char,
    pub fg: [f32; 4],
    pub bg: [f32; 4],
    pub style: FontStyle,
}

fn point_from_grid(col: i32, row: i32, display_offset: i32) -> Point {
    // ui row (0 = top of visible area) -> Line; cursor/mouse uses this mapping.
    Point::new(Line(row - display_offset), Column(col.max(0) as usize))
}

fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(
            ch,
            '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' |
            '{' | '}' | '^' | '$' | '\\' | '/'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn is_url_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ')' | ']' | '"' | '\'' | '>' | '<' | '{' | '}' | ',' | ';' | '|' | '`'
        )
}

fn scan_urls(cells: &[CellSnapshot]) -> Vec<UrlMatch> {
    // Group cells by row in their emitted order.
    let mut by_row: std::collections::BTreeMap<i32, Vec<(i32, char)>> =
        std::collections::BTreeMap::new();
    for cell in cells {
        by_row.entry(cell.row).or_default().push((cell.col, cell.c));
    }

    let mut out = Vec::new();
    for (row, mut cols) in by_row {
        cols.sort_by_key(|&(c, _)| c);
        let chars: Vec<char> = cols.iter().map(|&(_, c)| c).collect();
        let col_lookup: Vec<i32> = cols.iter().map(|&(c, _)| c).collect();

        let http = ['h', 't', 't', 'p', ':', '/', '/'];
        let https = ['h', 't', 't', 'p', 's', ':', '/', '/'];

        let mut i = 0;
        while i < chars.len() {
            let rest = &chars[i..];
            let matched = if rest.starts_with(&https) {
                Some(https.len())
            } else if rest.starts_with(&http) {
                Some(http.len())
            } else {
                None
            };
            if matched.is_some() {
                let mut j = i;
                while j < chars.len() && !is_url_boundary(chars[j]) {
                    j += 1;
                }
                let prefix_end = i + matched.unwrap();
                if j > prefix_end {
                    let url: String = chars[i..j].iter().collect();
                    out.push(UrlMatch {
                        row,
                        start_col: col_lookup[i],
                        end_col: col_lookup[j - 1] + 1,
                        url,
                    });
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

pub struct Pane {
    pub id: PaneId,
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub pty_tx: EventLoopSender,
    pub cols: usize,
    pub lines: usize,
    pub dirty: Arc<AtomicBool>,
    pub exited: Arc<AtomicBool>,
    pub title: Arc<Mutex<Option<String>>>,
    pub cached: Option<Arc<Frame>>,
    pub defaults: PaneDefaults,
    pub read_only: bool,
}

impl Pane {
    pub fn spawn(
        id: PaneId,
        cols: usize,
        lines: usize,
        cell_w: f32,
        cell_h: f32,
        ctx: egui::Context,
        term_config: Config,
        defaults: PaneDefaults,
        winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
    ) -> Self {
        let proxy = EventProxy::new(ctx, winit_proxy);
        let dirty = Arc::clone(&proxy.dirty);
        let exited = Arc::clone(&proxy.exited);
        let title = Arc::clone(&proxy.title);

        let dims = TermDims { cols, lines };
        let term = Term::new(term_config, &dims, proxy.clone());
        let terminal = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };

        let mut pty_opts = tty::Options::default();
        pty_opts.env.insert("TERM".into(), "xterm-256color".into());
        let pty = tty::new(&pty_opts, window_size, id).expect("failed to open pty");
        let event_loop = EventLoop::new(Arc::clone(&terminal), proxy, pty, false, false)
            .expect("failed to create pty event loop");
        let pty_tx = event_loop.channel();
        let _handle = event_loop.spawn();

        Self {
            id,
            terminal,
            pty_tx,
            cols,
            lines,
            dirty,
            exited,
            title,
            cached: None,
            defaults,
            read_only: false,
        }
    }

    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    pub fn resize(&mut self, cols: usize, lines: usize, cell_w: f32, cell_h: f32) {
        if cols == self.cols && lines == self.lines {
            return;
        }
        let dims = TermDims { cols, lines };
        {
            let mut term = self.terminal.lock();
            term.resize(dims);
        }
        let ws = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };
        let _ = self.pty_tx.send(Msg::Resize(ws));
        self.cols = cols;
        self.lines = lines;
        self.dirty.store(true, Ordering::Release);
        self.cached = None;
    }

    /// Returns the pane's current Frame, rebuilding only if the pane is marked dirty
    /// (or has no cache yet). Clears the dirty flag after a rebuild.
    pub fn frame(&mut self) -> Arc<Frame> {
        if self.cached.is_none() || self.dirty.swap(false, Ordering::AcqRel) {
            self.cached = Some(Arc::new(self.snapshot()));
        }
        Arc::clone(self.cached.as_ref().unwrap())
    }

    pub fn send_bytes(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.pty_tx.send(Msg::Input(bytes.into()));
    }

    /// True when the term currently swallows Text events itself (REPORT_ALL_KEYS_AS_ESC).
    pub fn text_is_suppressed(&self) -> bool {
        self.terminal
            .lock()
            .mode()
            .contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Encode and send a key event, using Kitty keyboard protocol when the term
    /// has enabled it, otherwise falling back to legacy terminal encoding.
    pub fn send_key(
        &self,
        key: egui::Key,
        mods: egui::Modifiers,
        legacy: impl FnOnce(egui::Key, egui::Modifiers) -> Option<Vec<u8>>,
    ) {
        let mode = *self.terminal.lock().mode();
        if let Some(bytes) = keyboard::encode(key, mods, mode) {
            self.send_bytes(bytes);
            return;
        }
        if let Some(bytes) = legacy(key, mods) {
            self.send_bytes(bytes);
        }
    }

    pub fn begin_selection(&self, col: i32, row: i32, ty: SelectionType) {
        let mut term = self.terminal.lock();
        let display_offset = term.grid().display_offset() as i32;
        let point = point_from_grid(col, row, display_offset);
        term.selection = Some(Selection::new(ty, point, Side::Left));
        self.dirty.store(true, Ordering::Release);
    }

    pub fn update_selection(&self, col: i32, row: i32) {
        let mut term = self.terminal.lock();
        let display_offset = term.grid().display_offset() as i32;
        let point = point_from_grid(col, row, display_offset);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, Side::Right);
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub fn clear_selection(&self) {
        let mut term = self.terminal.lock();
        if term.selection.is_some() {
            term.selection = None;
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        self.terminal.lock().selection_to_string()
    }

    pub fn mouse_reporting(&self) -> bool {
        self.terminal
            .lock()
            .mode()
            .intersects(TermMode::MOUSE_MODE)
    }

    pub fn send_mouse(
        &self,
        kind: MouseKind,
        button: MouseButton,
        col: i32,
        row: i32,
        mods: MouseMods,
    ) {
        let mode = *self.terminal.lock().mode();
        if let Some(bytes) = mouse::encode(kind, button, col, row, mods, mode) {
            self.send_bytes(bytes);
        }
    }

    pub fn scroll_by(&self, lines: i32) {
        if lines == 0 {
            return;
        }
        let mut term = self.terminal.lock();
        term.scroll_display(Scroll::Delta(lines));
        self.dirty.store(true, Ordering::Release);
    }

    /// Search scrollback for `needle` (plain text; regex-escaped internally).
    /// Searches backward from the cursor by default. Sets the selection to the
    /// match and scrolls it into view. Returns true if a match was found.
    pub fn search(&self, needle: &str, backward: bool) -> bool {
        if needle.is_empty() {
            return false;
        }
        let pattern = escape_regex(needle);
        let mut regex = match RegexSearch::new(&pattern) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("search regex build failed: {e}");
                return false;
            }
        };
        let mut term = self.terminal.lock();
        // Origin: skip past the current selection/cursor so consecutive searches advance.
        let origin = match (term.selection.as_ref().and_then(|s| s.to_range(&*term)), backward) {
            (Some(range), true) => range.start.sub(&*term, Boundary::None, 1),
            (Some(range), false) => range.end.add(&*term, Boundary::None, 1),
            (None, _) => term.grid().cursor.point,
        };
        let direction = if backward { GridDir::Left } else { GridDir::Right };
        let m = term.search_next(&mut regex, origin, direction, Side::Left, None);
        if let Some(m) = m {
            let start = *m.start();
            let end = *m.end();
            term.selection = Some(Selection::new(SelectionType::Simple, start, Side::Left));
            if let Some(sel) = term.selection.as_mut() {
                sel.update(end, Side::Right);
            }
            term.scroll_to_point(start);
            self.dirty.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    /// Send pasted text, wrapping in bracketed-paste sequences when the term
    /// has BRACKETED_PASTE enabled.
    pub fn send_paste(&self, text: &str) {
        let bracketed = self
            .terminal
            .lock()
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        let bytes = if bracketed {
            let mut v = Vec::with_capacity(text.len() + 12);
            v.extend_from_slice(b"\x1b[200~");
            v.extend_from_slice(text.as_bytes());
            v.extend_from_slice(b"\x1b[201~");
            v
        } else {
            text.as_bytes().to_vec()
        };
        self.send_bytes(bytes);
    }

    pub fn snapshot(&self) -> Frame {
        let term = self.terminal.lock();
        let lines = term.screen_lines() as i32;
        let content = term.renderable_content();
        let palette = content.colors;
        let mut default_bg = resolve_color(
            Color::Named(NamedColor::Background),
            palette,
            false,
            &self.defaults,
        );
        default_bg[3] = self.defaults.bg_opacity.clamp(0.0, 1.0);
        let display_offset = content.display_offset as i32;

        let cursor_shape = content.cursor.shape;
        let cursor_visible = cursor_shape != CursorShape::Hidden;
        let cursor_col = content.cursor.point.column.0 as i32;
        let cursor_row = content.cursor.point.line.0 + display_offset;
        let selection = content.selection;

        let cursor_color = {
            let [r, g, b] = self.defaults.cursor;
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        };
        let cursor_invert_at_cell = false;
        let cursor_overlay = if cursor_visible && cursor_row >= 0 && cursor_row < lines {
            match cursor_shape {
                CursorShape::Beam => Some(CursorOverlay::Beam {
                    col: cursor_col,
                    row: cursor_row,
                    color: cursor_color,
                }),
                CursorShape::Underline => Some(CursorOverlay::Underline {
                    col: cursor_col,
                    row: cursor_row,
                    color: cursor_color,
                }),
                CursorShape::Block | CursorShape::HollowBlock => {
                    Some(CursorOverlay::HollowBlock {
                        col: cursor_col,
                        row: cursor_row,
                        color: cursor_color,
                    })
                }
                _ => None,
            }
        } else {
            None
        };

        let mut cells = Vec::with_capacity(term.columns() * term.screen_lines());
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + display_offset;
            if row < 0 || row >= lines {
                continue;
            }
            let col = indexed.point.column.0 as i32;
            let mut fg = resolve_color(indexed.cell.fg, palette, true, &self.defaults);
            let mut bg = resolve_color(indexed.cell.bg, palette, false, &self.defaults);
            let is_cursor = cursor_invert_at_cell && col == cursor_col && row == cursor_row;
            let is_selected = selection
                .map(|s| s.contains(indexed.point))
                .unwrap_or(false);
            if is_cursor || is_selected {
                std::mem::swap(&mut fg, &mut bg);
            }
            let flags = indexed.cell.flags;
            let style = match (flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC)) {
                (true, true) => FontStyle::BoldItalic,
                (true, false) => FontStyle::Bold,
                (false, true) => FontStyle::Italic,
                (false, false) => FontStyle::Regular,
            };
            cells.push(CellSnapshot {
                col,
                row,
                c: indexed.cell.c,
                fg,
                bg,
                style,
            });
        }

        let urls = scan_urls(&cells);

        Frame {
            cells,
            default_bg,
            cursor: cursor_overlay,
            urls,
        }
    }
}
