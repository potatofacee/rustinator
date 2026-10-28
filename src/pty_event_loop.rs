//! The main event loop which performs I/O on the pseudoterminal.
//!
//! Vendored from alacritty_terminal 0.26.0 src/event_loop.rs. The crate
//! offers no way to interpose a `vte::ansi::Handler` between the parser and
//! `Term`, so we carry our own copy that advances the parser into
//! `TermHandlerProxy` instead of `Term` directly. Differences from upstream:
//! `EventLoop::new` takes a `PaneHooks` (clear_wipes_scrollback flag + shared
//! pane defaults for answering color queries), the loop keeps a clone of its
//! own `EventLoopSender` so the proxy can write query replies back to the PTY,
//! and both parser dispatch sites (`advance`, `stop_sync`) go through the
//! proxy; the PTY poll token constants are replicated locally because upstream
//! keeps them pub(crate). Teardown also differs: `spawn` returns
//! `JoinHandle<()>` and the thread drops the loop (and so the `Pty`) itself,
//! and `Msg::Shutdown` hangs up the child and keeps the loop running until the
//! child exits, escalating to SIGKILL after `SHUTDOWN_GRACE`, instead of
//! breaking at once (see `begin_shutdown`). Re-sync this file when bumping
//! alacritty_terminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{self, Event, EventListener, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi;
use alacritty_terminal::{thread, tty};
use log::error;
use polling::{Event as PollingEvent, Events, PollMode, Poller};

use crate::pane::PaneDefaults;
use crate::term_handler::TermHandlerProxy;

/// Per-pane state the main thread hands this loop (mostly shared with its
/// parse-time handler proxy). Bundled so `EventLoop::new` stays close to
/// upstream's signature as more hooks are added.
pub struct PaneHooks {
    /// When true, ED 2 on the primary screen also wipes scrollback.
    pub clear_wipes_scrollback: Arc<AtomicBool>,
    /// Pane default colors, used to answer OSC 10/11/12 and OSC 4 queries when
    /// the program has not overridden the color. Written by the main thread.
    pub defaults: Arc<Mutex<PaneDefaults>>,
    /// Pid of the PTY's direct child, signalled on `Msg::Shutdown`. The loop is
    /// generic over `EventedPty`, which does not expose the child.
    pub child_pid: u32,
}

// Replicated from alacritty_terminal's tty module, where they are pub(crate).
// Values must match the keys the Pty implementations register with.
#[cfg(not(windows))]
const PTY_READ_WRITE_TOKEN: usize = 0;
#[cfg(not(windows))]
const PTY_CHILD_EVENT_TOKEN: usize = 1;
#[cfg(windows)]
const PTY_CHILD_EVENT_TOKEN: usize = 1;
#[cfg(windows)]
const PTY_READ_WRITE_TOKEN: usize = 2;

/// Max bytes to read from the PTY before forced terminal synchronization.
pub(crate) const READ_BUFFER_SIZE: usize = 0x10_0000;

/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

/// How long a shutting-down loop waits after SIGHUP before sending SIGKILL.
/// Bounds teardown for a direct child that ignores SIGHUP or cannot act on it
/// (traps it, is stopped, or is stuck on a tty write nobody else drains).
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Messages that may be sent to the `EventLoop`.
#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    /// Indicates that the `EventLoop` should shut down, as Alacritty is shutting down.
    Shutdown,

    /// Instruction to resize the PTY.
    Resize(WindowSize),
}

/// The main event loop.
///
/// Handles all the PTY I/O and runs the PTY parser which updates terminal
/// state.
pub struct EventLoop<T: tty::EventedPty, U: EventListener> {
    poll: Arc<Poller>,
    pty: T,
    rx: PeekableReceiver<Msg>,
    tx: Sender<Msg>,
    terminal: Arc<FairMutex<Term<U>>>,
    event_proxy: U,
    drain_on_exit: bool,
    ref_test: bool,
    hooks: PaneHooks,
    /// Clone of this loop's own sender, handed to the parse-time proxy so
    /// query replies can be queued for the PTY (the send wakes the poller;
    /// the next loop iteration drains and writes).
    sender: EventLoopSender,
}

impl<T, U> EventLoop<T, U>
where
    T: tty::EventedPty + event::OnResize + Send + 'static,
    U: EventListener + Send + 'static,
{
    /// Create a new event loop.
    pub fn new(
        terminal: Arc<FairMutex<Term<U>>>,
        event_proxy: U,
        pty: T,
        drain_on_exit: bool,
        ref_test: bool,
        hooks: PaneHooks,
    ) -> io::Result<EventLoop<T, U>> {
        let (tx, rx) = mpsc::channel();
        let poll: Arc<Poller> = Poller::new()?.into();
        let sender = EventLoopSender { sender: tx.clone(), poller: Arc::clone(&poll) };
        Ok(EventLoop {
            poll,
            pty,
            tx,
            rx: PeekableReceiver::new(rx),
            terminal,
            event_proxy,
            drain_on_exit,
            ref_test,
            hooks,
            sender,
        })
    }

    pub fn channel(&self) -> EventLoopSender {
        EventLoopSender { sender: self.tx.clone(), poller: self.poll.clone() }
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    // Deviation: keeps draining past Shutdown (a message left queued may have
    // had its poller wake consumed already), and once shutting down ignores
    // everything: the pane is gone, so input and resizes have no audience, and
    // a repeated Shutdown changes nothing.
    fn drain_recv_channel(&mut self, state: &mut State) -> bool {
        let mut shutdown = false;
        while let Some(msg) = self.rx.recv() {
            match msg {
                _ if shutdown || state.shutdown.is_some() => (),
                Msg::Input(input) => state.write_list.push_back(input),
                Msg::Resize(window_size) => self.pty.on_resize(window_size),
                Msg::Shutdown => shutdown = true,
            }
        }

        !shutdown
    }

    /// Start a graceful shutdown: hang up the child and arm the SIGKILL
    /// deadline. The loop keeps running (and reading, so a child blocked
    /// writing to the tty can get to its SIGHUP handling) until the normal
    /// child-exit event ends it. The child is only reaped by that event
    /// (`next_child_event`'s try_wait), so its pid cannot be recycled while we
    /// may still signal it.
    #[cfg(unix)]
    fn begin_shutdown(&mut self, state: &mut State) {
        signal_child(self.hooks.child_pid, libc::SIGHUP);
        // Upstream discarded unwritten input by breaking at once; do the same,
        // so no write (and no write error) can end the loop before the child.
        state.write_list.clear();
        state.writing = None;
        state.shutdown = Some(Shutdown { kill_at: Some(Instant::now() + SHUTDOWN_GRACE) });
    }

    #[inline]
    fn pty_read<X>(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        mut writer: Option<&mut X>,
    ) -> io::Result<()>
    where
        X: Write,
    {
        let mut unprocessed = 0;
        let mut processed = 0;

        // Reserve the next terminal lock for PTY reading.
        let _terminal_lease = Some(self.terminal.lease());
        let mut terminal = None;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => break,
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            break;
                        }
                    },
                    _ => return Err(err),
                },
            }

            // Attempt to lock the terminal.
            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.terminal.try_lock_unfair() {
                    // Force block if we are at the buffer size limit.
                    None if unprocessed >= READ_BUFFER_SIZE => self.terminal.lock_unfair(),
                    None => continue,
                    Some(terminal) => terminal,
                }),
            };

            // Write a copy of the bytes to the ref test file.
            if let Some(writer) = &mut writer {
                writer.write_all(&buf[..unprocessed]).unwrap();
            }

            // Parse the incoming bytes.
            let mut handler = TermHandlerProxy {
                term: &mut **terminal,
                clear_wipes_scrollback: self.hooks.clear_wipes_scrollback.load(Ordering::Relaxed),
                writer: &self.sender,
                defaults: *self.hooks.defaults.lock().unwrap(),
            };
            state.parser.advance(&mut handler, &buf[..unprocessed]);

            processed += unprocessed;
            unprocessed = 0;

            // Assure we're not blocking the terminal too long unnecessarily.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        // Queue terminal redraw unless all processed bytes were synchronized.
        if state.parser.sync_bytes_count() < processed && processed > 0 {
            self.event_proxy.send_event(Event::Wakeup);
        }

        Ok(())
    }

    #[inline]
    fn pty_write(&mut self, state: &mut State) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));
                        break 'write_many;
                    },
                    Ok(n) => {
                        current.advance(n);
                        if current.finished() {
                            state.goto_next();
                            break 'write_one;
                        }
                    },
                    Err(err) => {
                        state.set_current(Some(current));
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    },
                }
            }
        }

        Ok(())
    }

    // Deviation: upstream hands the loop back through `JoinHandle<(Self, State)>`,
    // so its `Pty` (whose Drop SIGHUPs the child, blocks in wait() and only then
    // closes the master) drops wherever the handle is joined. Here the thread
    // always drops it itself, which is what alacritty gets by detaching.
    pub fn spawn(mut self) -> JoinHandle<()> {
        thread::spawn_named("PTY reader", move || {
            let mut state = State::default();
            let mut buf = [0u8; READ_BUFFER_SIZE];

            let poll_opts = PollMode::Level;
            let mut interest = PollingEvent::readable(0);

            // Register TTY through EventedRW interface.
            if let Err(err) = unsafe { self.pty.register(&self.poll, interest, poll_opts) } {
                error!("Event loop registration error: {err}");
                return;
            }

            let mut events = Events::with_capacity(NonZeroUsize::new(1024).unwrap());

            let mut pipe = if self.ref_test {
                Some(File::create("./alacritty.recording").expect("create alacritty recording"))
            } else {
                None
            };

            // Deviation: set when the child-exit event reaped the child.
            let mut child_reaped = false;

            'event_loop: loop {
                // Wakeup the event loop when a synchronized update timeout was reached.
                let handler = state.parser.sync_timeout();
                let sync_deadline = handler.sync_timeout();
                // Deviation: while shutting down, also wake for the SIGKILL deadline.
                let kill_deadline = state.shutdown.as_ref().and_then(|s| s.kill_at);
                let deadline = match (sync_deadline, kill_deadline) {
                    (Some(sync), Some(kill)) => Some(sync.min(kill)),
                    (sync, kill) => sync.or(kill),
                };
                let timeout = deadline.map(|st| st.saturating_duration_since(Instant::now()));

                events.clear();
                if let Err(err) = self.poll.wait(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            error!("Event loop polling error: {err}");
                            break 'event_loop;
                        },
                    }
                }

                // Deviation: escalate a shutdown whose grace period has run out.
                #[cfg(unix)]
                if let Some(shutdown) = &mut state.shutdown {
                    if shutdown.kill_at.is_some_and(|at| at <= Instant::now()) {
                        shutdown.kill_at = None;
                        signal_child(self.hooks.child_pid, libc::SIGKILL);
                    }
                }

                // Handle synchronized update timeout.
                // Deviation: upstream's only timeout is the sync one, so it reads any
                // empty wake as that. While shutting down the wake may be the kill
                // deadline instead, so then the sync timer must actually be due.
                if events.is_empty()
                    && self.rx.peek().is_none()
                    && (state.shutdown.is_none()
                        || sync_deadline.is_some_and(|at| at <= Instant::now()))
                {
                    let mut terminal = self.terminal.lock();
                    let mut handler = TermHandlerProxy {
                        term: &mut *terminal,
                        clear_wipes_scrollback: self
                            .hooks
                            .clear_wipes_scrollback
                            .load(Ordering::Relaxed),
                        writer: &self.sender,
                        defaults: *self.hooks.defaults.lock().unwrap(),
                    };
                    state.parser.stop_sync(&mut handler);
                    drop(terminal);
                    self.event_proxy.send_event(Event::Wakeup);
                    continue;
                }

                // Handle channel events, if there are any.
                // Deviation: hang up and wait for the child instead of breaking.
                if !self.drain_recv_channel(&mut state) && state.shutdown.is_none() {
                    #[cfg(unix)]
                    self.begin_shutdown(&mut state);
                    #[cfg(not(unix))]
                    break;
                }

                for event in events.iter() {
                    match event.key {
                        PTY_CHILD_EVENT_TOKEN => {
                            if let Some(tty::ChildEvent::Exited(status)) =
                                self.pty.next_child_event()
                            {
                                if let Some(status) = status {
                                    self.event_proxy.send_event(Event::ChildExit(status));
                                }
                                if self.drain_on_exit {
                                    let _ = self.pty_read(&mut state, &mut buf, pipe.as_mut());
                                }
                                self.terminal.lock().exit();
                                self.event_proxy.send_event(Event::Wakeup);
                                child_reaped = true;
                                break 'event_loop;
                            }
                        },

                        PTY_READ_WRITE_TOKEN => {
                            if event.is_interrupt() {
                                // Don't try to do I/O on a dead PTY.
                                continue;
                            }

                            if event.readable {
                                if let Err(err) = self.pty_read(&mut state, &mut buf, pipe.as_mut())
                                {
                                    // On Linux, a `read` on the master side of a PTY can fail
                                    // with `EIO` if the client side hangs up.  In that case,
                                    // just loop back round for the inevitable `Exited` event.
                                    // This sucks, but checking the process is either racy or
                                    // blocking. (During shutdown this is the usual way a
                                    // hung-up child's exit shows up first, and the
                                    // SIGKILL deadline still bounds the wait.)
                                    #[cfg(target_os = "linux")]
                                    if err.raw_os_error() == Some(libc::EIO) {
                                        continue;
                                    }

                                    error!("Error reading from PTY in event loop: {err}");
                                    break 'event_loop;
                                }
                            }

                            if event.writable {
                                if let Err(err) = self.pty_write(&mut state) {
                                    error!("Error writing to PTY in event loop: {err}");
                                    break 'event_loop;
                                }
                            }
                        },
                        _ => (),
                    }
                }

                // Register write interest if necessary.
                let needs_write = state.needs_write();
                if needs_write != interest.writable {
                    interest.writable = needs_write;

                    // Re-register with new interest.
                    self.pty.reregister(&self.poll, interest, poll_opts).unwrap();
                }
            }

            // The evented instances are not dropped here so deregister them explicitly.
            let _ = self.pty.deregister(&self.poll);

            // Deviation: an I/O or poll error can end the loop mid-shutdown with the
            // child still alive (it never got the exit event). Kill it so the
            // `Pty::drop` wait below is not left to a child that ignores SIGHUP.
            #[cfg(unix)]
            if state.shutdown.is_some() && !child_reaped {
                signal_child(self.hooks.child_pid, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = child_reaped;

            // Drop the `Pty` (SIGHUP, reap, close the master) and the loop's
            // terminal reference here, on this thread.
            drop(state);
            drop(self);
        })
    }
}

/// Send `signal` to the PTY's direct child. Only called while the child is
/// unreaped (see `begin_shutdown`), so the pid still names it.
#[cfg(unix)]
fn signal_child(pid: u32, signal: libc::c_int) {
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

/// Graceful-shutdown progress, present once `Msg::Shutdown` was received.
struct Shutdown {
    /// When to send SIGKILL; `None` once it has been sent.
    kill_at: Option<Instant>,
}

/// Helper type which tracks how much of a buffer has been written.
struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

// Unused here (panes send through EventLoopSender directly); kept so the file
// stays a faithful copy of upstream for re-sync diffs.
#[allow(dead_code)]
pub struct Notifier(pub EventLoopSender);

impl event::Notify for Notifier {
    fn notify<B>(&self, bytes: B)
    where
        B: Into<Cow<'static, [u8]>>,
    {
        let bytes = bytes.into();
        // Terminal hangs if we send 0 bytes through.
        if bytes.is_empty() {
            return;
        }

        let _ = self.0.send(Msg::Input(bytes));
    }
}

impl event::OnResize for Notifier {
    fn on_resize(&mut self, window_size: WindowSize) {
        let _ = self.0.send(Msg::Resize(window_size));
    }
}

#[derive(Debug)]
pub enum EventLoopSendError {
    /// Error polling the event loop.
    Io(io::Error),

    /// Error sending a message to the event loop.
    Send(mpsc::SendError<Msg>),
}

impl Display for EventLoopSendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            EventLoopSendError::Io(err) => err.fmt(f),
            EventLoopSendError::Send(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for EventLoopSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EventLoopSendError::Io(err) => err.source(),
            EventLoopSendError::Send(err) => err.source(),
        }
    }
}

#[derive(Clone)]
pub struct EventLoopSender {
    sender: Sender<Msg>,
    poller: Arc<Poller>,
}

impl EventLoopSender {
    pub fn send(&self, msg: Msg) -> Result<(), EventLoopSendError> {
        self.sender.send(msg).map_err(EventLoopSendError::Send)?;
        self.poller.notify().map_err(EventLoopSendError::Io)
    }
}

/// All of the mutable state needed to run the event loop.
///
/// Contains list of items to write, current write state, etc. Anything that
/// would otherwise be mutated on the `EventLoop` goes here.
#[derive(Default)]
pub struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    parser: ansi::Processor,
    /// Deviation: graceful-shutdown progress (see `begin_shutdown`).
    shutdown: Option<Shutdown>,
}

impl State {
    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing { source: c, written: 0 }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

struct PeekableReceiver<T> {
    rx: Receiver<T>,
    peeked: Option<T>,
}

impl<T> PeekableReceiver<T> {
    fn new(rx: Receiver<T>) -> Self {
        Self { rx, peeked: None }
    }

    fn peek(&mut self) -> Option<&T> {
        if self.peeked.is_none() {
            self.peeked = self.rx.try_recv().ok();
        }

        self.peeked.as_ref()
    }

    fn recv(&mut self) -> Option<T> {
        if self.peeked.is_some() {
            self.peeked.take()
        } else {
            match self.rx.try_recv() {
                Err(TryRecvError::Disconnected) => panic!("event loop channel closed"),
                res => res.ok(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Block A P0 safety tests: lock the PTY write loop's state machine before
    // the Phase 4a resize-debounce refactor reroutes message cadence through it.

    /// The write queue should load an item into `writing`, hand it out via
    /// `take_current`/`goto_next`, and signal `needs_write` exactly while there
    /// is pending work (either an in-flight `writing` or a non-empty queue).
    #[test]
    fn state_write_queue_load_consume_and_needs_write_cycle() {
        let mut state = State::default();

        // Empty state: nothing to write.
        assert!(!state.needs_write());
        assert!(state.writing.is_none());

        // Enqueue two items. The queue alone is enough to need a write.
        state.write_list.push_back(Cow::Borrowed(&b"abc"[..]));
        state.write_list.push_back(Cow::Borrowed(&b"de"[..]));
        assert!(state.needs_write());
        assert!(state.writing.is_none());

        // ensure_next loads the first item out of the queue into `writing`.
        state.ensure_next();
        assert_eq!(state.write_list.len(), 1);
        assert_eq!(state.writing.as_ref().unwrap().remaining_bytes(), b"abc");

        // ensure_next is idempotent while an item is already in flight.
        state.ensure_next();
        assert_eq!(state.write_list.len(), 1);
        assert_eq!(state.writing.as_ref().unwrap().remaining_bytes(), b"abc");

        // take_current removes the in-flight item; the queued item keeps the
        // needs-write signal asserted.
        let current = state.take_current().expect("current item");
        assert_eq!(current.remaining_bytes(), b"abc");
        assert!(state.writing.is_none());
        assert!(state.needs_write());

        // goto_next pulls the second item out of the queue.
        state.goto_next();
        assert_eq!(state.write_list.len(), 0);
        assert_eq!(state.writing.as_ref().unwrap().remaining_bytes(), b"de");
        assert!(state.needs_write());

        // set_current can restore an in-progress item (mirrors a short write
        // pushing the partially written item back).
        state.set_current(Some(Writing::new(Cow::Borrowed(&b"xy"[..]))));
        assert_eq!(state.writing.as_ref().unwrap().remaining_bytes(), b"xy");
        assert!(state.needs_write());

        // Drain the last item: both the queue and `writing` are now empty.
        let _ = state.take_current();
        assert!(state.writing.is_none());
        assert!(!state.needs_write());

        // ensure_next over an empty queue leaves nothing in flight.
        state.ensure_next();
        assert!(state.writing.is_none());
        assert!(!state.needs_write());
    }

    /// `Writing` tracks how far through a source buffer we have written.
    /// Partial advances narrow `remaining_bytes`; a fully written (or empty)
    /// source reports `finished` with no remaining bytes.
    #[test]
    fn writing_partial_write_accounting_and_empty_source() {
        let mut w = Writing::new(Cow::Borrowed(&b"hello"[..]));
        assert!(!w.finished());
        assert_eq!(w.remaining_bytes(), b"hello");

        // A partial write of 2 bytes shifts the remaining window.
        w.advance(2);
        assert_eq!(w.remaining_bytes(), b"llo");
        assert!(!w.finished());

        // Writing the rest finishes the item with no remaining bytes.
        w.advance(3);
        assert_eq!(w.remaining_bytes(), b"");
        assert!(w.finished());

        // An empty source is finished immediately and yields no bytes.
        let empty = Writing::new(Cow::Borrowed(&b""[..]));
        assert!(empty.finished());
        assert_eq!(empty.remaining_bytes(), b"");
    }

    /// `peek` must not consume: repeated peeks return the same head item, and a
    /// following sequence of `recv` calls drains every item in FIFO order
    /// (starting with the peeked one). An empty-but-connected channel returns
    /// `None` from both without panicking.
    #[test]
    fn peekable_receiver_peek_nonconsuming_then_recv_drains_in_order() {
        let (tx, rx) = mpsc::channel::<u32>();
        let mut peekable = PeekableReceiver::new(rx);

        tx.send(1).unwrap();
        tx.send(2).unwrap();
        tx.send(3).unwrap();

        // Peeking twice returns the same head item without consuming it.
        assert_eq!(peekable.peek(), Some(&1));
        assert_eq!(peekable.peek(), Some(&1));

        // recv drains in order, beginning with the previously peeked item.
        assert_eq!(peekable.recv(), Some(1));
        assert_eq!(peekable.recv(), Some(2));
        assert_eq!(peekable.recv(), Some(3));

        // Channel is empty but still connected: no item, no panic.
        assert_eq!(peekable.peek(), None);
        assert_eq!(peekable.recv(), None);

        // Keep the sender alive so the above exercises the connected-empty path
        // (a dropped sender would make recv panic on Disconnected).
        drop(tx);
    }

    // Teardown regression tests on a real PTY (openpty, so no controlling tty
    // is needed). Pane teardown may only signal the loop and hand off the Term;
    // the loop thread hangs up, escalates, reaps and closes on its own.
    #[cfg(unix)]
    mod teardown {
        use super::*;
        use std::time::Duration;

        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::Config;
        use alacritty_terminal::term::test::TermSize;

        use crate::pane::{DropOffThread, PaneDefaults};

        type TestTerm = Arc<FairMutex<Term<VoidListener>>>;

        fn screen_text(term: &Term<VoidListener>) -> String {
            let grid = term.grid();
            let mut text = String::new();
            for line in 0..grid.screen_lines() {
                for col in 0..grid.columns() {
                    text.push(grid[Line(line as i32)][Column(col)].c);
                }
            }
            text
        }

        /// Run `sh -c script` under a spawned `EventLoop`, returning once the
        /// script printed `ready` (so its setup, e.g. a trap, is in place).
        fn spawn_ready(
            script: &str,
        ) -> (EventLoopSender, u32, DropOffThread<TestTerm>, JoinHandle<()>) {
            let window_size =
                WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
            let opts = tty::Options {
                shell: Some(tty::Shell::new("/bin/sh".into(), vec!["-c".into(), script.into()])),
                ..Default::default()
            };
            let pty = tty::new(&opts, window_size, 0).expect("open pty");
            let child_pid = pty.child().id();
            let term: TestTerm = Arc::new(FairMutex::new(Term::new(
                Config::default(),
                &TermSize::new(80, 24),
                VoidListener,
            )));
            let hooks = PaneHooks {
                clear_wipes_scrollback: Arc::new(AtomicBool::new(false)),
                defaults: Arc::new(Mutex::new(PaneDefaults::default())),
                child_pid,
            };
            let event_loop =
                EventLoop::new(Arc::clone(&term), VoidListener, pty, false, false, hooks)
                    .expect("create event loop");
            let tx = event_loop.channel();
            let thread = event_loop.spawn();

            let deadline = Instant::now() + Duration::from_secs(5);
            while !screen_text(&term.lock()).contains("ready") {
                assert!(Instant::now() < deadline, "child never printed its ready marker");
                std::thread::sleep(Duration::from_millis(10));
            }
            (tx, child_pid, DropOffThread::new(term), thread)
        }

        /// Tear down the way `Pane::drop` does (send Shutdown, drop the Term
        /// handle) and check that returns at once; then wait for the child pid
        /// to vanish (the loop thread reaps it, so not even a zombie is left)
        /// and for the loop thread to end. Returns how long the child took.
        fn teardown(
            tx: EventLoopSender,
            pid: u32,
            term: DropOffThread<TestTerm>,
            thread: JoinHandle<()>,
        ) -> Duration {
            let start = Instant::now();
            let _ = tx.send(Msg::Shutdown);
            drop(term);
            let handoff = start.elapsed();
            assert!(handoff < Duration::from_millis(200), "teardown blocked for {handoff:?}");

            let deadline = start + SHUTDOWN_GRACE + Duration::from_secs(3);
            let pid_gone = || {
                let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
                rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            };
            while !pid_gone() {
                assert!(Instant::now() < deadline, "child {pid} still exists");
                std::thread::sleep(Duration::from_millis(10));
            }
            let gone = start.elapsed();
            while !thread.is_finished() {
                assert!(Instant::now() < deadline, "PTY loop thread never finished");
                std::thread::sleep(Duration::from_millis(10));
            }
            thread.join().expect("PTY loop thread panicked");
            gone
        }

        /// A child that ignores SIGHUP used to freeze the UI forever in
        /// `Pty::drop`'s wait. Now the caller returns at once and the loop
        /// SIGKILLs the child when the grace period runs out.
        #[test]
        #[ignore = "slow: waits out the 3 s SHUTDOWN_GRACE; run with \
                    `cargo test -- --ignored shutdown_kills_child_that_ignores_sighup`"]
        fn shutdown_kills_child_that_ignores_sighup() {
            let (tx, pid, term, thread) =
                spawn_ready("trap '' HUP; echo ready; while :; do sleep 1; done");
            let gone = teardown(tx, pid, term, thread);
            // It outlived the SIGHUP, so it was the SIGKILL that ended it.
            assert!(
                gone >= SHUTDOWN_GRACE - Duration::from_millis(50),
                "child exited after {gone:?}, before the grace period ran out"
            );
        }

        /// A child that exits on SIGHUP (the shell case) goes right away, without
        /// waiting out the grace period.
        #[test]
        fn shutdown_hangs_up_child_promptly() {
            let (tx, pid, term, thread) = spawn_ready("echo ready; while :; do sleep 1; done");
            let gone = teardown(tx, pid, term, thread);
            assert!(gone < SHUTDOWN_GRACE / 2, "child took {gone:?} to exit on SIGHUP");
        }
    }
}
