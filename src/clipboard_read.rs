//! Explicit paste reads (the Paste bindings, Shift+Insert, the context menu,
//! middle-click, and egui-winit's paste chord in an egui text field) off the
//! UI thread.
//!
//! An X11 selection read is a round trip to the selection owner, and arboard
//! waits up to 4 s for each target format it tries (`LONG_TIMEOUT_DUR`,
//! `read_single`). `get_text` tries six targets in turn (UTF8_STRING,
//! text/plain;charset=utf-8, text/plain;charset=UTF-8, STRING, TEXT,
//! text/plain), moving on when one times out, so an owner that never answers
//! costs about 24 s; arboard offers no way to ask for fewer targets or a
//! shorter wait. Under Xvnc the owner is often the VNC server's clipboard
//! bridge, which answers late or never while the viewer is idle, so a read
//! on the UI thread froze painting and PTY output for that long. Reads run
//! on one FIFO worker thread instead, and the result comes back to the
//! winit loop as `UserEvent::ClipboardRead`.
//!
//! The blocking read used to order things for free: keys typed after a
//! paste reached the PTY after the pasted text. `PasteGate` keeps that per
//! target: input for a pane (or an egui field, `TextPastes`) with a paste in
//! flight queues behind it and goes out, in order, once its text has been
//! sent, while every other target's input goes straight through.

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
    Input(K),
    Paste(PasteRead),
}

/// What the gate releases next for a target, in that target's input order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step<K> {
    Input(K),
    /// A finished paste: its text, or None when there is nothing to paste.
    Paste(Option<String>),
}

/// Pastes in flight and the input held behind them, per target (a pane, an
/// egui text field). Input for a target with nothing queued is the caller's
/// to send at once; input for a target with a paste still being read, or
/// with input already held, queues behind it and goes out after the pasted
/// text. Other targets are never held up. Each target's queue is released
/// in order, so its pastes complete in the order they were asked for,
/// whatever order their reads finish in.
///
/// The target is the caller's, fixed when the input arrives: a held key
/// goes to the pane it was typed for, not to whichever pane has focus when
/// it is released.
pub(crate) struct PasteGate<T, K> {
    /// Only targets with something queued; few at a time.
    queues: Vec<(T, VecDeque<Slot<K>>)>,
}

impl<T: Copy + PartialEq, K> PasteGate<T, K> {
    pub(crate) fn new() -> Self {
        Self { queues: Vec::new() }
    }

    fn queue_mut(&mut self, target: T) -> &mut VecDeque<Slot<K>> {
        let at = match self.queues.iter().position(|(t, _)| *t == target) {
            Some(at) => at,
            None => {
                self.queues.push((target, VecDeque::new()));
                self.queues.len() - 1
            }
        };
        &mut self.queues[at].1
    }

    /// Whether input for `target` must wait: a paste of its own is queued
    /// (read or not yet sent), or input already waits for it.
    pub(crate) fn holds(&self, target: T) -> bool {
        self.queues.iter().any(|(t, _)| *t == target)
    }

    /// Queue `input` for `target` behind what is already queued for it. The
    /// caller sends input itself while `holds(target)` is false.
    pub(crate) fn hold(&mut self, target: T, input: K) {
        self.queue_mut(target).push_back(Slot::Input(input));
    }

    /// A paste into each of `targets`, after whatever is queued for it.
    pub(crate) fn paste(&mut self, targets: &[T], read: PasteRead) {
        for &target in targets {
            self.queue_mut(target).push_back(Slot::Paste(read.clone()));
        }
    }

    /// Whether a queued paste waits on read `id`.
    pub(crate) fn waits_on(&self, id: u64) -> bool {
        self.queues.iter().flat_map(|(_, q)| q).any(|slot| matches!(slot, Slot::Paste(PasteRead::Waiting(w)) if *w == id))
    }

    /// The worker finished read `id`: every paste waiting on it (one per
    /// target of a broadcast paste) has its text. False when none waits.
    pub(crate) fn complete(&mut self, id: u64, text: Option<String>) -> bool {
        let mut found = false;
        for slot in self.queues.iter_mut().flat_map(|(_, q)| q) {
            if let Slot::Paste(read) = slot {
                if *read == PasteRead::Waiting(id) {
                    *read = PasteRead::Done(text.clone());
                    found = true;
                }
            }
        }
        found
    }

    /// The next input or finished paste of any target, with its target;
    /// None when every queue is empty or stops at a read still running.
    pub(crate) fn next(&mut self) -> Option<(T, Step<K>)> {
        let at = self.queues.iter().position(|(_, q)| !matches!(q.front(), Some(Slot::Paste(PasteRead::Waiting(_)))))?;
        let (target, queue) = &mut self.queues[at];
        let target = *target;
        let step = match queue.pop_front() {
            Some(Slot::Input(input)) => Some(Step::Input(input)),
            Some(Slot::Paste(PasteRead::Done(text))) => Some(Step::Paste(text)),
            Some(Slot::Paste(PasteRead::Waiting(_))) => unreachable!("checked above"),
            None => None,
        };
        if queue.is_empty() {
            self.queues.remove(at);
        }
        Some((target, step?))
    }

    /// Drop everything queued for the targets `keep` rejects (closed panes):
    /// their held input and pastes go nowhere.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(T) -> bool) {
        self.queues.retain(|(t, _)| keep(*t));
    }

    /// Remove `target`'s queue: its pastes are dropped and its held input is
    /// returned, in order.
    pub(crate) fn take_held(&mut self, target: T) -> Vec<K> {
        let Some(at) = self.queues.iter().position(|(t, _)| *t == target) else {
            return Vec::new();
        };
        let (_, queue) = self.queues.remove(at);
        queue
            .into_iter()
            .filter_map(|slot| match slot {
                Slot::Input(input) => Some(input),
                Slot::Paste(_) => None,
            })
            .collect()
    }

    fn targets(&self) -> Vec<T> {
        self.queues.iter().map(|(t, _)| *t).collect()
    }
}

/// Pastes into a window's egui text fields (the search bar, a dialog, the
/// tab rename, the preferences): egui-winit's paste chord, read off the UI
/// thread like the terminal's pastes, its text injected as the
/// `egui::Event::Paste` egui-winit itself would have pushed.
///
/// egui-winit answers its paste chord (`is_paste_command`: the Paste key,
/// Ctrl+V with or without Shift, Cmd+V on macOS, Shift+Insert on Windows
/// only) by reading the clipboard inline in `on_keyboard_input` and pushing
/// `Event::Paste` (CRLF made LF, nothing when empty or failed) where the
/// chord's key event would have gone; the chord itself becomes no `Key` or
/// `Text` event. Here the chord is withheld from egui-winit
/// (`GlWindow::on_window_event`) and recorded with its place among the
/// events egui-winit has queued for the next frame and the widget focused
/// when it was typed. In that frame (`filter_frame`) the paste takes its
/// place, and the keyboard events behind it are held for that widget until
/// its text is in, so they reach the field after the pasted text, as they
/// did when the read blocked. Pointer events are never held.
///
/// A field that loses focus before its paste arrives (a click elsewhere,
/// its dialog closed) gets nothing: the paste and the presses and text held
/// for it are dropped, as a key for a closed pane is; held key releases
/// still go out, so egui's key-down state completes.
pub(crate) struct TextPastes {
    gate: PasteGate<egui::Id, egui::Event>,
    /// Paste chords since the last frame began: where in egui-winit's
    /// pending events each one came, the widget focused then, its read.
    chords: Vec<(usize, egui::Id, PasteRead)>,
}

impl TextPastes {
    pub(crate) fn new() -> Self {
        Self { gate: PasteGate::new(), chords: Vec::new() }
    }

    /// A paste chord typed into `widget` while egui-winit held `at` events
    /// for the next frame.
    pub(crate) fn chord(&mut self, at: usize, widget: egui::Id, read: PasteRead) {
        self.chords.push((at, widget, read));
    }

    pub(crate) fn waits_on(&self, id: u64) -> bool {
        self.gate.waits_on(id)
    }

    pub(crate) fn complete(&mut self, id: u64, text: Option<String>) -> bool {
        self.gate.complete(id, text)
    }

    /// The events of the frame about to run, given the widget focused as it
    /// begins (`Memory::focused`, the state the frame's keys were typed
    /// under): finished pastes and the events they held go first, then this
    /// frame's events with each chord's paste in its place.
    pub(crate) fn filter_frame(&mut self, focus: Option<egui::Id>, events: Vec<egui::Event>) -> Vec<egui::Event> {
        let mut out = Vec::new();
        for target in self.gate.targets() {
            if Some(target) != focus {
                out.extend(self.gate.take_held(target).into_iter().filter(is_key_release));
            }
        }
        self.release_ready(&mut out);
        let mut chords = std::mem::take(&mut self.chords).into_iter().peekable();
        for (i, event) in events.into_iter().enumerate() {
            while let Some((_, widget, read)) = chords.next_if(|(at, _, _)| *at <= i) {
                self.start(widget, read, focus, &mut out);
            }
            match focus {
                Some(f) if is_keyboard(&event) && self.gate.holds(f) => self.gate.hold(f, event),
                _ => out.push(event),
            }
        }
        for (_, widget, read) in chords {
            self.start(widget, read, focus, &mut out);
        }
        out
    }

    fn start(&mut self, widget: egui::Id, read: PasteRead, focus: Option<egui::Id>, out: &mut Vec<egui::Event>) {
        // Focus moves only in a pass, so the widget still has it; if not,
        // the paste has no field to go to.
        if Some(widget) == focus {
            self.gate.paste(&[widget], read);
            self.release_ready(out);
        }
    }

    fn release_ready(&mut self, out: &mut Vec<egui::Event>) {
        while let Some((_, step)) = self.gate.next() {
            match step {
                Step::Input(event) => out.push(event),
                Step::Paste(Some(text)) => {
                    let text = text.replace("\r\n", "\n");
                    if !text.is_empty() {
                        out.push(egui::Event::Paste(text));
                    }
                }
                Step::Paste(None) => {}
            }
        }
    }
}

/// An event egui gives the focused widget.
fn is_keyboard(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::Paste(_)
            | egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Ime(_)
    )
}

fn is_key_release(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Key { pressed: false, .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Gate = PasteGate<PaneId, char>;

    fn drain(gate: &mut Gate) -> Vec<(PaneId, Step<char>)> {
        std::iter::from_fn(|| gate.next()).collect()
    }

    fn paste(target: PaneId, text: &str) -> (PaneId, Step<char>) {
        (target, Step::Paste(Some(text.into())))
    }

    fn key(target: PaneId, c: char) -> (PaneId, Step<char>) {
        (target, Step::Input(c))
    }

    /// What the window does with a key typed for `targets`: send it to the
    /// panes nothing is held for (returned), hold it for the others.
    fn type_key(gate: &mut Gate, targets: &[PaneId], c: char) -> Vec<PaneId> {
        let mut now = Vec::new();
        for &t in targets {
            if gate.holds(t) {
                gate.hold(t, c);
            } else {
                now.push(t);
            }
        }
        now
    }

    #[test]
    fn keys_pass_straight_through_with_no_paste() {
        let mut gate = Gate::new();
        assert_eq!(type_key(&mut gate, &[1], 'a'), vec![1]);
        assert!(!gate.holds(1));
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn only_the_pasted_panes_keys_are_held() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(7));
        // Pane 1's keys wait for its text; pane 2's go at once.
        assert_eq!(type_key(&mut gate, &[1], 'a'), vec![]);
        assert_eq!(type_key(&mut gate, &[2], 'b'), vec![2]);
        assert_eq!(type_key(&mut gate, &[1], 'c'), vec![]);
        assert_eq!(gate.next(), None);
        assert!(!gate.holds(2));
        assert!(gate.complete(7, Some("xy".into())));
        assert_eq!(drain(&mut gate), vec![paste(1, "xy"), key(1, 'a'), key(1, 'c')]);
        assert!(!gate.holds(1));
    }

    #[test]
    fn a_broadcast_key_is_held_only_for_the_pasted_pane() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(1));
        assert_eq!(type_key(&mut gate, &[1, 2, 3], 'a'), vec![2, 3]);
        assert!(gate.complete(1, Some("p".into())));
        assert_eq!(drain(&mut gate), vec![paste(1, "p"), key(1, 'a')]);
    }

    #[test]
    fn a_held_key_keeps_the_target_it_was_typed_for() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(3));
        type_key(&mut gate, &[1], 'a');
        // Focus moves to pane 2 (a click, a binding) before the text is in:
        // what is typed now goes to pane 2 at once, and 'a' stays pane 1's.
        assert_eq!(type_key(&mut gate, &[2], 'b'), vec![2]);
        assert!(gate.complete(3, Some("t".into())));
        assert_eq!(drain(&mut gate), vec![paste(1, "t"), key(1, 'a')]);
    }

    #[test]
    fn a_closed_panes_paste_and_keys_are_dropped() {
        let mut gate = Gate::new();
        gate.paste(&[1, 2], PasteRead::Waiting(4));
        type_key(&mut gate, &[1, 2], 'a');
        // Pane 1 closes before the read finishes.
        gate.retain(|t| t != 1);
        assert!(!gate.holds(1));
        assert!(gate.waits_on(4));
        assert!(gate.complete(4, Some("z".into())));
        assert_eq!(drain(&mut gate), vec![paste(2, "z"), key(2, 'a')]);
        // A pane closed after its read finished is dropped the same way.
        gate.paste(&[3], PasteRead::Done(Some("q".into())));
        gate.hold(3, 'b');
        gate.retain(|t| t != 3);
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn interleaved_pastes_to_two_panes_each_keep_their_own_order() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(1));
        type_key(&mut gate, &[1], 'a');
        gate.paste(&[2], PasteRead::Waiting(2));
        type_key(&mut gate, &[2], 'b');
        type_key(&mut gate, &[1], 'c');
        gate.paste(&[1], PasteRead::Waiting(3));
        type_key(&mut gate, &[1], 'd');
        // Pane 2's read finishing first releases pane 2 alone.
        assert!(gate.complete(2, Some("two".into())));
        assert_eq!(drain(&mut gate), vec![paste(2, "two"), key(2, 'b')]);
        assert!(!gate.holds(2));
        // Pane 1's second read finishing before its first releases nothing.
        assert!(gate.complete(3, Some("three".into())));
        assert_eq!(gate.next(), None);
        assert!(gate.complete(1, Some("one".into())));
        assert_eq!(
            drain(&mut gate),
            vec![paste(1, "one"), key(1, 'a'), key(1, 'c'), paste(1, "three"), key(1, 'd')]
        );
    }

    #[test]
    fn a_broadcast_paste_completes_in_every_target() {
        let mut gate = Gate::new();
        gate.paste(&[1, 2], PasteRead::Waiting(5));
        type_key(&mut gate, &[2], 'a');
        assert!(gate.complete(5, Some("b".into())));
        assert_eq!(drain(&mut gate), vec![paste(1, "b"), paste(2, "b"), key(2, 'a')]);
    }

    #[test]
    fn an_inline_read_releases_at_once() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Done(Some("t".into())));
        assert_eq!(drain(&mut gate), vec![paste(1, "t")]);
        assert_eq!(type_key(&mut gate, &[1], 'b'), vec![1]);
    }

    #[test]
    fn a_failed_read_just_releases_the_hold() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(3));
        type_key(&mut gate, &[1], 'a');
        assert!(gate.complete(3, None));
        assert_eq!(drain(&mut gate), vec![(1, Step::Paste(None)), key(1, 'a')]);
    }

    #[test]
    fn unknown_completion_is_ignored() {
        let mut gate = Gate::new();
        gate.paste(&[1], PasteRead::Waiting(1));
        assert!(!gate.complete(9, Some("x".into())));
        assert_eq!(gate.next(), None);
    }

    // --- egui text fields ---

    fn id(n: u64) -> egui::Id {
        egui::Id::new(n)
    }

    fn text(s: &str) -> egui::Event {
        egui::Event::Text(s.into())
    }

    fn pasted(s: &str) -> egui::Event {
        egui::Event::Paste(s.into())
    }

    fn release() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn pointer() -> egui::Event {
        egui::Event::PointerMoved(egui::Pos2::ZERO)
    }

    #[test]
    fn text_paste_takes_the_chords_place_and_holds_the_keys_behind_it() {
        let mut tp = TextPastes::new();
        let field = Some(id(1));
        // "a", the chord, "b", a pointer move: one frame's events.
        tp.chord(1, id(1), PasteRead::Waiting(7));
        let out = tp.filter_frame(field, vec![text("a"), text("b"), pointer()]);
        assert_eq!(out, vec![text("a"), pointer()]);
        // Keys of later frames wait too.
        assert_eq!(tp.filter_frame(field, vec![text("c")]), vec![]);
        assert!(tp.complete(7, Some("x\r\ny".into())));
        assert_eq!(tp.filter_frame(field, vec![text("d")]), vec![pasted("x\ny"), text("b"), text("c"), text("d")]);
    }

    #[test]
    fn an_inline_text_paste_lands_in_the_same_frame() {
        let mut tp = TextPastes::new();
        tp.chord(1, id(1), PasteRead::Done(Some("p".into())));
        let out = tp.filter_frame(Some(id(1)), vec![text("a"), text("b")]);
        assert_eq!(out, vec![text("a"), pasted("p"), text("b")]);
        // A chord as the frame's last event.
        tp.chord(1, id(1), PasteRead::Done(Some("q".into())));
        assert_eq!(tp.filter_frame(Some(id(1)), vec![text("c")]), vec![text("c"), pasted("q")]);
    }

    #[test]
    fn an_empty_or_failed_text_paste_inserts_nothing() {
        let mut tp = TextPastes::new();
        tp.chord(0, id(1), PasteRead::Done(Some(String::new())));
        tp.chord(0, id(1), PasteRead::Done(None));
        assert_eq!(tp.filter_frame(Some(id(1)), vec![text("a")]), vec![text("a")]);
    }

    #[test]
    fn a_field_that_lost_focus_gets_no_paste() {
        let mut tp = TextPastes::new();
        tp.chord(0, id(1), PasteRead::Waiting(2));
        assert_eq!(tp.filter_frame(Some(id(1)), vec![text("a"), release()]), vec![]);
        // A click moved focus to another field: its keys pass, and what was
        // held for the first field is dropped but for the key release.
        assert_eq!(tp.filter_frame(Some(id(2)), vec![text("b")]), vec![release(), text("b")]);
        assert!(!tp.complete(2, Some("late".into())));
        assert_eq!(tp.filter_frame(Some(id(2)), vec![]), vec![]);
    }

    #[test]
    fn text_pastes_to_two_fields_are_independent() {
        let mut tp = TextPastes::new();
        tp.chord(0, id(1), PasteRead::Waiting(1));
        assert_eq!(tp.filter_frame(Some(id(1)), vec![text("a")]), vec![]);
        // Focus moves to field 2 and a paste goes there too: field 1's paste
        // is abandoned, field 2's completes on its own.
        tp.chord(0, id(2), PasteRead::Waiting(2));
        assert_eq!(tp.filter_frame(Some(id(2)), vec![text("b")]), vec![]);
        assert!(!tp.complete(1, Some("one".into())));
        assert!(tp.complete(2, Some("two".into())));
        assert_eq!(tp.filter_frame(Some(id(2)), vec![]), vec![pasted("two"), text("b")]);
    }
}
