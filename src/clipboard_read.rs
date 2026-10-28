//! Explicit paste reads (the Paste bindings, Shift+Insert, the context menu,
//! middle-click) off the UI thread.
//!
//! An X11 selection read is a round trip to the selection owner, and arboard
//! waits up to 4 s for each target format it tries (`LONG_TIMEOUT_DUR`,
//! `read_single`). Under Xvnc the owner is often the VNC server's clipboard
//! bridge, which answers late or never while the viewer is idle, so a read
//! on the UI thread froze painting and PTY output for that long. Reads run
//! on one FIFO worker thread instead, and the result comes back to the
//! winit loop as `UserEvent::ClipboardRead`.
//!
//! The blocking read used to order things for free: keys typed after a
//! paste reached the PTY after the pasted text. `PasteGate` keeps that: keys
//! queue behind an unfinished paste and are routed, in order, once its text
//! has been sent.

use std::collections::VecDeque;

use crate::pane::PaneId;

/// Which selection a paste reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Clipboard,
    Primary,
}

/// A paste asked for by an action, with its targets resolved when it was
/// asked for, as the blocking read did. They are looked up again when the
/// text arrives, and panes gone by then are skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasteRequest {
    pub(crate) selection: Selection,
    pub(crate) targets: Vec<PaneId>,
}

/// Read `selection` on the calling thread: the read the paste actions did
/// inline before, with its failure message. None when there is nothing to
/// paste.
pub(crate) fn read_now(selection: Selection) -> Option<String> {
    match selection {
        Selection::Clipboard => match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("clipboard read failed: {e}");
                None
            }
        },
        Selection::Primary => crate::pane_ui::read_primary(),
    }
}

pub(crate) use reader::ClipboardReader;

/// The X11/Wayland backends, whose reads are selection round trips to
/// another client, read off the UI thread.
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod reader {
    use winit::event_loop::EventLoopProxy;

    use super::{read_now, Selection};
    use crate::window::UserEvent;

    struct Job {
        id: u64,
        selection: Selection,
    }

    enum Worker {
        NotStarted,
        Running(std::sync::mpsc::Sender<Job>),
        /// The thread could not be started, or has exited: reads are inline.
        Unavailable,
    }

    /// The process's clipboard-reader thread, started on the first read.
    pub(crate) struct ClipboardReader {
        proxy: EventLoopProxy<UserEvent>,
        worker: Worker,
        next_id: u64,
    }

    impl ClipboardReader {
        pub(crate) fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
            Self { proxy, worker: Worker::NotStarted, next_id: 0 }
        }

        /// Queue a read of `selection`. `Some(id)`: the text arrives as
        /// `UserEvent::ClipboardRead { id, .. }`, exactly once. `None`: there
        /// is no worker, and the caller reads with `read_now` itself.
        pub(crate) fn submit(&mut self, selection: Selection) -> Option<u64> {
            if let Worker::NotStarted = self.worker {
                self.worker = spawn_worker(self.proxy.clone());
            }
            let Worker::Running(tx) = &self.worker else {
                return None;
            };
            let id = self.next_id;
            if tx.send(Job { id, selection }).is_err() {
                self.worker = Worker::Unavailable;
                return None;
            }
            self.next_id += 1;
            Some(id)
        }
    }

    fn spawn_worker(proxy: EventLoopProxy<UserEvent>) -> Worker {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("clipboard-read".into())
            .spawn(move || {
                for job in rx {
                    // Every job is answered, a panicking read included, so a
                    // paste the gate holds keys behind always completes.
                    let text = std::panic::catch_unwind(|| read_now(job.selection)).unwrap_or(None);
                    if proxy.send_event(UserEvent::ClipboardRead { id: job.id, text }).is_err() {
                        return; // the event loop is gone
                    }
                }
            });
        match spawned {
            Ok(_) => Worker::Running(tx),
            Err(e) => {
                eprintln!("clipboard reader thread failed to start ({e}); reading inline");
                Worker::Unavailable
            }
        }
    }
}

/// Every other backend reads inline. On macOS arboard asserts `Send` for its
/// `NSPasteboard` handle, but AppKit does not document NSPasteboard as
/// thread-safe, iTerm2 reads it on the main thread, and a read is a call to
/// the pasteboard server with none of the X11 selection timeouts; PRIMARY is
/// an in-process buffer there.
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
mod reader {
    use winit::event_loop::EventLoopProxy;

    use super::Selection;
    use crate::window::UserEvent;

    pub(crate) struct ClipboardReader;

    impl ClipboardReader {
        pub(crate) fn new(_proxy: EventLoopProxy<UserEvent>) -> Self {
            Self
        }

        /// Always None: the caller reads with `read_now` itself.
        pub(crate) fn submit(&mut self, _selection: Selection) -> Option<u64> {
            None
        }
    }
}

/// Where a paste's read stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PasteRead {
    /// Queued on the worker under this id.
    Waiting(u64),
    /// Done: the text, or None when there is nothing to paste.
    Done(Option<String>),
}

enum Slot<K> {
    Key(K),
    Paste { targets: Vec<PaneId>, read: PasteRead },
}

/// What the gate releases next, in input order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step<K> {
    Key(K),
    Paste { targets: Vec<PaneId>, text: Option<String> },
}

/// The window's keyboard input and pastes, in the order they happened. Keys
/// behind a paste whose read has not finished stay queued; everything ahead
/// of it is released in order, the paste itself once its text is in. Pastes
/// complete in the order they were asked for, whatever order their reads
/// finish in.
pub(crate) struct PasteGate<K> {
    queue: VecDeque<Slot<K>>,
}

impl<K> PasteGate<K> {
    pub(crate) fn new() -> Self {
        Self { queue: VecDeque::new() }
    }

    /// Queue keys that arrived after everything already queued.
    pub(crate) fn push_keys(&mut self, keys: impl IntoIterator<Item = K>) {
        self.queue.extend(keys.into_iter().map(Slot::Key));
    }

    /// A paste asked for by the key `next` just released: it comes before
    /// every key still queued.
    pub(crate) fn paste_before_queued(&mut self, targets: Vec<PaneId>, read: PasteRead) {
        self.queue.push_front(Slot::Paste { targets, read });
    }

    /// A paste asked for outside key routing (a menu item, a middle-click):
    /// it comes after every key already queued.
    pub(crate) fn paste_after_queued(&mut self, targets: Vec<PaneId>, read: PasteRead) {
        self.queue.push_back(Slot::Paste { targets, read });
    }

    /// Whether a queued paste waits on read `id`.
    pub(crate) fn waits_on(&self, id: u64) -> bool {
        self.queue
            .iter()
            .any(|slot| matches!(slot, Slot::Paste { read: PasteRead::Waiting(w), .. } if *w == id))
    }

    /// The worker finished read `id`. False when no queued paste waits on it.
    pub(crate) fn complete(&mut self, id: u64, text: Option<String>) -> bool {
        for slot in &mut self.queue {
            if let Slot::Paste { read, .. } = slot {
                if *read == PasteRead::Waiting(id) {
                    *read = PasteRead::Done(text);
                    return true;
                }
            }
        }
        false
    }

    /// The next key or finished paste; None when the queue is empty or its
    /// front is a paste still being read.
    pub(crate) fn next(&mut self) -> Option<Step<K>> {
        if matches!(self.queue.front()?, Slot::Paste { read: PasteRead::Waiting(_), .. }) {
            return None;
        }
        Some(match self.queue.pop_front()? {
            Slot::Key(k) => Step::Key(k),
            Slot::Paste { targets, read } => {
                let PasteRead::Done(text) = read else { unreachable!("checked above") };
                Step::Paste { targets, text }
            }
        })
    }

    /// The front paste, if it is finished: lets a paste asked for outside key
    /// routing go out at once without releasing any key.
    pub(crate) fn next_paste(&mut self) -> Option<(Vec<PaneId>, Option<String>)> {
        if !matches!(self.queue.front()?, Slot::Paste { read: PasteRead::Done(_), .. }) {
            return None;
        }
        match self.queue.pop_front()? {
            Slot::Paste { targets, read: PasteRead::Done(text) } => Some((targets, text)),
            _ => unreachable!("checked above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(gate: &mut PasteGate<char>) -> Vec<Step<char>> {
        std::iter::from_fn(|| gate.next()).collect()
    }

    fn paste(targets: &[PaneId], text: &str) -> Step<char> {
        Step::Paste { targets: targets.to_vec(), text: Some(text.into()) }
    }

    #[test]
    fn keys_pass_straight_through_with_no_paste() {
        let mut gate = PasteGate::new();
        gate.push_keys(['a', 'b']);
        assert_eq!(drain(&mut gate), vec![Step::Key('a'), Step::Key('b')]);
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn keys_after_a_paste_key_wait_for_its_text() {
        let mut gate = PasteGate::new();
        gate.push_keys(['a', 'P', 'b']);
        assert_eq!(gate.next(), Some(Step::Key('a')));
        assert_eq!(gate.next(), Some(Step::Key('P')));
        // 'P' asked for a paste: 'b' and later keys queue behind it.
        gate.paste_before_queued(vec![1], PasteRead::Waiting(7));
        assert_eq!(gate.next(), None);
        gate.push_keys(['c']);
        assert_eq!(gate.next(), None);
        assert!(gate.complete(7, Some("xy".into())));
        assert_eq!(drain(&mut gate), vec![paste(&[1], "xy"), Step::Key('b'), Step::Key('c')]);
    }

    #[test]
    fn an_inline_read_releases_at_once() {
        let mut gate = PasteGate::new();
        gate.push_keys(['P', 'b']);
        assert_eq!(gate.next(), Some(Step::Key('P')));
        gate.paste_before_queued(vec![1], PasteRead::Done(Some("t".into())));
        assert_eq!(drain(&mut gate), vec![paste(&[1], "t"), Step::Key('b')]);
    }

    #[test]
    fn two_pastes_in_flight_complete_in_request_order() {
        let mut gate = PasteGate::new();
        gate.push_keys(['P', 'a']);
        assert_eq!(gate.next(), Some(Step::Key('P')));
        gate.paste_before_queued(vec![1], PasteRead::Waiting(1));
        // A middle-click while 'a' is held: after 'a'.
        gate.paste_after_queued(vec![2], PasteRead::Waiting(2));
        gate.push_keys(['b']);
        // The second read finishing first releases nothing.
        assert!(gate.complete(2, Some("second".into())));
        assert_eq!(gate.next(), None);
        assert!(gate.complete(1, Some("first".into())));
        assert_eq!(
            drain(&mut gate),
            vec![paste(&[1], "first"), Step::Key('a'), paste(&[2], "second"), Step::Key('b')]
        );
    }

    #[test]
    fn a_second_paste_key_held_behind_the_first_waits_its_turn() {
        let mut gate = PasteGate::new();
        gate.push_keys(['P', 'Q', 'a']);
        assert_eq!(gate.next(), Some(Step::Key('P')));
        gate.paste_before_queued(vec![1], PasteRead::Waiting(1));
        assert!(gate.complete(1, Some("one".into())));
        assert_eq!(gate.next(), Some(paste(&[1], "one")));
        assert_eq!(gate.next(), Some(Step::Key('Q')));
        gate.paste_before_queued(vec![1], PasteRead::Waiting(2));
        assert_eq!(gate.next(), None);
        assert!(gate.complete(2, Some("two".into())));
        assert_eq!(drain(&mut gate), vec![paste(&[1], "two"), Step::Key('a')]);
    }

    #[test]
    fn a_failed_read_just_releases_the_hold() {
        let mut gate = PasteGate::new();
        gate.push_keys(['P', 'a']);
        assert_eq!(gate.next(), Some(Step::Key('P')));
        gate.paste_before_queued(vec![1], PasteRead::Waiting(3));
        assert!(gate.complete(3, None));
        assert_eq!(
            drain(&mut gate),
            vec![Step::Paste { targets: vec![1], text: None }, Step::Key('a')]
        );
    }

    #[test]
    fn unknown_completion_is_ignored() {
        let mut gate: PasteGate<char> = PasteGate::new();
        gate.paste_after_queued(vec![1], PasteRead::Waiting(1));
        assert!(!gate.complete(9, Some("x".into())));
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn next_paste_never_releases_a_key() {
        let mut gate = PasteGate::new();
        gate.paste_after_queued(vec![4], PasteRead::Done(Some("m".into())));
        gate.push_keys(['a']);
        assert_eq!(gate.next_paste(), Some((vec![4], Some("m".into()))));
        assert_eq!(gate.next_paste(), None);
        assert_eq!(gate.next(), Some(Step::Key('a')));
        gate.paste_after_queued(vec![4], PasteRead::Waiting(5));
        assert_eq!(gate.next_paste(), None);
    }
}
