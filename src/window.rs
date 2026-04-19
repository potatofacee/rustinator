use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;

use egui_glow::ShaderVersion;
use glow::HasContext as _;
use glutin::config::{Config as GlConfig, ConfigTemplateBuilder, GlConfig as _};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
    PossiblyCurrentGlContext as _,
};
use glutin::display::{Display, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::App;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Repaint,
}

pub struct GlState {
    gl_context: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    gl_display: Display,
    gl_config: GlConfig,
    window: Window,
    pub gl: Arc<glow::Context>,
}

impl GlState {
    fn new(event_loop: &ActiveEventLoop) -> Self {
        let window_attrs = WindowAttributes::default()
            .with_title("rustinator")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0f32, 560.0))
            .with_transparent(true);

        let display = create_display(event_loop);

        log::info!("GL display backend: {}", display_backend_name(&display));

        let gl_config = pick_gl_config(&display);

        log::info!(
            "GL config: alpha_size={}, transparency={:?}",
            gl_config.alpha_size(),
            gl_config.supports_transparency(),
        );

        let window = glutin_winit::finalize_window(event_loop, window_attrs, &gl_config)
            .expect("failed to create window");

        let (gl_context, gl_surface) = create_context_and_surface(&display, &gl_config, &window);

        let gl = unsafe {
            Arc::new(glow::Context::from_loader_function(|name| {
                let cname = CString::new(name).unwrap();
                display.get_proc_address(&cname)
            }))
        };

        unsafe {
            gl.get_error();
        }

        let alpha_bits = gl_config.alpha_size();
        log::info!("GL config alpha_size: {alpha_bits}");
        if alpha_bits == 0 {
            log::warn!("GL config has no alpha — transparency will not work");
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            use glutin::platform::x11::X11GlConfigExt as _;
            if let Some(visual) = gl_config.x11_visual() {
                log::info!("X11 visual ID: 0x{:x}", visual.visual_id());
            } else {
                log::warn!("no X11 visual on GL config — compositor transparency won't work");
            }
        }

        Self {
            gl_context,
            gl_surface,
            gl_display: display,
            gl_config,
            window,
            gl,
        }
    }

    fn swap_buffers(&self) {
        self.gl_surface.swap_buffers(&self.gl_context).ok();
    }

    fn resize(&self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(&self.gl_context, w, h);
        }
    }

    fn make_current(&self) {
        self.gl_context.make_current(&self.gl_surface).ok();
    }
}

fn create_context_and_surface(
    display: &Display,
    gl_config: &GlConfig,
    window: &Window,
) -> (PossiblyCurrentContext, Surface<WindowSurface>) {
    let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());

    let gl_context_attrs = ContextAttributesBuilder::new().build(raw_window_handle);

    let gl_context = unsafe {
        display
            .create_context(gl_config, &gl_context_attrs)
            .or_else(|_| {
                let attrs = ContextAttributesBuilder::new()
                    .with_context_api(ContextApi::Gles(None))
                    .build(raw_window_handle);
                display.create_context(gl_config, &attrs)
            })
            .expect("failed to create GL context")
    };

    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_window_handle.expect("window handle required for surface"),
        NonZeroU32::new(window.inner_size().width.max(1)).unwrap(),
        NonZeroU32::new(window.inner_size().height.max(1)).unwrap(),
    );

    let gl_surface = unsafe {
        display
            .create_window_surface(gl_config, &surface_attrs)
            .expect("failed to create GL surface")
    };

    let gl_context = gl_context
        .make_current(&gl_surface)
        .expect("failed to make context current");

    (gl_context, gl_surface)
}

fn pick_gl_config(display: &Display) -> GlConfig {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    use glutin::platform::x11::X11GlConfigExt as _;

    // First try: configs with transparency and alpha.
    let transparent_template = ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .with_transparency(true)
        .build();

    if let Ok(configs) = unsafe { display.find_configs(transparent_template) } {
        let mut best: Option<GlConfig> = None;
        for config in configs {
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                if config.x11_visual().is_none() {
                    log::debug!(
                        "skipping GL config (no X11 visual): alpha_size={}",
                        config.alpha_size()
                    );
                    continue;
                }
            }
            if config.alpha_size() >= best.as_ref().map_or(0, |c| c.alpha_size()) {
                best = Some(config);
            }
        }
        if let Some(config) = best {
            return config;
        }
    }

    log::warn!("no transparent GL config with X11 visual found; trying without transparency filter");

    // Fallback: any config with alpha, no transparency filter.
    let alpha_template = ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .build();

    if let Ok(configs) = unsafe { display.find_configs(alpha_template) } {
        let mut best: Option<GlConfig> = None;
        for config in configs {
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                if config.x11_visual().is_none() {
                    continue;
                }
            }
            if config.alpha_size() >= best.as_ref().map_or(0, |c| c.alpha_size()) {
                best = Some(config);
            }
        }
        if let Some(config) = best {
            return config;
        }
    }

    // Last resort: any config at all.
    log::warn!("no GL config with alpha found; transparency will not work");
    let any_template = ConfigTemplateBuilder::new().build();
    unsafe {
        display
            .find_configs(any_template)
            .expect("failed to find any GL config")
            .next()
            .expect("no GL configs available at all")
    }
}

fn display_backend_name(display: &Display) -> &'static str {
    let s = format!("{display:?}");
    if s.starts_with("Glx") {
        "GLX"
    } else if s.starts_with("Egl") {
        "EGL"
    } else if s.starts_with("Cgl") {
        "CGL"
    } else if s.starts_with("Wgl") {
        "WGL"
    } else {
        "unknown"
    }
}

fn create_display(event_loop: &ActiveEventLoop) -> Display {
    use glutin::display::DisplayApiPreference;

    let raw_display = event_loop
        .display_handle()
        .expect("no display handle")
        .as_raw();

    #[cfg(target_os = "macos")]
    {
        return unsafe {
            Display::new(raw_display, DisplayApiPreference::Cgl)
                .expect("failed to create CGL display")
        };
    }

    #[cfg(target_os = "windows")]
    {
        return unsafe {
            Display::new(raw_display, DisplayApiPreference::WglThenEgl(None))
                .expect("failed to create WGL display")
        };
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let preference = DisplayApiPreference::GlxThenEgl(Box::new(
            winit::platform::x11::register_xlib_error_hook,
        ));
        unsafe { Display::new(raw_display, preference).expect("failed to create GL display") }
    }
}

// --- Prefs window (secondary OS window) ---

struct PrefsWindowState {
    window: Window,
    gl_context: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
    egui_ctx: egui::Context,
    egui_winit: egui_winit::State,
}

impl PrefsWindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        gl_display: &Display,
        gl_config: &GlConfig,
        gl: &Arc<glow::Context>,
    ) -> Self {
        let window_attrs = WindowAttributes::default()
            .with_title("Rustinator — Preferences")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0f32, 460.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(520.0f32, 360.0));

        let window = glutin_winit::finalize_window(event_loop, window_attrs, gl_config)
            .expect("failed to create prefs window");

        let (gl_context, gl_surface) =
            create_context_and_surface(gl_display, gl_config, &window);

        let painter = egui_glow::Painter::new(
            Arc::clone(gl),
            "",
            Some(ShaderVersion::get(gl)),
            false,
        )
        .expect("failed to create prefs painter");

        let egui_ctx = egui::Context::default();

        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::from_hash_of("prefs"),
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            Some(painter.max_texture_side()),
        );

        Self {
            window,
            gl_context,
            gl_surface,
            painter,
            egui_ctx,
            egui_winit,
        }
    }

    fn make_current(&self) {
        self.gl_context.make_current(&self.gl_surface).ok();
    }

    fn swap_buffers(&self) {
        self.gl_surface.swap_buffers(&self.gl_context).ok();
    }

    fn resize(&self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(&self.gl_context, w, h);
        }
    }

    fn paint(&mut self, app: &mut App) {
        self.make_current();

        let raw_input = self.egui_winit.take_egui_input(&self.window);

        let mut close_requested = false;
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                close_requested = true;
            }
            app.draw_prefs_content(ui);
        });

        if close_requested {
            app.prefs_open = false;
        }

        self.egui_winit
            .handle_platform_output(&self.window, full_output.platform_output);

        let screen_size: [u32; 2] = self.window.inner_size().into();
        let pixels_per_point = self.egui_ctx.pixels_per_point();

        let clipped_primitives = self.egui_ctx.tessellate(full_output.shapes, pixels_per_point);

        let bg = self.egui_ctx.global_style().visuals.panel_fill;
        let clear_color = [
            bg.r() as f32 / 255.0,
            bg.g() as f32 / 255.0,
            bg.b() as f32 / 255.0,
            bg.a() as f32 / 255.0,
        ];

        self.painter.clear(screen_size, clear_color);
        self.painter.paint_and_update_textures(
            screen_size,
            pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        );

        self.swap_buffers();
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
    pending_keys: Vec<egui::Event>,
    current_modifiers: winit::event::Modifiers,
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
            pending_keys: Vec::new(),
            current_modifiers: winit::event::Modifiers::default(),
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

        let app = App::new(
            Arc::clone(&gl_state.gl),
            self.egui_ctx.clone(),
            self.event_loop_proxy.clone(),
        );

        self.main_window_id = Some(gl_state.window.id());
        self.gl_state = Some(gl_state);
        self.painter = Some(painter);
        self.egui_winit = Some(egui_winit);
        self.app = Some(app);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint => {
                if let Some(gl_state) = &self.gl_state {
                    gl_state.window.request_redraw();
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
        if let Some(prefs) = &mut self.prefs {
            if window_id == prefs.window.id() {
                let response = prefs.egui_winit.on_window_event(&prefs.window, &event);
                if response.repaint {
                    prefs.window.request_redraw();
                }
                if response.consumed {
                    return;
                }
                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(app) = &mut self.app {
                            app.prefs_open = false;
                        }
                    }
                    WindowEvent::Resized(size) => {
                        prefs.resize(size.width, size.height);
                        prefs.window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => {
                        if let Some(app) = &mut self.app {
                            prefs.paint(app);
                            // Restore main GL context.
                            if let Some(gl_state) = &self.gl_state {
                                gl_state.make_current();
                            }
                        }
                    }
                    _ => {}
                }
                return;
            }
        }

        // Main window events.
        let Some(gl_state) = &self.gl_state else { return };
        let Some(egui_winit) = &mut self.egui_winit else { return };
        let Some(app) = &mut self.app else { return };

        if let WindowEvent::ModifiersChanged(mods) = &event {
            self.current_modifiers = *mods;
        }

        // Intercept keyboard events that egui-winit may fail to translate
        // (Ctrl+letter produces control characters in logical_key, not letter keys).
        if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
            if let Some(egui_ev) = translate_key_event(key_event, self.current_modifiers) {
                self.pending_keys.push(egui_ev);
            }
        }

        // Intercept Ctrl+scroll for font zoom before egui turns it into smooth zoom_delta.
        if let WindowEvent::MouseWheel { delta, .. } = &event {
            if self.current_modifiers.state().control_key() {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y as i32,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.y / 20.0) as i32,
                };
                if lines != 0 {
                    app.pending_zoom_steps += lines;
                    return;
                }
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
                if app.confirmed_close || !app.should_confirm_close() {
                    event_loop.exit();
                } else {
                    app.close_dialog_open = true;
                    gl_state.window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                gl_state.resize(size.width, size.height);
                gl_state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.paint(event_loop);
            }
            _ => {}
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(gl_state) = &self.gl_state {
                gl_state.window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.shutting_down {
            return;
        }
        if let Some(gl_state) = &self.gl_state {
            gl_state.window.request_redraw();
        }
        if let Some(prefs) = &self.prefs {
            prefs.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutting_down = true;
        if let Some(mut prefs) = self.prefs.take() {
            prefs.painter.destroy();
        }
        if let Some(gl_state) = &self.gl_state {
            gl_state.make_current();
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

        gl_state.make_current();

        let mut raw_input = egui_winit.take_egui_input(&gl_state.window);
        raw_input.events.append(&mut self.pending_keys);
        let clear_color = app.clear_color();

        {
            let mut style = (*self.egui_ctx.global_style()).clone();
            style.visuals.panel_fill = egui::Color32::TRANSPARENT;
            self.egui_ctx.set_global_style(style);
        }

        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            app.logic(ui.ctx());
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

        // Manage prefs pop-out window lifecycle.
        if app.prefs_open && self.prefs.is_none() {
            self.prefs = Some(PrefsWindowState::new(
                event_loop,
                &gl_state.gl_display,
                &gl_state.gl_config,
                &gl_state.gl,
            ));
            gl_state.make_current();
        } else if !app.prefs_open && self.prefs.is_some() {
            if let Some(mut prefs) = self.prefs.take() {
                prefs.painter.destroy();
            }
            gl_state.make_current();
        }
    }
}

fn translate_key_event(event: &winit::event::KeyEvent, modifiers: winit::event::Modifiers) -> Option<egui::Event> {
    if !event.state.is_pressed() {
        return None;
    }
    let mods = winit_mods_to_egui(modifiers);
    if !mods.ctrl && !mods.alt {
        return None;
    }

    let key = match &event.key_without_modifiers() {
        Key::Character(c) => char_to_egui_key(c)?,
        Key::Named(named) => named_to_egui_key(*named)?,
        _ => return None,
    };

    Some(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: mods,
    })
}

fn winit_mods_to_egui(mods: winit::event::Modifiers) -> egui::Modifiers {
    let state = mods.state();
    egui::Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        shift: state.shift_key(),
        mac_cmd: state.super_key() && cfg!(target_os = "macos"),
        command: state.control_key() || (state.super_key() && cfg!(target_os = "macos")),
    }
}

fn char_to_egui_key(c: &str) -> Option<egui::Key> {
    let ch = c.chars().next()?;
    Some(match ch.to_ascii_lowercase() {
        'a' => egui::Key::A, 'b' => egui::Key::B, 'c' => egui::Key::C,
        'd' => egui::Key::D, 'e' => egui::Key::E, 'f' => egui::Key::F,
        'g' => egui::Key::G, 'h' => egui::Key::H, 'i' => egui::Key::I,
        'j' => egui::Key::J, 'k' => egui::Key::K, 'l' => egui::Key::L,
        'm' => egui::Key::M, 'n' => egui::Key::N, 'o' => egui::Key::O,
        'p' => egui::Key::P, 'q' => egui::Key::Q, 'r' => egui::Key::R,
        's' => egui::Key::S, 't' => egui::Key::T, 'u' => egui::Key::U,
        'v' => egui::Key::V, 'w' => egui::Key::W, 'x' => egui::Key::X,
        'y' => egui::Key::Y, 'z' => egui::Key::Z,
        '0' => egui::Key::Num0, '1' => egui::Key::Num1, '2' => egui::Key::Num2,
        '3' => egui::Key::Num3, '4' => egui::Key::Num4, '5' => egui::Key::Num5,
        '6' => egui::Key::Num6, '7' => egui::Key::Num7, '8' => egui::Key::Num8,
        '9' => egui::Key::Num9,
        '[' => egui::Key::OpenBracket, ']' => egui::Key::CloseBracket,
        '\\' => egui::Key::Backslash, ';' => egui::Key::Semicolon,
        '\'' => egui::Key::Quote, '`' => egui::Key::Backtick,
        ',' => egui::Key::Comma, '.' => egui::Key::Period,
        '/' => egui::Key::Slash, '-' => egui::Key::Minus, '=' => egui::Key::Equals,
        _ => return None,
    })
}

fn named_to_egui_key(named: NamedKey) -> Option<egui::Key> {
    Some(match named {
        NamedKey::Enter => egui::Key::Enter,
        NamedKey::Tab => egui::Key::Tab,
        NamedKey::Backspace => egui::Key::Backspace,
        NamedKey::Escape => egui::Key::Escape,
        NamedKey::Space => egui::Key::Space,
        NamedKey::Delete => egui::Key::Delete,
        NamedKey::Insert => egui::Key::Insert,
        NamedKey::Home => egui::Key::Home,
        NamedKey::End => egui::Key::End,
        NamedKey::PageUp => egui::Key::PageUp,
        NamedKey::PageDown => egui::Key::PageDown,
        NamedKey::ArrowUp => egui::Key::ArrowUp,
        NamedKey::ArrowDown => egui::Key::ArrowDown,
        NamedKey::ArrowLeft => egui::Key::ArrowLeft,
        NamedKey::ArrowRight => egui::Key::ArrowRight,
        NamedKey::F1 => egui::Key::F1, NamedKey::F2 => egui::Key::F2,
        NamedKey::F3 => egui::Key::F3, NamedKey::F4 => egui::Key::F4,
        NamedKey::F5 => egui::Key::F5, NamedKey::F6 => egui::Key::F6,
        NamedKey::F7 => egui::Key::F7, NamedKey::F8 => egui::Key::F8,
        NamedKey::F9 => egui::Key::F9, NamedKey::F10 => egui::Key::F10,
        NamedKey::F11 => egui::Key::F11, NamedKey::F12 => egui::Key::F12,
        _ => return None,
    })
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()?;
    let proxy = event_loop.create_proxy();
    let mut winit_app = WinitApp::new(proxy);
    event_loop.run_app(&mut winit_app)?;
    Ok(())
}
