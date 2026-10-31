use std::io;
use std::process::{Child, Command};

use winit::window::Window;

/// Launch a program we never wait on (URL opener, a new instance) without
/// leaving a zombie when it exits. alacritty's `spawn_daemon` double fork: the
/// direct child forks the real program and exits at once, so it is reaped
/// right here by a wait on its own pid. A wait on any child (`waitpid(-1)`)
/// would steal exit statuses each PTY reaps via its own `try_wait` on SIGCHLD.
/// The program is orphaned to init, which reaps it. A thread parked in
/// `wait()` would also work, but would live as long as the launched window.
/// stdio and session are inherited, as with a plain `spawn`.
pub(crate) fn spawn_detached(cmd: &mut Command) {
    if let Err(e) = detach(cmd) {
        log::warn!("failed to launch {:?}: {e}", cmd.get_program());
    }
}

/// `spawn_detached`'s body: returns the already-reaped direct child, so a test
/// can check its pid is gone and read the program's piped output.
#[cfg(unix)]
fn detach(cmd: &mut Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;

    // SAFETY: the closure runs between fork and exec and calls only
    // async-signal-safe functions: fork (POSIX.1-2017, and no atfork handlers
    // are registered) and _exit.
    unsafe {
        cmd.pre_exec(|| match libc::fork() {
            -1 => Err(io::Error::last_os_error()),
            // The grandchild goes on to exec. If that fails it still reports
            // through std's CLOEXEC error pipe, so `spawn` returns the error.
            0 => Ok(()),
            _ => libc::_exit(0),
        });
    }
    let mut child = cmd.spawn()?;
    // Bounded: the direct child exits right after its fork, and `spawn` has
    // already blocked until the grandchild's exec.
    child.wait()?;
    Ok(child)
}

/// No zombies off Unix: dropping a `Child` just closes its handle.
#[cfg(not(unix))]
fn detach(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn()
}

#[cfg(target_os = "macos")]
fn macos_screen_insets(window: &Window) -> Option<(f64, f64, f64, f64)> {
    use objc2::MainThreadMarker;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSScreen;
    use raw_window_handle::HasWindowHandle as _;

    let (full, visible) = unsafe {
        let mut result = None;
        if let Ok(handle) = window.window_handle() {
            if let raw_window_handle::RawWindowHandle::AppKit(app_handle) = handle.as_raw() {
                let ns_view = &*(app_handle.ns_view.as_ptr() as *const AnyObject);
                let ns_window: *const AnyObject = objc2::msg_send![ns_view, window];
                if !ns_window.is_null() {
                    let ns_screen: *const AnyObject = objc2::msg_send![&*ns_window, screen];
                    if !ns_screen.is_null() {
                        let screen = &*(ns_screen as *const NSScreen);
                        result = Some((screen.frame(), screen.visibleFrame()));
                    }
                }
            }
        }
        result
    }
    .or_else(|| {
        let mtm = MainThreadMarker::new()?;
        let screen = NSScreen::mainScreen(mtm)?;
        Some((screen.frame(), screen.visibleFrame()))
    })?;

    let top = (full.origin.y + full.size.height) - (visible.origin.y + visible.size.height);
    let left = visible.origin.x - full.origin.x;
    let bottom = visible.origin.y - full.origin.y;
    let right = (full.origin.x + full.size.width) - (visible.origin.x + visible.size.width);

    Some((top, left, bottom, right))
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_frontmost_pid() -> Option<i32> {
    use objc2::runtime::{AnyClass, AnyObject};

    unsafe {
        let cls = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = objc2::msg_send![cls, sharedWorkspace];
        if workspace.is_null() { return None; }
        let app: *mut AnyObject = objc2::msg_send![&*workspace, frontmostApplication];
        if app.is_null() { return None; }
        let pid: i32 = objc2::msg_send![&*app, processIdentifier];
        Some(pid)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn preferred_locale_id() -> Option<String> {
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    unsafe {
        let cls = AnyClass::get(c"NSLocale")?;
        let locale: *mut AnyObject = objc2::msg_send![cls, currentLocale];
        if locale.is_null() { return None; }
        let ident: *mut NSString = objc2::msg_send![&*locale, localeIdentifier];
        if ident.is_null() { return None; }
        let id = (*ident).to_string();
        if id.is_empty() { return None; }
        // Strip locale modifiers (e.g. "en_US@calendar=gregorian") — keep the bare id.
        Some(id.split('@').next().unwrap_or(&id).to_string())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_activate_pid(pid: i32) {
    use objc2::runtime::{AnyClass, AnyObject};

    unsafe {
        let Some(cls) = AnyClass::get(c"NSRunningApplication") else { return };
        let app: *mut AnyObject =
            objc2::msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if !app.is_null() {
            let _: bool = objc2::msg_send![&*app, activateWithOptions: 2usize];
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_order_window_back(window: &Window) {
    use objc2::runtime::AnyObject;
    use raw_window_handle::HasWindowHandle as _;

    let Ok(handle) = window.window_handle() else { return };
    let raw_window_handle::RawWindowHandle::AppKit(app_handle) = handle.as_raw() else { return };

    unsafe {
        let ns_view = &*(app_handle.ns_view.as_ptr() as *const AnyObject);
        let ns_window: *const AnyObject = objc2::msg_send![ns_view, window];
        if !ns_window.is_null() {
            let _: () = objc2::msg_send![&*ns_window, orderBack: std::ptr::null_mut::<AnyObject>()];
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn x11_workarea() -> Option<(i32, i32, u32, u32)> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;

    let desktop_atom = conn
        .intern_atom(false, b"_NET_CURRENT_DESKTOP")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let desktop_reply = conn
        .get_property(false, root, desktop_atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let desktop_idx = desktop_reply.value32()?.next()? as usize;

    let workarea_atom = conn
        .intern_atom(false, b"_NET_WORKAREA")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let workarea_reply = conn
        .get_property(false, root, workarea_atom, AtomEnum::CARDINAL, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    let values: Vec<u32> = workarea_reply.value32()?.collect();

    let base = desktop_idx * 4;
    if base + 3 >= values.len() {
        return None;
    }

    Some((
        values[base] as i32,
        values[base + 1] as i32,
        values[base + 2],
        values[base + 3],
    ))
}

pub(crate) fn apply_hotkey_geometry(window: &Window, height_pct: u32) {
    let monitor = window
        .current_monitor()
        .or_else(|| window.primary_monitor());
    if let Some(mon) = monitor {
        let size = mon.size();
        let pos = mon.position();
        let scale = window.scale_factor();

        #[cfg(target_os = "macos")]
        let (top_inset, left_inset, bottom_inset, right_inset) = macos_screen_insets(window)
            .map(|(t, l, b, r)| (
                (t * scale) as i32,
                (l * scale) as i32,
                (b * scale) as i32,
                (r * scale) as i32,
            ))
            .unwrap_or((0, 0, 0, 0));

        #[cfg(not(target_os = "macos"))]
        let (top_inset, left_inset, bottom_inset, right_inset) = {
            #[cfg(not(target_os = "windows"))]
            if let Some((wa_x, wa_y, wa_w, wa_h)) = x11_workarea() {
                let mon_right = pos.x + size.width as i32;
                let mon_bottom = pos.y + size.height as i32;
                (
                    wa_y.max(pos.y) - pos.y,
                    wa_x.max(pos.x) - pos.x,
                    mon_bottom - (wa_y + wa_h as i32).min(mon_bottom),
                    mon_right - (wa_x + wa_w as i32).min(mon_right),
                )
            } else {
                let margin = (40.0 * scale) as i32;
                (margin, margin, 0i32, margin)
            }

            #[cfg(target_os = "windows")]
            {
                let margin = (40.0 * scale) as i32;
                (margin, margin, 0i32, margin)
            }
        };

        let usable_w = (size.width as i32 - left_inset - right_inset).max(100) as u32;
        let usable_h = (size.height as i32 - top_inset - bottom_inset).max(100) as u32;
        let h = (usable_h as f32 * height_pct as f32 / 100.0) as u32;

        log::info!(
            "hotkey geometry: monitor={}x{} pos={},{} insets t={} l={} b={} r={} -> {}x{} at {},{}",
            size.width, size.height, pos.x, pos.y,
            top_inset, left_inset, bottom_inset, right_inset,
            usable_w, h, pos.x + left_inset, pos.y + top_inset,
        );

        window.set_outer_position(winit::dpi::PhysicalPosition::new(
            pos.x + left_inset,
            pos.y + top_inset,
        ));
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(usable_w, h));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::Stdio;

    #[test]
    fn detach_reaps_direct_child_and_orphans_program() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo $PPID"]).stdout(Stdio::piped());
        let mut child = detach(&mut cmd).expect("spawn sh");

        // Reaped, not a zombie: a zombie still answers kill(pid, 0).
        let rc = unsafe { libc::kill(child.id() as libc::pid_t, 0) };
        assert_eq!(rc, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));

        // The program ran (EOF once it exits) and was never our child.
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        let ppid: u32 = out.trim().parse().expect("sh printed its ppid");
        assert_ne!(ppid, std::process::id());
    }

    #[test]
    fn detach_reports_exec_failure() {
        let mut cmd = Command::new("/nonexistent/rustinator-detach-test");
        assert!(detach(&mut cmd).is_err());
    }
}
