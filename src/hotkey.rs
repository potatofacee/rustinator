use std::thread;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use winit::event_loop::EventLoopProxy;

use crate::window::UserEvent;

pub struct HotkeyHandle {
    _manager: GlobalHotKeyManager,
}

pub fn spawn(combo: &str, proxy: EventLoopProxy<UserEvent>) -> Option<HotkeyHandle> {
    let hotkey = parse_combo(combo).or_else(|| {
        log::warn!("hotkey: can't parse combo '{combo}'");
        None
    })?;

    let manager = GlobalHotKeyManager::new()
        .map_err(|e| log::warn!("hotkey: failed to create manager: {e}"))
        .ok()?;

    manager
        .register(hotkey)
        .map_err(|e| log::warn!("hotkey: failed to register '{combo}': {e}"))
        .ok()?;

    let receiver = GlobalHotKeyEvent::receiver();
    thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if event.state() == HotKeyState::Pressed {
                let _ = proxy.send_event(UserEvent::HotkeyTogglePressed);
            }
        }
    });

    log::info!("hotkey: registered global hotkey '{combo}'");
    Some(HotkeyHandle { _manager: manager })
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
