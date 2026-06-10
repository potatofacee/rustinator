use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_glow::ShaderVersion;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{WindowAttributes, WindowId, WindowLevel};

use crate::App;
use crate::gl_setup::GlState;
use crate::gl_window::GlWindow;
use crate::input::encode_raw_key;
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

    fn paint(&mut self, app: &mut App, main_context: &glutin::context::PossiblyCurrentContext) {
        let raw_input = self.gl_window.begin_frame(main_context);

        let mut close_requested = false;
        let full_output = self.gl_window.egui_ctx.run_ui(raw_input, |ui| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                close_requested = true;
            }
            app.draw_prefs_content(ui);
        });

        if close_requested {
            app.prefs.open = false;
        }

        let bg = self.gl_window.egui_ctx.global_style().visuals.panel_fill;
        let clear_color = [
            bg.r() as f32 / 255.0,
            bg.g() as f32 / 255.0,
            bg.b() as f32 / 255.0,
            bg.a() as f32 / 255.0,
        ];

        self.gl_window.end_frame(main_context, full_output, clear_color);
    }
}

struct HotkeyWindowState {
    gl_window: GlWindow,
    current_modifiers: winit::event::Modifiers,
    zoom_pixel_accumulator: f64,
    shown_at: Option<Instant>,
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
        }
    }

    fn destroy(self, main_context: &glutin::context::PossiblyCurrentContext) {
        self.gl_window.destroy(main_context);
    }

    fn resize(&self, main_context: &glutin::context::PossiblyCurrentContext, width: u32, height: u32) {
        self.gl_window.resize(main_context, width, height);
    }

    fn paint(&mut self, app: &mut App, main_context: &glutin::context::PossiblyCurrentContext) {
        let raw_input = self.gl_window.begin_frame(main_context);

        let full_output = self.gl_window.egui_ctx.run_ui(raw_input, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });

        if app.fullscreen_pending {
            app.fullscreen_pending = false;
        }

        if let Some(vp_out) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
            for cmd in &vp_out.commands {
                if let egui::ViewportCommand::Title(t) = cmd {
                    self.gl_window.window.set_title(t);
                }
            }
        }

        let profile = app.user_config.active();
        let [r, g, b] = profile.background_rgb();
        let opacity = profile.transparency.opacity;
        let clear_color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            opacity,
        ];

        self.gl_window.end_frame(main_context, full_output, clear_color);
    }
}

// --- Main application handler ---

struct WinitApp {
    gl_state: Option<GlState>,
    egui_ctx: egui::Context,
    egui_winit: Option<egui_winit::State>,
    painter: Option<egui_glow::Painter>,
    app: Option<App>,
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
    terminal_repaint_pending: bool,
    // Frame governor: cap terminal-driven repaints to a fixed rate so a high
    // PTY output rate can't drive an unbounded redraw loop. vsync is unreliable
    // on some Linux drivers (software GL ignores swap interval), so this is the
    // real throttle, not a fallback.
    last_frame: Option<Instant>,
    frame_interval: Duration,
    // Deadline egui asked for via repaint_delay (cursor blink etc). Folded
    // into the control flow in about_to_wait; never set on the loop directly.
    egui_repaint_at: Option<Instant>,
}

impl WinitApp {
    fn new(event_loop_proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            gl_state: None,
            egui_ctx: egui::Context::default(),
            egui_winit: None,
            painter: None,
            app: None,
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
            terminal_repaint_pending: false,
            last_frame: None,
            frame_interval: Duration::from_micros(16_667), // ~60 fps
            egui_repaint_at: None,
        }
    }
}

impl ApplicationHandler<UserEvent> for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gl_state.is_some() {
            return;
        }

        let gl_state = GlState::new(event_loop);
        let painter = egui_glow::Painter::new(
            Arc::clone(&gl_state.gl),
            "",
            Some(ShaderVersion::get(&gl_state.gl)),
            true,
        )
        .expect("failed to create egui_glow painter");

        let egui_winit = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            event_loop,
            Some(gl_state.window.scale_factor() as f32),
            None,
            Some(painter.max_texture_side()),
        );

        let app = match App::new(
            Arc::clone(&gl_state.gl),
            self.egui_ctx.clone(),
            self.event_loop_proxy.clone(),
            gl_state.window.scale_factor() as f32,
        ) {
            Ok(app) => app,
            Err(e) => {
                eprintln!("rustinator: fatal error during startup: {e}");
                std::process::exit(1);
            }
        };

        let hk_cfg = &app.user_config.hotkey_window;
        if hk_cfg.enabled {
            self.hotkey_height_pct = hk_cfg.height_percent.clamp(1, 100);
            self.hotkey_hide_on_focus_loss = hk_cfg.hide_on_focus_loss;

            if let Some(handle) =
                crate::hotkey::spawn(&hk_cfg.hotkey, self.event_loop_proxy.clone())
            {
                self.hotkey_handle = Some(handle);
                self.hotkey_window = Some(HotkeyWindowState::new(
                    event_loop,
                    &gl_state.gl_display,
                    &gl_state.gl_config,
                    &gl_state.gl,
                    &gl_state.gl_context,
                    self.hotkey_height_pct,
                    hk_cfg.always_on_top,
                ));
                gl_state.make_current();
            }
        }

        self.main_window_id = Some(gl_state.window.id());
        self.gl_state = Some(gl_state);
        self.painter = Some(painter);
        self.egui_winit = Some(egui_winit);
        self.app = Some(app);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint => {
                self.terminal_repaint_pending = true;
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
                        #[cfg(target_os = "macos")]
                        if self.hotkey_previous_app_pid.is_some() {
                            if let Some(gl_state) = &self.gl_state {
                                macos_order_window_back(&gl_state.window);
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
        // Prefs window events.
        let is_prefs_window = self.prefs.as_ref().is_some_and(|p| window_id == p.gl_window.window.id());
        if is_prefs_window {
            let mut prefs = self.prefs.take().unwrap();
            let response = prefs.gl_window.egui_winit.on_window_event(&prefs.gl_window.window, &event);
            if response.repaint {
                prefs.gl_window.window.request_redraw();
            }
            let consumed = response.consumed;
            if !consumed {
                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(app) = &mut self.app {
                            app.prefs.open = false;
                        }
                    }
                    WindowEvent::Resized(size) => {
                        if let Some(gl_state) = &self.gl_state {
                            prefs.resize(&gl_state.gl_context, size.width, size.height);
                        }
                        prefs.gl_window.window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => {
                        if let (Some(app), Some(gl_state)) = (&mut self.app, &self.gl_state) {
                            prefs.paint(app, &gl_state.gl_context);
                            gl_state.make_current();
                        }
                    }
                    _ => {}
                }
            }
            self.prefs = Some(prefs);
            return;
        }

        // Hotkey window events.
        let is_hotkey_window = self
            .hotkey_window
            .as_ref()
            .is_some_and(|h| window_id == h.gl_window.window.id());
        if is_hotkey_window {
            let mut hk = self.hotkey_window.take().unwrap();
            let app = self.app.as_mut().unwrap();

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
                    app.input.pending_raw_keys.push(raw);
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
                                app.pending_zoom_steps += direction;
                            }
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            hk.zoom_pixel_accumulator += pos.y;
                            const THRESHOLD: f64 = 30.0;
                            while hk.zoom_pixel_accumulator >= THRESHOLD {
                                app.pending_zoom_steps += 1;
                                hk.zoom_pixel_accumulator -= THRESHOLD;
                            }
                            while hk.zoom_pixel_accumulator <= -THRESHOLD {
                                app.pending_zoom_steps -= 1;
                                hk.zoom_pixel_accumulator += THRESHOLD;
                            }
                        }
                    }
                    hk.gl_window.window.request_redraw();
                    self.hotkey_window = Some(hk);
                    return;
                }
            }

            let response = hk.gl_window.egui_winit.on_window_event(&hk.gl_window.window, &event);
            if response.repaint {
                hk.gl_window.window.request_redraw();
            }
            let consumed = response.consumed;
            if !consumed {
                match event {
                    WindowEvent::CloseRequested => {
                        hk.shown_at = None;
                        hk.gl_window.window.set_visible(false);
                    }
                    WindowEvent::Resized(size) => {
                        if let Some(gl_state) = &self.gl_state {
                            hk.resize(&gl_state.gl_context, size.width, size.height);
                        }
                        hk.gl_window.window.request_redraw();
                    }
                    WindowEvent::Focused(focused) => {
                        app.notify_focus(focused);
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
                            if let Some(gl_state) = &self.gl_state {
                                hk.paint(app, &gl_state.gl_context);
                                gl_state.make_current();
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.hotkey_window = Some(hk);
            return;
        }

        // Main window events.
        let Some(gl_state) = &self.gl_state else { return };
        let Some(egui_winit) = &mut self.egui_winit else { return };
        let Some(app) = &mut self.app else { return };



        if let WindowEvent::ModifiersChanged(mods) = &event {
            self.current_modifiers = *mods;
        }

        if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
            if let Some(raw) = encode_raw_key(key_event, self.current_modifiers) {
                app.input.pending_raw_keys.push(raw);
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
                            app.pending_zoom_steps += direction;
                        }
                    }
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        self.zoom_pixel_accumulator += pos.y;
                        const THRESHOLD: f64 = 30.0;
                        while self.zoom_pixel_accumulator >= THRESHOLD {
                            app.pending_zoom_steps += 1;
                            self.zoom_pixel_accumulator -= THRESHOLD;
                        }
                        while self.zoom_pixel_accumulator <= -THRESHOLD {
                            app.pending_zoom_steps -= 1;
                            self.zoom_pixel_accumulator += THRESHOLD;
                        }
                    }
                }
                gl_state.window.request_redraw();
                return;
            }
        }

        let response = egui_winit.on_window_event(&gl_state.window, &event);

        if response.repaint {
            gl_state.window.request_redraw();
        }
        if response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if app.dialogs.confirmed_close || !app.should_confirm_close() {
                    event_loop.exit();
                } else {
                    app.dialogs.close_dialog_open = true;
                    gl_state.window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                gl_state.resize(size.width, size.height);
                gl_state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                app.set_scale_factor(scale_factor as f32);
                gl_state.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                app.notify_focus(focused);
            }
            WindowEvent::RedrawRequested => {
                self.paint(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.shutting_down {
            return;
        }
        let now = Instant::now();
        let mut next_wake: Option<Instant> = None;

        // Frame governor. A pending terminal repaint only triggers a redraw once
        // at least frame_interval has elapsed since the last frame; otherwise we
        // wake when the frame is due. This caps repaints at ~60 fps no matter how
        // fast the PTY produces output, and costs nothing when idle.
        if self.terminal_repaint_pending {
            if let Some(gl_state) = &self.gl_state {
                match self.last_frame {
                    Some(last) if now < last + self.frame_interval => {
                        next_wake = Some(last + self.frame_interval);
                    }
                    _ => {
                        self.terminal_repaint_pending = false;
                        gl_state.window.request_redraw();
                    }
                }
            }
        }

        // egui-requested repaint (cursor blink etc).
        if let Some(at) = self.egui_repaint_at {
            if at <= now {
                self.egui_repaint_at = None;
                if let Some(gl_state) = &self.gl_state {
                    gl_state.window.request_redraw();
                }
            } else {
                next_wake = Some(next_wake.map_or(at, |w| w.min(at)));
            }
        }

        if let Some(hk) = &self.hotkey_window {
            if hk.shown_at.is_some() {
                hk.gl_window.window.request_redraw();
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
        if let Some(gl_state) = &self.gl_state {
            gl_state.make_current();
        }
        if let Some(hk) = self.hotkey_window.take() {
            if let Some(gl_state) = &self.gl_state {
                hk.destroy(&gl_state.gl_context);
                gl_state.make_current();
            }
        }
        if let Some(prefs) = self.prefs.take() {
            if let Some(gl_state) = &self.gl_state {
                prefs.destroy(&gl_state.gl_context);
                gl_state.make_current();
            }
        }
        if let Some(painter) = &mut self.painter {
            painter.destroy();
        }
    }
}

impl WinitApp {
    fn paint(&mut self, event_loop: &ActiveEventLoop) {
        let gl_state = self.gl_state.as_ref().unwrap();
        let painter = self.painter.as_mut().unwrap();
        let egui_winit = self.egui_winit.as_mut().unwrap();
        let app = self.app.as_mut().unwrap();

        // This frame satisfies any pending terminal repaint. Clearing here (not
        // before) lets PTY output that arrives mid-paint re-arm the flag so the
        // governor schedules the next frame.
        self.terminal_repaint_pending = false;

        gl_state.make_current();

        let mut raw_input = egui_winit.take_egui_input(&gl_state.window);
        // Keep egui's focus traversal from eating Tab/Shift+Tab. egui decides
        // focus movement in begin_pass from these events, before app.logic can
        // strip them, so it grabs keyboard focus on a widget — after which
        // `egui_wants_keyboard_input()` makes app.logic drop all terminal keys
        // until the window is refocused. The terminal gets Tab via the separate
        // winit pending_raw_keys path, so removing it here costs nothing.
        raw_input.events.retain(|ev| {
            !matches!(ev, egui::Event::Key { key: egui::Key::Tab, pressed: true, .. })
        });
        let clear_color = app.clear_color();

        {
            let mut style = (*self.egui_ctx.global_style()).clone();
            style.visuals.panel_fill = egui::Color32::TRANSPARENT;
            self.egui_ctx.set_global_style(style);
        }

        // When the hotkey dropdown is open it owns keyboard/input processing, so
        // the main window must not also run `app.logic` (that would drain the
        // shared pending-key queue out from under the hotkey window). It MUST,
        // however, keep running `app.ui` so the main window continues to paint
        // its terminal content instead of going blank while the dropdown is up.
        let hotkey_visible = self.hotkey_window.as_ref()
            .is_some_and(|hk| hk.shown_at.is_some());
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            if !hotkey_visible {
                app.logic(ui.ctx());
            }
            app.ui(ui);
        });

        egui_winit.handle_platform_output(&gl_state.window, full_output.platform_output);

        if app.fullscreen_pending {
            app.fullscreen_pending = false;
            let is_fullscreen = gl_state.window.fullscreen().is_some();
            if is_fullscreen {
                gl_state.window.set_fullscreen(None);
            } else {
                gl_state.window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
        }

        if app.hotkey_changed {
            app.hotkey_changed = false;
            let hk_cfg = &app.user_config.hotkey_window;
            if hk_cfg.enabled {
                self.hotkey_height_pct = hk_cfg.height_percent.clamp(1, 100);
                self.hotkey_hide_on_focus_loss = hk_cfg.hide_on_focus_loss;
                // Tear down old hotkey state.
                self.hotkey_handle = None;
                if let Some(old_hk) = self.hotkey_window.take() {
                    old_hk.destroy(&gl_state.gl_context);
                    gl_state.make_current();
                }
                if let Some(handle) =
                    crate::hotkey::spawn(&hk_cfg.hotkey, self.event_loop_proxy.clone())
                {
                    self.hotkey_handle = Some(handle);
                    self.hotkey_window = Some(HotkeyWindowState::new(
                        event_loop,
                        &gl_state.gl_display,
                        &gl_state.gl_config,
                        &gl_state.gl,
                        &gl_state.gl_context,
                        self.hotkey_height_pct,
                        hk_cfg.always_on_top,
                    ));
                    gl_state.make_current();
                }
            } else {
                self.hotkey_handle = None;
                if let Some(old_hk) = self.hotkey_window.take() {
                    old_hk.destroy(&gl_state.gl_context);
                    gl_state.make_current();
                }
            }
        }

        if let Some(vp_out) = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
        {
            for cmd in &vp_out.commands {
                match cmd {
                    egui::ViewportCommand::Close => event_loop.exit(),
                    egui::ViewportCommand::Title(t) => gl_state.window.set_title(t),
                    _ => {}
                }
            }
            if vp_out.repaint_delay.is_zero() {
                gl_state.window.request_redraw();
            } else if vp_out.repaint_delay < std::time::Duration::from_secs(86400) {
                self.egui_repaint_at = Some(Instant::now() + vp_out.repaint_delay);
            } else {
                self.egui_repaint_at = None;
            }
        }

        let screen_size: [u32; 2] = gl_state.window.inner_size().into();
        let pixels_per_point = self.egui_ctx.pixels_per_point();

        let clipped_primitives =
            self.egui_ctx.tessellate(full_output.shapes, pixels_per_point);

        painter.clear(screen_size, clear_color);
        painter.paint_and_update_textures(
            screen_size,
            pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        );

        gl_state.swap_buffers();
        self.last_frame = Some(Instant::now());

        // Manage prefs pop-out window lifecycle.
        if app.prefs.open && self.prefs.is_none() {
            self.prefs = Some(PrefsWindowState::new(
                event_loop,
                &gl_state.gl_display,
                &gl_state.gl_config,
                &gl_state.gl,
                &gl_state.gl_context,
            ));
            gl_state.make_current();
        } else if !app.prefs.open && self.prefs.is_some() {
            if let Some(prefs) = self.prefs.take() {
                prefs.destroy(&gl_state.gl_context);
            }
            gl_state.make_current();
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
