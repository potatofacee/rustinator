use winit::window::Window;

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
