use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use crate::pty_event_loop::{EventLoop, EventLoopSender, Msg, PaneHooks, State as EventLoopState};
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

/// JoinHandle for the alacritty PTY EventLoop thread. `EventLoop::spawn`
/// returns `JoinHandle<(EventLoop, State)>`; we keep it so the old thread can
/// be reaped deterministically on respawn.
type PtyLoopHandle = JoinHandle<(EventLoop<tty::Pty, EventProxy>, EventLoopState)>;

// Xterm-ish ANSI palette (NamedColor::Black..BrightWhite = 0..15).
pub(crate) const ANSI: [[u8; 3]; 16] = [
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneDefaults {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    /// ANSI palette colors 0-15 (normal 0-7, bright 8-15) from the profile.
    pub palette: [[u8; 3]; 16],
    /// Selection override colors. None = invert the cell's fg/bg.
    pub selection_bg: Option<[u8; 3]>,
    pub selection_fg: Option<[u8; 3]>,
    /// 0.0..=1.0; alpha to apply to the *default* bg only. Non-default cell
    /// backgrounds stay fully opaque.
    pub bg_opacity: f32,
    /// Whether clearing the screen (ED 2 on the primary screen) also wipes
    /// scrollback history. Mirrored into the pane's atomic for the PTY thread.
    pub clear_wipes_scrollback: bool,
}

impl Default for PaneDefaults {
    fn default() -> Self {
        Self {
            fg: [0xe5, 0xe5, 0xe5],
            bg: [0x1a, 0x1a, 0x1a],
            cursor: [0xe5, 0xe5, 0xe5],
            palette: ANSI,
            selection_bg: None,
            selection_fg: None,
            bg_opacity: 1.0,
            clear_wipes_scrollback: false,
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
            if idx < 16 { defaults.palette[idx] } else { defaults.fg }
        }
    }
}

pub(crate) fn indexed_default(i: u8, defaults: &PaneDefaults) -> [u8; 3] {
    let i = i as usize;
    if i < 16 {
        defaults.palette[i]
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
            let [r, g, b] = indexed_default(i, defaults);
            Rgb { r, g, b }
        }),
    };
    rgb_to_f32(rgb.r, rgb.g, rgb.b)
}

#[derive(Clone)]
pub struct EventProxy {
    pub dirty: Arc<AtomicBool>,
    // True while a Repaint user event sent for PTY output is still unconsumed
    // (cleared when the pane next snapshots for a frame). Coalesces sustained
    // output into at most one winit wake per pane per painted frame. Separate
    // from `dirty`, which the main thread also sets for non-PTY reasons.
    pub wake_pending: Arc<AtomicBool>,
    pub has_new_output: Arc<AtomicBool>,
    pub exited: Arc<AtomicBool>,
    pub title: Arc<Mutex<Option<String>>>,
    // True when this pane is in the active tab. PTY output (Wakeup) on a
    // background pane sets dirty but skips waking the winit loop — a background
    // process (build log, tail -f) shouldn't keep the event loop hot when its
    // output isn't visible. Rare events (bell/title/exit) always wake.
    pub visible: Arc<AtomicBool>,
    pub winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
    // The fields below are read in send_event, which Term calls WHILE the PTY
    // thread holds the terminal FairMutex. Locking pane.terminal from any of
    // these paths would deadlock; only these mirrored Arcs and channel sends
    // may be touched.
    /// Sender into the pane's PTY event loop, for answering terminal queries
    /// (DA, DSR, text-area size). None only between proxy creation and
    /// EventLoop::new during (re)spawn, before the loop can emit events.
    pub pty_writer: Arc<Mutex<Option<EventLoopSender>>>,
    /// Mirror of the pane's current size, kept fresh by Pane::resize, so
    /// TextAreaSizeRequest can be answered without touching the terminal.
    pub window_size: Arc<Mutex<WindowSize>>,
    /// OSC 52 store waiting for the main thread, which owns clipboard access
    /// (egui_ctx.copy_text). Drained once per frame in App::logic.
    pub pending_clipboard_store: Arc<Mutex<Option<String>>>,
}

impl EventProxy {
    pub fn new(
        winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
        window_size: WindowSize,
    ) -> Self {
        Self {
            dirty: Arc::new(AtomicBool::new(true)),
            wake_pending: Arc::new(AtomicBool::new(false)),
            has_new_output: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
            title: Arc::new(Mutex::new(None)),
            visible: Arc::new(AtomicBool::new(true)),
            winit_proxy,
            pty_writer: Arc::new(Mutex::new(None)),
            window_size: Arc::new(Mutex::new(window_size)),
            pending_clipboard_store: Arc::new(Mutex::new(None)),
        }
    }

    /// Queue reply bytes for the PTY. No-op if the event loop's sender is not
    /// installed yet (it is filled right after EventLoop::new in (re)spawn).
    fn reply(&self, text: String) {
        if let Some(tx) = self.pty_writer.lock().unwrap().as_ref() {
            let _ = tx.send(Msg::Input(text.into_bytes().into()));
        }
    }

    fn wake(&self) {
        if let Some(proxy) = &self.winit_proxy {
            let _ = proxy.send_event(crate::window::UserEvent::Repaint);
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                self.dirty.store(true, Ordering::Release);
                self.has_new_output.store(true, Ordering::Release);
                // Coalesce wakes: skip the winit user event if one is already
                // in flight for this pane. Only wake the loop for visible
                // panes — background panes stay dirty and repaint when their
                // tab is next shown.
                if !self.wake_pending.swap(true, Ordering::AcqRel)
                    && self.visible.load(Ordering::Acquire)
                {
                    self.wake();
                }
            }
            Event::Bell | Event::MouseCursorDirty => {
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            Event::Title(ref t) => {
                *self.title.lock().unwrap() = Some(t.clone());
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
            // Reply paths for terminal queries (DA, DSR, XTWINOPS 14/18).
            // Without these, programs that query and wait (e.g. glow) hang.
            Event::PtyWrite(text) => {
                self.reply(text);
            }
            Event::TextAreaSizeRequest(fmt) => {
                let window_size = *self.window_size.lock().unwrap();
                self.reply(fmt(window_size));
            }
            // OSC 52 store: only the main thread can write the system
            // clipboard, so stash the text for App::logic to drain. The
            // ClipboardType distinction is dropped — the app's only clipboard
            // write path (egui_ctx.copy_text) targets the system clipboard.
            Event::ClipboardStore(_ty, text) => {
                *self.pending_clipboard_store.lock().unwrap() = Some(text);
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            // OSC 52 load: always answer with an empty paste. A real read
            // would expose the clipboard to any program writing to the tty
            // (our Term config keeps the crate default Osc52::OnlyCopy, which
            // already denies loads), but if a load ever gets through, a reply
            // must still be sent so the requesting app does not block.
            Event::ClipboardLoad(_ty, fmt) => {
                self.reply(fmt(""));
            }
            Event::CursorBlinkingChange => {
                self.dirty.store(true, Ordering::Release);
                self.wake();
            }
            // Never emitted: TermHandlerProxy::dynamic_color_sequence answers
            // color queries in place instead of forwarding to Term (the only
            // emitter of this event).
            Event::ColorRequest(_, _) => {}
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
    pub cursor_blink_requested: bool,
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
    Block { col: i32, row: i32, color: [f32; 4] },
    HollowBlock { col: i32, row: i32, color: [f32; 4] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

pub struct CellSnapshot {
    pub col: i32,
    pub row: i32,
    pub c: char,
    pub fg: [f32; 4],
    pub bg: [f32; 4],
    pub style: FontStyle,
    pub underline: Underline,
    pub underline_color: [f32; 4],
    pub strikeout: bool,
    /// True for the base cell of a double-width (CJK / full-width) glyph
    /// (Flags::WIDE_CHAR). The following column is a WIDE_CHAR_SPACER whose `c`
    /// is ' ' (so the glyph loop skips it) and whose bg matches this cell.
    pub wide: bool,
    /// HIDDEN (SGR 8). Normal text is drawn invisibly via fg == bg, but color
    /// (emoji) glyphs ignore fg, so the renderer needs this flag to drop them.
    pub hidden: bool,
    /// Extra codepoints alacritty stores on this cell via `Cell::zerowidth()`:
    /// combining marks (accents), ZWJ joiners, variation selectors, skin-tone
    /// modifiers. `c` is the base scalar; these stack on top of it. Empty for
    /// the overwhelmingly common single-scalar case, so no allocation there.
    /// CellSnapshot is intentionally NOT `Copy` (and never was), so a `Vec`
    /// here adds no ripple — every use reads scalar fields or borrows the cell.
    pub zerowidth: Vec<char>,
}

/// Map cell flags to an underline style. Alacritty stores at most one
/// underline style, but if several bits are set we mirror alacritty's own
/// renderer precedence (undercurl > double > dotted > dashed > single).
pub fn underline_from_flags(flags: Flags) -> Underline {
    if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    }
}

/// Collect the extra (zero-width / combining) codepoints alacritty stores on a
/// cell into an owned, render-order list for the snapshot. Returns an empty Vec
/// (no allocation) when the cell carries no extras, which is the common case.
pub(crate) fn extract_zerowidth(zerowidth: Option<&[char]>) -> Vec<char> {
    zerowidth.map(<[char]>::to_vec).unwrap_or_default()
}

/// Resolve a pixel-derived (col, row) hit to the authoritative grid column.
///
/// Our pixel->cell math (`cell_at`) divides by a fixed cell width, so a click
/// on the right half of a double-width glyph lands on its WIDE_CHAR_SPACER
/// column (N+1) rather than the base char (N). Alacritty treats the whole glyph
/// as the base point, so map a spacer hit back to the wide cell to its left.
/// `cells` is the current frame snapshot; the spacer is identified as the
/// blank cell immediately following a `wide` cell on the same row.
pub(crate) fn resolve_wide_click_col(cells: &[CellSnapshot], row: i32, col: i32) -> i32 {
    if col <= 0 {
        return col;
    }
    let is_wide_at = |c: i32| {
        cells
            .iter()
            .any(|cell| cell.row == row && cell.col == c && cell.wide)
    };
    if is_wide_at(col - 1) {
        col - 1
    } else {
        col
    }
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

// Match URLs within a single row's cells, already sorted by column.
// `chars`/`col_lookup` are parallel arrays (char and its source column).
fn match_urls_in_row(row: i32, chars: &[char], col_lookup: &[i32], out: &mut Vec<UrlMatch>) {
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
        if let Some(prefix_len) = matched {
            let mut j = i;
            while j < chars.len() && !is_url_boundary(chars[j]) {
                j += 1;
            }
            if j > i + prefix_len {
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

#[cfg(test)]
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
        match_urls_in_row(row, &chars, &col_lookup, &mut out);
    }
    out
}

// On-demand URL hit-test for a single cell. Scans only the pointer's row, so it
// runs on hover/Ctrl-click instead of on every frame snapshot.
pub(crate) fn scan_url_at(cells: &[CellSnapshot], row: i32, col: i32) -> Option<UrlMatch> {
    let mut cols: Vec<(i32, char)> =
        cells.iter().filter(|c| c.row == row).map(|c| (c.col, c.c)).collect();
    cols.sort_by_key(|&(c, _)| c);
    let chars: Vec<char> = cols.iter().map(|&(_, c)| c).collect();
    let col_lookup: Vec<i32> = cols.iter().map(|&(c, _)| c).collect();

    let mut out = Vec::new();
    match_urls_in_row(row, &chars, &col_lookup, &mut out);
    out.into_iter()
        .find(|u| col >= u.start_col && col < u.end_col)
}

pub struct Pane {
    pub id: PaneId,
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub pty_tx: EventLoopSender,
    pub child_pid: u32,
    pub cols: usize,
    pub lines: usize,
    pub dirty: Arc<AtomicBool>,
    pub wake_pending: Arc<AtomicBool>,
    pub has_new_output: Arc<AtomicBool>,
    pub exited: Arc<AtomicBool>,
    pub title: Arc<Mutex<Option<String>>>,
    pub visible: Arc<AtomicBool>,
    pub cached: Option<Arc<Frame>>,
    /// Read by the PTY event loop on every parse: when true, ED 2 (clear all)
    /// on the primary screen also wipes scrollback. Nothing sets it yet; the
    /// preference UI will. Shared with the loop, so it survives respawn.
    pub clear_wipes_scrollback: Arc<AtomicBool>,
    pub defaults: PaneDefaults,
    /// Copy of `defaults` shared with the PTY loop, read at parse time to
    /// answer OSC 4/10/11/12 color queries. Kept in sync wherever `defaults`
    /// is pushed; survives respawn.
    pub shared_defaults: Arc<Mutex<PaneDefaults>>,
    /// Mirror of the pane's size shared with the EventProxy so XTWINOPS size
    /// queries are answered without locking the terminal. Updated on resize.
    window_size: Arc<Mutex<WindowSize>>,
    /// OSC 52 store waiting for the main thread's clipboard write (drained in
    /// App::logic each frame).
    pub pending_clipboard_store: Arc<Mutex<Option<String>>>,
    pub read_only: bool,
    pub scrollbar_visible: bool,
    /// True once the child process has exited and the pane is being held open
    /// (ExitAction::Hold). Distinct from the transient `exited` event flag:
    /// `exited` fires the Exit *event* once and is consumed by the reaper,
    /// while `dead` is a persistent status. A dead pane's PTY channel is
    /// closed, so input must be a no-op rather than silently dropped.
    dead: Cell<bool>,
    ui_selection: Mutex<Option<Selection>>,
    /// JoinHandle for the live PTY EventLoop thread, kept so the old thread can
    /// be joined (child reaped) before being replaced in `respawn`.
    pty_handle: Option<PtyLoopHandle>,
}

impl Pane {
    pub fn spawn(
        id: PaneId,
        cols: usize,
        lines: usize,
        cell_w: f32,
        cell_h: f32,
        term_config: Config,
        defaults: PaneDefaults,
        winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
        working_dir: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        let window_size = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };

        let proxy = EventProxy::new(winit_proxy, window_size);
        let dirty = Arc::clone(&proxy.dirty);
        let wake_pending = Arc::clone(&proxy.wake_pending);
        let has_new_output = Arc::clone(&proxy.has_new_output);
        let exited = Arc::clone(&proxy.exited);
        let title = Arc::clone(&proxy.title);
        let visible = Arc::clone(&proxy.visible);
        let pty_writer = Arc::clone(&proxy.pty_writer);
        let shared_window_size = Arc::clone(&proxy.window_size);
        let pending_clipboard_store = Arc::clone(&proxy.pending_clipboard_store);

        let dims = TermDims { cols, lines };
        let term = Term::new(term_config, &dims, proxy.clone());
        let terminal = Arc::new(FairMutex::new(term));

        let mut pty_opts = tty::Options::default();
        #[cfg(not(target_os = "macos"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            pty_opts.shell = Some(tty::Shell::new(shell, vec!["--login".into()]));
        }
        pty_opts.env.insert("TERM".into(), "xterm-256color".into());
        pty_opts.env.insert("COLORTERM".into(), "truecolor".into());
        pty_opts.env.insert("TERM_PROGRAM".into(), "rustinator".into());
        pty_opts.env.insert("CLICOLOR".into(), "1".into());
        pty_opts.env.insert("CLICOLOR_FORCE".into(), "1".into());
        crate::shell_integration::inject_env(&mut pty_opts.env);
        ensure_utf8_locale(&mut pty_opts.env);
        if let Some(dir) = working_dir {
            pty_opts.working_directory = Some(dir.to_path_buf());
        } else if let Ok(home) = std::env::var("HOME") {
            // A macOS .app launched from Finder/Dock inherits cwd `/` from launchd.
            // Default a fresh shell (no inherited pane cwd) to $HOME instead of root.
            pty_opts.working_directory = Some(std::path::PathBuf::from(home));
        }
        let pty = tty::new(&pty_opts, window_size, id)
            .map_err(|e| format!("failed to open pty: {e}"))?;
        let child_pid = pty.child().id();
        let clear_wipes_scrollback = Arc::new(AtomicBool::new(defaults.clear_wipes_scrollback));
        let shared_defaults = Arc::new(Mutex::new(defaults));
        let event_loop = EventLoop::new(
            Arc::clone(&terminal),
            proxy,
            pty,
            false,
            false,
            PaneHooks {
                clear_wipes_scrollback: Arc::clone(&clear_wipes_scrollback),
                defaults: Arc::clone(&shared_defaults),
            },
        )
        .map_err(|e| format!("failed to create pty event loop: {e}"))?;
        let pty_tx = event_loop.channel();
        *pty_writer.lock().unwrap() = Some(pty_tx.clone());
        let pty_handle = Some(event_loop.spawn());

        Ok(Self {
            id,
            terminal,
            pty_tx,
            child_pid,
            cols,
            lines,
            dirty,
            wake_pending,
            has_new_output,
            exited,
            title,
            visible,
            cached: None,
            clear_wipes_scrollback,
            defaults,
            shared_defaults,
            window_size: shared_window_size,
            pending_clipboard_store,
            read_only: false,
            scrollbar_visible: true,
            dead: Cell::new(false),
            ui_selection: Mutex::new(None),
            pty_handle,
        })
    }

    pub fn respawn(
        &mut self,
        cell_w: f32,
        cell_h: f32,
        term_config: Config,
        winit_proxy: Option<winit::event_loop::EventLoopProxy<crate::window::UserEvent>>,
    ) -> Result<(), String> {
        // Shut down the old PTY event loop thread before replacing the sender,
        // then join it so the old child is reaped deterministically before we
        // overwrite the pty/terminal (otherwise the thread is detached and the
        // child may linger).
        let _ = self.pty_tx.send(Msg::Shutdown);
        if let Some(handle) = self.pty_handle.take() {
            let _ = handle.join();
        }

        let window_size = WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };

        let proxy = EventProxy::new(winit_proxy, window_size);
        let dirty = Arc::clone(&proxy.dirty);
        let wake_pending = Arc::clone(&proxy.wake_pending);
        let has_new_output = Arc::clone(&proxy.has_new_output);
        let exited = Arc::clone(&proxy.exited);
        let title = Arc::clone(&proxy.title);
        let visible = Arc::clone(&proxy.visible);
        let pty_writer = Arc::clone(&proxy.pty_writer);
        let shared_window_size = Arc::clone(&proxy.window_size);
        let pending_clipboard_store = Arc::clone(&proxy.pending_clipboard_store);

        let dims = TermDims { cols: self.cols, lines: self.lines };
        let term = Term::new(term_config, &dims, proxy.clone());
        let terminal = Arc::new(FairMutex::new(term));

        let mut pty_opts = tty::Options::default();
        #[cfg(not(target_os = "macos"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            pty_opts.shell = Some(tty::Shell::new(shell, vec!["--login".into()]));
        }
        pty_opts.env.insert("TERM".into(), "xterm-256color".into());
        pty_opts.env.insert("COLORTERM".into(), "truecolor".into());
        pty_opts.env.insert("TERM_PROGRAM".into(), "rustinator".into());
        pty_opts.env.insert("CLICOLOR".into(), "1".into());
        pty_opts.env.insert("CLICOLOR_FORCE".into(), "1".into());
        crate::shell_integration::inject_env(&mut pty_opts.env);
        ensure_utf8_locale(&mut pty_opts.env);
        let pty = tty::new(&pty_opts, window_size, self.id)
            .map_err(|e| format!("failed to open pty: {e}"))?;
        let child_pid = pty.child().id();
        let event_loop = EventLoop::new(
            Arc::clone(&terminal),
            proxy,
            pty,
            false,
            false,
            PaneHooks {
                clear_wipes_scrollback: Arc::clone(&self.clear_wipes_scrollback),
                defaults: Arc::clone(&self.shared_defaults),
            },
        )
        .map_err(|e| format!("failed to create pty event loop: {e}"))?;
        let pty_tx = event_loop.channel();
        *pty_writer.lock().unwrap() = Some(pty_tx.clone());
        let pty_handle = Some(event_loop.spawn());

        self.terminal = terminal;
        self.pty_tx = pty_tx;
        self.pty_handle = pty_handle;
        self.child_pid = child_pid;
        self.dirty = dirty;
        self.wake_pending = wake_pending;
        self.has_new_output = has_new_output;
        self.exited = exited;
        self.title = title;
        self.visible = visible;
        self.window_size = shared_window_size;
        self.pending_clipboard_store = pending_clipboard_store;
        self.cached = None;
        self.dead.set(false);
        *self.ui_selection.lock().unwrap() = None;

        Ok(())
    }

    /// Whether the pane's child has exited and the pane is being held open.
    /// A dead pane's PTY channel is closed; input is a no-op.
    #[allow(dead_code)]
    pub fn is_dead(&self) -> bool {
        self.dead.get()
    }

    /// Mark the pane dead (child exited, held open). Idempotent.
    pub fn mark_dead(&self) {
        self.dead.set(true);
    }

    /// Apply updated terminal options (scrollback history, semantic escape
    /// chars, etc.) to the live `Term`. `Term::set_options` replaces the config
    /// and calls `grid.update_history`, so scrollback changes take effect on
    /// the existing grid (the inactive/alt grid updates on its next screen
    /// switch). Marks the pane dirty so the change is reflected on next frame.
    pub fn apply_term_config(&self, term_config: Config) {
        let mut term = self.terminal.lock();
        term.set_options(term_config);
        drop(term);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        pane_cwd(self.child_pid)
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
        self.cols = cols;
        self.lines = lines;
        self.dirty.store(true, Ordering::Release);
        self.cached = None;
        let ws = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };
        *self.window_size.lock().unwrap() = ws;
        let _ = self.pty_tx.send(Msg::Resize(ws));
    }

    pub fn force_pty_resize(&mut self, cell_w: f32, cell_h: f32) {
        let ws = WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.cols as u16,
            cell_width: cell_w.round() as u16,
            cell_height: cell_h.round() as u16,
        };
        *self.window_size.lock().unwrap() = ws;
        let _ = self.pty_tx.send(Msg::Resize(ws));
    }

    pub fn frame(&mut self) -> Arc<Frame> {
        // Re-arm the PTY wake before reading the terminal: output that lands
        // mid-snapshot must send a fresh Repaint event or it would be lost
        // until the next unrelated wake.
        self.wake_pending.store(false, Ordering::Release);
        let was_dirty = self.dirty.swap(false, Ordering::AcqRel);
        if was_dirty || self.cached.is_none() {
            self.cached = Some(Arc::new(self.snapshot()));
        }
        Arc::clone(self.cached.as_ref().unwrap())
    }

    pub fn send_bytes(&self, bytes: Vec<u8>) {
        if bytes.is_empty() || self.dead.get() {
            return;
        }
        let _ = self.pty_tx.send(Msg::Input(bytes.into()));
    }

    /// Encode and send a key event, using Kitty keyboard protocol when the term
    /// has enabled it, otherwise sending the pre-encoded legacy bytes.
    /// When APP_CURSOR (DECCKM) is active, unmodified arrow keys are rewritten
    /// from CSI to SS3 format.
    pub fn send_key(
        &self,
        key: egui::Key,
        mods: egui::Modifiers,
        legacy_bytes: Option<Vec<u8>>,
    ) {
        let mode = *self.terminal.lock().mode();
        if let Some(bytes) = keyboard::encode(key, mods, mode) {
            self.send_bytes(bytes);
            return;
        }
        if let Some(bytes) = legacy_bytes {
            if mode.contains(TermMode::APP_CURSOR) {
                if let Some(app) = decckm_override(key, mods) {
                    self.send_bytes(app);
                    return;
                }
            }
            self.send_bytes(bytes);
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.terminal.lock().mode()
    }

    pub fn send_focus_event(&self, focused: bool) {
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            let seq = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.send_bytes(seq.to_vec());
        }
    }

    pub fn begin_selection(&self, col: i32, row: i32, ty: SelectionType) {
        let mut term = self.terminal.lock();
        let display_offset = term.grid().display_offset() as i32;
        let point = point_from_grid(col, row, display_offset);
        let sel = Selection::new(ty, point, Side::Left);
        term.selection = Some(sel.clone());
        *self.ui_selection.lock().unwrap() = Some(sel);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn update_selection(&self, col: i32, row: i32) {
        let mut term = self.terminal.lock();
        let display_offset = term.grid().display_offset() as i32;
        let point = point_from_grid(col, row, display_offset);
        let mut ui_sel = self.ui_selection.lock().unwrap();
        if let Some(sel) = ui_sel.as_mut() {
            sel.update(point, Side::Right);
            term.selection = Some(sel.clone());
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub fn selection_auto_scroll(&self, delta: i32, cols: i32) {
        let mut term = self.terminal.lock();
        term.scroll_display(Scroll::Delta(delta));
        let display_offset = term.grid().display_offset() as i32;
        let row = if delta > 0 { 0 } else { term.screen_lines() as i32 - 1 };
        let col = if delta > 0 { 0 } else { cols.saturating_sub(1) };
        let point = point_from_grid(col, row, display_offset);
        let mut ui_sel = self.ui_selection.lock().unwrap();
        if let Some(sel) = ui_sel.as_mut() {
            sel.update(point, Side::Right);
            term.selection = Some(sel.clone());
        }
        drop(ui_sel);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn clear_selection(&self) {
        // Lock order: terminal first, then ui_selection (matches begin/update/
        // snapshot) to avoid a deadlock with the PTY thread.
        let mut term = self.terminal.lock();
        *self.ui_selection.lock().unwrap() = None;
        if term.selection.is_some() {
            term.selection = None;
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        // Lock order: terminal first, then ui_selection (matches begin/update/
        // snapshot) to avoid a deadlock with the PTY thread.
        let mut term = self.terminal.lock();
        let sel = self.ui_selection.lock().unwrap().clone();
        if let Some(s) = sel {
            term.selection = Some(s);
        }
        term.selection_to_string()
    }

    pub fn mouse_reporting(&self) -> bool {
        self.terminal
            .lock()
            .mode()
            .intersects(TermMode::MOUSE_MODE)
    }

    pub fn term_mode(&self) -> TermMode {
        *self.terminal.lock().mode()
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

    pub fn scroll_info(&self) -> (usize, usize, usize) {
        let term = self.terminal.lock();
        let offset = term.grid().display_offset();
        let history = term.grid().total_lines().saturating_sub(term.grid().screen_lines());
        let screen = term.grid().screen_lines();
        (offset, history, screen)
    }

    pub fn scroll_to_position(&self, offset: usize) {
        let mut term = self.terminal.lock();
        let current = term.grid().display_offset();
        let delta = offset as i32 - current as i32;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
            self.dirty.store(true, Ordering::Release);
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

    pub fn scroll_to_bottom(&self) {
        let mut term = self.terminal.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
            self.dirty.store(true, Ordering::Release);
        }
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
        let fixed = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self
            .terminal
            .lock()
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        let bytes = if bracketed {
            let mut v = Vec::with_capacity(fixed.len() + 12);
            v.extend_from_slice(b"\x1b[200~");
            v.extend_from_slice(fixed.as_bytes());
            v.extend_from_slice(b"\x1b[201~");
            v
        } else {
            fixed.into_bytes()
        };
        self.send_bytes(bytes);
    }

    pub fn snapshot(&self) -> Frame {
        let mut term = self.terminal.lock();
        let ui_sel = self.ui_selection.lock().unwrap();
        if let Some(sel) = ui_sel.as_ref() {
            term.selection = Some(sel.clone());
        }
        drop(ui_sel);
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

        let cursor_style = term.cursor_style();
        let cursor_blink_requested = cursor_style.blinking;
        let cursor_shape = content.cursor.shape;
        let cursor_visible = cursor_shape != CursorShape::Hidden;
        let cursor_col = content.cursor.point.column.0 as i32;
        let cursor_row = content.cursor.point.line.0 + display_offset;
        let selection = content.selection;

        let cursor_color = {
            let [r, g, b] = self.defaults.cursor;
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        };
        // The snapshot has no focus / blink state, so it cannot know whether a
        // SOLID block cursor is actually being shown for this pane (an
        // unfocused or blink-off block is downgraded to a hollow outline in
        // pane_ui). Inverting the cell here would also invert it when the
        // cursor is hollow / hidden, which is wrong. Instead, the cell under a
        // solid block cursor is inverted at glyph-build time in pane_ui
        // (paint_pane), where focus / blink are known. Keep this false.
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
                CursorShape::Block => Some(CursorOverlay::Block {
                    col: cursor_col,
                    row: cursor_row,
                    color: cursor_color,
                }),
                CursorShape::HollowBlock => {
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
            let flags = indexed.cell.flags;
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if flags.contains(Flags::DIM) {
                fg[0] *= 0.66;
                fg[1] *= 0.66;
                fg[2] *= 0.66;
            }
            let is_cursor = cursor_invert_at_cell && col == cursor_col && row == cursor_row;
            let is_selected = selection
                .map(|s| s.contains(indexed.point))
                .unwrap_or(false);
            if is_selected && !is_cursor {
                // Profile selection colors when set; classic invert otherwise.
                match self.defaults.selection_bg {
                    Some([r, g, b]) => {
                        bg = rgb_to_f32(r, g, b);
                        if let Some([r, g, b]) = self.defaults.selection_fg {
                            fg = rgb_to_f32(r, g, b);
                        }
                    }
                    None => std::mem::swap(&mut fg, &mut bg),
                }
            } else if is_cursor {
                std::mem::swap(&mut fg, &mut bg);
            }
            let style = match (flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC)) {
                (true, true) => FontStyle::BoldItalic,
                (true, false) => FontStyle::Bold,
                (false, true) => FontStyle::Italic,
                (false, false) => FontStyle::Regular,
            };
            let underline = underline_from_flags(flags);
            let strikeout = flags.contains(Flags::STRIKEOUT);
            // SGR 58 underline color (cell.underline_color) when set, else the
            // cell's resolved fg so the decoration tracks the glyph through
            // inverse/dim/selection swaps.
            let underline_color = match indexed.cell.underline_color() {
                Some(c) => {
                    let mut uc = resolve_color(c, palette, true, &self.defaults);
                    if flags.contains(Flags::DIM) {
                        uc[0] *= 0.66;
                        uc[1] *= 0.66;
                        uc[2] *= 0.66;
                    }
                    uc
                }
                None => fg,
            };
            cells.push(CellSnapshot {
                col,
                row,
                c: indexed.cell.c,
                fg,
                bg,
                style,
                underline,
                underline_color,
                strikeout,
                wide: flags.contains(Flags::WIDE_CHAR),
                hidden: flags.contains(Flags::HIDDEN),
                // Combining marks / joiners alacritty parked on this cell.
                // Allocates only when present (clusters are rare).
                zerowidth: extract_zerowidth(indexed.cell.zerowidth()),
            });
        }

        Frame {
            cells,
            default_bg,
            cursor: cursor_overlay,
            cursor_blink_requested,
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.pty_tx.send(Msg::Shutdown);
        if let Some(handle) = self.pty_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Treat empty inherited locale strings as unset (the real env here has
/// LANG/LC_ALL/LC_CTYPE set-but-empty, which POSIX treats as unset).
fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// True when a locale string designates a UTF-8 codeset, case-insensitively.
fn locale_is_utf8(locale: &str) -> bool {
    let lower = locale.to_ascii_lowercase();
    let codeset = lower.rsplit('.').next().unwrap_or(&lower);
    codeset == "utf-8" || codeset == "utf8" || lower.contains("utf-8") || lower.contains("utf8")
}

/// Given the inherited LC_ALL / LC_CTYPE / LANG values (None = unset/empty)
/// and a UTF-8 fallback locale (e.g. "en_US.UTF-8"), decide what to inject.
/// Returns None when the effective ctype locale is already UTF-8 (do nothing).
/// Returns Some(locale) to set as LANG when the effective locale is not UTF-8.
///
/// Design: caller-validates. This fn is pure (no setlocale); when the effective
/// locale is non-UTF-8 it returns the language-preserving candidate
/// ("<lang>.UTF-8"); the caller probes it with setlocale and falls back if invalid.
fn choose_utf8_locale(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
    fallback: &str,
) -> Option<String> {
    // POSIX ctype precedence: LC_ALL > LC_CTYPE > LANG.
    let effective = non_empty(lc_all)
        .or_else(|| non_empty(lc_ctype))
        .or_else(|| non_empty(lang));

    match effective {
        None => Some(fallback.to_string()),
        Some(loc) if locale_is_utf8(loc) => None,
        Some(loc) => {
            // Preserve language/territory; drop codeset and modifier, force UTF-8.
            let lang_part = loc.split('.').next().unwrap_or(loc);
            let lang_part = lang_part.split('@').next().unwrap_or(lang_part);
            Some(format!("{lang_part}.UTF-8"))
        }
    }
}

/// Probe whether `candidate` is a valid LC_CTYPE locale without disturbing our
/// own process locale: save the current locale, try the candidate, then restore.
fn locale_is_valid(candidate: &str) -> bool {
    use std::ffi::CString;
    let Ok(c_candidate) = CString::new(candidate) else { return false };
    unsafe {
        // setlocale(.., null) returns the current locale in a static buffer; copy
        // it immediately before the next setlocale call clobbers that buffer.
        let saved_ptr = libc::setlocale(libc::LC_CTYPE, std::ptr::null());
        let saved = if saved_ptr.is_null() {
            None
        } else {
            Some(std::ffi::CStr::from_ptr(saved_ptr).to_owned())
        };

        let ok = !libc::setlocale(libc::LC_CTYPE, c_candidate.as_ptr()).is_null();

        // Restore the locale we found on entry.
        if let Some(saved) = saved {
            libc::setlocale(libc::LC_CTYPE, saved.as_ptr());
        }
        ok
    }
}

/// Inject a UTF-8 ctype locale into the child env when the inherited locale is
/// not already UTF-8. Standard terminal behavior (see iTerm2 "Set locale
/// variables automatically", alacritty macOS locale fix) so vim/less/etc don't
/// fall back to latin1 and mangle multibyte input.
fn ensure_utf8_locale(env: &mut std::collections::HashMap<String, String>) {
    let lc_all = std::env::var("LC_ALL").ok();
    let lc_ctype = std::env::var("LC_CTYPE").ok();
    let lang = std::env::var("LANG").ok();

    let lc_all_eff = non_empty(lc_all.as_deref());
    let lc_ctype_eff = non_empty(lc_ctype.as_deref());
    let lang_eff = non_empty(lang.as_deref());

    // Platform fallback: macOS derives from NSLocale; glibc/musl ship C.UTF-8.
    #[cfg(target_os = "macos")]
    let fallback = crate::platform::preferred_locale_id()
        .map(|id| format!("{id}.UTF-8"))
        .filter(|loc| locale_is_valid(loc))
        .unwrap_or_else(|| "en_US.UTF-8".to_string());
    #[cfg(not(target_os = "macos"))]
    let fallback = {
        if locale_is_valid("C.UTF-8") {
            "C.UTF-8".to_string()
        } else {
            "en_US.UTF-8".to_string()
        }
    };

    let Some(candidate) = choose_utf8_locale(lc_all_eff, lc_ctype_eff, lang_eff, &fallback) else {
        return;
    };

    // Validate the candidate; fall back to the platform fallback, then C.UTF-8.
    let chosen = if locale_is_valid(&candidate) {
        candidate
    } else if locale_is_valid(&fallback) {
        fallback
    } else if locale_is_valid("C.UTF-8") {
        "C.UTF-8".to_string()
    } else {
        // Nothing usable — leave the env untouched rather than guess.
        return;
    };

    env.insert("LANG".into(), chosen.clone());

    // A non-UTF-8 LC_ALL would override LANG and force latin1; overriding it to
    // the UTF-8 variant of the same locale is the only way to fix encoding.
    if lc_all_eff.map(|v| !locale_is_utf8(v)).unwrap_or(false) {
        env.insert("LC_ALL".into(), chosen.clone());
    } else if lc_all_eff.is_none()
        && lc_ctype_eff.map(|v| !locale_is_utf8(v)).unwrap_or(false)
    {
        // LC_CTYPE (no LC_ALL) would override LANG for ctype; align it too.
        env.insert("LC_CTYPE".into(), chosen);
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
fn pane_cwd(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
fn pane_cwd(pid: u32) -> Option<std::path::PathBuf> {
    use std::process::Command;
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-Fn", "-d", "cwd"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(path) = line.strip_prefix('n') {
            return Some(std::path::PathBuf::from(path));
        }
    }
    None
}

#[cfg(not(any(
    target_os = "linux", target_os = "freebsd", target_os = "netbsd",
    target_os = "openbsd", target_os = "macos"
)))]
fn pane_cwd(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// When DECCKM (application cursor mode) is active, unmodified cursor keys
/// use SS3 format instead of CSI. Only applies without modifiers — modified
/// keys always use CSI 1;{mod} format which is already correct.
fn decckm_override(key: egui::Key, mods: egui::Modifiers) -> Option<Vec<u8>> {
    if mods.shift || mods.alt || mods.ctrl || mods.mac_cmd {
        return None;
    }
    let seq: &[u8] = match key {
        egui::Key::ArrowUp => b"\x1bOA",
        egui::Key::ArrowDown => b"\x1bOB",
        egui::Key::ArrowRight => b"\x1bOC",
        egui::Key::ArrowLeft => b"\x1bOD",
        egui::Key::Home => b"\x1bOH",
        egui::Key::End => b"\x1bOF",
        _ => return None,
    };
    Some(seq.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FB: &str = "en_US.UTF-8";

    #[test]
    fn zerowidth_extraction_preserves_order_and_empty() {
        // No extras -> empty Vec, no allocation path.
        assert!(extract_zerowidth(None).is_empty());
        // e + COMBINING ACUTE -> carried in order on top of the base scalar.
        let combining = ['\u{0301}'];
        assert_eq!(extract_zerowidth(Some(&combining)), vec!['\u{0301}']);
        // Emoji ZWJ family: base scalar is the cell's `c`; the joiner +
        // following scalars are the zerowidth list, kept in source order.
        let zwj = ['\u{200D}', '\u{1F469}', '\u{200D}', '\u{1F467}'];
        assert_eq!(
            extract_zerowidth(Some(&zwj)),
            vec!['\u{200D}', '\u{1F469}', '\u{200D}', '\u{1F467}'],
        );
    }

    #[test]
    fn snapshot_cell_carries_zerowidth_cluster() {
        // A CellSnapshot built with extras exposes base + zerowidth in order,
        // so a downstream consumer (paint overlay, future copy path) sees the
        // full grapheme cluster, not just the first scalar.
        let cell = CellSnapshot {
            col: 0,
            row: 0,
            c: 'e',
            fg: [1.0; 4],
            bg: [0.0, 0.0, 0.0, 1.0],
            style: FontStyle::Regular,
            underline: Underline::None,
            underline_color: [1.0; 4],
            strikeout: false,
            wide: false,
            hidden: false,
            zerowidth: extract_zerowidth(Some(&['\u{0301}'])),
        };
        let cluster: Vec<char> = std::iter::once(cell.c).chain(cell.zerowidth.iter().copied()).collect();
        assert_eq!(cluster, vec!['e', '\u{0301}']);
    }

    #[test]
    fn underline_flag_mapping_and_precedence() {
        assert_eq!(underline_from_flags(Flags::empty()), Underline::None);
        assert_eq!(underline_from_flags(Flags::UNDERLINE), Underline::Single);
        assert_eq!(
            underline_from_flags(Flags::DOUBLE_UNDERLINE),
            Underline::Double
        );
        assert_eq!(underline_from_flags(Flags::UNDERCURL), Underline::Curly);
        assert_eq!(
            underline_from_flags(Flags::DOTTED_UNDERLINE),
            Underline::Dotted
        );
        assert_eq!(
            underline_from_flags(Flags::DASHED_UNDERLINE),
            Underline::Dashed
        );
        // Precedence: undercurl wins over a co-set single underline.
        assert_eq!(
            underline_from_flags(Flags::UNDERCURL | Flags::UNDERLINE),
            Underline::Curly
        );
        // Strikeout is independent of underline style.
        assert_eq!(underline_from_flags(Flags::STRIKEOUT), Underline::None);
    }

    #[test]
    fn locale_all_empty_uses_fallback() {
        assert_eq!(
            choose_utf8_locale(None, None, None, FB),
            Some(FB.to_string())
        );
    }

    #[test]
    fn locale_empty_strings_treated_as_unset() {
        assert_eq!(
            choose_utf8_locale(Some(""), Some(""), Some(""), FB),
            Some(FB.to_string())
        );
    }

    #[test]
    fn locale_lang_utf8_returns_none() {
        assert_eq!(choose_utf8_locale(None, None, Some("en_US.UTF-8"), FB), None);
    }

    #[test]
    fn locale_lang_utf8_lowercase_spelling_returns_none() {
        // case/spelling: "en_US.utf8" is already UTF-8.
        assert_eq!(choose_utf8_locale(None, None, Some("en_US.utf8"), FB), None);
    }

    #[test]
    fn locale_lc_all_c_gets_utf8_variant() {
        assert_eq!(
            choose_utf8_locale(Some("C"), None, None, FB),
            Some("C.UTF-8".to_string())
        );
    }

    #[test]
    fn locale_lc_ctype_latin1_preserves_language() {
        assert_eq!(
            choose_utf8_locale(None, Some("en_US.ISO8859-1"), None, FB),
            Some("en_US.UTF-8".to_string())
        );
    }

    #[test]
    fn locale_strips_codeset_and_modifier() {
        assert_eq!(
            choose_utf8_locale(None, Some("en_US.ISO8859-1@euro"), None, FB),
            Some("en_US.UTF-8".to_string())
        );
    }

    #[test]
    fn locale_lc_all_precedence_beats_lang() {
        // LC_ALL=C (non-UTF-8) wins over a UTF-8 LANG.
        assert_eq!(
            choose_utf8_locale(Some("C"), None, Some("en_US.UTF-8"), FB),
            Some("C.UTF-8".to_string())
        );
        // And a UTF-8 LC_ALL wins over a non-UTF-8 LANG -> no change.
        assert_eq!(
            choose_utf8_locale(Some("en_US.UTF-8"), None, Some("C"), FB),
            None
        );
    }

    #[test]
    fn locale_is_utf8_recognizes_variants() {
        assert!(locale_is_utf8("C.UTF-8"));
        assert!(locale_is_utf8("en_US.UTF-8"));
        assert!(locale_is_utf8("en_US.utf8"));
        assert!(locale_is_utf8("de_DE.UTF-8@euro"));
        assert!(!locale_is_utf8("C"));
        assert!(!locale_is_utf8("en_US.ISO8859-1"));
    }

    fn make_cells(text: &str, row: i32) -> Vec<CellSnapshot> {
        text.chars()
            .enumerate()
            .map(|(i, c)| CellSnapshot {
                col: i as i32,
                row,
                c,
                fg: [1.0, 1.0, 1.0, 1.0],
                bg: [0.0, 0.0, 0.0, 1.0],
                style: FontStyle::Regular,
                underline: Underline::None,
                underline_color: [1.0, 1.0, 1.0, 1.0],
                strikeout: false,
                wide: false,
                hidden: false,
                zerowidth: Vec::new(),
            })
            .collect()
    }

    fn wide_cell(col: i32, row: i32, c: char, wide: bool) -> CellSnapshot {
        CellSnapshot {
            col,
            row,
            c,
            fg: [1.0; 4],
            bg: [0.0, 0.0, 0.0, 1.0],
            style: FontStyle::Regular,
            underline: Underline::None,
            underline_color: [1.0; 4],
            strikeout: false,
            wide,
            hidden: false,
            zerowidth: Vec::new(),
        }
    }

    #[test]
    fn resolve_wide_click_maps_spacer_to_base() {
        // Row 0: 'A' (col 0), wide '中' base (col 1), spacer ' ' (col 2), 'B' (col 3).
        let cells = vec![
            wide_cell(0, 0, 'A', false),
            wide_cell(1, 0, '中', true),
            wide_cell(2, 0, ' ', false),
            wide_cell(3, 0, 'B', false),
        ];
        // Click on the base column stays put.
        assert_eq!(resolve_wide_click_col(&cells, 0, 1), 1);
        // Click on the spacer (right half) resolves to the base column.
        assert_eq!(resolve_wide_click_col(&cells, 0, 2), 1);
        // Narrow columns are unaffected.
        assert_eq!(resolve_wide_click_col(&cells, 0, 0), 0);
        assert_eq!(resolve_wide_click_col(&cells, 0, 3), 3);
        // A wide cell on another row must not bleed across rows.
        assert_eq!(resolve_wide_click_col(&cells, 1, 2), 2);
    }

    #[test]
    fn scan_urls_finds_https() {
        let cells = make_cells("visit https://example.com for info", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
    }

    #[test]
    fn scan_urls_finds_http() {
        let cells = make_cells("see http://example.org/path?q=1 now", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.starts_with("http://example.org"));
    }

    #[test]
    fn scan_urls_none_in_plain_text() {
        let cells = make_cells("nothing special here", 0);
        let urls = scan_urls(&cells);
        assert!(urls.is_empty());
    }

    // ── Gap inventory guardrails ──────────────────────────────────────

    // Gap #3 (partial): email address detection
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect email addresses"]
    fn scan_urls_detects_email() {
        let cells = make_cells("contact user@example.com for help", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.contains("user@example.com"));
    }

    // Gap #3 (partial): mailto: URI
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect mailto: URIs"]
    fn scan_urls_detects_mailto() {
        let cells = make_cells("send to mailto:user@example.com now", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.starts_with("mailto:"));
    }

    // Gap #3 (partial): file:// URI
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect file:// URIs"]
    fn scan_urls_detects_file_uri() {
        let cells = make_cells("open file:///home/user/doc.txt please", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.starts_with("file:///"));
    }

    // Gap #3 (partial): ssh:// URI
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect ssh:// URIs"]
    fn scan_urls_detects_ssh_uri() {
        let cells = make_cells("connect via ssh://user@host.com:22", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.starts_with("ssh://"));
    }

    // Gap #3 (partial): ftp:// URI
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect ftp:// URIs"]
    fn scan_urls_detects_ftp_uri() {
        let cells = make_cells("download from ftp://files.example.com/pub", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.starts_with("ftp://"));
    }

    // Gap #3 (partial): bare domain (www.example.com)
    #[test]
    #[ignore = "gap #3 partial: scan_urls doesn't detect bare www. domains"]
    fn scan_urls_detects_bare_www() {
        let cells = make_cells("visit www.example.com for info", 0);
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].url.contains("www.example.com"));
    }

    // Gap #42: OSC-8 hyperlinks
    #[test]
    #[ignore = "gap #42: OSC-8 hyperlink support not yet implemented"]
    fn url_match_has_hyperlink_flag() {
        panic!("add is_hyperlink: bool to UrlMatch for OSC-8 support");
    }

    // ---- DECCKM (application cursor mode) ----

    fn no_mods() -> egui::Modifiers {
        egui::Modifiers::default()
    }

    #[test]
    fn decckm_arrow_up() {
        assert_eq!(decckm_override(egui::Key::ArrowUp, no_mods()).unwrap(), b"\x1bOA");
    }

    #[test]
    fn decckm_arrow_down() {
        assert_eq!(decckm_override(egui::Key::ArrowDown, no_mods()).unwrap(), b"\x1bOB");
    }

    #[test]
    fn decckm_arrow_right() {
        assert_eq!(decckm_override(egui::Key::ArrowRight, no_mods()).unwrap(), b"\x1bOC");
    }

    #[test]
    fn decckm_arrow_left() {
        assert_eq!(decckm_override(egui::Key::ArrowLeft, no_mods()).unwrap(), b"\x1bOD");
    }

    #[test]
    fn decckm_home() {
        assert_eq!(decckm_override(egui::Key::Home, no_mods()).unwrap(), b"\x1bOH");
    }

    #[test]
    fn decckm_end() {
        assert_eq!(decckm_override(egui::Key::End, no_mods()).unwrap(), b"\x1bOF");
    }

    #[test]
    fn decckm_ignores_modified_keys() {
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(decckm_override(egui::Key::ArrowUp, ctrl), None);

        let shift = egui::Modifiers { shift: true, ..Default::default() };
        assert_eq!(decckm_override(egui::Key::ArrowUp, shift), None);

        let alt = egui::Modifiers { alt: true, ..Default::default() };
        assert_eq!(decckm_override(egui::Key::ArrowUp, alt), None);
    }

    #[test]
    fn decckm_ignores_non_cursor_keys() {
        assert_eq!(decckm_override(egui::Key::A, no_mods()), None);
        assert_eq!(decckm_override(egui::Key::Enter, no_mods()), None);
        assert_eq!(decckm_override(egui::Key::F1, no_mods()), None);
    }

    // ---- escape_regex ----

    #[test]
    fn escape_regex_plain_text() {
        assert_eq!(escape_regex("hello"), "hello");
    }

    #[test]
    fn escape_regex_special_chars() {
        assert_eq!(escape_regex("."), "\\.");
        assert_eq!(escape_regex("+"), "\\+");
        assert_eq!(escape_regex("*"), "\\*");
        assert_eq!(escape_regex("?"), "\\?");
        assert_eq!(escape_regex("("), "\\(");
        assert_eq!(escape_regex(")"), "\\)");
        assert_eq!(escape_regex("|"), "\\|");
        assert_eq!(escape_regex("["), "\\[");
        assert_eq!(escape_regex("]"), "\\]");
        assert_eq!(escape_regex("{"), "\\{");
        assert_eq!(escape_regex("}"), "\\}");
        assert_eq!(escape_regex("^"), "\\^");
        assert_eq!(escape_regex("$"), "\\$");
        assert_eq!(escape_regex("\\"), "\\\\");
        assert_eq!(escape_regex("/"), "\\/");
    }

    #[test]
    fn escape_regex_mixed() {
        assert_eq!(escape_regex("a.b*c"), "a\\.b\\*c");
    }

    #[test]
    fn escape_regex_empty() {
        assert_eq!(escape_regex(""), "");
    }

    // ---- is_url_boundary ----

    #[test]
    fn url_boundary_whitespace() {
        assert!(is_url_boundary(' '));
        assert!(is_url_boundary('\t'));
        assert!(is_url_boundary('\n'));
    }

    #[test]
    fn url_boundary_delimiters() {
        for c in [')', ']', '"', '\'', '>', '<', '{', '}', ',', ';', '|', '`'] {
            assert!(is_url_boundary(c), "expected '{}' to be a boundary", c);
        }
    }

    #[test]
    fn url_boundary_non_boundary() {
        assert!(!is_url_boundary('a'));
        assert!(!is_url_boundary('/'));
        assert!(!is_url_boundary(':'));
        assert!(!is_url_boundary('.'));
        assert!(!is_url_boundary('-'));
        assert!(!is_url_boundary('_'));
    }

    // ---- scan_urls ----

    fn cells_from_str(row: i32, text: &str) -> Vec<CellSnapshot> {
        text.chars()
            .enumerate()
            .map(|(i, c)| CellSnapshot {
                col: i as i32,
                row,
                c,
                fg: [1.0; 4],
                bg: [0.0, 0.0, 0.0, 1.0],
                style: FontStyle::Regular,
                underline: Underline::None,
                underline_color: [1.0; 4],
                strikeout: false,
                wide: false,
                hidden: false,
                zerowidth: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn scan_urls_https() {
        let cells = cells_from_str(0, "visit https://example.com today");
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[0].row, 0);
        assert_eq!(urls[0].start_col, 6);
        assert_eq!(urls[0].end_col, 25);
    }

    #[test]
    fn scan_urls_http() {
        let cells = cells_from_str(0, "http://foo.bar/baz");
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "http://foo.bar/baz");
    }

    #[test]
    fn scan_urls_multiple_on_row() {
        let cells = cells_from_str(0, "https://a.com and https://b.com");
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].url, "https://a.com");
        assert_eq!(urls[1].url, "https://b.com");
    }

    #[test]
    fn scan_urls_no_url() {
        let cells = cells_from_str(0, "nothing here");
        assert!(scan_urls(&cells).is_empty());
    }

    #[test]
    fn scan_urls_bare_protocol_no_content() {
        let cells = cells_from_str(0, "http:// ");
        assert!(scan_urls(&cells).is_empty());
    }

    #[test]
    fn scan_urls_stops_at_boundary() {
        let cells = cells_from_str(0, "(https://x.com)");
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://x.com");
    }

    #[test]
    fn scan_urls_multiple_rows() {
        let mut cells = cells_from_str(0, "https://row0.com");
        cells.extend(cells_from_str(1, "https://row1.com"));
        let urls = scan_urls(&cells);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].row, 0);
        assert_eq!(urls[1].row, 1);
    }

    #[test]
    fn scan_urls_empty_cells() {
        assert!(scan_urls(&[]).is_empty());
    }

    // ---- indexed_default (256-color palette) ----

    #[test]
    fn indexed_default_ansi_range() {
        assert_eq!(indexed_default(0, &PaneDefaults::default()), [0x00, 0x00, 0x00]);
        assert_eq!(indexed_default(1, &PaneDefaults::default()), [0xcd, 0x00, 0x00]);
        assert_eq!(indexed_default(15, &PaneDefaults::default()), ANSI[15]);
    }

    #[test]
    fn indexed_default_cube_start() {
        // Index 16 = rgb(0,0,0) in the 6x6x6 cube.
        assert_eq!(indexed_default(16, &PaneDefaults::default()), [0, 0, 0]);
    }

    #[test]
    fn indexed_default_cube_white() {
        // Index 231 = rgb(5,5,5) = (0xff, 0xff, 0xff).
        assert_eq!(indexed_default(231, &PaneDefaults::default()), [0xff, 0xff, 0xff]);
    }

    #[test]
    fn indexed_default_cube_mid() {
        // Index 196 = n=180, r=180/36=5 -> 0xff, g=(180/6)%6=0 -> 0, b=180%6=0 -> 0.
        assert_eq!(indexed_default(196, &PaneDefaults::default()), [0xff, 0x00, 0x00]);
    }

    #[test]
    fn indexed_default_grayscale_start() {
        // Index 232 = 8 + 0*10 = 8.
        assert_eq!(indexed_default(232, &PaneDefaults::default()), [8, 8, 8]);
    }

    #[test]
    fn indexed_default_grayscale_end() {
        // Index 255 = 8 + 23*10 = 238.
        assert_eq!(indexed_default(255, &PaneDefaults::default()), [238, 238, 238]);
    }

    // ---- point_from_grid ----

    #[test]
    fn point_from_grid_basic() {
        let p = point_from_grid(5, 3, 0);
        assert_eq!(p.line, Line(3));
        assert_eq!(p.column, Column(5));
    }

    #[test]
    fn point_from_grid_with_display_offset() {
        let p = point_from_grid(0, 3, 10);
        assert_eq!(p.line, Line(-7));
    }

    #[test]
    fn point_from_grid_negative_col_clamps() {
        let p = point_from_grid(-1, 0, 0);
        assert_eq!(p.column, Column(0));
    }
}
