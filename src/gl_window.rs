use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_glow::ShaderVersion;
use glutin::config::Config as GlConfig;
use glutin::context::{PossiblyCurrentContext, PossiblyCurrentGlContext as _};
use glutin::display::{Display, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::HasWindowHandle as _;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

pub(crate) struct GlWindow {
    pub(crate) window: Window,
    pub(crate) gl_surface: Surface<WindowSurface>,
    pub(crate) painter: egui_glow::Painter,
    pub(crate) egui_ctx: egui::Context,
    pub(crate) egui_winit: egui_winit::State,
    /// Mouse buttons currently held, mirrored from `MouseInput`. While one is
    /// held the window has the implicit pointer grab (see `on_window_event`).
    held_mouse_buttons: HashSet<MouseButton>,
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
            held_mouse_buttons: HashSet::new(),
        }
    }

    /// Feed a winit event to egui, keeping the implicit pointer grab a
    /// toolkit would keep. While a mouse button is held, X11 still delivers
    /// the pointer's motion and release to this window after it crosses the
    /// border (the implicit grab) but also sends `LeaveNotify` at the
    /// crossing; egui-winit turns that into `PointerGone`, which drops egui's
    /// click/drag interest in the pressed widget and makes it discard a
    /// release that arrives with no motion in between. GTK/VTE ignore the
    /// crossing and finish the drag (a selection, a divider) on the release
    /// wherever it happens, so a `CursorLeft` during a held button is not
    /// forwarded. The leave that ends the grab arrives after the release,
    /// with nothing held, and is forwarded as usual. Wayland sends no leave
    /// during a grab; a button held from outside the window is never seen
    /// pressed, so its crossings are forwarded unchanged.
    pub(crate) fn on_window_event(&mut self, event: &WindowEvent) -> egui_winit::EventResponse {
        if withheld_by_grab(&mut self.held_mouse_buttons, event) {
            return egui_winit::EventResponse { consumed: false, repaint: false };
        }
        self.egui_winit.on_window_event(&self.window, event)
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

/// The implicit-grab filter behind `GlWindow::on_window_event`: tracks the
/// held buttons through `MouseInput` and says whether `event` is a
/// `CursorLeft` to withhold from egui. Pure so the sequence is testable.
fn withheld_by_grab(held: &mut HashSet<MouseButton>, event: &WindowEvent) -> bool {
    match event {
        WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
            held.insert(*button);
        }
        WindowEvent::MouseInput { state: ElementState::Released, button, .. } => {
            held.remove(button);
        }
        WindowEvent::CursorLeft { .. } => return !held.is_empty(),
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::DeviceId;

    fn button(state: ElementState, button: MouseButton) -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button }
    }

    fn left() -> WindowEvent {
        WindowEvent::CursorLeft { device_id: DeviceId::dummy() }
    }

    #[test]
    fn cursor_left_withheld_only_while_a_button_is_held() {
        // R-012: the border crossing during a drag (X11 LeaveNotify under the
        // implicit grab) must not reach egui as PointerGone; the leave that
        // ends the grab, after the release, must.
        let mut held = HashSet::new();
        assert!(!withheld_by_grab(&mut held, &left()));
        assert!(!withheld_by_grab(&mut held, &button(ElementState::Pressed, MouseButton::Left)));
        assert!(withheld_by_grab(&mut held, &left()));
        assert!(!withheld_by_grab(&mut held, &button(ElementState::Released, MouseButton::Left)));
        assert!(!withheld_by_grab(&mut held, &left()));
    }

    #[test]
    fn grab_lasts_until_every_held_button_is_released() {
        let mut held = HashSet::new();
        withheld_by_grab(&mut held, &button(ElementState::Pressed, MouseButton::Left));
        withheld_by_grab(&mut held, &button(ElementState::Pressed, MouseButton::Middle));
        withheld_by_grab(&mut held, &button(ElementState::Released, MouseButton::Left));
        assert!(withheld_by_grab(&mut held, &left()));
        withheld_by_grab(&mut held, &button(ElementState::Released, MouseButton::Middle));
        assert!(!withheld_by_grab(&mut held, &left()));
    }

    #[test]
    fn button_held_from_outside_does_not_grab() {
        // A button pressed in another window and released over ours was never
        // seen pressed: its crossings are forwarded unchanged.
        let mut held = HashSet::new();
        assert!(!withheld_by_grab(&mut held, &button(ElementState::Released, MouseButton::Left)));
        assert!(!withheld_by_grab(&mut held, &left()));
    }
}
