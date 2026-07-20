use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{WindowAttributes, WindowId, WindowLevel};

use crate::app_shared::AppShared;
use crate::app_window::AppWindow;
use crate::gl_setup::SharedGl;
use crate::gl_window::GlWindow;
use crate::input::encode_raw_key;
use crate::pane_ui::ExternalDrop;
use crate::platform::apply_hotkey_geometry;
#[cfg(target_os = "macos")]
use crate::platform::{macos_activate_pid, macos_frontmost_pid, macos_order_window_back};

#[derive(Debug, Clone)]
pub enum UserEvent {
    Repaint,
    HotkeyTogglePressed,
}

struct PrefsWindowState {
    gl_window: GlWindow,
}

impl PrefsWindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        gl_display: &glutin::display::Display,
        gl_config: &glutin::config::Config,
        gl: &Arc<glow::Context>,
        main_context: &glutin::context::PossiblyCurrentContext,
    ) -> Self {
        let window_attrs = WindowAttributes::default()
            .with_title("Rustinator — Preferences")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0f32, 460.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(520.0f32, 360.0));

        Self {
            gl_window: GlWindow::new(
                event_loop,
                gl_display,
                gl_config,
                gl,
                main_context,
                window_attrs,
                egui::ViewportId::from_hash_of("prefs"),
                false,
            ),
        }
    }

    fn destroy(self, main_context: &glutin::context::PossiblyCurrentContext) {
        self.gl_window.destroy(main_context);
    }

    fn resize(&self, main_context: &glutin::context::PossiblyCurrentContext, width: u32, height: u32) {
        self.gl_window.resize(main_context, width, height);
    }

    fn paint(
        &mut self,
        window: &mut AppWindow,
        shared: &mut AppShared,
        main_context: &glutin::context::PossiblyCurrentContext,
    ) {
        let raw_input = self.gl_window.begin_frame(main_context);

        let mut close_requested = false;
        let full_output = self.gl_window.egui_ctx.run_ui(raw_input, |ui| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                close_requested = true;
            }
            window.draw_prefs_content(shared, ui);
        });

        if close_requested {
            shared.prefs.open = false;
            // Main window must repaint to restore un-previewed colors.
            shared.wake_main();
        }

        let bg = self.gl_window.egui_ctx.global_style().visuals.panel_fill;
        let clear_color = [
            bg.r() as f32 / 255.0,
            bg.g() as f32 / 255.0,
            bg.b() as f32 / 255.0,
            bg.a() as f32 / 255.0,
        ];

        let _ = self.gl_window.end_frame(main_context, full_output, clear_color);
    }
}

struct HotkeyWindowState {
    gl_window: GlWindow,
    current_modifiers: winit::event::Modifiers,
    zoom_pixel_accumulator: f64,
    shown_at: Option<Instant>,
    // Deadline this window's egui asked for via repaint_delay (cursor blink
    // etc). Folded into the control flow in about_to_wait.
    repaint_at: Option<Instant>,
    // When this window last painted; used by about_to_wait to cap its repaint
    // cadence to frame_interval.
    last_paint: Option<Instant>,
}

impl HotkeyWindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        gl_display: &glutin::display::Display,
        gl_config: &glutin::config::Config,
        gl: &Arc<glow::Context>,
        main_context: &glutin::context::PossiblyCurrentContext,
        height_pct: u32,
        always_on_top: bool,
    ) -> Self {
        let window_attrs = WindowAttributes::default()
            .with_title("rustinator")
            .with_decorations(false)
            .with_visible(false)
            .with_transparent(true);

        let gl_window = GlWindow::new(
            event_loop,
            gl_display,
            gl_config,
            gl,
            main_context,
            window_attrs,
            egui::ViewportId::from_hash_of("hotkey"),
            true,
        );

        if always_on_top {
            gl_window.window.set_window_level(WindowLevel::AlwaysOnTop);
        }
        apply_hotkey_geometry(&gl_window.window, height_pct);

        Self {
            gl_window,
            current_modifiers: winit::event::Modifiers::default(),
            zoom_pixel_accumulator: 0.0,
            shown_at: None,
            repaint_at: None,
            last_paint: None,
        }
    }

    fn destroy(self, main_context: &glutin::context::PossiblyCurrentContext) {
        self.gl_window.destroy(main_context);
    }

    fn resize(&self, main_context: &glutin::context::PossiblyCurrentContext, width: u32, height: u32) {
        self.gl_window.resize(main_context, width, height);
    }

    fn paint(
        &mut self,
        window: &mut AppWindow,
        shared: &mut AppShared,
        main_context: &glutin::context::PossiblyCurrentContext,
    ) {
        let raw_input = self.gl_window.begin_frame(main_context);

        let full_output = self.gl_window.egui_ctx.run_ui(raw_input, |ui| {
            window.logic(shared, ui.ctx());
            window.ui(shared, ui);
        });

        if window.fullscreen_pending {
            window.fullscreen_pending = false;
        }

        if let Some(vp_out) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
            for cmd in &vp_out.commands {
                if let egui::ViewportCommand::Title(t) = cmd {
                    self.gl_window.window.set_title(t);
                }
            }
        }

        let profile = shared.user_config.active();
        let [r, g, b] = profile.background_rgb();
        let opacity = profile.transparency.opacity;
        let clear_color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            opacity,
        ];

        self.repaint_at = self.gl_window.end_frame(main_context, full_output, clear_color);
        self.last_paint = Some(Instant::now());
    }
}

// --- Pure routing/governor seams (unit-tested below) ---

/// Which logical window an incoming `WindowId` targets. `window_event`
/// classifies the id, then dispatches to the matching handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Prefs,
    Hotkey,
    Terminal,
    Unknown,
}

/// Pure by-`WindowId` dispatch: map an incoming id to the window it targets.
/// Checked prefs-first, then hotkey, then the main/terminal window — the same
/// order the dispatch was inlined in `window_event`; anything else is `Unknown`.
fn classify(
    id: WindowId,
    prefs_id: Option<WindowId>,
    hotkey_id: Option<WindowId>,
    main_id: Option<WindowId>,
) -> WindowKind {
    if prefs_id == Some(id) {
        WindowKind::Prefs
    } else if hotkey_id == Some(id) {
        WindowKind::Hotkey
    } else if main_id == Some(id) {
        WindowKind::Terminal
    } else {
        WindowKind::Unknown
    }
}

/// Outcome of one frame-governor pass for the main window. `issue_redraw` asks
/// the caller to `request_redraw` now; `next_wake` is the merged deadline to arm
/// the loop with; `repaint_pending`/`egui_repaint_at` are the updated flag states
/// the caller writes back. Returned by value so `governor_next_wake` stays pure.
struct GovernorDecision {
    issue_redraw: bool,
    next_wake: Option<Instant>,
    repaint_pending: bool,
    egui_repaint_at: Option<Instant>,
}

/// Frame-governor math extracted from `about_to_wait`: fold a due egui repaint
/// deadline into `repaint_pending`, then decide whether the main window may
/// redraw now (at most once per `interval`) or must wait, merging the soonest
/// wake into `next_wake`. `gl_present` mirrors the original `if let Some(gl_state)`
/// guard so the governor only issues when a window exists. No side effects: the
/// caller performs the redraw and writes the returned flags back.
fn governor_next_wake(
    now: Instant,
    last_frame: Option<Instant>,
    interval: Duration,
    repaint_pending: bool,
    egui_repaint_at: Option<Instant>,
    gl_present: bool,
) -> GovernorDecision {
    let mut next_wake: Option<Instant> = None;
    let mut repaint_pending = repaint_pending;
    let mut egui_repaint_at = egui_repaint_at;

    // An egui-requested repaint deadline (cursor blink etc) that has come due
    // becomes a pending repaint, handled by the governor below.
    if let Some(at) = egui_repaint_at {
        if at <= now {
            egui_repaint_at = None;
            repaint_pending = true;
        } else {
            next_wake = Some(at);
        }
    }

    // Frame governor: a redraw is issued only once at least `interval` has
    // elapsed since the last frame, otherwise we wake when the frame is due.
    let mut issue_redraw = false;
    if repaint_pending && gl_present {
        match last_frame {
            Some(last) if now < last + interval => {
                let due = last + interval;
                next_wake = Some(next_wake.map_or(due, |w| w.min(due)));
            }
            _ => {
                repaint_pending = false;
                issue_redraw = true;
            }
        }
    }

    GovernorDecision {
        issue_redraw,
        next_wake,
        repaint_pending,
        egui_repaint_at,
    }
}

// --- Main application handler ---

struct WinitApp {
    // Process-global shared GL (the one context/display/config + glow handle),
    // outliving every window. Split out of the former `GlState`.
    gl: Option<SharedGl>,
    // Process-global App state (config, bindings, prefs, PaneId allocator, font
    // anchor), split off the former `App` god-struct (WF-E Stage B).
    shared: Option<AppShared>,
    // Terminal windows keyed by `WindowId`. Today holds EXACTLY ONE (the primary),
    // whose `GlWindow` lives inside its `AppWindow`; the router still targets it by
    // `main_window_id` (the multi-window generalization is Stage C).
    windows: HashMap<WindowId, AppWindow>,
    event_loop_proxy: EventLoopProxy<UserEvent>,
    main_window_id: Option<WindowId>,
    prefs: Option<PrefsWindowState>,
    shutting_down: bool,
    current_modifiers: winit::event::Modifiers,
    zoom_pixel_accumulator: f64,
    hotkey_handle: Option<crate::hotkey::HotkeyHandle>,
    hotkey_window: Option<HotkeyWindowState>,
    hotkey_height_pct: u32,
    hotkey_hide_on_focus_loss: bool,
    #[cfg(target_os = "macos")]
    hotkey_previous_app_pid: Option<i32>,
    // Any pending main-window repaint (PTY output, input events, egui
    // animations, due deadlines). The frame governor in about_to_wait turns it
    // into an actual redraw at most once per frame_interval.
    repaint_pending: bool,
    last_frame: Option<Instant>,
    frame_interval: Duration,
    // Deadline egui asked for via repaint_delay (cursor blink etc). Folded
    // into the control flow in about_to_wait; never set on the loop directly.
    egui_repaint_at: Option<Instant>,
    // Wall-clock cost of the last main-window paint. Stretches the governed
    // interval so a software rasterizer (llvmpipe over VNC) can't saturate a
    // core no matter how slow frames are.
    last_paint_cost: Option<Duration>,
    // Main window focus. Unfocused (and no hotkey dropdown shown) repaints
    // run at a reduced rate.
    main_focused: bool,
    // Last main-window pointer position (physical px). winit's DroppedFile
    // event carries no coordinates, so we use the most recent CursorMoved to
    // resolve which pane an external drop lands on.
    last_cursor_pos: Option<winit::dpi::PhysicalPosition<f64>>,
}

impl WinitApp {
    fn new(event_loop_proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            gl: None,
            shared: None,
            windows: HashMap::new(),
            event_loop_proxy,
            main_window_id: None,
            prefs: None,
            shutting_down: false,
            current_modifiers: winit::event::Modifiers::default(),
            zoom_pixel_accumulator: 0.0,
            hotkey_handle: None,
            hotkey_window: None,
            hotkey_height_pct: 50,
            hotkey_hide_on_focus_loss: true,
            #[cfg(target_os = "macos")]
            hotkey_previous_app_pid: None,
            repaint_pending: false,
            last_frame: None,
            frame_interval: Duration::from_micros(16_667), // ~60 fps
            egui_repaint_at: None,
            last_paint_cost: None,
            main_focused: true,
            last_cursor_pos: None,
        }
    }
}

impl ApplicationHandler<UserEvent> for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.main_window_id.is_some() {
            return;
        }

        // Builds the shared context and the primary `GlWindow` together. The
        // primary's painter (srgb=true) and the software-GL animation tweak that
        // `resumed` used to apply inline now happen inside `GlWindow::new_primary`,
        // against the same egui_ctx that drives the primary `AppWindow` and its
        // egui_winit.
        let (shared_gl, primary) = SharedGl::new(event_loop);

        // Process-global state first, then the primary window built from it —
        // the two halves of the old `App::new` (WF-E Stage B).
        let mut shared = AppShared::new(self.event_loop_proxy.clone());
        let scale_factor = primary.window.scale_factor() as f32;
        let window = match AppWindow::new_primary(
            primary,
            Arc::clone(&shared_gl.gl),
            &mut shared,
            scale_factor,
        ) {
            Ok(window) => window,
            Err(e) => {
                eprintln!("rustinator: fatal error during startup: {e}");
                std::process::exit(1);
            }
        };

        let hk_cfg = &shared.user_config.hotkey_window;
        if hk_cfg.enabled {
            self.hotkey_height_pct = hk_cfg.height_percent.clamp(1, 100);
            self.hotkey_hide_on_focus_loss = hk_cfg.hide_on_focus_loss;

            if let Some(handle) =
                crate::hotkey::spawn(&hk_cfg.hotkey, self.event_loop_proxy.clone())
            {
                self.hotkey_handle = Some(handle);
                self.hotkey_window = Some(HotkeyWindowState::new(
                    event_loop,
                    &shared_gl.gl_display,
                    &shared_gl.gl_config,
                    &shared_gl.gl,
                    shared_gl.context(),
                    self.hotkey_height_pct,
                    hk_cfg.always_on_top,
                ));
                window.gl_window.make_current(shared_gl.context());
            }
        }

        let main_id = window.gl_window.window.id();
        self.main_window_id = Some(main_id);
        self.gl = Some(shared_gl);
        self.shared = Some(shared);
        self.windows.insert(main_id, window);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint => {
                self.repaint_pending = true;
            }
            UserEvent::HotkeyTogglePressed => {
                if let Some(hk) = &mut self.hotkey_window {
                    if hk.shown_at.is_some() {
                        hk.shown_at = None;
                        hk.gl_window.window.set_visible(false);
                        #[cfg(target_os = "macos")]
                        if let Some(pid) = self.hotkey_previous_app_pid.take() {
                            macos_activate_pid(pid);
                        }
                    } else {
                        #[cfg(target_os = "macos")]
                        {
                            let prev = macos_frontmost_pid();
                            let ours = std::process::id() as i32;
                            if prev != Some(ours) {
                                self.hotkey_previous_app_pid = prev;
                            } else {
                                self.hotkey_previous_app_pid = None;
                            }
                        }
                        apply_hotkey_geometry(&hk.gl_window.window, self.hotkey_height_pct);
                        hk.shown_at = Some(Instant::now());
                        hk.gl_window.window.set_visible(true);
                        hk.gl_window.window.focus_window();
                        hk.repaint_at = None;
                        hk.gl_window.window.request_redraw();
                        #[cfg(target_os = "macos")]
                        if self.hotkey_previous_app_pid.is_some() {
                            if let Some(win) =
                                self.main_window_id.and_then(|id| self.windows.get(&id))
                            {
                                macos_order_window_back(&win.gl_window.window);
                            }
                        }
                    }
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.shutting_down {
            return;
        }
        let prefs_id = self.prefs.as_ref().map(|p| p.gl_window.window.id());
        let hotkey_id = self.hotkey_window.as_ref().map(|h| h.gl_window.window.id());
        let kind = classify(window_id, prefs_id, hotkey_id, self.main_window_id);

        if kind == WindowKind::Prefs {
            self.handle_prefs_window_event(event);
            return;
        }

        if kind == WindowKind::Hotkey {
            self.handle_hotkey_window_event(event);
            return;
        }

        self.handle_main_window_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.shutting_down {
            return;
        }
        let now = Instant::now();
        let interval = self.effective_frame_interval();

        // Frame governor: sole authority for main-window redraws. Every repaint
        // source (PTY output, input events, egui animations, due deadlines) sets
        // repaint_pending; the pure governor decides whether to redraw now
        // (capped to once per frame_interval) or to wake later. vsync is
        // unreliable on some Linux drivers (software GL ignores swap interval),
        // so this cap is the real throttle, not a fallback.
        let decision = governor_next_wake(
            now,
            self.last_frame,
            interval,
            self.repaint_pending,
            self.egui_repaint_at,
            !self.windows.is_empty(),
        );
        self.repaint_pending = decision.repaint_pending;
        self.egui_repaint_at = decision.egui_repaint_at;
        let mut next_wake = decision.next_wake;

        if decision.issue_redraw {
            if let Some(win) = self.main_window_id.and_then(|id| self.windows.get(&id)) {
                win.gl_window.window.request_redraw();
                // The hotkey dropdown shows the same terminal content, so
                // governed frames must reach it too.
                if let Some(hk) = &self.hotkey_window {
                    if hk.shown_at.is_some() {
                        hk.gl_window.window.request_redraw();
                    }
                }
            }
        }

        // Hotkey window: its egui deadline, capped to the same cadence as the
        // main window so a continuous animation can't repaint at swap rate.
        if let Some(hk) = &mut self.hotkey_window {
            if hk.shown_at.is_some() {
                if let Some(at) = hk.repaint_at {
                    let due = match hk.last_paint {
                        Some(last) => at.max(last + interval),
                        None => at,
                    };
                    if due <= now {
                        hk.repaint_at = None;
                        hk.gl_window.window.request_redraw();
                    } else {
                        next_wake = Some(next_wake.map_or(due, |w| w.min(due)));
                    }
                }
            }
        }

        // ControlFlow is sticky in winit: a stale WaitUntil in the past makes
        // the loop fire ResumeTimeReached on every iteration (busy loop).
        // Recompute it from scratch on every pass.
        event_loop.set_control_flow(match next_wake {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutting_down = true;
        let Some(gl) = self.gl.take() else { return };
        let primary = self
            .main_window_id
            .take()
            .and_then(|id| self.windows.remove(&id));
        if let Some(primary) = &primary {
            primary.gl_window.make_current(gl.context());
        }
        if let Some(hk) = self.hotkey_window.take() {
            hk.destroy(gl.context());
            if let Some(primary) = &primary {
                primary.gl_window.make_current(gl.context());
            }
        }
        if let Some(prefs) = self.prefs.take() {
            prefs.destroy(gl.context());
            if let Some(primary) = &primary {
                primary.gl_window.make_current(gl.context());
            }
        }
        // The primary's GlWindow is destroyed here: that tears down its painter
        // (the former inline `painter.destroy()`) plus its surface/window. The
        // shared context (`gl`) outlives this and drops at the end of the fn.
        if let Some(primary) = primary {
            primary.gl_window.destroy(gl.context());
        }
    }
}

impl WinitApp {
    // Prefs window event handling, moved verbatim out of `window_event` so the
    // router stays a thin classify-and-dispatch.
    fn handle_prefs_window_event(&mut self, event: WindowEvent) {
        let mut prefs = self.prefs.take().unwrap();
        let response = prefs.gl_window.egui_winit.on_window_event(&prefs.gl_window.window, &event);
        if response.repaint {
            prefs.gl_window.window.request_redraw();
        }
        let consumed = response.consumed;
        if !consumed {
            match event {
                WindowEvent::CloseRequested => {
                    if let Some(shared) = &mut self.shared {
                        shared.prefs.open = false;
                        // Main window must repaint to restore un-previewed
                        // colors (logic() does the actual restore).
                        shared.wake_main();
                    }
                }
                WindowEvent::Resized(size) => {
                    if let Some(gl) = &self.gl {
                        prefs.resize(gl.context(), size.width, size.height);
                    }
                    prefs.gl_window.window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    if let (Some(shared), Some(gl)) = (&mut self.shared, &self.gl) {
                        if let Some(win) =
                            self.main_window_id.and_then(|id| self.windows.get_mut(&id))
                        {
                            prefs.paint(win, shared, gl.context());
                            win.gl_window.make_current(gl.context());
                        }
                    }
                }
                _ => {}
            }
        }
        self.prefs = Some(prefs);
    }

    // Hotkey window event handling, moved verbatim out of `window_event`. The
    // trailing not-consumed match is split into `hotkey_apply_unconsumed` to
    // keep each function under 100 lines.
    fn handle_hotkey_window_event(&mut self, event: WindowEvent) {
        let mut hk = self.hotkey_window.take().unwrap();
        let win = self
            .main_window_id
            .and_then(|id| self.windows.get_mut(&id))
            .unwrap();

        if let WindowEvent::ModifiersChanged(mods) = &event {
            hk.current_modifiers = *mods;
        }

        if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
            if key_event.state.is_pressed()
                && key_event.logical_key == Key::Named(NamedKey::Escape)
                && hk.current_modifiers.state().is_empty()
            {
                hk.shown_at = None;
                hk.gl_window.window.set_visible(false);
                #[cfg(target_os = "macos")]
                if let Some(pid) = self.hotkey_previous_app_pid.take() {
                    macos_activate_pid(pid);
                }
                self.hotkey_window = Some(hk);
                return;
            }
            if let Some(raw) = encode_raw_key(key_event, hk.current_modifiers) {
                win.pending_raw_keys.push(raw);
            }
        }

        if let WindowEvent::MouseWheel { delta, .. } = &event {
            let zoom_mod = if cfg!(target_os = "macos") {
                hk.current_modifiers.state().super_key()
            } else {
                hk.current_modifiers.state().control_key()
            };
            if zoom_mod {
                match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => {
                        let direction = y.signum() as i32;
                        if direction != 0 {
                            win.pending_zoom_steps += direction;
                        }
                    }
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        hk.zoom_pixel_accumulator += pos.y;
                        const THRESHOLD: f64 = 30.0;
                        while hk.zoom_pixel_accumulator >= THRESHOLD {
                            win.pending_zoom_steps += 1;
                            hk.zoom_pixel_accumulator -= THRESHOLD;
                        }
                        while hk.zoom_pixel_accumulator <= -THRESHOLD {
                            win.pending_zoom_steps -= 1;
                            hk.zoom_pixel_accumulator += THRESHOLD;
                        }
                    }
                }
                hk.repaint_at = Some(Instant::now());
                self.hotkey_window = Some(hk);
                return;
            }
        }

        let response = hk.gl_window.egui_winit.on_window_event(&hk.gl_window.window, &event);
        if response.repaint {
            hk.repaint_at = Some(Instant::now());
        }
        let consumed = response.consumed;
        if !consumed {
            self.hotkey_apply_unconsumed(&mut hk, event);
        }
        self.hotkey_window = Some(hk);
    }

    // The not-consumed branch of the hotkey window's event handling. Split out
    // of `handle_hotkey_window_event`; the primary `AppWindow` is re-borrowed
    // here (disjoint from `gl`) exactly where the original held it across the
    // match.
    fn hotkey_apply_unconsumed(&mut self, hk: &mut HotkeyWindowState, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                hk.shown_at = None;
                hk.gl_window.window.set_visible(false);
            }
            WindowEvent::Resized(size) => {
                if let Some(gl) = &self.gl {
                    hk.resize(gl.context(), size.width, size.height);
                }
                hk.repaint_at = Some(Instant::now());
            }
            WindowEvent::Focused(focused) => {
                // See main-window note: clear any phantom modifier
                // latched while unfocused so typing isn't swallowed.
                hk.current_modifiers = winit::event::Modifiers::default();
                self.main_window_id
                    .and_then(|id| self.windows.get_mut(&id))
                    .unwrap()
                    .notify_focus(focused);
                if !focused && self.hotkey_hide_on_focus_loss {
                    let dominated_by_grace = hk.shown_at
                        .is_some_and(|t| t.elapsed().as_millis() < 500);
                    if !dominated_by_grace {
                        hk.shown_at = None;
                        hk.gl_window.window.set_visible(false);
                        #[cfg(target_os = "macos")]
                        if let Some(pid) = self.hotkey_previous_app_pid.take() {
                            macos_activate_pid(pid);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if hk.shown_at.is_some() {
                    if let (Some(shared), Some(gl)) = (&mut self.shared, &self.gl) {
                        if let Some(win) =
                            self.main_window_id.and_then(|id| self.windows.get_mut(&id))
                        {
                            hk.paint(win, shared, gl.context());
                            win.gl_window.make_current(gl.context());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Main window event handling, moved verbatim out of `window_event`. The
    // trailing match is split into `handle_main_window_action` to keep each
    // function under 100 lines.
    fn handle_main_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(win) = self.main_window_id.and_then(|id| self.windows.get_mut(&id)) else {
            return;
        };

        if let WindowEvent::ModifiersChanged(mods) = &event {
            self.current_modifiers = *mods;
        }

        if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
            if let Some(raw) = encode_raw_key(key_event, self.current_modifiers) {
                win.pending_raw_keys.push(raw);
            }
        }

        if let WindowEvent::MouseWheel { delta, .. } = &event {
            let zoom_mod = if cfg!(target_os = "macos") {
                self.current_modifiers.state().super_key()
            } else {
                self.current_modifiers.state().control_key()
            };
            if zoom_mod {
                match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => {
                        let direction = y.signum() as i32;
                        if direction != 0 {
                            win.pending_zoom_steps += direction;
                        }
                    }
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        self.zoom_pixel_accumulator += pos.y;
                        const THRESHOLD: f64 = 30.0;
                        while self.zoom_pixel_accumulator >= THRESHOLD {
                            win.pending_zoom_steps += 1;
                            self.zoom_pixel_accumulator -= THRESHOLD;
                        }
                        while self.zoom_pixel_accumulator <= -THRESHOLD {
                            win.pending_zoom_steps -= 1;
                            self.zoom_pixel_accumulator += THRESHOLD;
                        }
                    }
                }
                self.repaint_pending = true;
                return;
            }
        }

        let response = win
            .gl_window
            .egui_winit
            .on_window_event(&win.gl_window.window, &event);

        if response.repaint {
            self.repaint_pending = true;
        }
        if response.consumed {
            return;
        }

        self.handle_main_window_action(event_loop, event);
    }

    // The not-consumed branch of the main window's event handling. Split out of
    // `handle_main_window_event`; `gl`/`shared`/the primary `AppWindow` are
    // re-borrowed here exactly as the original held them across this match.
    fn handle_main_window_action(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(gl) = &self.gl else { return };
        let Some(shared) = &mut self.shared else { return };
        let Some(win) = self.main_window_id.and_then(|id| self.windows.get_mut(&id)) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                if win.dialogs.confirmed_close || !win.should_confirm_close(shared) {
                    event_loop.exit();
                } else {
                    win.dialogs.close_dialog_open = true;
                    self.repaint_pending = true;
                }
            }
            WindowEvent::Resized(size) => {
                win.gl_window.resize(gl.context(), size.width, size.height);
                self.repaint_pending = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                win.set_scale_factor(shared, scale_factor as f32);
                self.repaint_pending = true;
            }
            WindowEvent::Focused(focused) => {
                self.main_focused = focused;
                // A modifier released while we were unfocused (e.g. Cmd let go
                // mid Cmd+Tab) never reaches us as a ModifiersChanged, leaving a
                // phantom modifier latched — after which every keystroke encodes
                // as Cmd+key and produces no bytes. Resync to empty on any focus
                // change; the next real ModifiersChanged repopulates it.
                self.current_modifiers = winit::event::Modifiers::default();
                win.notify_focus(focused);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor_pos = Some(position);
            }
            WindowEvent::DroppedFile(path) => {
                // winit gives only a file path with no coordinates, so resolve
                // the target pane from the most recent cursor position. A
                // draw_panes consumer pastes the path into the pane under it.
                let scale = win.gl_window.window.scale_factor();
                if let Some(phys) = self.last_cursor_pos {
                    let pos = egui::pos2(
                        (phys.x / scale) as f32,
                        (phys.y / scale) as f32,
                    );
                    win.pane_view.pending_external_drop = Some(ExternalDrop {
                        text: path.to_string_lossy().into_owned(),
                        pos,
                    });
                    self.repaint_pending = true;
                }
            }
            WindowEvent::RedrawRequested => {
                self.paint(event_loop);
            }
            _ => {}
        }
    }

    // Governed frame interval for this moment: base cadence, slowed when the
    // app is unfocused (nobody is watching closely), stretched by the last
    // paint's cost so rendering is capped at roughly a third of a core when
    // the GL stack rasterizes in software. On a real GPU a frame costs ~1ms
    // and the base interval always wins.
    fn effective_frame_interval(&self) -> Duration {
        const UNFOCUSED_INTERVAL: Duration = Duration::from_millis(100);
        const PAINT_DUTY_FACTOR: u32 = 3;
        let active = self.main_focused
            || self.hotkey_window.as_ref().is_some_and(|hk| hk.shown_at.is_some());
        let base = if active { self.frame_interval } else { UNFOCUSED_INTERVAL };
        match self.last_paint_cost {
            Some(cost) => base.max(cost * PAINT_DUTY_FACTOR),
            None => base,
        }
    }

    fn paint(&mut self, event_loop: &ActiveEventLoop) {
        let paint_started = Instant::now();

        // This frame satisfies any pending repaint. Clearing here (not before)
        // lets work that arrives mid-paint re-arm the flag so the governor
        // schedules the next frame.
        self.repaint_pending = false;

        // Begin frame on the primary GlWindow (make-current + take input, the
        // same flow the prefs/hotkey windows use), then run logic+ui into the
        // primary's egui_ctx. clear_color is captured pre-frame, exactly as the
        // inline paint did, and handed to end_frame below.
        let (full_output, clear_color) = {
            let gl = self.gl.as_ref().unwrap();
            let shared = self.shared.as_mut().unwrap();
            let win = self
                .windows
                .get_mut(&self.main_window_id.unwrap())
                .unwrap();

            let mut raw_input = win.gl_window.begin_frame(gl.context());
            // Keep egui's focus traversal from eating Tab/Shift+Tab. egui decides
            // focus movement in begin_pass from these events, before app.logic can
            // strip them, so it grabs keyboard focus on a widget — after which
            // `egui_wants_keyboard_input()` makes app.logic drop all terminal keys
            // until the window is refocused. The terminal gets Tab via the separate
            // winit pending_raw_keys path, so removing it here costs nothing.
            raw_input.events.retain(|ev| {
                !matches!(ev, egui::Event::Key { key: egui::Key::Tab, pressed: true, .. })
            });
            let clear_color = win.clear_color();

            {
                let mut style = (*win.gl_window.egui_ctx.global_style()).clone();
                style.visuals.panel_fill = egui::Color32::TRANSPARENT;
                win.gl_window.egui_ctx.set_global_style(style);
            }

            // When the hotkey dropdown is open it owns keyboard/input processing,
            // so the main window must not also run `app.logic` (that would drain
            // the shared pending-key queue out from under the hotkey window). It
            // MUST, however, keep running `app.ui` so the main window keeps
            // painting its terminal content instead of going blank while it is up.
            let hotkey_visible = self.hotkey_window.as_ref()
                .is_some_and(|hk| hk.shown_at.is_some());
            // egui::Context is a cheap Arc handle; clone it so the closure can
            // borrow the AppWindow mutably (the ctx lives on win.gl_window).
            let egui_ctx = win.gl_window.egui_ctx.clone();
            let full_output = egui_ctx.run_ui(raw_input, |ui| {
                if !hotkey_visible {
                    win.logic(shared, ui.ctx());
                }
                win.ui(shared, ui);
            });
            (full_output, clear_color)
        };

        // Viewport commands (Close/Title), fullscreen toggle, and egui's
        // repaint-delay scheduling; then the global hotkey-config rebuild (kept
        // here in Stage A). All three are mutually independent and must run
        // before end_frame presents — the hotkey rebuild leaves the primary
        // current, so end_frame paints the primary surface.
        self.apply_viewport_commands(event_loop, &full_output);
        self.rebuild_hotkey_if_changed(event_loop);

        // End frame on the primary GlWindow: handle_platform_output + tessellate
        // + clear + paint + swap. Its repaint deadline is already folded in by
        // apply_viewport_commands, so the returned value is intentionally unused.
        {
            let gl = self.gl.as_ref().unwrap();
            let win = self
                .windows
                .get_mut(&self.main_window_id.unwrap())
                .unwrap();
            let _ = win.gl_window.end_frame(gl.context(), full_output, clear_color);
        }

        let paint_ended = Instant::now();
        self.last_frame = Some(paint_ended);
        self.last_paint_cost = Some(paint_ended - paint_started);

        self.manage_prefs_lifecycle(event_loop);
    }

    // Title / Close / fullscreen for the primary window plus folding egui's
    // per-frame `repaint_delay` into the governor flags — the viewport handling
    // the inline paint did between `run_ui` and tessellation. Mirrors the hotkey
    // window's Title handling (window.rs Title arm) and the primary's own
    // fullscreen/Close/repaint logic, unchanged. Borrows `full_output`; end_frame
    // consumes it afterward.
    fn apply_viewport_commands(
        &mut self,
        event_loop: &ActiveEventLoop,
        full_output: &egui::FullOutput,
    ) {
        let win = self
            .windows
            .get_mut(&self.main_window_id.unwrap())
            .unwrap();

        if win.fullscreen_pending {
            win.fullscreen_pending = false;
            let is_fullscreen = win.gl_window.window.fullscreen().is_some();
            if is_fullscreen {
                win.gl_window.window.set_fullscreen(None);
            } else {
                win.gl_window.window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
        }

        if let Some(vp_out) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
            for cmd in &vp_out.commands {
                match cmd {
                    egui::ViewportCommand::Close => event_loop.exit(),
                    egui::ViewportCommand::Title(t) => win.gl_window.window.set_title(t),
                    _ => {}
                }
            }
            if vp_out.repaint_delay.is_zero() {
                // Continuous animation: pend the repaint through the frame
                // governor so it runs at the capped rate, not swap rate.
                self.repaint_pending = true;
                self.egui_repaint_at = None;
            } else if vp_out.repaint_delay < std::time::Duration::from_secs(86400) {
                self.egui_repaint_at = Some(Instant::now() + vp_out.repaint_delay);
            } else {
                self.egui_repaint_at = None;
            }
        }
    }

    // The global hotkey-window rebuild triggered by a prefs change
    // (`shared.hotkey_changed`). Verbatim from the inline paint; the design
    // hoists it to the router in a later stage. Tears down the old hotkey window
    // and respawns from the new config, restoring the primary as current after
    // each surface swap. `event_loop` builds the replacement window.
    fn rebuild_hotkey_if_changed(&mut self, event_loop: &ActiveEventLoop) {
        let shared = self.shared.as_mut().unwrap();
        if !shared.hotkey_changed {
            return;
        }
        shared.hotkey_changed = false;
        let gl = self.gl.as_ref().unwrap();
        let win = self.windows.get(&self.main_window_id.unwrap()).unwrap();
        let hk_cfg = &shared.user_config.hotkey_window;
        if hk_cfg.enabled {
            self.hotkey_height_pct = hk_cfg.height_percent.clamp(1, 100);
            self.hotkey_hide_on_focus_loss = hk_cfg.hide_on_focus_loss;
            // Tear down old hotkey state.
            self.hotkey_handle = None;
            if let Some(old_hk) = self.hotkey_window.take() {
                old_hk.destroy(gl.context());
                win.gl_window.make_current(gl.context());
            }
            if let Some(handle) =
                crate::hotkey::spawn(&hk_cfg.hotkey, self.event_loop_proxy.clone())
            {
                self.hotkey_handle = Some(handle);
                self.hotkey_window = Some(HotkeyWindowState::new(
                    event_loop,
                    &gl.gl_display,
                    &gl.gl_config,
                    &gl.gl,
                    gl.context(),
                    self.hotkey_height_pct,
                    hk_cfg.always_on_top,
                ));
                win.gl_window.make_current(gl.context());
            }
        } else {
            self.hotkey_handle = None;
            if let Some(old_hk) = self.hotkey_window.take() {
                old_hk.destroy(gl.context());
                win.gl_window.make_current(gl.context());
            }
        }
    }

    // Create or tear down the prefs pop-out window to match `shared.prefs.open`,
    // restoring the primary as current after the surface swap. Verbatim from the
    // tail of the inline paint.
    fn manage_prefs_lifecycle(&mut self, event_loop: &ActiveEventLoop) {
        let shared = self.shared.as_ref().unwrap();
        let gl = self.gl.as_ref().unwrap();
        let win = self.windows.get(&self.main_window_id.unwrap()).unwrap();
        if shared.prefs.open && self.prefs.is_none() {
            self.prefs = Some(PrefsWindowState::new(
                event_loop,
                &gl.gl_display,
                &gl.gl_config,
                &gl.gl,
                gl.context(),
            ));
            win.gl_window.make_current(gl.context());
        } else if !shared.prefs.open && self.prefs.is_some() {
            if let Some(prefs) = self.prefs.take() {
                prefs.destroy(gl.context());
            }
            win.gl_window.make_current(gl.context());
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()?;
    let proxy = event_loop.create_proxy();
    let mut winit_app = WinitApp::new(proxy);
    event_loop.run_app(&mut winit_app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wid(n: u64) -> WindowId {
        WindowId::from(n)
    }

    #[test]
    fn classify_routes_each_window_id() {
        let prefs = wid(1);
        let hotkey = wid(2);
        let main = wid(3);
        let (p, h, m) = (Some(prefs), Some(hotkey), Some(main));

        assert_eq!(classify(prefs, p, h, m), WindowKind::Prefs);
        assert_eq!(classify(hotkey, p, h, m), WindowKind::Hotkey);
        assert_eq!(classify(main, p, h, m), WindowKind::Terminal);
        assert_eq!(classify(wid(99), p, h, m), WindowKind::Unknown);
    }

    #[test]
    fn classify_with_only_main_window() {
        // Today's single-window reality: prefs/hotkey closed. The main id is
        // Terminal; any other id (none occur at runtime) is Unknown.
        let main = wid(3);
        assert_eq!(classify(main, None, None, Some(main)), WindowKind::Terminal);
        assert_eq!(classify(wid(7), None, None, Some(main)), WindowKind::Unknown);
    }

    #[test]
    fn governor_waits_when_frame_not_due() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        // Last frame just happened: not enough time elapsed, so do not redraw;
        // wake when the frame becomes due.
        let d = governor_next_wake(now, Some(now), interval, true, None, true);
        assert!(!d.issue_redraw);
        assert!(d.repaint_pending); // still pending
        assert_eq!(d.next_wake, Some(now + interval));
    }

    #[test]
    fn governor_issues_when_frame_due() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        let last = now - Duration::from_millis(20); // older than interval
        let d = governor_next_wake(now, Some(last), interval, true, None, true);
        assert!(d.issue_redraw);
        assert!(!d.repaint_pending); // cleared by the issued frame
        assert_eq!(d.next_wake, None);
    }

    #[test]
    fn governor_repaint_pending_forces_issue() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        // Pending repaint, no prior frame: the fallthrough arm issues now.
        let d = governor_next_wake(now, None, interval, true, None, true);
        assert!(d.issue_redraw);
        assert!(!d.repaint_pending);

        // No pending repaint: nothing to issue, nothing to wake for.
        let idle = governor_next_wake(now, None, interval, false, None, true);
        assert!(!idle.issue_redraw);
        assert_eq!(idle.next_wake, None);
    }

    #[test]
    fn governor_skips_issue_without_gl() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        // Pending + would be due, but no window/context yet: the gl_state guard
        // means no redraw issues and the flag stays pending (matches the
        // original `if let Some(gl_state)` guard in about_to_wait).
        let d = governor_next_wake(now, None, interval, true, None, false);
        assert!(!d.issue_redraw);
        assert!(d.repaint_pending);
    }

    #[test]
    fn governor_due_egui_deadline_forces_repaint() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        let egui_at = now - Duration::from_millis(1); // already due
        // A due egui deadline is consumed and turns into a pending repaint;
        // with no prior frame that pend issues immediately.
        let d = governor_next_wake(now, None, interval, false, Some(egui_at), true);
        assert_eq!(d.egui_repaint_at, None);
        assert!(d.issue_redraw);
    }

    #[test]
    fn governor_merges_soonest_deadline() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        let last = now; // frame not due -> governor due at now + interval
        let egui_at = now + Duration::from_millis(5); // sooner than the frame
        let d = governor_next_wake(now, Some(last), interval, true, Some(egui_at), true);
        assert!(!d.issue_redraw);
        // next_wake is the MIN of the egui deadline and the frame-due time.
        assert_eq!(d.next_wake, Some(egui_at));
        // The egui deadline is still in the future, so it is preserved.
        assert_eq!(d.egui_repaint_at, Some(egui_at));
    }
}
