//! Hang watchdog: notices when the UI thread stops returning to the event
//! loop and leaves one report in the log saying where it is stuck and what
//! the other threads are doing.
//!
//! The UI thread only stores to atomics. `ui_busy` (winit's `new_events`)
//! and `ui_idle` (the end of `about_to_wait`, right before winit waits) keep
//! one word, `UI_STATE`: an iteration counter and a busy bit. Waiting for
//! events is not a stall; one iteration staying busy is. `phase` pushes a
//! label (and a number: a pane id, a byte count) on a small stack before the
//! steps that could block, and pops it when the step ends.
//!
//! Other threads mark their long or blocking sections (the PTY loop holding
//! the terminal, `TermGate::hold`, the reaper freeing a grid, the clipboard
//! read, the PTY spawn) with `activity`, in a fixed table of slots, one per
//! thread, claimed on first use and freed when the thread exits. Each slot
//! records the thread id, so a report can match it to `/proc/self/task`.
//!
//! The "watchdog" thread wakes every `TICK` and reads `UI_STATE`; nothing
//! else unless the same busy iteration is still running. At `ARM_MS` it
//! takes a baseline of the page-fault counters (Linux), at `REPORT_MS` it
//! writes one report (`log::warn!`, one record), then one line every
//! `FOLLOW_UP_MS` while the stall lasts and one when it ends. Durations are
//! measured on the watchdog's own clock, from the first tick that saw the
//! iteration busy, so they read up to one tick short. The first iteration
//! (startup, during which the watchdog starts) is not watched.
//!
//! The decisions are in `Detector` (fed a clock and samples), the text in
//! `format_*` (fed plain data), so both are tested without threads or time.

use std::cell::Cell;
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use winit::event::WindowEvent;

/// How often the watchdog thread looks at the UI thread.
const TICK: Duration = Duration::from_millis(250);
/// Busy this long: take the counter baseline the report's deltas start from.
const ARM_MS: u64 = 500;
/// Busy this long: write the report.
const REPORT_MS: u64 = 2_000;
/// Then one line this often while the stall lasts.
const FOLLOW_UP_MS: u64 = 10_000;
/// The iterations not watched: the first (winit's `Init` wake and `resumed`:
/// window, GL context and font setup, slow on a software rasterizer), which
/// is running when the watchdog starts.
const STARTUP_ITERATION: u64 = 1;

/// Phase stack depth kept; deeper phases are counted, not recorded.
const PHASE_DEPTH: usize = 8;
/// Activity slots: one per thread that ever marks an activity, while it lives.
const SLOTS: usize = 64;

/// Report layout limits, which keep it within `REPORT_MAX_LINES`.
const SLOT_LINES: usize = 8;
const THREAD_LINES: usize = 22;
#[cfg(test)]
const REPORT_MAX_LINES: usize = 40;

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// How a label's number reads in a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    None,
    Pane,
    Tab,
    Pid,
    Lines,
    Bytes,
    Event,
    UserEvent,
    Selection,
    Hold,
}

impl Detail {
    fn describe(self, value: u64) -> Option<String> {
        Some(match self {
            Detail::None => return None,
            Detail::Pane => format!("pane {value}"),
            Detail::Tab => format!("tab {value}"),
            Detail::Pid => format!("pid {value}"),
            Detail::Lines => format!("{value} lines"),
            Detail::Bytes => format!("{value} bytes"),
            Detail::Event => EVENT_NAMES.get(value as usize).copied().unwrap_or("?").to_string(),
            Detail::UserEvent => match value {
                USER_EVENT_REPAINT => "Repaint",
                USER_EVENT_CLIPBOARD => "ClipboardRead",
                USER_EVENT_HOTKEY => "HotkeyToggle",
                _ => "?",
            }
            .to_string(),
            Detail::Selection => (if value == SELECTION_PRIMARY { "PRIMARY" } else { "CLIPBOARD" }).to_string(),
            Detail::Hold => (if value == HOLD_REFLOW { "reflow" } else { "search" }).to_string(),
        })
    }
}

macro_rules! labels {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => ($text:literal, $detail:ident)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum $name { $($variant),+ }

        impl $name {
            const ALL: &'static [$name] = &[$($name::$variant),+];

            pub(crate) fn label(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            fn detail(self) -> Detail {
                match self { $($name::$variant => Detail::$detail),+ }
            }

            /// Never 0, which marks an empty word.
            fn code(self) -> u64 {
                self as u64 + 1
            }

            fn from_code(code: u64) -> Option<Self> {
                (code as usize).checked_sub(1).and_then(|i| Self::ALL.get(i).copied())
            }
        }
    };
}

labels! {
    /// What the UI thread is doing (`phase`).
    Phase {
        UserEvent => ("user event", UserEvent),
        MainEvent => ("main window", Event),
        PrefsEvent => ("prefs window", Event),
        HotkeyEvent => ("hotkey window", Event),
        AboutToWait => ("about_to_wait", None),
        BeginFrame => ("begin_frame (make current)", None),
        Logic => ("logic", None),
        Ui => ("ui", None),
        PaneResize => ("pane resize", Pane),
        PaneFrame => ("pane snapshot", Pane),
        EndFrame => ("end_frame", None),
        PlatformOutput => ("platform output", None),
        Tessellate => ("tessellate", None),
        GlPaint => ("GL paint", None),
        SwapBuffers => ("swap_buffers", None),
        PrefsPaint => ("prefs paint", None),
        HotkeyPaint => ("hotkey paint", None),
        ClosePane => ("close pane", Pane),
        CloseTab => ("close tab", Tab),
        ReapExited => ("reap exited panes", None),
        Respawn => ("respawn", Pane),
        Spawn => ("spawn pane", Pane),
        ClipboardRead => ("clipboard read", Selection),
        Copy => ("copy selection", Pane),
        PrimaryWrite => ("write PRIMARY", Bytes),
        Paste => ("send paste", Bytes),
        PrefsApply => ("prefs apply", None),
        ConfigSave => ("config save", None),
        FontChange => ("font change", None),
        LayoutRestore => ("layout restore", None),
        Search => ("search start", None),
        LaunchProcess => ("launch process", None),
        WindowSetup => ("window create/destroy", None),
    }
}

labels! {
    /// What another thread is doing (`activity`).
    Activity {
        PtyRead => ("pty_read (terminal lease/lock)", Pid),
        SyncFlush => ("sync flush (terminal lock)", Pid),
        PtyExit => ("child exit (terminal lock)", Pid),
        PtyReap => ("Pty drop (SIGHUP, wait)", Pid),
        GateWait => ("TermGate::hold waiting", Hold),
        GateHoldSearch => ("TermGate::hold search", Lines),
        GateHoldReflow => ("TermGate::hold reflow", Lines),
        ReaperDrop => ("reaper dropping a grid", None),
        ClipboardRead => ("clipboard read", Selection),
        PtySpawn => ("PTY spawn (openpty/fork/exec)", None),
    }
}

/// `Phase::UserEvent` details.
pub(crate) const USER_EVENT_REPAINT: u64 = 0;
pub(crate) const USER_EVENT_CLIPBOARD: u64 = 1;
pub(crate) const USER_EVENT_HOTKEY: u64 = 2;
/// `Detail::Selection` values.
pub(crate) const SELECTION_CLIPBOARD: u64 = 0;
pub(crate) const SELECTION_PRIMARY: u64 = 1;
/// `Activity::GateWait` details.
pub(crate) const HOLD_SEARCH: u64 = 0;
pub(crate) const HOLD_REFLOW: u64 = 1;

const EVENT_NAMES: &[&str] = &[
    "other",
    "Resized",
    "Moved",
    "CloseRequested",
    "Destroyed",
    "DroppedFile",
    "Focused",
    "KeyboardInput",
    "ModifiersChanged",
    "Ime",
    "CursorMoved",
    "CursorEntered",
    "CursorLeft",
    "MouseWheel",
    "MouseInput",
    "ScaleFactorChanged",
    "ThemeChanged",
    "Occluded",
    "RedrawRequested",
];

/// A window event's `Phase::*Event` detail.
pub(crate) fn event_code(event: &WindowEvent) -> u64 {
    match event {
        WindowEvent::Resized(_) => 1,
        WindowEvent::Moved(_) => 2,
        WindowEvent::CloseRequested => 3,
        WindowEvent::Destroyed => 4,
        WindowEvent::DroppedFile(_) => 5,
        WindowEvent::Focused(_) => 6,
        WindowEvent::KeyboardInput { .. } => 7,
        WindowEvent::ModifiersChanged(_) => 8,
        WindowEvent::Ime(_) => 9,
        WindowEvent::CursorMoved { .. } => 10,
        WindowEvent::CursorEntered { .. } => 11,
        WindowEvent::CursorLeft { .. } => 12,
        WindowEvent::MouseWheel { .. } => 13,
        WindowEvent::MouseInput { .. } => 14,
        WindowEvent::ScaleFactorChanged { .. } => 15,
        WindowEvent::ThemeChanged(_) => 16,
        WindowEvent::Occluded(_) => 17,
        WindowEvent::RedrawRequested => 18,
        _ => 0,
    }
}

/// A label and its number in one word: the code in the top byte.
const DETAIL_MASK: u64 = (1 << 56) - 1;

fn pack(code: u64, detail: u64) -> u64 {
    (code << 56) | (detail & DETAIL_MASK)
}

fn unpack(word: u64) -> (u64, u64) {
    (word >> 56, word & DETAIL_MASK)
}

fn describe_word<L: Copy>(word: u64, from_code: fn(u64) -> Option<L>, text: fn(L) -> (&'static str, Detail)) -> String {
    let (code, value) = unpack(word);
    match from_code(code) {
        Some(label) => {
            let (name, detail) = text(label);
            match detail.describe(value) {
                Some(d) => format!("{name}: {d}"),
                None => name.to_string(),
            }
        }
        None => "?".to_string(),
    }
}

fn describe_phase(word: u64) -> String {
    describe_word(word, Phase::from_code, |p| (p.label(), p.detail()))
}

fn describe_activity(word: u64) -> String {
    describe_word(word, Activity::from_code, |a| (a.label(), a.detail()))
}

// ---------------------------------------------------------------------------
// UI-thread instrumentation
// ---------------------------------------------------------------------------

/// `(iteration << 1) | busy`. Written by the UI thread only.
static UI_STATE: AtomicU64 = AtomicU64::new(0);
static UI_PHASES: PhaseStack = PhaseStack::new();
/// Published by the UI thread each frame (`publish_panes`).
static PANES: AtomicU64 = AtomicU64::new(0);
static HISTORY_LINES: AtomicU64 = AtomicU64::new(0);
/// The UI thread's id, to mark it in the thread list.
static UI_TID: AtomicU64 = AtomicU64::new(0);

/// `UI_STATE` for the next iteration, busy.
fn next_busy(state: u64) -> u64 {
    (((state >> 1) + 1) << 1) | 1
}

/// `UI_STATE` after an event: unchanged inside a busy iteration, else a new one.
fn on_event(state: u64) -> u64 {
    if state & 1 == 1 { state } else { next_busy(state) }
}

/// `UI_STATE` once the iteration ends.
fn idle_of(state: u64) -> u64 {
    state & !1
}

/// The event loop woke (winit `new_events`): a new busy iteration.
#[inline]
pub(crate) fn ui_busy() {
    UI_STATE.store(next_busy(UI_STATE.load(Ordering::Relaxed)), Ordering::Relaxed);
}

/// An event handler ran: a new busy iteration unless one is running. Covers
/// a platform that dispatches an event outside `new_events`/`about_to_wait`.
#[inline]
pub(crate) fn ui_event() {
    let state = UI_STATE.load(Ordering::Relaxed);
    if state & 1 == 0 {
        UI_STATE.store(on_event(state), Ordering::Relaxed);
    }
}

/// The event loop is about to wait for events: idle, not stalled.
#[inline]
pub(crate) fn ui_idle() {
    UI_STATE.store(idle_of(UI_STATE.load(Ordering::Relaxed)), Ordering::Relaxed);
}

/// Calls `ui_idle` when dropped, for `about_to_wait`'s every return path.
pub(crate) struct IdleOnDrop(PhantomData<*const ()>);

pub(crate) fn idle_on_return() -> IdleOnDrop {
    IdleOnDrop(PhantomData)
}

impl Drop for IdleOnDrop {
    #[inline]
    fn drop(&mut self) {
        ui_idle();
    }
}

/// A stack of packed phase words with one writer. A reader on another
/// thread may see a word from just before a push; fine for a report.
struct PhaseStack {
    words: [AtomicU64; PHASE_DEPTH],
    len: AtomicUsize,
}

impl PhaseStack {
    const fn new() -> Self {
        PhaseStack { words: [const { AtomicU64::new(0) }; PHASE_DEPTH], len: AtomicUsize::new(0) }
    }

    /// Push `word`; returns the depth to pop back to.
    #[inline]
    fn push(&self, word: u64) -> usize {
        let depth = self.len.load(Ordering::Relaxed);
        if let Some(slot) = self.words.get(depth) {
            slot.store(word, Ordering::Relaxed);
        }
        self.len.store(depth + 1, Ordering::Release);
        depth
    }

    #[inline]
    fn pop_to(&self, depth: usize) {
        self.len.store(depth, Ordering::Release);
    }

    /// The recorded words, outermost first, and how many deeper phases were
    /// pushed but not recorded.
    fn read(&self) -> (Vec<u64>, usize) {
        let len = self.len.load(Ordering::Acquire);
        let kept = len.min(PHASE_DEPTH);
        (self.words[..kept].iter().map(|w| w.load(Ordering::Relaxed)).collect(), len - kept)
    }
}

/// A phase on the UI thread's stack, popped when dropped. Not `Send`: the
/// stack is the UI thread's.
#[must_use = "the phase ends when the guard drops"]
pub(crate) struct PhaseGuard {
    depth: usize,
    _not_send: PhantomData<*const ()>,
}

/// Push `phase` (with `detail`) for the guard's lifetime. UI thread only.
#[inline]
pub(crate) fn phase(phase: Phase, detail: u64) -> PhaseGuard {
    PhaseGuard { depth: UI_PHASES.push(pack(phase.code(), detail)), _not_send: PhantomData }
}

impl Drop for PhaseGuard {
    #[inline]
    fn drop(&mut self) {
        UI_PHASES.pop_to(self.depth);
    }
}

/// The pane count and their scrollback lines (as of each pane's last
/// snapshot), for the report. UI thread, once per frame.
#[inline]
pub(crate) fn publish_panes(panes: usize, history_lines: usize) {
    PANES.store(panes as u64, Ordering::Relaxed);
    HISTORY_LINES.store(history_lines as u64, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Activity slots (other threads)
// ---------------------------------------------------------------------------

/// One thread's activity. Aligned so threads writing their own slots never
/// share a cache line (128: Apple silicon's line size).
#[repr(align(128))]
struct Slot {
    /// Owning thread's id; 0 while free.
    tid: AtomicU64,
    /// Packed `Activity` and detail; 0 while idle.
    what: AtomicU64,
    /// `COARSE_MS` when it started.
    since: AtomicU64,
}

static SLOT_TABLE: [Slot; SLOTS] = [const {
    Slot { tid: AtomicU64::new(0), what: AtomicU64::new(0), since: AtomicU64::new(0) }
}; SLOTS];
/// Threads that found every slot taken (their activities go unrecorded).
static SLOTS_FULL: AtomicU64 = AtomicU64::new(0);
/// Milliseconds on the watchdog's clock, stored each tick: activity start
/// times without a clock read on the marking thread.
static COARSE_MS: AtomicU64 = AtomicU64::new(0);

const SLOT_UNCLAIMED: u8 = 0;
const SLOT_NONE: u8 = u8::MAX;

/// This thread's slot: `SLOT_UNCLAIMED`, `SLOT_NONE`, or index + 1. Frees
/// the slot when the thread exits.
struct ThreadSlot(Cell<u8>);

impl Drop for ThreadSlot {
    fn drop(&mut self) {
        if let Some(slot) = slot_at(self.0.get()) {
            slot.what.store(0, Ordering::Relaxed);
            slot.tid.store(0, Ordering::Release);
        }
    }
}

thread_local! {
    static THREAD_SLOT: ThreadSlot = const { ThreadSlot(Cell::new(SLOT_UNCLAIMED)) };
}

fn slot_at(state: u8) -> Option<&'static Slot> {
    match state {
        SLOT_UNCLAIMED | SLOT_NONE => None,
        n => SLOT_TABLE.get(n as usize - 1),
    }
}

fn my_slot() -> Option<&'static Slot> {
    THREAD_SLOT
        .try_with(|mine| {
            if mine.0.get() == SLOT_UNCLAIMED {
                let tid = current_tid().max(1);
                let free = SLOT_TABLE.iter().position(|slot| {
                    slot.tid.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed).is_ok()
                });
                mine.0.set(match free {
                    Some(i) => i as u8 + 1,
                    None => {
                        SLOTS_FULL.fetch_add(1, Ordering::Relaxed);
                        SLOT_NONE
                    }
                });
            }
            slot_at(mine.0.get())
        })
        .ok()
        .flatten()
}

/// An activity in this thread's slot, ended (the previous one restored)
/// when dropped.
#[must_use = "the activity ends when the guard drops"]
pub(crate) struct ActivityGuard {
    slot: Option<&'static Slot>,
    prev_what: u64,
    prev_since: u64,
    _not_send: PhantomData<*const ()>,
}

/// Mark this thread as doing `activity` (with `detail`) for the guard's
/// lifetime. Allocation-free; the first call on a thread claims its slot.
#[inline]
pub(crate) fn activity(activity: Activity, detail: u64) -> ActivityGuard {
    let slot = my_slot();
    let (mut prev_what, mut prev_since) = (0, 0);
    if let Some(slot) = slot {
        prev_what = slot.what.load(Ordering::Relaxed);
        prev_since = slot.since.load(Ordering::Relaxed);
        slot.since.store(COARSE_MS.load(Ordering::Relaxed), Ordering::Relaxed);
        slot.what.store(pack(activity.code(), detail), Ordering::Release);
    }
    ActivityGuard { slot, prev_what, prev_since, _not_send: PhantomData }
}

impl ActivityGuard {
    /// Move on to `activity` within the same section (its age restarts).
    #[inline]
    pub(crate) fn set(&mut self, activity: Activity, detail: u64) {
        if let Some(slot) = self.slot {
            slot.since.store(COARSE_MS.load(Ordering::Relaxed), Ordering::Relaxed);
            slot.what.store(pack(activity.code(), detail), Ordering::Release);
        }
    }
}

impl Drop for ActivityGuard {
    #[inline]
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            slot.since.store(self.prev_since, Ordering::Relaxed);
            slot.what.store(self.prev_what, Ordering::Release);
        }
    }
}

#[cfg(target_os = "linux")]
fn current_tid() -> u64 {
    // A raw syscall because glibc only gained a gettid wrapper in 2.30, and
    // RHEL 8 ships 2.28: the wrapper compiles there but fails to link.
    // SAFETY: gettid has no preconditions.
    unsafe { libc::syscall(libc::SYS_gettid) as u64 }
}

#[cfg(target_os = "macos")]
fn current_tid() -> u64 {
    let mut tid = 0u64;
    // SAFETY: a null thread means the calling thread; `tid` is a valid out pointer.
    unsafe { libc::pthread_threadid_np(0, &mut tid) };
    tid
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_tid() -> u64 {
    0
}

/// An active slot, as the report shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotView {
    tid: u64,
    what: u64,
    age_ms: u64,
}

fn read_slots(now_ms: u64) -> Vec<SlotView> {
    SLOT_TABLE
        .iter()
        .filter_map(|slot| {
            let tid = slot.tid.load(Ordering::Acquire);
            let what = slot.what.load(Ordering::Acquire);
            if tid == 0 || what == 0 {
                return None;
            }
            let since = slot.since.load(Ordering::Relaxed);
            Some(SlotView { tid, what, age_ms: now_ms.saturating_sub(since) })
        })
        .collect()
}

fn read_phases() -> (Vec<u64>, usize) {
    UI_PHASES.read()
}

// ---------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Turn the watchdog on or off (`hang_watchdog`). On starts the thread if
/// it is not running; off makes it exit at its next tick. Call from the UI
/// thread (it records the UI thread's id).
pub(crate) fn configure(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        return;
    }
    UI_TID.store(current_tid(), Ordering::Relaxed);
    EPOCH.get_or_init(Instant::now);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(e) = std::thread::Builder::new().name("watchdog".into()).spawn(run) {
        RUNNING.store(false, Ordering::SeqCst);
        log::warn!("hang watchdog thread failed to start: {e}");
    }
}

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn run() {
    log::debug!("hang watchdog running");
    let mut watcher = Watcher::default();
    loop {
        std::thread::sleep(TICK);
        if !ENABLED.load(Ordering::SeqCst) {
            RUNNING.store(false, Ordering::SeqCst);
            // `configure(true)` between the load and the store saw RUNNING
            // still set and started nothing: carry on in its place.
            if ENABLED.load(Ordering::SeqCst) && !RUNNING.swap(true, Ordering::SeqCst) {
                continue;
            }
            return;
        }
        let now = now_ms();
        COARSE_MS.store(now, Ordering::Relaxed);
        if let Some(text) = watcher.tick(now, UI_STATE.load(Ordering::Relaxed)) {
            log::warn!("{text}");
        }
    }
}

/// What a tick decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    None,
    /// The iteration has been busy `ARM_MS`: take the baseline.
    Arm,
    /// Busy `REPORT_MS`: write the report.
    Report { stalled_ms: u64 },
    /// Still busy, `FOLLOW_UP_MS` after the last line.
    FollowUp { stalled_ms: u64 },
    /// A reported stall ended (the UI went idle or on to a new iteration).
    Recovered { stalled_ms: u64 },
    /// An armed but unreported stall ended: drop the baseline.
    Disarm,
}

/// Stall detection on samples of `UI_STATE`, on an injected clock.
#[derive(Debug, Default)]
struct Detector {
    /// The last sample.
    last: u64,
    /// When `last` was first seen.
    seen_at: u64,
    armed: bool,
    reported: bool,
    /// Stall duration at which the next follow-up is due.
    next_follow_up: u64,
}

impl Detector {
    fn tick(&mut self, now_ms: u64, state: u64) -> Action {
        if state >> 1 <= STARTUP_ITERATION {
            *self = Detector { last: state, seen_at: now_ms, ..Detector::default() };
            return Action::None;
        }
        let busy = state & 1 == 1;
        if !busy || state != self.last {
            let stalled_ms = now_ms.saturating_sub(self.seen_at);
            let action = if self.reported {
                Action::Recovered { stalled_ms }
            } else if self.armed {
                Action::Disarm
            } else {
                Action::None
            };
            *self = Detector { last: state, seen_at: now_ms, ..Detector::default() };
            return action;
        }
        let stalled_ms = now_ms.saturating_sub(self.seen_at);
        // Ahead of arming: a tick that came late (the watchdog itself held
        // up) still reports on time.
        if !self.reported && stalled_ms >= REPORT_MS {
            self.reported = true;
            self.armed = true;
            self.next_follow_up = stalled_ms + FOLLOW_UP_MS;
            return Action::Report { stalled_ms };
        }
        if !self.armed && stalled_ms >= ARM_MS {
            self.armed = true;
            return Action::Arm;
        }
        if self.reported && stalled_ms >= self.next_follow_up {
            while self.next_follow_up <= stalled_ms {
                self.next_follow_up += FOLLOW_UP_MS;
            }
            return Action::FollowUp { stalled_ms };
        }
        Action::None
    }
}

/// Process-wide counters (Linux).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counters {
    majflt: u64,
    rss_kb: Option<u64>,
    swap_kb: Option<u64>,
}

/// Counters taken when a stall arms, for the report's deltas.
#[derive(Debug, Default)]
struct Baseline {
    at_ms: u64,
    /// (tid, majflt) per thread.
    threads: Vec<(u64, u64)>,
    counters: Option<Counters>,
}

impl Baseline {
    fn take(now_ms: u64) -> Self {
        let threads = procfs::tasks(false).into_iter().map(|t| (t.tid, t.majflt)).collect();
        Baseline { at_ms: now_ms, threads, counters: procfs::counters() }
    }
}

#[derive(Default)]
struct Watcher {
    detector: Detector,
    baseline: Option<Baseline>,
    /// Counters and time at the last line written, for follow-up deltas.
    last_line: Option<(u64, Option<Counters>)>,
    /// The phases as reported, for the recovery line.
    reported_phases: String,
}

impl Watcher {
    /// One tick on a sample of `UI_STATE`: the text to log, if any.
    fn tick(&mut self, now_ms: u64, ui_state: u64) -> Option<String> {
        match self.detector.tick(now_ms, ui_state) {
            Action::None => None,
            Action::Arm => {
                self.baseline = Some(Baseline::take(now_ms));
                None
            }
            Action::Disarm => {
                self.baseline = None;
                None
            }
            Action::Report { stalled_ms } => {
                let baseline = self.baseline.get_or_insert_with(|| Baseline::take(now_ms));
                let snapshot = Snapshot::gather(now_ms, stalled_ms, baseline);
                self.reported_phases = snapshot.phases.clone();
                self.last_line = Some((now_ms, snapshot.counters));
                Some(format_report(&snapshot))
            }
            Action::FollowUp { stalled_ms } => {
                let (phases, deeper) = read_phases();
                let counters = procfs::counters();
                let (since, before) = self.last_line.unwrap_or((now_ms, None));
                self.last_line = Some((now_ms, counters));
                Some(format_follow_up(
                    stalled_ms,
                    &describe_phases(&phases, deeper),
                    now_ms.saturating_sub(since),
                    before,
                    counters,
                ))
            }
            Action::Recovered { stalled_ms } => {
                self.baseline = None;
                self.last_line = None;
                Some(format_recovered(stalled_ms, &self.reported_phases))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// /proc (Linux)
// ---------------------------------------------------------------------------

/// One thread from `/proc/self/task`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskInfo {
    tid: u64,
    comm: String,
    state: char,
    majflt: u64,
    wchan: String,
}

/// From a `/proc/<pid>/stat` line: (comm, state, majflt). The comm is in
/// parentheses and may hold anything, spaces and parentheses included, so
/// it runs to the *last* `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(line: &str) -> Option<(String, char, u64)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = line[open + 1..close].to_string();
    // After the comm: state(3) ppid pgrp session tty_nr tpgid flags minflt
    // cminflt majflt(12).
    let mut fields = line[close + 1..].split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let majflt = fields.nth(8)?.parse().ok()?;
    Some((comm, state, majflt))
}

/// A `kB` value from `/proc/<pid>/status`, such as `VmSwap:   1234 kB`.
#[cfg(any(target_os = "linux", test))]
fn parse_status_kb(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        rest.split_ascii_whitespace().next()?.parse().ok()
    })
}

#[cfg(target_os = "linux")]
mod procfs {
    use super::{Counters, TaskInfo, parse_stat, parse_status_kb};

    /// Every thread of this process; `wchan` read only when asked for.
    pub(super) fn tasks(with_wchan: bool) -> Vec<TaskInfo> {
        let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
            return Vec::new();
        };
        let mut tasks: Vec<TaskInfo> = dir
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let tid: u64 = entry.file_name().to_str()?.parse().ok()?;
                let path = entry.path();
                let stat = std::fs::read_to_string(path.join("stat")).ok()?;
                let (comm, state, majflt) = parse_stat(&stat)?;
                let wchan = if with_wchan {
                    std::fs::read_to_string(path.join("wchan")).unwrap_or_default().trim().to_string()
                } else {
                    String::new()
                };
                Some(TaskInfo { tid, comm, state, majflt, wchan })
            })
            .collect();
        tasks.sort_by_key(|t| t.tid);
        tasks
    }

    pub(super) fn counters() -> Option<Counters> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let (_, _, majflt) = parse_stat(&stat)?;
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        Some(Counters {
            majflt,
            rss_kb: parse_status_kb(&status, "VmRSS"),
            swap_kb: parse_status_kb(&status, "VmSwap"),
        })
    }
}

/// No /proc: the report shows phases, slots, durations and panes only.
#[cfg(not(target_os = "linux"))]
mod procfs {
    use super::{Counters, TaskInfo};

    pub(super) fn tasks(_with_wchan: bool) -> Vec<TaskInfo> {
        Vec::new()
    }

    pub(super) fn counters() -> Option<Counters> {
        None
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// Everything a report shows, gathered once.
#[derive(Debug, Default)]
struct Snapshot {
    stalled_ms: u64,
    phases: String,
    slots: Vec<(SlotView, String)>,
    slots_full: u64,
    panes: u64,
    history_lines: u64,
    threads: Vec<ThreadRow>,
    /// How long ago the baseline was taken.
    baseline_age_ms: u64,
    counters: Option<Counters>,
    baseline_counters: Option<Counters>,
}

impl Snapshot {
    fn gather(now_ms: u64, stalled_ms: u64, baseline: &Baseline) -> Self {
        let (phases, deeper) = read_phases();
        let slots: Vec<(SlotView, String)> = read_slots(now_ms)
            .into_iter()
            .map(|s| {
                let text = describe_activity(s.what);
                (s, text)
            })
            .collect();
        // Not this thread: it is always running, reading /proc.
        let me = current_tid();
        let tasks: Vec<TaskInfo> = procfs::tasks(true).into_iter().filter(|t| t.tid != me).collect();
        let threads = thread_rows(&tasks, &baseline.threads, &slots, UI_TID.load(Ordering::Relaxed));
        Snapshot {
            stalled_ms,
            phases: describe_phases(&phases, deeper),
            slots,
            slots_full: SLOTS_FULL.load(Ordering::Relaxed),
            panes: PANES.load(Ordering::Relaxed),
            history_lines: HISTORY_LINES.load(Ordering::Relaxed),
            threads,
            baseline_age_ms: now_ms.saturating_sub(baseline.at_ms),
            counters: procfs::counters(),
            baseline_counters: baseline.counters,
        }
    }
}

fn describe_phases(phases: &[u64], deeper: usize) -> String {
    if phases.is_empty() {
        return "no phase (winit dispatch or unlabelled code)".to_string();
    }
    let mut text = phases.iter().map(|&w| describe_phase(w)).collect::<Vec<_>>().join(" > ");
    if deeper > 0 {
        let _ = write!(text, " > ({deeper} deeper)");
    }
    text
}

/// One thread in the report.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ThreadRow {
    tid: u64,
    comm: String,
    state: char,
    wchan: String,
    /// Major faults since the baseline; None for a thread born since.
    majflt_delta: Option<u64>,
    /// The activity in its slot.
    slot: Option<String>,
    ui: bool,
}

impl ThreadRow {
    /// Worth a line of its own: the UI thread, a slot holder, anything not
    /// sleeping, or anything that faulted.
    fn notable(&self) -> bool {
        self.ui || self.slot.is_some() || !matches!(self.state, 'S' | 'I') || self.majflt_delta.unwrap_or(0) > 0
    }

    fn rank(&self) -> u8 {
        match () {
            _ if self.ui => 0,
            _ if self.slot.is_some() => 1,
            _ if self.state == 'D' => 2,
            _ if self.state == 'R' => 3,
            _ if self.notable() => 4,
            _ => 5,
        }
    }
}

fn thread_rows(tasks: &[TaskInfo], baseline: &[(u64, u64)], slots: &[(SlotView, String)], ui_tid: u64) -> Vec<ThreadRow> {
    tasks
        .iter()
        .map(|t| ThreadRow {
            tid: t.tid,
            comm: t.comm.clone(),
            state: t.state,
            wchan: if t.wchan.is_empty() || t.wchan == "0" { "-".to_string() } else { t.wchan.clone() },
            majflt_delta: baseline
                .iter()
                .find(|(tid, _)| *tid == t.tid)
                .map(|(_, before)| t.majflt.saturating_sub(*before)),
            slot: slots.iter().find(|(s, _)| s.tid == t.tid).map(|(_, text)| text.clone()),
            ui: t.tid == ui_tid,
        })
        .collect()
}

/// A thread name with its trailing pool index dropped: `llvmpipe-12` ->
/// `llvmpipe-`, so a pool's workers group together.
fn pool_name(comm: &str) -> &str {
    comm.trim_end_matches(|c: char| c.is_ascii_digit())
}

fn thread_line(row: &ThreadRow) -> String {
    let mut line = format!("    {:>7} {:<15} {} {:<24}", row.tid, row.comm, row.state, row.wchan);
    match row.majflt_delta {
        Some(d) => {
            let _ = write!(line, " majflt +{d}");
        }
        None => line.push_str(" (new)"),
    }
    if row.ui {
        line.push_str("  [UI thread]");
    }
    if let Some(slot) = &row.slot {
        let _ = write!(line, "  slot: {slot}");
    }
    line.trim_end().to_string()
}

/// The thread list: notable threads one per line, then idle threads, those
/// of one pool in the same state (all of Mesa's sleeping llvmpipe workers)
/// folded into one line, cut to `max_lines`.
fn format_threads(rows: &[ThreadRow], max_lines: usize) -> Vec<String> {
    let mut notable: Vec<&ThreadRow> = rows.iter().filter(|r| r.notable()).collect();
    notable.sort_by_key(|r| (r.rank(), r.tid));

    // Idle threads grouped by (pool, state, wchan), in first-seen order.
    let mut groups: Vec<((&str, char, &str), Vec<&ThreadRow>)> = Vec::new();
    for row in rows.iter().filter(|r| !r.notable()) {
        let key = (pool_name(&row.comm), row.state, row.wchan.as_str());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(row),
            None => groups.push((key, vec![row])),
        }
    }
    let (singles, mut pools): (Vec<_>, Vec<_>) = groups.into_iter().partition(|(_, m)| m.len() == 1);
    pools.sort_by_key(|(_, m)| std::cmp::Reverse(m.len()));

    // Each line with the number of threads it stands for.
    let mut lines: Vec<(String, usize)> = notable.iter().map(|r| (thread_line(r), 1)).collect();
    lines.extend(singles.iter().map(|(_, m)| (thread_line(m[0]), 1)));
    for ((pool, state, wchan), members) in &pools {
        let same_name = members.iter().all(|m| m.comm == members[0].comm);
        let name = if same_name { members[0].comm.clone() } else { format!("{pool}*") };
        let count = format!("x{}", members.len());
        lines.push((format!("    {count:>7} {name:<15} {state} {wchan:<24} (idle pool)"), members.len()));
    }

    if lines.len() > max_lines {
        lines.truncate(max_lines.saturating_sub(1));
        let shown: usize = lines.iter().map(|(_, n)| n).sum();
        lines.push((format!("    ... {} more threads", rows.len().saturating_sub(shown)), 0));
    }
    lines.into_iter().map(|(line, _)| line).collect()
}

fn secs(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

fn size_kb(kb: u64) -> String {
    if kb >= 10 * 1024 * 1024 {
        format!("{:.1} GiB", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{} MiB", kb / 1024)
    } else {
        format!("{kb} kB")
    }
}

fn signed_kb(delta: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    format!("{sign}{}", size_kb(delta.unsigned_abs()))
}

/// The memory line: RSS, swap and its change, and major faults since `before`.
fn memory_line(now: Counters, before: Option<Counters>, over_ms: u64) -> String {
    let mut line = String::from("memory:");
    if let Some(rss) = now.rss_kb {
        let _ = write!(line, " VmRSS {},", size_kb(rss));
    }
    match (now.swap_kb, before.and_then(|b| b.swap_kb)) {
        (Some(swap), Some(was)) => {
            let _ = write!(line, " VmSwap {} ({}),", size_kb(swap), signed_kb(swap as i64 - was as i64));
        }
        (Some(swap), None) => {
            let _ = write!(line, " VmSwap {},", size_kb(swap));
        }
        _ => {}
    }
    match before {
        Some(b) => {
            let _ = write!(line, " majflt +{} in the last {}", now.majflt.saturating_sub(b.majflt), secs(over_ms));
        }
        None => {
            let _ = write!(line, " majflt {} total", now.majflt);
        }
    }
    line
}

fn format_report(s: &Snapshot) -> String {
    let mut out = format!(
        "UI thread stalled for {} without returning to the event loop (hang watchdog)\n",
        secs(s.stalled_ms)
    );
    let _ = writeln!(out, "  in: {}", s.phases);
    let _ = writeln!(out, "  panes: {}, scrollback lines: {}", s.panes, s.history_lines);
    if let Some(now) = s.counters {
        let _ = writeln!(out, "  {}", memory_line(now, s.baseline_counters, s.baseline_age_ms));
    }
    if s.slots.is_empty() {
        out.push_str("  activity slots: none active\n");
    } else {
        let mut slots: Vec<&(SlotView, String)> = s.slots.iter().collect();
        slots.sort_by_key(|(v, _)| std::cmp::Reverse(v.age_ms));
        let _ = writeln!(out, "  activity slots ({} active, oldest first):", slots.len());
        let shown = if slots.len() > SLOT_LINES { SLOT_LINES - 1 } else { slots.len() };
        for (view, text) in &slots[..shown] {
            let _ = writeln!(out, "    tid {:>7}  {}  for {}", view.tid, text, secs(view.age_ms));
        }
        if shown < slots.len() {
            let _ = writeln!(out, "    ... {} more", slots.len() - shown);
        }
    }
    if s.slots_full > 0 {
        let _ = writeln!(out, "  ({} threads found no free activity slot)", s.slots_full);
    }
    if !s.threads.is_empty() {
        let _ = writeln!(
            out,
            "  threads ({}; tid comm state wchan, majflt since {} ago):",
            s.threads.len(),
            secs(s.baseline_age_ms)
        );
        for line in format_threads(&s.threads, THREAD_LINES) {
            let _ = writeln!(out, "{line}");
        }
    }
    out.truncate(out.trim_end().len());
    out
}

fn format_follow_up(stalled_ms: u64, phases: &str, over_ms: u64, before: Option<Counters>, now: Option<Counters>) -> String {
    let mut line = format!("UI thread still stalled after {}, in: {}", secs(stalled_ms), phases);
    if let Some(now) = now {
        let _ = write!(line, "; {}", memory_line(now, before, over_ms));
    }
    line
}

fn format_recovered(stalled_ms: u64, phases: &str) -> String {
    format!("UI thread recovered after ~{} (was in: {})", secs(stalled_ms), phases)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(iteration: u64) -> u64 {
        (iteration << 1) | 1
    }

    fn idle(iteration: u64) -> u64 {
        iteration << 1
    }

    /// Feed `state` every tick from `from` to `to` (inclusive), collecting
    /// the non-None actions with their times.
    fn run(d: &mut Detector, from: u64, to: u64, state: u64) -> Vec<(u64, Action)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            let a = d.tick(t, state);
            if a != Action::None {
                out.push((t, a));
            }
            t += 250;
        }
        out
    }

    #[test]
    fn idle_and_short_iterations_never_act() {
        let mut d = Detector::default();
        assert!(run(&mut d, 0, 60_000, idle(0)).is_empty(), "idle is not a stall");
        // A new busy iteration every tick: busy, but never stalled.
        let mut t = 60_000;
        for i in 1..400 {
            assert_eq!(d.tick(t, busy(i)), Action::None);
            t += 250;
        }
        // Busy 1.5 s, under the report threshold: arms, then disarms silently.
        let actions = run(&mut d, t, t + 1_500, busy(1_000));
        assert_eq!(actions, vec![(t + 500, Action::Arm)]);
        assert_eq!(d.tick(t + 1_750, idle(1_000)), Action::Disarm);
    }

    #[test]
    fn a_stall_reports_once_follows_up_every_ten_seconds_and_recovers() {
        let mut d = Detector::default();
        d.tick(0, idle(4));
        let actions = run(&mut d, 1_000, 26_000, busy(5));
        assert_eq!(
            actions,
            vec![
                (1_500, Action::Arm),
                (3_000, Action::Report { stalled_ms: 2_000 }),
                (13_000, Action::FollowUp { stalled_ms: 12_000 }),
                (23_000, Action::FollowUp { stalled_ms: 22_000 }),
            ]
        );
        assert_eq!(d.tick(26_250, idle(5)), Action::Recovered { stalled_ms: 25_250 });
        assert_eq!(d.tick(26_500, idle(5)), Action::None, "recovered once");
    }

    #[test]
    fn a_new_busy_iteration_ends_the_stall_and_starts_over() {
        let mut d = Detector::default();
        let actions = run(&mut d, 0, 2_500, busy(11));
        assert_eq!(actions.last(), Some(&(2_000, Action::Report { stalled_ms: 2_000 })));
        // Straight into the next busy iteration: recovered, and the new one
        // is timed from scratch.
        assert_eq!(d.tick(2_750, busy(12)), Action::Recovered { stalled_ms: 2_750 });
        assert_eq!(d.tick(3_000, busy(12)), Action::None);
        assert_eq!(d.tick(3_250, busy(12)), Action::Arm);
        assert_eq!(d.tick(4_750, busy(12)), Action::Report { stalled_ms: 2_000 });
    }

    #[test]
    fn the_startup_iteration_is_not_watched() {
        let mut d = Detector::default();
        // Window, GL and font setup taking 8 s on a software rasterizer.
        assert!(run(&mut d, 0, 8_000, busy(1)).is_empty());
        // The first iteration after it is timed from its own start.
        assert_eq!(d.tick(8_250, busy(2)), Action::None);
        assert_eq!(d.tick(8_750, busy(2)), Action::Arm);
        assert_eq!(d.tick(10_250, busy(2)), Action::Report { stalled_ms: 2_000 });
    }

    #[test]
    fn a_late_tick_reports_without_arming_first() {
        let mut d = Detector::default();
        d.tick(0, busy(11));
        // The watchdog itself was held up for 5 s.
        assert_eq!(d.tick(5_000, busy(11)), Action::Report { stalled_ms: 5_000 });
        assert_eq!(d.tick(5_250, busy(11)), Action::None);
        // A tick missed past several follow-ups writes one line, not a burst.
        assert_eq!(d.tick(40_000, busy(11)), Action::FollowUp { stalled_ms: 40_000 });
        assert_eq!(d.tick(40_250, busy(11)), Action::None);
        assert_eq!(d.tick(45_000, busy(11)), Action::FollowUp { stalled_ms: 45_000 });
    }

    #[test]
    fn ui_state_counts_iterations_and_events_inside_one_do_not() {
        let s = next_busy(idle(0));
        assert_eq!(s, busy(1));
        assert_eq!(on_event(s), s, "an event inside the iteration");
        let s = idle_of(s);
        assert_eq!(s, idle(1));
        assert_eq!(on_event(s), busy(2), "an event dispatched while idle starts one");
        assert_eq!(next_busy(busy(2)), busy(3), "new_events always starts one");
    }

    #[test]
    fn labels_round_trip_through_their_codes() {
        for &p in Phase::ALL {
            assert_eq!(Phase::from_code(p.code()), Some(p));
        }
        for &a in Activity::ALL {
            assert_eq!(Activity::from_code(a.code()), Some(a));
        }
        assert_eq!(Phase::from_code(0), None);
        let word = pack(Phase::PaneResize.code(), 7);
        assert_eq!(describe_phase(word), "pane resize: pane 7");
        let word = pack(Phase::MainEvent.code(), event_code(&WindowEvent::RedrawRequested));
        assert_eq!(describe_phase(word), "main window: RedrawRequested");
        assert_eq!(describe_activity(pack(Activity::PtyRead.code(), 4242)), "pty_read (terminal lease/lock): pid 4242");
        assert_eq!(describe_activity(pack(Activity::ReaperDrop.code(), 0)), "reaper dropping a grid");
        assert_eq!(unpack(pack(3, u64::MAX)), (3, DETAIL_MASK), "detail cannot spill into the code");
    }

    #[test]
    fn the_phase_stack_nests_unwinds_and_caps_its_depth() {
        // A stack of its own: the UI thread's is pushed to by any test that
        // runs instrumented app code.
        let stack = PhaseStack::new();
        let outer = stack.push(pack(Phase::MainEvent.code(), event_code(&WindowEvent::RedrawRequested)));
        let mid = stack.push(pack(Phase::EndFrame.code(), 0));
        let inner = stack.push(pack(Phase::SwapBuffers.code(), 0));
        let (words, deeper) = stack.read();
        assert_eq!(describe_phases(&words, deeper), "main window: RedrawRequested > end_frame > swap_buffers");
        stack.pop_to(inner);
        stack.pop_to(mid);
        assert_eq!(describe_phases(&stack.read().0, 0), "main window: RedrawRequested");
        stack.pop_to(outer);
        assert_eq!(stack.read(), (Vec::new(), 0));
        assert_eq!(describe_phases(&[], 0), "no phase (winit dispatch or unlabelled code)");

        let depths: Vec<usize> = (0..PHASE_DEPTH + 2).map(|_| stack.push(pack(Phase::Ui.code(), 0))).collect();
        let (words, deeper) = stack.read();
        assert_eq!((words.len(), deeper), (PHASE_DEPTH, 2));
        assert!(describe_phases(&words, deeper).ends_with("ui > (2 deeper)"));
        for depth in depths.into_iter().rev() {
            stack.pop_to(depth);
        }
        assert_eq!(stack.read(), (Vec::new(), 0));
    }

    #[test]
    fn activity_slots_record_the_thread_and_free_on_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let mut guard = activity(Activity::GateWait, HOLD_REFLOW);
            guard.set(Activity::GateHoldReflow, 1234);
            tx.send(current_tid()).unwrap();
            done_rx.recv().unwrap();
            drop(guard);
            tx.send(current_tid()).unwrap();
        });
        let tid = rx.recv().unwrap();
        let held = read_slots(COARSE_MS.load(Ordering::Relaxed));
        let mine: Vec<_> = held.iter().filter(|s| s.tid == tid.max(1)).collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(describe_activity(mine[0].what), "TermGate::hold reflow: 1234 lines");
        done_tx.send(()).unwrap();
        rx.recv().unwrap();
        worker.join().unwrap();
        assert!(
            SLOT_TABLE.iter().all(|s| s.tid.load(Ordering::Acquire) != tid.max(1)),
            "the slot is freed when its thread exits"
        );
    }

    #[test]
    fn parses_stat_lines_with_awkward_comms() {
        let plain = "4321 (rustinator) S 1 4321 4321 0 -1 4194304 52311 0 77 0 1200 300 0 0 20 0 41 0 123 0 0";
        assert_eq!(parse_stat(plain), Some(("rustinator".to_string(), 'S', 77)));
        let spaced = "4400 (PTY reader) D 1 4321 4321 0 -1 4194368 10 0 9 0 0 0 0 0 20 0 1 0";
        assert_eq!(parse_stat(spaced), Some(("PTY reader".to_string(), 'D', 9)));
        let parens = "4500 (a) b (c)) R 1 4321 4321 0 -1 0 1 2 3 4 5";
        assert_eq!(parse_stat(parens), Some(("a) b (c)".to_string(), 'R', 3)));
        assert_eq!(parse_stat("4600 (short) S 1 2"), None, "truncated line");
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn parses_status_kb_fields() {
        let status = "Name:\trustinator\nVmPeak:\t 2000000 kB\nVmRSS:\t  831488 kB\nVmSwapX:\t1 kB\nVmSwap:\t 1232896 kB\n";
        assert_eq!(parse_status_kb(status, "VmRSS"), Some(831_488));
        assert_eq!(parse_status_kb(status, "VmSwap"), Some(1_232_896));
        assert_eq!(parse_status_kb(status, "VmHWM"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_this_process_from_proc() {
        let tasks = procfs::tasks(true);
        assert!(tasks.iter().any(|t| t.tid == std::process::id() as u64), "the main thread is listed");
        let counters = procfs::counters().expect("/proc/self/stat");
        assert!(counters.rss_kb.is_some_and(|kb| kb > 0));
    }

    fn row(tid: u64, comm: &str, state: char, wchan: &str) -> ThreadRow {
        ThreadRow {
            tid,
            comm: comm.to_string(),
            state,
            wchan: wchan.to_string(),
            majflt_delta: Some(0),
            slot: None,
            ui: false,
        }
    }

    /// 40 threads as on the Xvnc box: the UI thread in D faulting, two PTY
    /// readers (one holding the lock), a reaper, the watchdog, and 34 idle
    /// llvmpipe workers.
    fn forty_threads() -> Vec<ThreadRow> {
        let mut rows = vec![
            ThreadRow { majflt_delta: Some(412), ui: true, ..row(1000, "rustinator", 'D', "folio_wait_bit_common") },
            ThreadRow {
                slot: Some("pty_read (terminal lease/lock): pid 2001".into()),
                ..row(1001, "PTY reader", 'S', "do_sys_poll")
            },
            row(1002, "PTY reader", 'S', "do_sys_poll"),
            row(1003, "term reaper", 'S', "futex_wait_queue"),
            row(1004, "reflow", 'R', "-"),
            ThreadRow { majflt_delta: None, ..row(1005, "clipboard-read", 'S', "futex_wait_queue") },
        ];
        for i in 0..34 {
            rows.push(row(1100 + i, &format!("llvmpipe-{i}"), 'S', "futex_wait_queue"));
        }
        assert_eq!(rows.len(), 40);
        rows
    }

    #[test]
    fn idle_llvmpipe_workers_fold_into_one_line() {
        let lines = format_threads(&forty_threads(), THREAD_LINES);
        let pool: Vec<_> = lines.iter().filter(|l| l.contains("llvmpipe")).collect();
        assert_eq!(pool.len(), 1, "{lines:#?}");
        assert!(pool[0].contains("x34") && pool[0].contains("llvmpipe-*"), "{}", pool[0]);
        assert!(lines[0].contains("[UI thread]") && lines[0].contains("majflt +412"), "UI thread first: {lines:#?}");
        assert!(lines[1].contains("slot: pty_read"), "then slot holders: {lines:#?}");
        // A worker that is not idle keeps its own line.
        let mut rows = forty_threads();
        rows[10].state = 'R';
        let lines = format_threads(&rows, THREAD_LINES);
        assert!(lines.iter().any(|l| l.contains("x33")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains(&format!("{} llvmpipe-4", rows[10].tid))), "{lines:#?}");
    }

    #[test]
    fn the_thread_list_is_cut_to_its_budget() {
        // 40 threads, all distinct and busy: nothing folds.
        let rows: Vec<ThreadRow> = (0..40).map(|i| row(2000 + i, &format!("worker{}x", i), 'R', "-")).collect();
        let lines = format_threads(&rows, THREAD_LINES);
        assert_eq!(lines.len(), THREAD_LINES);
        assert_eq!(lines.last().unwrap().trim(), format!("... {} more threads", 40 - (THREAD_LINES - 1)));
    }

    fn sample_snapshot(threads: Vec<ThreadRow>, slots: usize) -> Snapshot {
        let phases = [
            pack(Phase::MainEvent.code(), event_code(&WindowEvent::RedrawRequested)),
            pack(Phase::EndFrame.code(), 0),
            pack(Phase::SwapBuffers.code(), 0),
        ];
        let mut slot_list = vec![
            (
                SlotView { tid: 1001, what: pack(Activity::PtyRead.code(), 2001), age_ms: 2_250 },
                describe_activity(pack(Activity::PtyRead.code(), 2001)),
            ),
            (
                SlotView { tid: 1003, what: pack(Activity::ReaperDrop.code(), 0), age_ms: 7_500 },
                describe_activity(pack(Activity::ReaperDrop.code(), 0)),
            ),
        ];
        for i in slot_list.len()..slots {
            let what = pack(Activity::PtyRead.code(), 3000 + i as u64);
            slot_list.push((SlotView { tid: 1200 + i as u64, what, age_ms: 100 }, describe_activity(what)));
        }
        Snapshot {
            stalled_ms: 2_000,
            phases: describe_phases(&phases, 0),
            slots: slot_list,
            slots_full: 0,
            panes: 6,
            history_lines: 184_320,
            threads,
            baseline_age_ms: 1_500,
            counters: Some(Counters { majflt: 90_412, rss_kb: Some(831_488), swap_kb: Some(1_232_896) }),
            baseline_counters: Some(Counters { majflt: 89_980, rss_kb: Some(820_000), swap_kb: Some(1_167_360) }),
        }
    }

    #[test]
    fn the_report_stays_within_its_line_budget() {
        let report = format_report(&sample_snapshot(forty_threads(), 2));
        println!("{report}");
        let lines = report.lines().count();
        assert!(lines <= REPORT_MAX_LINES, "{lines} lines:\n{report}");
        assert!(report.contains("in: main window: RedrawRequested > end_frame > swap_buffers"), "{report}");
        assert!(report.contains("VmSwap 1204 MiB (+64 MiB)"), "{report}");
        assert!(report.contains("majflt +432 in the last 1.5 s"), "{report}");
        // The worst case: every slot taken, 40 distinct busy threads.
        let rows: Vec<ThreadRow> = (0..40).map(|i| row(2000 + i, &format!("worker{}x", i), 'R', "-")).collect();
        let mut worst = sample_snapshot(rows, SLOTS);
        worst.slots_full = 3;
        let report = format_report(&worst);
        let lines = report.lines().count();
        assert!(lines <= REPORT_MAX_LINES, "{lines} lines:\n{report}");
    }

    #[test]
    fn a_report_without_proc_shows_phases_slots_and_panes() {
        let mut s = sample_snapshot(Vec::new(), 2);
        s.counters = None;
        s.baseline_counters = None;
        let report = format_report(&s);
        assert!(!report.contains("memory:") && !report.contains("threads ("), "{report}");
        assert!(report.contains("reaper dropping a grid  for 7.5 s"), "{report}");
        assert!(report.contains("panes: 6, scrollback lines: 184320"), "{report}");
    }

    /// The whole path on this process: a stalled UI sample on an injected
    /// clock, real /proc reads (Linux), a sleeping llvmpipe-style pool and a
    /// thread holding a slot.
    #[test]
    fn the_watcher_writes_one_report_a_follow_up_and_a_recovery_line() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let mut workers = Vec::new();
        for i in 0..8 {
            let (rx, ready) = (std::sync::Arc::clone(&release_rx), ready_tx.clone());
            workers.push(
                std::thread::Builder::new()
                    .name(format!("llvmpipe-{i}"))
                    .spawn(move || {
                        let _slot = (i == 0).then(|| activity(Activity::PtyRead, 4242));
                        ready.send(()).unwrap();
                        let _ = rx.lock().unwrap().recv();
                    })
                    .unwrap(),
            );
        }
        for _ in 0..8 {
            ready_rx.recv().unwrap();
        }

        let mut w = Watcher::default();
        let mut lines = Vec::new();
        assert_eq!(w.tick(0, idle(9)), None);
        let mut t = 0;
        while t < 12_250 {
            t += 250;
            lines.extend(w.tick(t, busy(10)).map(|l| (t, l)));
        }
        lines.extend(w.tick(t + 250, idle(10)).map(|l| (t + 250, l)));
        drop(release_tx);
        for worker in workers {
            worker.join().unwrap();
        }

        let times: Vec<u64> = lines.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![2_250, 12_250, 12_500], "{lines:#?}");
        let report = &lines[0].1;
        println!("{report}\n{}\n{}", lines[1].1, lines[2].1);
        assert!(report.starts_with("UI thread stalled for 2.0 s"), "{report}");
        assert!(report.lines().count() <= REPORT_MAX_LINES, "{report}");
        assert!(report.contains("pty_read (terminal lease/lock): pid 4242"), "{report}");
        if cfg!(target_os = "linux") {
            assert!(report.contains("memory: VmRSS"), "{report}");
            assert!(report.contains("llvmpipe-"), "{report}");
        }
        assert!(lines[1].1.starts_with("UI thread still stalled after 12.0 s"), "{}", lines[1].1);
        assert!(lines[2].1.starts_with("UI thread recovered after ~12.2 s"), "{}", lines[2].1);
    }

    #[test]
    fn follow_up_and_recovery_lines_are_one_line_each() {
        let before = Counters { majflt: 100, rss_kb: Some(2048), swap_kb: Some(4096) };
        let now = Counters { majflt: 350, rss_kb: Some(2048), swap_kb: Some(3072) };
        let line = format_follow_up(12_000, "logic", 10_000, Some(before), Some(now));
        assert_eq!(
            line,
            "UI thread still stalled after 12.0 s, in: logic; memory: VmRSS 2 MiB, VmSwap 3 MiB (-1 MiB), \
             majflt +250 in the last 10.0 s"
        );
        assert_eq!(format_follow_up(12_000, "logic", 10_000, None, None), "UI thread still stalled after 12.0 s, in: logic");
        assert_eq!(format_recovered(3_400, "ui"), "UI thread recovered after ~3.4 s (was in: ui)");
    }
}
