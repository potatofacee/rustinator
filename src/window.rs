use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

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
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use crate::{App, RawTermKey};

#[derive(Debug, Clone)]
pub enum UserEvent {
    Repaint,
    HotkeyTogglePressed,
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
        main_context: &PossiblyCurrentContext,
    ) -> Self {
        let window_attrs = WindowAttributes::default()
            .with_title("Rustinator — Preferences")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0f32, 460.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(520.0f32, 360.0));

        let window = glutin_winit::finalize_window(event_loop, window_attrs, gl_config)
            .expect("failed to create prefs window");

        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle.expect("window handle required for surface"),
            NonZeroU32::new(window.inner_size().width.max(1)).unwrap(),
            NonZeroU32::new(window.inner_size().height.max(1)).unwrap(),
        );
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(gl_config, &surface_attrs)
                .expect("failed to create prefs surface")
        };

        main_context
            .make_current(&gl_surface)
            .expect("failed to make main context current on prefs surface");

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

        let phys = window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) {
            gl_surface.resize(main_context, w, h);
        }

        Self {
            window,
            gl_surface,
            painter,
            egui_ctx,
            egui_winit,
        }
    }

    fn destroy(mut self, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
        self.painter.destroy();
        drop(self.gl_surface);
        drop(self.window);
    }

    fn swap_buffers(&self, main_context: &PossiblyCurrentContext) {
        self.gl_surface.swap_buffers(main_context).ok();
    }

    fn resize(&self, main_context: &PossiblyCurrentContext, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(main_context, w, h);
        }
    }

    fn paint(&mut self, app: &mut App, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
        let phys = self.window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) {
            self.gl_surface.resize(main_context, w, h);
        }

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
        let pixels_per_point = egui_winit::pixels_per_point(&self.egui_ctx, &self.window);

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

        self.swap_buffers(main_context);
    }
}

// --- Main application handler ---

// --- Hotkey dropdown window (secondary OS window with its own terminal) ---

struct HotkeyWindowState {
    window: Window,
    gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
    egui_ctx: egui::Context,
    egui_winit: egui_winit::State,
    app: App,
    pending_keys: Vec<egui::Event>,
    current_modifiers: winit::event::Modifiers,
    zoom_pixel_accumulator: f64,
    shown_at: Option<Instant>,
}

impl HotkeyWindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        gl_display: &Display,
        gl_config: &GlConfig,
        gl: &Arc<glow::Context>,
        main_context: &PossiblyCurrentContext,
        proxy: EventLoopProxy<UserEvent>,
        height_pct: u32,
        always_on_top: bool,
    ) -> Option<Self> {
        let window_attrs = WindowAttributes::default()
            .with_title("rustinator")
            .with_decorations(false)
            .with_visible(false)
            .with_transparent(true);

        let window = glutin_winit::finalize_window(event_loop, window_attrs, gl_config)
            .expect("failed to create hotkey window");

        if always_on_top {
            window.set_window_level(WindowLevel::AlwaysOnTop);
        }
        apply_hotkey_geometry(&window, height_pct);

        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle.expect("window handle required for surface"),
            NonZeroU32::new(window.inner_size().width.max(1)).unwrap(),
            NonZeroU32::new(window.inner_size().height.max(1)).unwrap(),
        );
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(gl_config, &surface_attrs)
                .expect("failed to create hotkey surface")
        };

        main_context
            .make_current(&gl_surface)
            .expect("failed to make context current on hotkey surface");

        let painter = egui_glow::Painter::new(
            Arc::clone(gl),
            "",
            Some(ShaderVersion::get(gl)),
            true,
        )
        .expect("failed to create hotkey painter");

        let egui_ctx = egui::Context::default();

        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::from_hash_of("hotkey"),
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            Some(painter.max_texture_side()),
        );

        let phys = window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) {
            gl_surface.resize(main_context, w, h);
        }

        let app = match App::new(
            Arc::clone(gl),
            egui_ctx.clone(),
            proxy,
            window.scale_factor() as f32,
        ) {
            Ok(app) => app,
            Err(e) => {
                log::warn!("hotkey window: failed to create app: {e}");
                return None;
            }
        };

        Some(Self {
            window,
            gl_surface,
            painter,
            egui_ctx,
            egui_winit,
            app,
            pending_keys: Vec::new(),
            current_modifiers: winit::event::Modifiers::default(),
            zoom_pixel_accumulator: 0.0,
            shown_at: None,
        })
    }

    fn destroy(mut self, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
        self.painter.destroy();
        drop(self.gl_surface);
        drop(self.window);
    }

    fn resize(&self, main_context: &PossiblyCurrentContext, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(main_context, w, h);
        }
    }

    fn paint(&mut self, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
        let phys = self.window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) {
            self.gl_surface.resize(main_context, w, h);
        }

        let mut raw_input = self.egui_winit.take_egui_input(&self.window);
        raw_input.events.append(&mut self.pending_keys);

        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            self.app.logic(ui.ctx());
            self.app.ui(ui);
        });

        self.egui_winit
            .handle_platform_output(&self.window, full_output.platform_output);

        if self.app.fullscreen_pending {
            self.app.fullscreen_pending = false;
        }

        if let Some(vp_out) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
            for cmd in &vp_out.commands {
                if let egui::ViewportCommand::Title(t) = cmd {
                    self.window.set_title(t);
                }
            }
        }

        let screen_size: [u32; 2] = self.window.inner_size().into();
        let pixels_per_point = egui_winit::pixels_per_point(&self.egui_ctx, &self.window);

        let clipped_primitives = self.egui_ctx.tessellate(full_output.shapes, pixels_per_point);

        let profile = self.app.user_config.active();
        let [r, g, b] = profile.background_rgb();
        let opacity = profile.transparency.opacity;
        let clear_color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            opacity,
        ];

        self.painter.clear(screen_size, clear_color);
        self.painter.paint_and_update_textures(
            screen_size,
            pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        );

        self.gl_surface.swap_buffers(main_context).ok();
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
    zoom_pixel_accumulator: f64,
    hotkey_handle: Option<crate::hotkey::HotkeyHandle>,
    hotkey_window: Option<HotkeyWindowState>,
    hotkey_height_pct: u32,
    hotkey_hide_on_focus_loss: bool,
    hotkey_previous_app_pid: Option<i32>,
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
            zoom_pixel_accumulator: 0.0,
            hotkey_handle: None,
            hotkey_window: None,
            hotkey_height_pct: 50,
            hotkey_hide_on_focus_loss: true,
            hotkey_previous_app_pid: None,
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
                self.hotkey_window = HotkeyWindowState::new(
                    event_loop,
                    &gl_state.gl_display,
                    &gl_state.gl_config,
                    &gl_state.gl,
                    &gl_state.gl_context,
                    self.event_loop_proxy.clone(),
                    self.hotkey_height_pct,
                    hk_cfg.always_on_top,
                );
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
                if let Some(gl_state) = &self.gl_state {
                    gl_state.window.request_redraw();
                }
            }
            UserEvent::HotkeyTogglePressed => {
                if let Some(hk) = &mut self.hotkey_window {
                    if hk.window.is_visible().unwrap_or(true) {
                        hk.shown_at = None;
                        hk.window.set_visible(false);
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
                        apply_hotkey_geometry(&hk.window, self.hotkey_height_pct);
                        hk.shown_at = Some(Instant::now());
                        hk.window.set_visible(true);
                        hk.window.focus_window();
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
        let is_prefs_window = self.prefs.as_ref().is_some_and(|p| window_id == p.window.id());
        if is_prefs_window {
            // Temporarily take prefs out of self so we can borrow other fields.
            let mut prefs = self.prefs.take().unwrap();
            let response = prefs.egui_winit.on_window_event(&prefs.window, &event);
            if response.repaint {
                prefs.window.request_redraw();
            }
            let consumed = response.consumed;
            if !consumed {
                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(app) = &mut self.app {
                            app.prefs_open = false;
                        }
                    }
                    WindowEvent::Resized(size) => {
                        if let Some(gl_state) = &self.gl_state {
                            prefs.resize(&gl_state.gl_context, size.width, size.height);
                        }
                        prefs.window.request_redraw();
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
            .is_some_and(|h| window_id == h.window.id());
        if is_hotkey_window {
            let mut hk = self.hotkey_window.take().unwrap();

            if let WindowEvent::ModifiersChanged(mods) = &event {
                hk.current_modifiers = *mods;
            }

            if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
                if key_event.state.is_pressed()
                    && key_event.logical_key == Key::Named(NamedKey::Escape)
                    && hk.current_modifiers.state().is_empty()
                {
                    hk.window.set_visible(false);
                    #[cfg(target_os = "macos")]
                    if let Some(pid) = self.hotkey_previous_app_pid.take() {
                        macos_activate_pid(pid);
                    }
                    self.hotkey_window = Some(hk);
                    return;
                }
                if let Some(egui_ev) = translate_key_event(key_event, hk.current_modifiers) {
                    hk.pending_keys.push(egui_ev);
                }
                if let Some(raw) = encode_raw_key(key_event, hk.current_modifiers) {
                    hk.app.pending_raw_keys.push(raw);
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
                                hk.app.pending_zoom_steps += direction;
                            }
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            hk.zoom_pixel_accumulator += pos.y;
                            const THRESHOLD: f64 = 30.0;
                            while hk.zoom_pixel_accumulator >= THRESHOLD {
                                hk.app.pending_zoom_steps += 1;
                                hk.zoom_pixel_accumulator -= THRESHOLD;
                            }
                            while hk.zoom_pixel_accumulator <= -THRESHOLD {
                                hk.app.pending_zoom_steps -= 1;
                                hk.zoom_pixel_accumulator += THRESHOLD;
                            }
                        }
                    }
                    self.hotkey_window = Some(hk);
                    return;
                }
            }

            let response = hk.egui_winit.on_window_event(&hk.window, &event);
            if response.repaint {
                hk.window.request_redraw();
            }
            let consumed = response.consumed;
            if !consumed {
                match event {
                    WindowEvent::CloseRequested => {
                        hk.window.set_visible(false);
                    }
                    WindowEvent::Resized(size) => {
                        if let Some(gl_state) = &self.gl_state {
                            hk.resize(&gl_state.gl_context, size.width, size.height);
                        }
                        hk.window.request_redraw();
                    }
                    WindowEvent::Focused(focused) => {
                        hk.app.notify_focus(focused);
                        if !focused && self.hotkey_hide_on_focus_loss {
                            let dominated_by_grace = hk.shown_at
                                .is_some_and(|t| t.elapsed().as_millis() < 500);
                            if !dominated_by_grace {
                                hk.shown_at = None;
                                hk.window.set_visible(false);
                                #[cfg(target_os = "macos")]
                                if let Some(pid) = self.hotkey_previous_app_pid.take() {
                                    macos_activate_pid(pid);
                                }
                            }
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        if let Some(gl_state) = &self.gl_state {
                            hk.paint(&gl_state.gl_context);
                            gl_state.make_current();
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
            if let Some(egui_ev) = translate_key_event(key_event, self.current_modifiers) {
                self.pending_keys.push(egui_ev);
            }
            if let Some(raw) = encode_raw_key(key_event, self.current_modifiers) {
                app.pending_raw_keys.push(raw);
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
            WindowEvent::Focused(focused) => {
                app.notify_focus(focused);
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
        if let Some(hk) = &self.hotkey_window {
            if hk.window.is_visible().unwrap_or(false) {
                hk.window.request_redraw();
            }
        }
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
                    self.hotkey_window = HotkeyWindowState::new(
                        event_loop,
                        &gl_state.gl_display,
                        &gl_state.gl_config,
                        &gl_state.gl,
                        &gl_state.gl_context,
                        self.event_loop_proxy.clone(),
                        self.hotkey_height_pct,
                        hk_cfg.always_on_top,
                    );
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
                &gl_state.gl_context,
            ));
            gl_state.make_current();
        } else if !app.prefs_open && self.prefs.is_some() {
            if let Some(prefs) = self.prefs.take() {
                prefs.destroy(&gl_state.gl_context);
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

/// Encode a winit key event into legacy terminal bytes, using winit's
/// `text_with_all_modifiers()` for printable characters and a table for
/// functional keys. This runs at capture time so the lossy egui conversion
/// doesn't discard modifier information.
fn encode_raw_key(event: &winit::event::KeyEvent, modifiers: winit::event::Modifiers) -> Option<RawTermKey> {
    if !event.state.is_pressed() {
        return None;
    }
    let mods = winit_mods_to_egui(modifiers);
    let state = modifiers.state();
    let alt = state.alt_key();
    let ctrl = state.control_key();
    let shift = state.shift_key();

    // Map to egui::Key for the binding-matching side.
    let egui_key = match &event.key_without_modifiers() {
        Key::Character(c) => char_to_egui_key(c),
        Key::Named(named) => named_to_egui_key(*named),
        _ => None,
    };

    // --- Functional keys (no text representation) ---
    // These always need a lookup table regardless of modifiers.
    if let Key::Named(named) = &event.logical_key {
        if let Some(bytes) = encode_named_key(*named, shift, alt, ctrl) {
            return Some(RawTermKey {
                key: egui_key.unwrap_or(egui::Key::Escape),
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    // --- Printable characters: use text_with_all_modifiers ---
    // This gives us the OS-level result of the keypress with all modifiers
    // applied (e.g. Ctrl+A → 0x01, Shift+A → "A").
    if let Some(text) = event.text_with_all_modifiers() {
        if !text.is_empty() {
            let egui_key = egui_key?;
            let mut bytes = text.as_bytes().to_vec();
            if alt {
                bytes.insert(0, 0x1b);
            }
            return Some(RawTermKey {
                key: egui_key,
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    // Fallback for Ctrl+punctuation that winit may not produce text for.
    // These are standard xterm control character mappings.
    if ctrl {
        let unmod = event.key_without_modifiers();
        let ctrl_byte: Option<u8> = match &unmod {
            Key::Character(c) => match c.as_ref() {
                "[" => Some(0x1b), // ESC
                "\\" => Some(0x1c),
                "]" => Some(0x1d),
                "^" | "6" => Some(0x1e),
                "_" | "-" => Some(0x1f),
                "2" | "@" | " " => Some(0x00), // NUL
                _ => None,
            },
            _ => None,
        };
        if let Some(b) = ctrl_byte {
            let egui_key = egui_key?;
            let mut bytes = vec![b];
            if alt {
                bytes.insert(0, 0x1b);
            }
            return Some(RawTermKey {
                key: egui_key,
                mods,
                legacy_bytes: bytes,
            });
        }
    }

    None
}

fn legacy_mod_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    let mut m: u8 = 0;
    if shift { m |= 1; }
    if alt { m |= 2; }
    if ctrl { m |= 4; }
    1 + m
}

fn encode_named_key(named: NamedKey, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    let has_mods = shift || alt || ctrl;

    // Shift+Tab is the standard backtab sequence.
    if named == NamedKey::Tab && shift && !alt && !ctrl {
        return Some(b"\x1b[Z".to_vec());
    }

    // Simple named keys (no modifier encoding in legacy).
    if !has_mods {
        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f",
            NamedKey::Tab => b"\t",
            NamedKey::Escape => b"\x1b",
            NamedKey::Space => b" ",
            _ => &[],
        };
        if !seq.is_empty() {
            return Some(seq.to_vec());
        }
    }

    // Alt+Enter, Alt+Backspace, etc. — ESC prefix.
    if alt && !ctrl {
        let base: Option<u8> = match named {
            NamedKey::Enter => Some(b'\r'),
            NamedKey::Backspace => Some(0x7f),
            NamedKey::Space => Some(b' '),
            _ => None,
        };
        if let Some(b) = base {
            return Some(vec![0x1b, b]);
        }
    }

    // CSI <final> keys — modified form is CSI 1;{mod} <final>.
    let final_byte: Option<u8> = match named {
        NamedKey::ArrowUp => Some(b'A'),
        NamedKey::ArrowDown => Some(b'B'),
        NamedKey::ArrowRight => Some(b'C'),
        NamedKey::ArrowLeft => Some(b'D'),
        NamedKey::Home => Some(b'H'),
        NamedKey::End => Some(b'F'),
        _ => None,
    };
    if let Some(fb) = final_byte {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(vec![0x1b, b'[', fb])
        };
    }

    // CSI <num> ~ keys — modified form is CSI <num>;{mod} ~.
    let tilde_num: Option<u8> = match named {
        NamedKey::Insert => Some(2),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(num) = tilde_num {
        return if has_mods {
            Some(format!("\x1b[{};{}~", num, legacy_mod_param(shift, alt, ctrl)).into_bytes())
        } else {
            Some(format!("\x1b[{}~", num).into_bytes())
        };
    }

    // F-keys: F1-F4 use SS3, F5-F12 use CSI <num> ~.
    let fkey: Option<(&[u8], u8)> = match named {
        NamedKey::F1 => Some((b"\x1bOP", b'P')),
        NamedKey::F2 => Some((b"\x1bOQ", b'Q')),
        NamedKey::F3 => Some((b"\x1bOR", b'R')),
        NamedKey::F4 => Some((b"\x1bOS", b'S')),
        _ => None,
    };
    if let Some((unmod, fb)) = fkey {
        return if has_mods {
            Some(format!("\x1b[1;{}{}", legacy_mod_param(shift, alt, ctrl), fb as char).into_bytes())
        } else {
            Some(unmod.to_vec())
        };
    }
    let fkey_tilde: Option<u8> = match named {
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        _ => None,
    };
    if let Some(num) = fkey_tilde {
        return if has_mods {
            Some(format!("\x1b[{};{}~", num, legacy_mod_param(shift, alt, ctrl)).into_bytes())
        } else {
            Some(format!("\x1b[{}~", num).into_bytes())
        };
    }

    None
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- legacy_mod_param ----

    #[test]
    fn mod_param_no_mods() {
        assert_eq!(legacy_mod_param(false, false, false), 1);
    }

    #[test]
    fn mod_param_shift() {
        assert_eq!(legacy_mod_param(true, false, false), 2);
    }

    #[test]
    fn mod_param_alt() {
        assert_eq!(legacy_mod_param(false, true, false), 3);
    }

    #[test]
    fn mod_param_ctrl() {
        assert_eq!(legacy_mod_param(false, false, true), 5);
    }

    #[test]
    fn mod_param_shift_alt_ctrl() {
        assert_eq!(legacy_mod_param(true, true, true), 8);
    }

    // ---- encode_named_key: simple unmodified ----

    #[test]
    fn enter_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, false, false).unwrap(), b"\r");
    }

    #[test]
    fn backspace_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, false, false).unwrap(), b"\x7f");
    }

    #[test]
    fn tab_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, false, false).unwrap(), b"\t");
    }

    #[test]
    fn escape_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Escape, false, false, false).unwrap(), b"\x1b");
    }

    #[test]
    fn space_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Space, false, false, false).unwrap(), b" ");
    }

    // ---- Shift+Tab (backtab) ----

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(encode_named_key(NamedKey::Tab, true, false, false).unwrap(), b"\x1b[Z");
    }

    // ---- Alt+key (ESC prefix) ----

    #[test]
    fn alt_enter() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, true, false).unwrap(), b"\x1b\r");
    }

    #[test]
    fn alt_backspace() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, true, false).unwrap(), b"\x1b\x7f");
    }

    #[test]
    fn alt_space() {
        assert_eq!(encode_named_key(NamedKey::Space, false, true, false).unwrap(), b"\x1b ");
    }

    // ---- Arrows unmodified ----

    #[test]
    fn arrow_up() {
        assert_eq!(encode_named_key(NamedKey::ArrowUp, false, false, false).unwrap(), b"\x1b[A");
    }

    #[test]
    fn arrow_down() {
        assert_eq!(encode_named_key(NamedKey::ArrowDown, false, false, false).unwrap(), b"\x1b[B");
    }

    #[test]
    fn arrow_right() {
        assert_eq!(encode_named_key(NamedKey::ArrowRight, false, false, false).unwrap(), b"\x1b[C");
    }

    #[test]
    fn arrow_left() {
        assert_eq!(encode_named_key(NamedKey::ArrowLeft, false, false, false).unwrap(), b"\x1b[D");
    }

    #[test]
    fn home_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Home, false, false, false).unwrap(), b"\x1b[H");
    }

    #[test]
    fn end_unmodified() {
        assert_eq!(encode_named_key(NamedKey::End, false, false, false).unwrap(), b"\x1b[F");
    }

    // ---- Arrows with modifiers ----

    #[test]
    fn ctrl_arrow_up() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowUp, false, false, true).unwrap(),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn shift_arrow_left() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowLeft, true, false, false).unwrap(),
            b"\x1b[1;2D"
        );
    }

    #[test]
    fn alt_shift_arrow_right() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowRight, true, true, false).unwrap(),
            b"\x1b[1;4C"
        );
    }

    // ---- Tilde keys ----

    #[test]
    fn insert_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Insert, false, false, false).unwrap(), b"\x1b[2~");
    }

    #[test]
    fn delete_unmodified() {
        assert_eq!(encode_named_key(NamedKey::Delete, false, false, false).unwrap(), b"\x1b[3~");
    }

    #[test]
    fn page_up_unmodified() {
        assert_eq!(encode_named_key(NamedKey::PageUp, false, false, false).unwrap(), b"\x1b[5~");
    }

    #[test]
    fn page_down_unmodified() {
        assert_eq!(encode_named_key(NamedKey::PageDown, false, false, false).unwrap(), b"\x1b[6~");
    }

    #[test]
    fn ctrl_delete() {
        assert_eq!(
            encode_named_key(NamedKey::Delete, false, false, true).unwrap(),
            b"\x1b[3;5~"
        );
    }

    #[test]
    fn shift_page_up() {
        assert_eq!(
            encode_named_key(NamedKey::PageUp, true, false, false).unwrap(),
            b"\x1b[5;2~"
        );
    }

    // ---- F-keys: F1-F4 (SS3 unmodified, CSI modified) ----

    #[test]
    fn f1_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F1, false, false, false).unwrap(), b"\x1bOP");
    }

    #[test]
    fn f2_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F2, false, false, false).unwrap(), b"\x1bOQ");
    }

    #[test]
    fn f3_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F3, false, false, false).unwrap(), b"\x1bOR");
    }

    #[test]
    fn f4_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F4, false, false, false).unwrap(), b"\x1bOS");
    }

    #[test]
    fn shift_f1() {
        assert_eq!(
            encode_named_key(NamedKey::F1, true, false, false).unwrap(),
            b"\x1b[1;2P"
        );
    }

    #[test]
    fn ctrl_f3() {
        assert_eq!(
            encode_named_key(NamedKey::F3, false, false, true).unwrap(),
            b"\x1b[1;5R"
        );
    }

    // ---- F-keys: F5-F12 (CSI tilde) ----

    #[test]
    fn f5_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F5, false, false, false).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn f6_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F6, false, false, false).unwrap(), b"\x1b[17~");
    }

    #[test]
    fn f7_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F7, false, false, false).unwrap(), b"\x1b[18~");
    }

    #[test]
    fn f8_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F8, false, false, false).unwrap(), b"\x1b[19~");
    }

    #[test]
    fn f9_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F9, false, false, false).unwrap(), b"\x1b[20~");
    }

    #[test]
    fn f10_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F10, false, false, false).unwrap(), b"\x1b[21~");
    }

    #[test]
    fn f11_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F11, false, false, false).unwrap(), b"\x1b[23~");
    }

    #[test]
    fn f12_unmodified() {
        assert_eq!(encode_named_key(NamedKey::F12, false, false, false).unwrap(), b"\x1b[24~");
    }

    #[test]
    fn alt_f12() {
        assert_eq!(
            encode_named_key(NamedKey::F12, false, true, false).unwrap(),
            b"\x1b[24;3~"
        );
    }

    // ---- Unknown named key returns None ----

    #[test]
    fn unknown_named_key() {
        assert_eq!(encode_named_key(NamedKey::CapsLock, false, false, false), None);
    }

    // ---- char_to_egui_key ----

    #[test]
    fn char_to_key_letter() {
        assert_eq!(char_to_egui_key("a"), Some(egui::Key::A));
        assert_eq!(char_to_egui_key("A"), Some(egui::Key::A));
    }

    #[test]
    fn char_to_key_digit() {
        assert_eq!(char_to_egui_key("5"), Some(egui::Key::Num5));
    }

    #[test]
    fn char_to_key_punctuation() {
        assert_eq!(char_to_egui_key("["), Some(egui::Key::OpenBracket));
        assert_eq!(char_to_egui_key("]"), Some(egui::Key::CloseBracket));
        assert_eq!(char_to_egui_key("/"), Some(egui::Key::Slash));
        assert_eq!(char_to_egui_key("-"), Some(egui::Key::Minus));
    }

    #[test]
    fn char_to_key_unknown() {
        assert_eq!(char_to_egui_key("€"), None);
    }

    // ---- named_to_egui_key ----

    #[test]
    fn named_to_egui_arrows() {
        assert_eq!(named_to_egui_key(NamedKey::ArrowUp), Some(egui::Key::ArrowUp));
        assert_eq!(named_to_egui_key(NamedKey::ArrowDown), Some(egui::Key::ArrowDown));
    }

    #[test]
    fn named_to_egui_fkeys() {
        assert_eq!(named_to_egui_key(NamedKey::F1), Some(egui::Key::F1));
        assert_eq!(named_to_egui_key(NamedKey::F12), Some(egui::Key::F12));
    }

    #[test]
    fn named_to_egui_unknown() {
        assert_eq!(named_to_egui_key(NamedKey::CapsLock), None);
    }

    // ---- legacy_mod_param: remaining combos ----

    #[test]
    fn mod_param_shift_alt() {
        assert_eq!(legacy_mod_param(true, true, false), 4);
    }

    #[test]
    fn mod_param_shift_ctrl() {
        assert_eq!(legacy_mod_param(true, false, true), 6);
    }

    #[test]
    fn mod_param_alt_ctrl() {
        assert_eq!(legacy_mod_param(false, true, true), 7);
    }

    // ---- encode_named_key: more modifier combos on arrows ----

    #[test]
    fn alt_arrow_up() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowUp, false, true, false).unwrap(),
            b"\x1b[1;3A"
        );
    }

    #[test]
    fn ctrl_alt_arrow_down() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowDown, false, true, true).unwrap(),
            b"\x1b[1;7B"
        );
    }

    #[test]
    fn shift_ctrl_arrow_right() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowRight, true, false, true).unwrap(),
            b"\x1b[1;6C"
        );
    }

    #[test]
    fn shift_alt_ctrl_arrow_left() {
        assert_eq!(
            encode_named_key(NamedKey::ArrowLeft, true, true, true).unwrap(),
            b"\x1b[1;8D"
        );
    }

    #[test]
    fn ctrl_home() {
        assert_eq!(
            encode_named_key(NamedKey::Home, false, false, true).unwrap(),
            b"\x1b[1;5H"
        );
    }

    #[test]
    fn shift_end() {
        assert_eq!(
            encode_named_key(NamedKey::End, true, false, false).unwrap(),
            b"\x1b[1;2F"
        );
    }

    // ---- encode_named_key: tilde keys with more modifiers ----

    #[test]
    fn alt_insert() {
        assert_eq!(
            encode_named_key(NamedKey::Insert, false, true, false).unwrap(),
            b"\x1b[2;3~"
        );
    }

    #[test]
    fn shift_ctrl_delete() {
        assert_eq!(
            encode_named_key(NamedKey::Delete, true, false, true).unwrap(),
            b"\x1b[3;6~"
        );
    }

    #[test]
    fn alt_page_down() {
        assert_eq!(
            encode_named_key(NamedKey::PageDown, false, true, false).unwrap(),
            b"\x1b[6;3~"
        );
    }

    // ---- encode_named_key: F-key modifier combos ----

    #[test]
    fn alt_f1() {
        assert_eq!(
            encode_named_key(NamedKey::F1, false, true, false).unwrap(),
            b"\x1b[1;3P"
        );
    }

    #[test]
    fn ctrl_alt_f4() {
        assert_eq!(
            encode_named_key(NamedKey::F4, false, true, true).unwrap(),
            b"\x1b[1;7S"
        );
    }

    #[test]
    fn shift_f5() {
        assert_eq!(
            encode_named_key(NamedKey::F5, true, false, false).unwrap(),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn ctrl_f9() {
        assert_eq!(
            encode_named_key(NamedKey::F9, false, false, true).unwrap(),
            b"\x1b[20;5~"
        );
    }

    #[test]
    fn shift_alt_ctrl_f12() {
        assert_eq!(
            encode_named_key(NamedKey::F12, true, true, true).unwrap(),
            b"\x1b[24;8~"
        );
    }

    // ---- encode_named_key: Tab/Enter/Backspace with modifiers that don't match special paths ----

    #[test]
    fn ctrl_tab_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, false, true), None);
    }

    #[test]
    fn alt_tab_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Tab, false, true, false), None);
    }

    #[test]
    fn ctrl_enter_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Enter, false, false, true), None);
    }

    #[test]
    fn shift_enter_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Enter, true, false, false), None);
    }

    #[test]
    fn ctrl_backspace_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Backspace, false, false, true), None);
    }

    #[test]
    fn shift_escape_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Escape, true, false, false), None);
    }

    #[test]
    fn ctrl_space_returns_none() {
        assert_eq!(encode_named_key(NamedKey::Space, false, false, true), None);
    }

    // ---- char_to_egui_key: all punctuation ----

    #[test]
    fn char_to_key_all_punct() {
        assert_eq!(char_to_egui_key("\\"), Some(egui::Key::Backslash));
        assert_eq!(char_to_egui_key(";"), Some(egui::Key::Semicolon));
        assert_eq!(char_to_egui_key("'"), Some(egui::Key::Quote));
        assert_eq!(char_to_egui_key("`"), Some(egui::Key::Backtick));
        assert_eq!(char_to_egui_key(","), Some(egui::Key::Comma));
        assert_eq!(char_to_egui_key("."), Some(egui::Key::Period));
        assert_eq!(char_to_egui_key("="), Some(egui::Key::Equals));
    }

    #[test]
    fn char_to_key_empty_string() {
        assert_eq!(char_to_egui_key(""), None);
    }

    #[test]
    fn char_to_key_multi_char_uses_first() {
        assert_eq!(char_to_egui_key("abc"), Some(egui::Key::A));
    }

    // ---- named_to_egui_key: spot checks ----

    #[test]
    fn named_to_egui_enter_tab_space() {
        assert_eq!(named_to_egui_key(NamedKey::Enter), Some(egui::Key::Enter));
        assert_eq!(named_to_egui_key(NamedKey::Tab), Some(egui::Key::Tab));
        assert_eq!(named_to_egui_key(NamedKey::Space), Some(egui::Key::Space));
    }

    #[test]
    fn named_to_egui_navigation() {
        assert_eq!(named_to_egui_key(NamedKey::Home), Some(egui::Key::Home));
        assert_eq!(named_to_egui_key(NamedKey::End), Some(egui::Key::End));
        assert_eq!(named_to_egui_key(NamedKey::PageUp), Some(egui::Key::PageUp));
        assert_eq!(named_to_egui_key(NamedKey::PageDown), Some(egui::Key::PageDown));
        assert_eq!(named_to_egui_key(NamedKey::Insert), Some(egui::Key::Insert));
        assert_eq!(named_to_egui_key(NamedKey::Delete), Some(egui::Key::Delete));
    }
}

#[cfg(target_os = "macos")]
fn macos_screen_insets(window: &Window) -> Option<(f64, f64, f64, f64)> {
    use objc2::MainThreadMarker;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSScreen;

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
        let mtm = MainThreadMarker::new().expect("must be called from main thread");
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
fn macos_frontmost_pid() -> Option<i32> {
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
fn macos_activate_pid(pid: i32) {
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
fn macos_order_window_back(window: &Window) {
    use objc2::runtime::AnyObject;

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

fn apply_hotkey_geometry(window: &Window, height_pct: u32) {
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

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()?;
    let proxy = event_loop.create_proxy();
    let mut winit_app = WinitApp::new(proxy);
    event_loop.run_app(&mut winit_app)?;
    Ok(())
}
