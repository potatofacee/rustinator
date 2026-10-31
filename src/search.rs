//! Scrollback search off the UI thread.
//!
//! A search with no match walks every line of history, up to
//! `MAX_SCROLLBACK_LINES` of them: about half a second per 300k rows in RAM,
//! far longer when they are swapped out. It runs on one FIFO "search" thread
//! instead of in the search bar's frame.
//!
//! The match is applied (selected and scrolled to) by that thread under the
//! same terminal lock the scan ran under. A `Match` is a pair of grid points,
//! and grid points move whenever output scrolls, the window reflows or history
//! is cleared, so handing one back to the UI thread to apply after the lock
//! is dropped could select the wrong text.
//!
//! The scan holds the lock throughout: alacritty's `search_next` cannot be
//! resumed, and a scan split into pieces would read a grid that moves under it
//! between pieces. The PTY loop stops reading output for it (paused, still
//! writing input), as it waited for the UI thread's search. The UI thread
//! must not wait: it locks every visible pane's
//! terminal each frame (mode, scroll position, a snapshot when dirty). It
//! goes through the pane's `TermGate` instead (see `crate::term_gate`):
//! reads get the mode and scroll position the search thread published when
//! it took the lock, which are exact, since only the search's own selection
//! and scroll change the terminal until it lets go; mutations that must not
//! be lost wait for it, and gestures (a click, a wheel scroll) are dropped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::index::{Boundary, Direction as GridDir, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::RegexSearch;
use winit::event_loop::EventLoopProxy;

use crate::pane::{DropOffThread, EventProxy};
use crate::term_gate::{HoldKind, TermGate};
use crate::window::UserEvent;

/// One pane's search state, shared with the jobs it submits. Replaced with
/// the terminal on respawn, so a job still running on the old terminal
/// touches only the old slot.
#[derive(Default)]
pub(crate) struct SearchSlot {
    /// Bumped for every search the UI starts and when it abandons them. A
    /// job selects its match only if this still equals its ticket, checked
    /// under the terminal lock: a newer search, or the bar closing, makes it
    /// stale.
    generation: AtomicU64,
}

impl SearchSlot {
    /// A ticket for a new search, making every earlier one stale.
    pub(crate) fn begin(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Make every search started so far stale.
    pub(crate) fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn is_current(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::Acquire) == ticket
    }
}

/// A search for the search thread.
pub(crate) struct SearchJob<T: Send + 'static> {
    /// Its own handle on the pane's terminal: if the pane closes mid-scan,
    /// this is the last one, and its drop goes to the reaper as the pane's
    /// would have.
    pub(crate) terminal: DropOffThread<Arc<FairMutex<Term<T>>>>,
    pub(crate) slot: Arc<SearchSlot>,
    /// The pane's terminal gate, published to while the scan holds the lock.
    pub(crate) gate: Arc<TermGate<T>>,
    pub(crate) ticket: u64,
    pub(crate) needle: String,
    pub(crate) backward: bool,
    pub(crate) dirty: Arc<AtomicBool>,
    /// Wakes the UI thread when the scan lets the terminal go, for the frame
    /// it deferred (`Pane::frame`) and the match it selected. None in tests.
    pub(crate) wake: Option<EventLoopProxy<UserEvent>>,
}

/// Queue `job` on the search thread, started on the first search. If the
/// thread cannot be started (or died), search inline as before rather than
/// not at all.
pub(crate) fn submit(job: SearchJob<EventProxy>) {
    static WORKER: OnceLock<Option<mpsc::Sender<SearchJob<EventProxy>>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<SearchJob<EventProxy>>();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || rx.into_iter().for_each(run))
            .ok()
            .map(|_| tx)
    });
    match worker {
        Some(tx) => {
            if let Err(mpsc::SendError(job)) = tx.send(job) {
                run(job);
            }
        }
        None => run(job),
    }
}

/// Run one search. A job made stale before it starts is skipped without
/// taking the lock; one made stale while it scans finishes the scan (it
/// cannot be interrupted) and selects nothing.
pub(crate) fn run<T: EventListener + Send + 'static>(job: SearchJob<T>) {
    if !job.slot.is_current(job.ticket) {
        return;
    }
    let pattern = crate::pane::escape_regex(&job.needle);
    let mut regex = match RegexSearch::new(&pattern) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("search regex build failed: {e}");
            return;
        }
    };
    let found = job.gate.hold(&job.terminal, HoldKind::Search, |_| {}, |term| {
        select_next_match(term, &mut regex, job.backward, || job.slot.is_current(job.ticket))
    });
    if matches!(found, Ok(true)) {
        job.dirty.store(true, Ordering::Release);
    }
    if let Some(proxy) = &job.wake {
        let _ = proxy.send_event(UserEvent::Repaint);
    }
}

/// Find the next match of `regex` past the selection (or from the cursor),
/// searching all of history, and if `current()` still holds, select it and
/// scroll it into view. Returns whether it selected one.
pub(crate) fn select_next_match<T: EventListener>(
    term: &mut Term<T>,
    regex: &mut RegexSearch,
    backward: bool,
    current: impl FnOnce() -> bool,
) -> bool {
    // Origin: skip past the current selection/cursor so consecutive searches advance.
    let origin = match (term.selection.as_ref().and_then(|s| s.to_range(term)), backward) {
        (Some(range), true) => range.start.sub(term, Boundary::None, 1),
        (Some(range), false) => range.end.add(term, Boundary::None, 1),
        (None, _) => term.grid().cursor.point,
    };
    let direction = if backward { GridDir::Left } else { GridDir::Right };
    let Some(m) = term.search_next(regex, origin, direction, Side::Left, None) else {
        return false;
    };
    if !current() {
        return false;
    }
    let start = *m.start();
    let end = *m.end();
    term.selection = Some(Selection::new(SelectionType::Simple, start, Side::Left));
    if let Some(sel) = term.selection.as_mut() {
        sel.update(end, Side::Right);
    }
    term.scroll_to_point(start);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    type TestTerm = Arc<FairMutex<Term<VoidListener>>>;

    /// A 20x5 terminal with `history` lines of scrollback, "needle" printed
    /// on the first line (now the oldest in history) and filler after it.
    fn term_with_needle_at_top(history: usize) -> TestTerm {
        let config = Config { scrolling_history: history, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(20, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut bytes = b"needle\r\n".to_vec();
        for i in 0..history + 3 {
            bytes.extend_from_slice(format!("filler {i}\r\n").as_bytes());
        }
        parser.advance(&mut term, &bytes);
        assert_eq!(term.grid().history_size(), history);
        Arc::new(FairMutex::new(term))
    }

    fn job(term: &TestTerm, slot: &Arc<SearchSlot>, ticket: u64) -> SearchJob<VoidListener> {
        SearchJob {
            terminal: DropOffThread::new(Arc::clone(term)),
            slot: Arc::clone(slot),
            gate: Arc::default(),
            ticket,
            needle: "needle".into(),
            backward: true,
            dirty: Arc::new(AtomicBool::new(false)),
            wake: None,
        }
    }

    #[test]
    fn a_newer_search_or_a_cancel_makes_a_ticket_stale() {
        let slot = SearchSlot::default();
        let first = slot.begin();
        assert!(slot.is_current(first));
        let second = slot.begin();
        assert!(!slot.is_current(first), "a newer search supersedes the first");
        assert!(slot.is_current(second));
        slot.cancel();
        assert!(!slot.is_current(second), "closing the bar abandons the search");
        assert!(slot.is_current(slot.begin()));
    }

    #[test]
    fn a_current_job_finds_and_selects_a_match_at_the_far_end_of_history() {
        // No line limit: the match is 2000 lines up, past alacritty's
        // 1000-line typing limit.
        let term = term_with_needle_at_top(2000);
        let slot = Arc::new(SearchSlot::default());
        let job = job(&term, &slot, slot.begin());
        let dirty = Arc::clone(&job.dirty);
        let gate = Arc::clone(&job.gate);
        run(job);
        let term = term.lock();
        let top = Line(-2000);
        let range = term.selection.as_ref().and_then(|s| s.to_range(&*term)).expect("selected");
        assert_eq!((range.start, range.end), (Point::new(top, Column(0)), Point::new(top, Column(5))));
        assert_eq!(term.grid().display_offset(), 2000, "scrolled to the match");
        assert!(dirty.load(Ordering::Acquire));
        assert!(!gate.gate().is_held(), "the gate is clear once the job is done");
    }

    #[test]
    fn a_job_made_stale_before_it_runs_selects_nothing() {
        let term = term_with_needle_at_top(50);
        let slot = Arc::new(SearchSlot::default());
        let job = job(&term, &slot, slot.begin());
        let dirty = Arc::clone(&job.dirty);
        slot.begin();
        run(job);
        assert!(term.lock().selection.is_none());
        assert_eq!(term.lock().grid().display_offset(), 0);
        assert!(!dirty.load(Ordering::Acquire));
    }

    #[test]
    fn a_match_found_after_the_job_went_stale_is_not_selected() {
        let term = term_with_needle_at_top(50);
        let mut term = term.lock();
        let mut regex = RegexSearch::new("needle").unwrap();
        assert!(!select_next_match(&mut term, &mut regex, true, || false));
        assert!(term.selection.is_none());
        assert_eq!(term.grid().display_offset(), 0);
        assert!(select_next_match(&mut term, &mut regex, true, || true));
        assert!(term.selection.is_some());
    }
}
