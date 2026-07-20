use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_glow::ShaderVersion;
use glutin::config::Config as GlConfig;
use glutin::context::{PossiblyCurrentContext, PossiblyCurrentGlContext as _};
use glutin::display::{Display, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::HasWindowHandle as _;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

pub(crate) struct GlWindow {
    pub(crate) window: Window,
    pub(crate) gl_surface: Surface<WindowSurface>,
    pub(crate) painter: egui_glow::Painter,
    pub(crate) egui_ctx: egui::Context,
    pub(crate) egui_winit: egui_winit::State,
}

impl GlWindow {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        gl_display: &Display,
        gl_config: &GlConfig,
        gl: &Arc<glow::Context>,
        main_context: &PossiblyCurrentContext,
        window_attrs: WindowAttributes,
        viewport_id: egui::ViewportId,
        srgb_framebuffers: bool,
    ) -> Self {
        let window = glutin_winit::finalize_window(event_loop, window_attrs, gl_config)
            .expect("failed to create window");

        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle.expect("window handle required for surface"),
            NonZeroU32::new(window.inner_size().width.max(1)).unwrap(),
            NonZeroU32::new(window.inner_size().height.max(1)).unwrap(),
        );
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(gl_config, &surface_attrs)
                .expect("failed to create GL surface")
        };

        Self::new_primary(
            event_loop,
            window,
            gl_surface,
            gl,
            main_context,
            viewport_id,
            srgb_framebuffers,
        )
    }

    /// Assemble a `GlWindow` from an already-created `window` + `gl_surface`. The
    /// primary window's surface is created in `SharedGl::new` (via
    /// `create_context_and_surface`), so it is passed in here rather than created.
    /// This is also the shared tail of `new`, so secondary windows take the exact
    /// same painter/egui setup. `srgb_framebuffers` matches the per-window painter
    /// setting — the primary passes `true`, as the inline primary painter did.
    pub(crate) fn new_primary(
        event_loop: &ActiveEventLoop,
        window: Window,
        gl_surface: Surface<WindowSurface>,
        gl: &Arc<glow::Context>,
        main_context: &PossiblyCurrentContext,
        viewport_id: egui::ViewportId,
        srgb_framebuffers: bool,
    ) -> Self {
        main_context
            .make_current(&gl_surface)
            .expect("failed to make context current on surface");

        let painter = egui_glow::Painter::new(
            Arc::clone(gl),
            "",
            Some(ShaderVersion::get(gl)),
            srgb_framebuffers,
        )
        .expect("failed to create painter");

        let egui_ctx = egui::Context::default();
        // Software GL: disable egui animations. Hover fades and dialog
        // transitions repaint several frames for pure decoration; on a CPU
        // rasterizer instant transitions are cheaper and feel better over VNC.
        if crate::gl_setup::is_software_renderer(gl) {
            let mut style = (*egui_ctx.global_style()).clone();
            style.animation_time = 0.0;
            egui_ctx.set_global_style(style);
        }

        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
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

    pub(crate) fn destroy(mut self, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
        self.painter.destroy();
        drop(self.gl_surface);
        drop(self.window);
    }

    pub(crate) fn resize(&self, main_context: &PossiblyCurrentContext, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(main_context, w, h);
        }
    }

    /// Make the shared context current on this window's surface. Used to restore
    /// the primary as current after a secondary window painted/destroyed on its
    /// own surface — the role the former `GlState::make_current` filled.
    pub(crate) fn make_current(&self, main_context: &PossiblyCurrentContext) {
        main_context.make_current(&self.gl_surface).ok();
    }

    pub(crate) fn swap_buffers(&self, main_context: &PossiblyCurrentContext) {
        self.gl_surface.swap_buffers(main_context).ok();
    }

    pub(crate) fn begin_frame(&mut self, main_context: &PossiblyCurrentContext) -> egui::RawInput {
        main_context.make_current(&self.gl_surface).ok();
        let phys = self.window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) {
            self.gl_surface.resize(main_context, w, h);
        }
        self.egui_winit.take_egui_input(&self.window)
    }

    pub(crate) fn end_frame(
        &mut self,
        main_context: &PossiblyCurrentContext,
        full_output: egui::FullOutput,
        clear_color: [f32; 4],
    ) -> Option<Instant> {
        self.egui_winit
            .handle_platform_output(&self.window, full_output.platform_output);

        let mut repaint_at = None;
        if let Some(vp_out) = full_output.viewport_output.get(&self.egui_ctx.viewport_id()) {
            // Zero delay (continuous animation) becomes a deadline of "now";
            // the caller's scheduler decides the actual cadence. Requesting a
            // redraw here directly would repaint at swap rate, which is
            // unbounded on drivers without a vsync brake (software GL on VNC).
            if vp_out.repaint_delay < Duration::from_secs(86400) {
                repaint_at = Some(Instant::now() + vp_out.repaint_delay);
            }
        }

        let screen_size: [u32; 2] = self.window.inner_size().into();
        let pixels_per_point = egui_winit::pixels_per_point(&self.egui_ctx, &self.window);

        let clipped_primitives = self.egui_ctx.tessellate(full_output.shapes, pixels_per_point);

        self.painter.clear(screen_size, clear_color);
        self.painter.paint_and_update_textures(
            screen_size,
            pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        );

        self.swap_buffers(main_context);

        repaint_at
    }
}
