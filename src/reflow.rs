//! Scrollback reflow off the UI thread.
//!
//! A change of a pane's column count makes alacritty's `Term::resize` rewrap
//! every row of history (both grids: on the alternate screen the inactive
//! primary grid still reflows), up to `MAX_SCROLLBACK_LINES` rows of
//! `cols × 24` bytes. In RAM that is a noticeable stall; swapped out it is
//! seconds to minutes, and it has to run in one piece under the terminal lock
//! (it takes the rows out of storage and puts them back). It runs on one FIFO
//! "reflow" thread, holding the terminal through the pane's `TermGate`, so
//! the UI thread never waits for it: the pane shows its last frame, reads get
//! the view published when the reflow took the terminal, and mutations wait
//! for it or are dropped (see `crate::term_gate`).
//!
//! Each pane has a `ReflowSlot` holding the newest size the UI asked for.
//! The UI only overwrites it, so latest size wins: a job takes the size, and
//! if a newer one arrived while it ran it goes to the back of the queue to
//! run again (a continuous drag round-robins across panes instead of letting
//! one starve the rest). The PTY is told the new size (SIGWINCH, and the
//! XTWINOPS mirror) only once the grid matches the final size, so the app
//! redraws once, for a grid that has its size.
//!
//! A change of lines only, with the columns kept, touches no history: it
//! stays on the UI thread when nothing holds the terminal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};

use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use winit::event_loop::EventLoopProxy;

use crate::pane::{DropOffThread, EventProxy, TermDims};
use crate::pty_event_loop::{EventLoopSender, Msg};
use crate::term_gate::{HoldKind, TermGate};
use crate::window::UserEvent;

/// What `ReflowSlot::request` tells the UI to do with a new size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// The columns are unchanged and nothing is queued: resize on the UI
    /// thread, if the gate is free (`ReflowSlot::queue` if not).
    Sync,
    /// Submit a job; the slot is now queued.
    Submit,
    /// A job is already queued or running; it will pick the size up.
    Coalesced,
    /// The pane is closing.
    Ignored,
}

/// One pane's reflow state, shared with its job. Replaced with the terminal
/// on respawn, so a job still running on the old terminal touches only the
/// old slot.
pub(crate) struct ReflowSlot {
    state: Mutex<SlotState>,
}

struct SlotState {
    /// The newest size asked for and not yet taken by a job.
    target: Option<WindowSize>,
    /// A job for this slot is queued or running.
    queued: bool,
    /// The pane is gone (or respawned): queued and re-queued jobs stop.
    closed: bool,
    /// The grid's (columns, lines).
    applied: (usize, usize),
    /// The size last sent to the PTY (and the XTWINOPS mirror).
    sent: WindowSize,
}

fn dims(ws: WindowSize) -> (usize, usize) {
    (ws.num_cols as usize, ws.num_lines as usize)
}

/// `WindowSize` has no `PartialEq`.
fn same_size(a: WindowSize, b: WindowSize) -> bool {
    (a.num_cols, a.num_lines, a.cell_width, a.cell_height)
        == (b.num_cols, b.num_lines, b.cell_width, b.cell_height)
}

impl ReflowSlot {
    /// A slot for a terminal built at `ws`, whose PTY was opened at `ws`.
    pub(crate) fn new(ws: WindowSize) -> Self {
        Self {
            state: Mutex::new(SlotState {
                target: None,
                queued: false,
                closed: false,
                applied: dims(ws),
                sent: ws,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The UI asks for `ws` (a size other than the last one it asked for).
    pub(crate) fn request(&self, ws: WindowSize) -> Request {
        let mut state = self.lock();
        if state.closed {
            Request::Ignored
        } else if state.queued {
            state.target = Some(ws);
            Request::Coalesced
        } else if dims(ws).0 == state.applied.0 {
            Request::Sync
        } else {
            state.target = Some(ws);
            state.queued = true;
            Request::Submit
        }
    }

    /// Queue `ws` for the reflow thread whatever its columns: for a `Sync`
    /// request the UI could not apply because a search holds the terminal.
    pub(crate) fn queue(&self, ws: WindowSize) -> Request {
        let mut state = self.lock();
        if state.closed {
            return Request::Ignored;
        }
        state.target = Some(ws);
        if state.queued {
            Request::Coalesced
        } else {
            state.queued = true;
            Request::Submit
        }
    }

    /// The UI resized the grid to `ws` itself (a `Sync` request): record it
    /// and tell the PTY through `send` if that changes what it was told.
    pub(crate) fn applied_now(&self, ws: WindowSize, send: impl FnOnce(WindowSize)) {
        let mut state = self.lock();
        debug_assert!(!state.queued, "a Sync resize with a reflow queued");
        state.applied = dims(ws);
        if !same_size(ws, state.sent) {
            state.sent = ws;
            send(ws);
        }
    }

    /// The cell size changed with the grid size kept (font zoom). While a
    /// reflow is pending, fold it into the pending size and return true: the
    /// job sends it with the final grid size. Otherwise tell the PTY now
    /// through `send` (always, as before) and return false.
    pub(crate) fn update_cells(&self, ws: WindowSize, send: impl FnOnce(WindowSize)) -> bool {
        let mut state = self.lock();
        if state.queued && !state.closed {
            state.target = Some(ws);
            return true;
        }
        state.sent = ws;
        send(ws);
        false
    }

    /// The pane is closing or respawning: drop the pending size and stop any
    /// job at its next check. A `Term::resize` already running finishes.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.target = None;
    }

    /// A job for this slot is queued or running.
    #[cfg(test)]
    pub(crate) fn is_pending(&self) -> bool {
        self.lock().queued
    }
}

/// A slot's job for the reflow thread. Owns handles on everything it
/// touches, so a pane closed or respawned meanwhile is unaffected.
pub(crate) struct ReflowJob<T: Send + 'static> {
    /// Its own handle on the pane's terminal: if the pane closes mid-reflow,
    /// this is the last one, and its drop goes to the reaper as the pane's
    /// would have.
    pub(crate) terminal: DropOffThread<Arc<FairMutex<Term<T>>>>,
    pub(crate) gate: Arc<TermGate<T>>,
    pub(crate) slot: Arc<ReflowSlot>,
    pub(crate) pty_tx: EventLoopSender,
    /// The XTWINOPS size mirror (`EventProxy::window_size`).
    pub(crate) window_size: Arc<Mutex<WindowSize>>,
    pub(crate) dirty: Arc<AtomicBool>,
    /// Wakes the UI thread for the frame it kept while the reflow ran. None
    /// in tests.
    pub(crate) wake: Option<EventLoopProxy<UserEvent>>,
}

/// What to do with a job after one run.
pub(crate) enum Next<T: Send + 'static> {
    /// A newer size arrived while it ran: queue it again, at the back.
    Again(ReflowJob<T>),
    Done,
}

/// Run one job with the production resize.
pub(crate) fn run<T: EventListener + Send + 'static>(job: ReflowJob<T>) -> Next<T> {
    run_with(job, |term, dims| term.resize(dims))
}

/// Run one job: take the slot's newest size and, if it differs from the
/// grid's, `apply` it holding the terminal through the gate. Then either
/// hand the job back to run again (a newer size arrived) or tell the PTY the
/// final size, and wake the UI.
pub(crate) fn run_with<T: Send + 'static>(
    job: ReflowJob<T>,
    apply: impl FnOnce(&mut Term<T>, TermDims),
) -> Next<T> {
    let (ws, applied) = {
        let mut state = job.slot.lock();
        let target = if state.closed { None } else { state.target.take() };
        match target {
            Some(ws) => (ws, state.applied),
            None => {
                state.queued = false;
                return Next::Done;
            }
        }
    };
    let (cols, lines) = dims(ws);
    if (cols, lines) != applied {
        let new_cols = cols != applied.0;
        let held = job.gate.hold(
            &job.terminal,
            HoldKind::Reflow,
            // `Term::resize` drops the selection on a column change; drop it
            // before the view is published so `has_selection` agrees.
            |term| {
                if new_cols {
                    term.selection = None;
                }
            },
            |term| apply(term, TermDims { cols, lines }),
        );
        if held.is_err() {
            log::error!("scrollback reflow to {cols}x{lines} panicked");
        }
    }

    let mut state = job.slot.lock();
    state.applied = (cols, lines);
    let again = state.target.is_some() && !state.closed;
    if !again {
        state.queued = false;
        if !state.closed && !same_size(ws, state.sent) {
            // Under the slot lock, so Resize messages go out in order.
            state.sent = ws;
            *job.window_size.lock().unwrap_or_else(PoisonError::into_inner) = ws;
            let _ = job.pty_tx.send(Msg::Resize(ws));
        }
    }
    drop(state);

    // Also between runs: the grid has a new size, and the UI may as well
    // show it while the job waits its turn again.
    job.dirty.store(true, Ordering::Release);
    if let Some(proxy) = &job.wake {
        let _ = proxy.send_event(UserEvent::Repaint);
    }
    if again { Next::Again(job) } else { Next::Done }
}

/// Queue `job` on the reflow thread, started on the first reflow. If the
/// thread cannot be started (or died), reflow inline rather than not at all.
pub(crate) fn submit(job: ReflowJob<EventProxy>) {
    static WORKER: OnceLock<Option<mpsc::Sender<ReflowJob<EventProxy>>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<ReflowJob<EventProxy>>();
        let requeue = tx.clone();
        std::thread::Builder::new()
            .name("reflow".into())
            .spawn(move || {
                for job in rx {
                    if let Next::Again(job) = run(job) {
                        // To the back of the queue, behind other panes' jobs.
                        // The thread holds `requeue`, so this cannot fail.
                        let _ = requeue.send(job);
                    }
                }
            })
            .ok()
            .map(|_| tx)
    });
    let job = match worker {
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    run_inline(job);
}

fn run_inline<T: EventListener + Send + 'static>(mut job: ReflowJob<T>) {
    while let Next::Again(next) = run(job) {
        job = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, TermMode};
    use alacritty_terminal::vte::ansi::Processor;

    use crate::pty_event_loop::LoopChannel;

    const HANG: Duration = Duration::from_secs(10);

    fn ws(cols: u16, lines: u16) -> WindowSize {
        WindowSize { num_cols: cols, num_lines: lines, cell_width: 8, cell_height: 16 }
    }

    fn feed<T: EventListener>(term: &mut Term<T>, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, bytes);
    }

    /// A pane's worth of reflow state around a terminal, with a PTY channel
    /// whose loop never starts.
    struct Fixture<T: EventListener + Send + 'static> {
        term: Arc<FairMutex<Term<T>>>,
        gate: Arc<TermGate<T>>,
        slot: Arc<ReflowSlot>,
        channel: LoopChannel,
        window_size: Arc<Mutex<WindowSize>>,
        dirty: Arc<AtomicBool>,
    }

    impl<T: EventListener + Send + 'static> Fixture<T> {
        fn new(cols: u16, lines: u16, history: usize, listener: T) -> Self {
            let config = Config { scrolling_history: history, ..Config::default() };
            let dims = TermDims { cols: cols as usize, lines: lines as usize };
            let mut term = Term::new(config, &dims, listener);
            let mut bytes = Vec::new();
            for i in 0..history + lines as usize {
                bytes.extend_from_slice(format!("history line {i}\r\n").as_bytes());
            }
            feed(&mut term, &bytes);
            Self {
                term: Arc::new(FairMutex::new(term)),
                gate: Arc::default(),
                slot: Arc::new(ReflowSlot::new(ws(cols, lines))),
                channel: LoopChannel::new().expect("loop channel"),
                window_size: Arc::new(Mutex::new(ws(cols, lines))),
                dirty: Arc::new(AtomicBool::new(false)),
            }
        }

        fn job(&self) -> ReflowJob<T> {
            ReflowJob {
                terminal: DropOffThread::new(Arc::clone(&self.term)),
                gate: Arc::clone(&self.gate),
                slot: Arc::clone(&self.slot),
                pty_tx: self.channel.sender(),
                window_size: Arc::clone(&self.window_size),
                dirty: Arc::clone(&self.dirty),
                wake: None,
            }
        }

        /// The sizes sent to the PTY since the last call; panics on any
        /// other message.
        fn resizes(&self) -> Vec<(u16, u16, u16, u16)> {
            self.channel
                .drain()
                .into_iter()
                .map(|msg| match msg {
                    Msg::Resize(w) => (w.num_cols, w.num_lines, w.cell_width, w.cell_height),
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        }

        fn grid_size(&self) -> (usize, usize) {
            let term = self.term.lock();
            (term.columns(), term.screen_lines())
        }
    }

    /// A job run on its own thread whose resize blocks until released.
    struct Blocked<T: EventListener + Send + 'static> {
        release: mpsc::Sender<()>,
        done: mpsc::Receiver<Next<T>>,
    }

    impl<T: EventListener + Send + 'static> Blocked<T> {
        /// Start `job`; returns once it is inside the hold, with the size
        /// it is applying.
        fn start(job: ReflowJob<T>, applied: mpsc::Sender<(usize, usize)>) -> (Self, (usize, usize)) {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel::<()>();
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                let next = run_with(job, |term, dims| {
                    applied.send((dims.cols, dims.lines)).unwrap();
                    entered_tx.send((dims.cols, dims.lines)).unwrap();
                    release_rx.recv().unwrap();
                    term.resize(dims);
                });
                let _ = done_tx.send(next);
            });
            let size = entered_rx.recv_timeout(HANG).expect("the job never took the terminal");
            (Self { release, done }, size)
        }

        fn finish(self) -> Next<T> {
            self.release.send(()).unwrap();
            self.done.recv_timeout(HANG).expect("the job never finished")
        }
    }

    #[test]
    fn requests_submit_once_then_coalesce_to_the_newest() {
        let slot = ReflowSlot::new(ws(80, 24));
        assert_eq!(slot.request(ws(80, 30)), Request::Sync, "same columns, nothing queued");
        assert_eq!(slot.request(ws(100, 24)), Request::Submit);
        assert!(slot.is_pending());
        assert_eq!(slot.request(ws(110, 24)), Request::Coalesced);
        assert_eq!(slot.request(ws(80, 30)), Request::Coalesced, "even same columns, once queued");
        assert!(same_size(slot.lock().target.unwrap(), ws(80, 30)), "the newest size wins");
        slot.close();
        assert_eq!(slot.request(ws(120, 24)), Request::Ignored);
        assert_eq!(slot.queue(ws(120, 24)), Request::Ignored);
        assert!(slot.lock().target.is_none());
    }

    #[test]
    fn queue_forces_a_job_for_a_sync_request() {
        let slot = ReflowSlot::new(ws(80, 24));
        assert_eq!(slot.queue(ws(80, 30)), Request::Submit);
        assert_eq!(slot.queue(ws(80, 31)), Request::Coalesced);
        assert!(same_size(slot.lock().target.unwrap(), ws(80, 31)));
    }

    /// The core of latest-size-wins: a size asked for while a job runs makes
    /// it run again, sizes in between are never applied, and the PTY hears
    /// only the final size, once, after the grid has it.
    #[test]
    fn a_size_asked_for_mid_run_runs_again_and_only_the_final_size_is_sent() {
        let f = Fixture::new(20, 5, 200, VoidListener);
        assert_eq!(f.slot.request(ws(30, 5)), Request::Submit);
        let (applied_tx, applied_rx) = mpsc::channel();
        let (blocked, size) = Blocked::start(f.job(), applied_tx.clone());
        assert_eq!(size, (30, 5));

        assert_eq!(f.slot.request(ws(40, 5)), Request::Coalesced);
        assert_eq!(f.slot.request(ws(50, 6)), Request::Coalesced);
        assert!(f.resizes().is_empty(), "nothing sent while the reflow runs");

        let Next::Again(job) = blocked.finish() else { panic!("a newer size must run again") };
        assert!(f.resizes().is_empty(), "no SIGWINCH for an intermediate size");
        assert_eq!(f.grid_size(), (30, 5));
        assert!(f.dirty.swap(false, Ordering::AcqRel), "the intermediate grid is repainted");

        let next = run_with(job, |term, dims| {
            applied_tx.send((dims.cols, dims.lines)).unwrap();
            term.resize(dims);
        });
        assert!(matches!(next, Next::Done));
        assert_eq!(applied_rx.try_iter().collect::<Vec<_>>(), vec![(30, 5), (50, 6)]);
        assert_eq!(f.grid_size(), (50, 6));
        assert_eq!(f.resizes(), vec![(50, 6, 8, 16)], "exactly one Resize, at the final size");
        assert!(same_size(*f.window_size.lock().unwrap(), ws(50, 6)), "XTWINOPS mirror");
        assert!(f.dirty.load(Ordering::Acquire));
        assert!(!f.slot.is_pending());
        assert_eq!(f.slot.request(ws(50, 9)), Request::Sync, "a later lines-only change is sync");
    }

    #[test]
    fn dragged_back_to_the_grid_size_skips_the_hold() {
        let f = Fixture::new(20, 5, 50, VoidListener);
        assert_eq!(f.slot.request(ws(30, 5)), Request::Submit);
        assert_eq!(f.slot.request(ws(20, 5)), Request::Coalesced);
        let next = run_with(f.job(), |_, _| panic!("applied a size the grid already has"));
        assert!(matches!(next, Next::Done));
        assert!(f.resizes().is_empty(), "the PTY already has this size");
        assert!(!f.slot.is_pending());

        // Back to the grid size but with new cells: no hold, one Resize.
        assert_eq!(f.slot.request(ws(30, 5)), Request::Submit);
        let cells = WindowSize { cell_width: 9, cell_height: 18, ..ws(20, 5) };
        assert_eq!(f.slot.request(cells), Request::Coalesced);
        let next = run_with(f.job(), |_, _| panic!("applied a size the grid already has"));
        assert!(matches!(next, Next::Done));
        assert_eq!(f.resizes(), vec![(20, 5, 9, 18)]);
        assert_eq!(f.grid_size(), (20, 5));
    }

    #[test]
    fn a_new_cell_size_folds_into_a_pending_reflow() {
        let f = Fixture::new(20, 5, 50, VoidListener);
        let send = |w: WindowSize| f.channel.sender().send(Msg::Resize(w)).unwrap();
        // Nothing pending: sent at once, as before.
        assert!(!f.slot.update_cells(WindowSize { cell_width: 9, ..ws(20, 5) }, send));
        assert_eq!(f.resizes(), vec![(20, 5, 9, 16)]);

        assert_eq!(f.slot.request(WindowSize { cell_width: 9, ..ws(30, 5) }), Request::Submit);
        let zoomed = WindowSize { cell_width: 10, cell_height: 20, ..ws(30, 5) };
        assert!(f.slot.update_cells(zoomed, |_| panic!("sent mid-reflow")));
        assert!(matches!(run(f.job()), Next::Done));
        assert_eq!(f.resizes(), vec![(30, 5, 10, 20)]);
    }

    #[test]
    fn a_closed_slot_runs_nothing() {
        let f = Fixture::new(20, 5, 50, VoidListener);
        assert_eq!(f.slot.request(ws(30, 5)), Request::Submit);
        f.slot.close();
        let next = run_with(f.job(), |_, _| panic!("reflowed a closed pane"));
        assert!(matches!(next, Next::Done));
        assert!(f.resizes().is_empty());
        assert_eq!(f.grid_size(), (20, 5));
    }

    /// Records which thread dropped it.
    struct DropProbe(mpsc::Sender<Option<String>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.send(std::thread::current().name().map(str::to_owned));
        }
    }

    #[derive(Clone)]
    struct ProbeListener(#[allow(dead_code)] Arc<DropProbe>);

    impl EventListener for ProbeListener {}

    /// The pane closes mid-reflow: the running resize finishes, the newer
    /// size is not run, and the terminal (whose last handle the job holds)
    /// frees on the reaper, not on the reflow thread.
    #[test]
    fn closing_mid_run_stops_the_job_and_frees_the_terminal_on_the_reaper() {
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let f = Fixture::new(20, 5, 50, ProbeListener(Arc::new(DropProbe(dropped_tx))));
        assert_eq!(f.slot.request(ws(30, 5)), Request::Submit);
        let (applied_tx, applied_rx) = mpsc::channel();
        let (blocked, _) = Blocked::start(f.job(), applied_tx);
        assert_eq!(f.slot.request(ws(40, 5)), Request::Coalesced);
        f.slot.close();
        let Fixture { term, gate, slot, channel, .. } = f;
        drop((term, gate));

        assert!(matches!(blocked.finish(), Next::Done), "a closed pane is not run again");
        assert_eq!(applied_rx.try_iter().collect::<Vec<_>>(), vec![(30, 5)]);
        assert!(!slot.is_pending());
        assert!(channel.drain().is_empty(), "nothing sent for a closed pane");
        let thread = dropped_rx.recv_timeout(HANG).expect("the terminal was never freed");
        assert_eq!(thread.as_deref(), Some("term reaper"));
    }

    /// `Term::resize` reflows the primary grid even while the alternate
    /// screen is shown, so a column change there goes to the reflow thread
    /// too, and the primary's history comes back rewrapped.
    #[test]
    fn the_alternate_screen_reflows_the_primary_history_too() {
        let f = Fixture::new(20, 5, 1000, VoidListener);
        let history_before = {
            let mut term = f.term.lock();
            let mut bytes = Vec::new();
            for _ in 0..100 {
                bytes.extend_from_slice(&[b'x'; 30]);
                bytes.extend_from_slice(b"\r\n");
            }
            feed(&mut *term, &bytes);
            let history = term.grid().history_size();
            feed(&mut *term, b"\x1b[?1049h");
            assert!(term.mode().contains(TermMode::ALT_SCREEN));
            assert_eq!(term.grid().history_size(), 0, "the alternate grid has no history");
            history
        };
        assert_eq!(f.slot.request(ws(40, 5)), Request::Submit, "columns change on the alt screen");
        assert!(matches!(run(f.job()), Next::Done));

        let mut term = f.term.lock();
        feed(&mut *term, b"\x1b[?1049l");
        assert!(!term.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(term.columns(), 40);
        // The 100 lines of 30 took two 20-column rows each; at 40 one.
        let history = term.grid().history_size();
        assert!(history + 90 < history_before, "primary history not rewrapped: {history_before} -> {history}");
    }
}
