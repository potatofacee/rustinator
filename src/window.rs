use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;

use egui_glow::ShaderVersion;
use glow::HasContext as _;
use glutin::config::{ConfigTemplateBuilder, GlConfig as _};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
};
use glutin::display::{Display, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
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
        let config_template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_transparency(true);

        let gl_config = unsafe {
            display
                .find_configs(config_template.build())
                .expect("failed to find GL config")
                .next()
                .expect("no GL configs available")
        };

        log::info!(
            "GL config: alpha_size={}, transparency={:?}",
            gl_config.alpha_size(),
            gl_config.supports_transparency(),
        );

        let window = glutin_winit::finalize_window(event_loop, window_attrs, &gl_config)
            .expect("failed to create window");

        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());

        let gl_context_attrs = ContextAttributesBuilder::new().build(raw_window_handle);

        let gl_context = unsafe {
            display
                .create_context(&gl_config, &gl_context_attrs)
                .or_else(|_| {
                    let attrs = ContextAttributesBuilder::new()
                        .with_context_api(ContextApi::Gles(None))
                        .build(raw_window_handle);
                    display.create_context(&gl_config, &attrs)
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
                .create_window_surface(&gl_config, &surface_attrs)
                .expect("failed to create GL surface")
        };

        let gl_context = gl_context
            .make_current(&gl_surface)
            .expect("failed to make GL context current");

        let gl = unsafe {
            Arc::new(glow::Context::from_loader_function(|name| {
                let cname = CString::new(name).unwrap();
                display.get_proc_address(&cname)
            }))
        };

        let alpha_bits = unsafe { gl.get_parameter_i32(glow::ALPHA_BITS) };
        log::info!("GL framebuffer alpha bits: {alpha_bits}");
        if alpha_bits == 0 {
            log::warn!("framebuffer has no alpha — transparency will not work");
        }

        Self {
            gl_context,
            gl_surface,
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

struct WinitApp {
    gl_state: Option<GlState>,
    egui_ctx: egui::Context,
    egui_winit: Option<egui_winit::State>,
    painter: Option<egui_glow::Painter>,
    app: Option<App>,
    event_loop_proxy: EventLoopProxy<UserEvent>,
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
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gl_state) = &self.gl_state else { return };
        let Some(egui_winit) = &mut self.egui_winit else { return };
        let Some(app) = &mut self.app else { return };

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
        if let Some(gl_state) = &self.gl_state {
            gl_state.window.request_redraw();
        }
    }
}

impl WinitApp {
    fn paint(&mut self, event_loop: &ActiveEventLoop) {
        let gl_state = self.gl_state.as_ref().unwrap();
        let painter = self.painter.as_mut().unwrap();
        let egui_winit = self.egui_winit.as_mut().unwrap();
        let app = self.app.as_mut().unwrap();

        let raw_input = egui_winit.take_egui_input(&gl_state.window);
        let clear_color = app.clear_color();

        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });

        egui_winit.handle_platform_output(&gl_state.window, full_output.platform_output);

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
