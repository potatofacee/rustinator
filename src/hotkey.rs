use std::sync::{Mutex, OnceLock};
use std::thread;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use winit::event_loop::EventLoopProxy;

use crate::window::UserEvent;

pub struct HotkeyHandle {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

/// The current event-loop proxy that the single reader thread forwards toggle
/// events to. Updated by each `spawn` so the latest registration receives
/// events, while the reader thread itself is started only once per process.
static CURRENT_PROXY: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);

/// Guards one-time startup of the global reader thread.
static READER_THREAD: OnceLock<()> = OnceLock::new();

fn ensure_reader_thread() {
    READER_THREAD.get_or_init(|| {
        // `GlobalHotKeyEvent::receiver()` is a process-global singleton. Reading
        // it from more than one thread would split events round-robin and leak
        // never-terminating threads, so we read it from exactly one thread for
        // the whole process lifetime and forward to the current proxy.
        let receiver = GlobalHotKeyEvent::receiver();
        thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if event.state() == HotKeyState::Pressed {
                    if let Some(proxy) = CURRENT_PROXY.lock().unwrap().as_ref() {
                        let _ = proxy.send_event(UserEvent::HotkeyTogglePressed);
                    }
                }
            }
        });
    });
}

pub fn spawn(combo: &str, proxy: EventLoopProxy<UserEvent>) -> Option<HotkeyHandle> {
    let hotkey = parse(combo)?;

    let manager = GlobalHotKeyManager::new()
        .map_err(|e| log::warn!("hotkey: failed to create manager: {e}"))
        .ok()?;

    register(&manager, hotkey, combo)?;
    Some(finish(manager, hotkey, combo, proxy))
}

/// Re-point an existing registration at `combo` on the same manager, rather
/// than dropping it and spawning a fresh one. On X11 a dropped manager's grab
/// outlives the drop: its thread only sees `DropThread` after its 50 ms poll
/// sleep, and the crate gives no signal when the grab is gone. A new manager
/// grabbing the same key in that window gets BadAccess (`AlreadyRegistered`),
/// which is what a height/focus/on-top change with an unchanged combo hit. On
/// one manager the ungrab and the grab share an X connection, so the server
/// handles them in order, and `register`'s checked grab is a round trip that
/// returns only once both are done.
pub fn rebind(
    old: HotkeyHandle,
    combo: &str,
    proxy: EventLoopProxy<UserEvent>,
) -> Option<HotkeyHandle> {
    // A combo that doesn't parse drops `old`, as the respawn this replaces did.
    let hotkey = parse(combo)?;
    let HotkeyHandle { manager, hotkey: old_hotkey } = old;

    // Same combo: the grab is still live on this manager, nothing to redo.
    if hotkey != old_hotkey {
        if let Err(e) = manager.unregister(old_hotkey) {
            log::warn!("hotkey: failed to unregister old hotkey: {e}");
        }
        register(&manager, hotkey, combo)?;
    }
    Some(finish(manager, hotkey, combo, proxy))
}

fn parse(combo: &str) -> Option<HotKey> {
    parse_combo(combo).or_else(|| {
        log::warn!("hotkey: can't parse combo '{combo}'");
        None
    })
}

fn register(manager: &GlobalHotKeyManager, hotkey: HotKey, combo: &str) -> Option<()> {
    manager
        .register(hotkey)
        .map_err(|e| log::warn!("hotkey: failed to register '{combo}': {e}"))
        .ok()
}

fn finish(
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
    combo: &str,
    proxy: EventLoopProxy<UserEvent>,
) -> HotkeyHandle {
    // Point the single reader thread at this registration's proxy, then ensure
    // that thread exists (started at most once for the process lifetime).
    *CURRENT_PROXY.lock().unwrap() = Some(proxy);
    ensure_reader_thread();

    log::info!("hotkey: registered global hotkey '{combo}'");
    HotkeyHandle { manager, hotkey }
}

fn parse_combo(s: &str) -> Option<HotKey> {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut mods = Modifiers::empty();
    for part in &parts[..parts.len() - 1] {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" | "meta" => mods |= Modifiers::ALT,
            "super" | "cmd" | "command" => mods |= Modifiers::SUPER,
            _ => return None,
        }
    }

    let code = parse_code(parts.last()?)?;
    let mods_opt = if mods.is_empty() { None } else { Some(mods) };
    Some(HotKey::new(mods_opt, code))
}

fn parse_code(s: &str) -> Option<Code> {
    Some(match s.to_ascii_lowercase().as_str() {
        "`" | "backtick" | "grave" | "backquote" => Code::Backquote,
        "a" => Code::KeyA, "b" => Code::KeyB, "c" => Code::KeyC, "d" => Code::KeyD,
        "e" => Code::KeyE, "f" => Code::KeyF, "g" => Code::KeyG, "h" => Code::KeyH,
        "i" => Code::KeyI, "j" => Code::KeyJ, "k" => Code::KeyK, "l" => Code::KeyL,
        "m" => Code::KeyM, "n" => Code::KeyN, "o" => Code::KeyO, "p" => Code::KeyP,
        "q" => Code::KeyQ, "r" => Code::KeyR, "s" => Code::KeyS, "t" => Code::KeyT,
        "u" => Code::KeyU, "v" => Code::KeyV, "w" => Code::KeyW, "x" => Code::KeyX,
        "y" => Code::KeyY, "z" => Code::KeyZ,
        "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2, "3" => Code::Digit3,
        "4" => Code::Digit4, "5" => Code::Digit5, "6" => Code::Digit6, "7" => Code::Digit7,
        "8" => Code::Digit8, "9" => Code::Digit9,
        "space" => Code::Space,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "escape" | "esc" => Code::Escape,
        "backspace" => Code::Backspace,
        "delete" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "up" | "arrowup" => Code::ArrowUp,
        "down" | "arrowdown" => Code::ArrowDown,
        "left" | "arrowleft" => Code::ArrowLeft,
        "right" | "arrowright" => Code::ArrowRight,
        "comma" | "," => Code::Comma,
        "period" | "." => Code::Period,
        "semicolon" | ";" => Code::Semicolon,
        "slash" | "/" => Code::Slash,
        "backslash" | "\\" => Code::Backslash,
        "minus" | "-" => Code::Minus,
        "equals" | "=" => Code::Equal,
        "f1" => Code::F1, "f2" => Code::F2, "f3" => Code::F3, "f4" => Code::F4,
        "f5" => Code::F5, "f6" => Code::F6, "f7" => Code::F7, "f8" => Code::F8,
        "f9" => Code::F9, "f10" => Code::F10, "f11" => Code::F11, "f12" => Code::F12,
        _ => return None,
    })
}
