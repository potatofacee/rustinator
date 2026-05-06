use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;

use glow::HasContext as _;
use glutin::config::{Config as GlConfig, ConfigTemplateBuilder, GlConfig as _};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
    PossiblyCurrentGlContext as _,
};
use glutin::display::{Display, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::HasWindowHandle as _;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

pub(crate) struct GlState {
    pub(crate) gl_context: PossiblyCurrentContext,
    pub(crate) gl_surface: Surface<WindowSurface>,
    pub(crate) gl_display: Display,
    pub(crate) gl_config: GlConfig,
    pub(crate) window: Window,
    pub(crate) gl: Arc<glow::Context>,
}

impl GlState {
    pub(crate) fn new(event_loop: &ActiveEventLoop) -> Self {
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

        gl_surface
            .set_swap_interval(&gl_context, glutin::surface::SwapInterval::Wait(NonZeroU32::new(1).unwrap()))
            .unwrap_or_else(|e| log::warn!("failed to set vsync: {e}"));

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

    pub(crate) fn swap_buffers(&self) {
        self.gl_surface.swap_buffers(&self.gl_context).ok();
    }

    pub(crate) fn resize(&self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.gl_surface.resize(&self.gl_context, w, h);
        }
    }

    pub(crate) fn make_current(&self) {
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
    use raw_window_handle::HasDisplayHandle as _;

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
