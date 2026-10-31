//! The gate between background threads that hold a pane's terminal for a
//! long time (a scrollback search, a scrollback reflow) and the UI thread.
//!
//! The UI thread locks every visible pane's terminal each frame (mode, scroll
//! position, a snapshot when dirty) and on most input. It must never wait out
//! a long holder, so it goes through the gate instead (R1 below): while a
//! holder has the terminal, a read gets the `TermView` the holder published
//! when it took the lock, a mutation that must not be lost is deferred until
//! the holder lets go, and one that may be dropped (a scroll or selection
//! gesture) is. Only the holder's own work changes the terminal until then,
//! so the view is exact, as if read just before it.
//!
//! Protocol:
//!
//! R1, a UI reader or mutator, never waits on `released`: it locks `state`;
//! if nothing is held, it locks the terminal (fairly) while still holding
//! `state`, so no holder can slip in between, does its work and lets go of
//! both; if something is held, it does not touch the terminal at all.
//!
//! R2, a holder (`TermGate::hold`): it waits on `released` for any other
//! holder *without* holding `state` (a holder that kept `state` while waiting
//! for the terminal would block every R1 for the length of the other hold).
//! Then, still holding `state`, it takes the terminal's lease and lock (the
//! wait is bounded: only the PTY thread's current read, or a short exit on
//! the spawn thread, can be in the way, as for any R1), publishes its view,
//! lets go of `state` and works. To let go, it locks `state`, runs the
//! deferred mutations in order, clears `held`, unlocks the terminal and the
//! lease, lets go of `state` and wakes the next holder.
//!
//! A holder pauses the terminal's PTY loop (`PaneHooks::paused`) right after
//! its turn comes and resumes it before letting go of `state`, so the loop
//! stops reading output and keeps writing input (keystrokes, pastes, query
//! replies), resizing and shutting down for the whole hold. It does not rely
//! on the pause: a loop in the middle of a read when the pause lands finishes
//! that read (bounded, as above) before the holder gets the lease.
//!
//! A holder keeps the lease for the whole hold, not just the lock: the PTY
//! thread reads under a lease and only `try_lock`s the terminal, so a loop
//! that is not paused (or not yet) parks in `lease()` instead of spinning on
//! a full buffer until the hold ends. Nothing may call `FairMutex::lock`
//! while holding the lease (parking_lot mutexes are not reentrant).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Term, TermMode};

use crate::pty_event_loop::EventLoopSender;
use crate::watchdog::{self, Activity};

/// What the UI thread reads from a terminal every frame (its mode and
/// `Pane::scroll_info`'s scroll position) and on every key press
/// (`has_selection`, for smart copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TermView {
    pub(crate) mode: TermMode,
    pub(crate) display_offset: usize,
    pub(crate) history: usize,
    pub(crate) screen: usize,
    pub(crate) has_selection: bool,
}

impl TermView {
    pub(crate) fn of<T>(term: &Term<T>) -> Self {
        let grid = term.grid();
        Self {
            mode: *term.mode(),
            display_offset: grid.display_offset(),
            history: grid.total_lines().saturating_sub(grid.screen_lines()),
            screen: grid.screen_lines(),
            has_selection: term.selection.as_ref().is_some_and(|s| !s.is_empty()),
        }
    }
}

/// Who holds the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldKind {
    Search,
    Reflow,
}

/// A mutation the UI made while a holder had the terminal, run when it lets
/// go.
pub(crate) type Deferred<T> = Box<dyn FnOnce(&mut Term<T>) + Send>;

struct Held {
    kind: HoldKind,
    view: TermView,
}

/// The gate's state, under its mutex.
pub(crate) struct GateState<T> {
    held: Option<Held>,
    deferred: Vec<Deferred<T>>,
}

impl<T> GateState<T> {
    /// Whether a holder has the terminal. A reader holding the gate may lock
    /// the terminal only while this is false.
    pub(crate) fn is_held(&self) -> bool {
        self.held.is_some()
    }
}

/// The terminal's PTY loop, paused for each hold (`PaneHooks::paused`).
pub(crate) struct PtyPause {
    pub(crate) paused: Arc<AtomicBool>,
    /// Wakes the loop to re-read `paused`.
    pub(crate) waker: EventLoopSender,
}

impl PtyPause {
    fn set(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        self.waker.notify();
    }
}

/// One terminal's gate, shared with the jobs that hold it. Replaced with the
/// terminal on respawn, so a job still running on the old terminal touches
/// only the old gate.
pub(crate) struct TermGate<T> {
    state: Mutex<GateState<T>>,
    /// Signalled when a holder lets go, for a holder waiting its turn.
    released: Condvar,
    /// The PTY loop to pause while held. None in tests without a loop.
    pause: Option<PtyPause>,
}

impl<T> Default for TermGate<T> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<T> TermGate<T> {
    pub(crate) fn new(pause: Option<PtyPause>) -> Self {
        Self {
            state: Mutex::new(GateState { held: None, deferred: Vec::new() }),
            released: Condvar::new(),
            pause,
        }
    }

    /// The gate, for a reader that must keep a holder out while it locks
    /// the terminal itself (`Pane::frame`). It may lock the terminal only
    /// while `is_held` is false, and never while it is true: the holder
    /// needs the gate to let go.
    ///
    /// Poisoning is ignored: nothing panics while holding it except a
    /// caller's own read, and the state stays consistent across one.
    pub(crate) fn gate(&self) -> MutexGuard<'_, GateState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Who holds the terminal, if anyone.
    pub(crate) fn held_kind(&self) -> Option<HoldKind> {
        self.gate().held.as_ref().map(|held| held.kind)
    }

    /// `read` the terminal, or, while a holder has it, the view it
    /// published.
    pub(crate) fn read<R>(
        &self,
        terminal: &FairMutex<Term<T>>,
        read: impl FnOnce(&Term<T>) -> R,
    ) -> Result<R, TermView> {
        let state = self.gate();
        match &state.held {
            Some(held) => Err(held.view),
            None => Ok(read(&terminal.lock())),
        }
    }

    /// The terminal's `TermView`, without waiting on a holder.
    pub(crate) fn view(&self, terminal: &FairMutex<Term<T>>) -> TermView {
        self.read(terminal, TermView::of).unwrap_or_else(|view| view)
    }

    /// Run `f` on the terminal unless a holder has it; `None` (and `f` not
    /// run) if one does. For mutations that may be dropped.
    pub(crate) fn try_mut<R>(
        &self,
        terminal: &FairMutex<Term<T>>,
        f: impl FnOnce(&mut Term<T>) -> R,
    ) -> Option<R> {
        let state = self.gate();
        if state.held.is_some() {
            return None;
        }
        Some(f(&mut terminal.lock()))
    }

    /// Run `op` on the terminal now, or, while a holder has it, when the
    /// holder lets go (in the order deferred, before anything else can lock
    /// the terminal). Returns whether it ran now.
    pub(crate) fn mutate_or_defer(&self, terminal: &FairMutex<Term<T>>, op: Deferred<T>) -> bool {
        let mut state = self.gate();
        if state.held.is_some() {
            state.deferred.push(op);
            return false;
        }
        op(&mut terminal.lock());
        true
    }

    /// Hold the terminal as `kind` (R2): wait for any other holder, lock
    /// the terminal, run `prepare`, publish the view, then run `work`
    /// without the gate. The gate is cleared, and the deferred mutations
    /// run, even if `prepare` or `work` panics, or every later read would
    /// see the view of a holder that is gone.
    ///
    /// `prepare` runs before the view is published, for changes the view
    /// should already show (a reflow clears the selection, as
    /// `Term::resize` will).
    pub(crate) fn hold<R>(
        &self,
        terminal: &FairMutex<Term<T>>,
        kind: HoldKind,
        prepare: impl FnOnce(&mut Term<T>),
        work: impl FnOnce(&mut Term<T>) -> R,
    ) -> std::thread::Result<R> {
        let mut activity = watchdog::activity(
            Activity::GateWait,
            match kind {
                HoldKind::Search => watchdog::HOLD_SEARCH,
                HoldKind::Reflow => watchdog::HOLD_REFLOW,
            },
        );
        let mut state = self.gate();
        while state.held.is_some() {
            state = self.released.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        // Paused and resumed under `state`, so the next holder's pause
        // cannot be undone by this one's resume.
        if let Some(pause) = &self.pause {
            pause.set(true);
        }
        let lease = terminal.lease();
        let mut term = terminal.lock_unfair();
        let prepared = catch_unwind(AssertUnwindSafe(|| prepare(&mut term)));
        let view = TermView::of(&term);
        let holding = match kind {
            HoldKind::Search => Activity::GateHoldSearch,
            HoldKind::Reflow => Activity::GateHoldReflow,
        };
        activity.set(holding, view.history as u64);
        state.held = Some(Held { kind, view });
        drop(state);

        let result = prepared.and_then(|()| catch_unwind(AssertUnwindSafe(|| work(&mut term))));

        let mut state = self.gate();
        for op in std::mem::take(&mut state.deferred) {
            if catch_unwind(AssertUnwindSafe(|| op(&mut term))).is_err() {
                log::error!("a deferred terminal mutation panicked");
            }
        }
        state.held = None;
        drop(term);
        drop(lease);
        if let Some(pause) = &self.pause {
            pause.set(false);
        }
        drop(state);
        self.released.notify_all();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc;
    use std::time::Duration;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    type TestTerm = Arc<FairMutex<Term<VoidListener>>>;
    type TestGate = Arc<TermGate<VoidListener>>;

    const HANG: Duration = Duration::from_secs(10);

    /// A 20x5 terminal with `history` lines of scrollback.
    fn term_with_history(history: usize) -> TestTerm {
        let config = Config { scrolling_history: history, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(20, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut bytes = Vec::new();
        for i in 0..history + 5 {
            bytes.extend_from_slice(format!("filler {i}\r\n").as_bytes());
        }
        parser.advance(&mut term, &bytes);
        assert_eq!(term.grid().history_size(), history);
        Arc::new(FairMutex::new(term))
    }

    /// A holder on its own thread, inside its hold until `release` is sent.
    struct Holding {
        release: mpsc::Sender<()>,
        thread: std::thread::JoinHandle<()>,
    }

    impl Holding {
        fn start(term: &TestTerm, gate: &TestGate, kind: HoldKind) -> Self {
            let (holding_tx, holding_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel::<()>();
            let thread = {
                let (term, gate) = (Arc::clone(term), Arc::clone(gate));
                std::thread::spawn(move || {
                    gate.hold(&term, kind, |_| {}, |_| {
                        holding_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                    .unwrap();
                })
            };
            holding_rx.recv_timeout(HANG).expect("holder never got the terminal");
            Self { release, thread }
        }

        fn finish(self) {
            self.release.send(()).unwrap();
            self.thread.join().unwrap();
        }
    }

    /// Run `f` on another thread and return its result, failing (instead of
    /// hanging) if it blocks.
    fn on_thread<R: Send + 'static>(what: &str, f: impl FnOnce() -> R + Send + 'static) -> R {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || tx.send(f()).unwrap());
        let result = rx.recv_timeout(HANG).unwrap_or_else(|_| panic!("{what} blocked"));
        thread.join().unwrap();
        result
    }

    #[test]
    fn reads_get_the_published_view_while_a_holder_has_the_terminal() {
        let term = term_with_history(50);
        let gate: TestGate = Arc::default();
        let before = gate.view(&term);
        assert_eq!(before, TermView::of(&term.lock()));
        assert_eq!((before.display_offset, before.history, before.screen), (0, 50, 5));
        assert!(!before.has_selection);

        let holding = Holding::start(&term, &gate, HoldKind::Search);
        let (view, read, kind) = {
            let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
            on_thread("a read of a held terminal", move || {
                (gate.view(&term), gate.read(&term, |_| ()), gate.held_kind())
            })
        };
        assert_eq!(view, before);
        assert_eq!(read, Err(before));
        assert_eq!(kind, Some(HoldKind::Search));

        holding.finish();
        assert_eq!(gate.read(&term, |_| ()), Ok(()), "reads lock the terminal again");
        assert_eq!(gate.held_kind(), None);
    }

    #[test]
    fn the_view_reports_a_non_empty_selection() {
        let term = term_with_history(5);
        let gate: TestGate = Arc::default();
        let point = Point::new(Line(0), Column(1));
        term.lock().selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
        assert!(!gate.view(&term).has_selection, "an empty selection is no selection");
        term.lock().selection.as_mut().unwrap().update(Point::new(Line(0), Column(4)), Side::Right);
        assert!(gate.view(&term).has_selection);
    }

    #[test]
    fn a_panicking_holder_still_clears_the_gate() {
        let term = term_with_history(5);
        let gate: TermGate<VoidListener> = TermGate::default();
        let result = gate.hold(&term, HoldKind::Search, |_| {}, |_| panic!("holder panicked"));
        assert!(result.is_err());
        assert!(!gate.gate().is_held());
        assert_eq!(gate.read(&term, |_| ()), Ok(()));
    }

    /// The hazard behind the condvar: a second holder must wait for the
    /// first without the gate mutex, or every UI read would block behind it
    /// for the rest of the first hold.
    #[test]
    fn a_second_holder_waits_without_blocking_readers() {
        let term = term_with_history(20);
        let gate: TestGate = Arc::default();
        let before = gate.view(&term);
        let first = Holding::start(&term, &gate, HoldKind::Search);

        let (entered_tx, entered_rx) = mpsc::channel();
        let second = {
            let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.hold(&term, HoldKind::Reflow, |_| {}, |term| {
                    entered_tx.send(()).unwrap();
                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(3));
                })
                .unwrap();
            })
        };

        // However long the second holder has been waiting, reads return the
        // first holder's view at once, and the second has not run.
        for _ in 0..100 {
            let (term2, gate2) = (Arc::clone(&term), Arc::clone(&gate));
            let (view, kind) = on_thread("a read while a second holder waits", move || {
                (gate2.view(&term2), gate2.held_kind())
            });
            assert_eq!(view, before);
            assert_eq!(kind, Some(HoldKind::Search));
            std::thread::yield_now();
        }
        assert!(entered_rx.try_recv().is_err(), "the second holder ran inside the first hold");

        first.finish();
        entered_rx.recv_timeout(HANG).expect("the second holder never ran");
        second.join().unwrap();
        assert_eq!(gate.view(&term).display_offset, 3);
        assert!(!gate.gate().is_held());
    }

    #[test]
    fn deferred_mutations_run_in_order_once_at_release() {
        let term = term_with_history(20);
        let gate: TestGate = Arc::default();
        let (log_tx, log_rx) = mpsc::channel();
        let holding = Holding::start(&term, &gate, HoldKind::Reflow);
        for i in 0..3 {
            let log_tx = log_tx.clone();
            let ran_now = {
                let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
                on_thread("a deferred mutation", move || {
                    gate.mutate_or_defer(&term, Box::new(move |_| log_tx.send(i).unwrap()))
                })
            };
            assert!(!ran_now);
        }
        assert!(log_rx.try_recv().is_err(), "a deferred mutation ran inside the hold");
        holding.finish();
        assert_eq!(log_rx.try_iter().collect::<Vec<_>>(), vec![0, 1, 2]);

        // With nothing held a mutation runs at once.
        assert!(gate.mutate_or_defer(&term, Box::new(move |_| log_tx.send(9).unwrap())));
        assert_eq!(log_rx.try_iter().collect::<Vec<_>>(), vec![9]);
    }

    #[test]
    fn deferred_mutations_run_when_the_holder_panics() {
        let term = term_with_history(20);
        let gate: TestGate = Arc::default();
        let (log_tx, log_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.hold(&term, HoldKind::Reflow, |_| {}, |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("reflow panicked");
                })
            })
        };
        entered_rx.recv_timeout(HANG).unwrap();
        assert!(!gate.mutate_or_defer(&term, Box::new(move |_| log_tx.send(1).unwrap())));
        release_tx.send(()).unwrap();
        assert!(holder.join().unwrap().is_err());
        assert_eq!(log_rx.try_iter().collect::<Vec<_>>(), vec![1]);
        assert!(!gate.gate().is_held());
    }

    #[test]
    fn try_mut_is_refused_while_held() {
        let term = term_with_history(20);
        let gate: TestGate = Arc::default();
        let holding = Holding::start(&term, &gate, HoldKind::Reflow);
        let refused = {
            let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
            on_thread("try_mut", move || gate.try_mut(&term, |_| ()).is_none())
        };
        assert!(refused);
        holding.finish();
        assert_eq!(gate.try_mut(&term, |_| 7), Some(7));
    }

    /// `prepare` runs before the view is published, so the view already
    /// shows what it changed.
    #[test]
    fn prepare_runs_before_the_view_is_published() {
        let term = term_with_history(20);
        let gate: TestGate = Arc::default();
        let point = Point::new(Line(0), Column(1));
        {
            let mut t = term.lock();
            t.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
            t.selection.as_mut().unwrap().update(Point::new(Line(0), Column(4)), Side::Right);
        }
        assert!(gate.view(&term).has_selection);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let (term, gate) = (Arc::clone(&term), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.hold(&term, HoldKind::Reflow, |t| t.selection = None, |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .unwrap();
            })
        };
        entered_rx.recv_timeout(HANG).unwrap();
        assert!(!gate.view(&term).has_selection);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }
}
