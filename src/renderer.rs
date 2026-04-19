use std::collections::HashMap;
use std::sync::Arc;

use crossfont::BitmapBuffer;
use glow::{self, HasContext};

use crate::font::{FontContext, FontStyle};

const ATLAS_SIZE: i32 = 1024;

// Unit quad (two triangles as a strip), (x,y) in [0,1].
const QUAD_VERTS: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];

/// One cell's bg rect (or a sub-cell rect like a beam/underline cursor).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct BgInstance {
    pub cell: [i32; 2], // col, row
    pub color: [f32; 4],
    pub offset_cells: [f32; 2], // sub-cell offset from the cell's top-left, in cell units
    pub size_cells: [f32; 2],   // extent in cell units (1.0 = full cell)
}

impl BgInstance {
    pub fn full(col: i32, row: i32, color: [f32; 4]) -> Self {
        Self {
            cell: [col, row],
            color,
            offset_cells: [0.0, 0.0],
            size_cells: [1.0, 1.0],
        }
    }
}

/// One cell's foreground glyph.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct GlyphInstance {
    pub cell: [i32; 2],
    pub color: [f32; 4],
    pub uv_rect: [f32; 4],   // u0,v0,u1,v1
    pub size_px: [f32; 2],   // glyph pixel size
    pub offset_px: [f32; 2], // glyph offset from cell top-left
}

#[derive(Copy, Clone)]
struct AtlasEntry {
    uv_rect: [f32; 4],
    size_px: [f32; 2],
    offset_px: [f32; 2],
    empty: bool, // space / missing glyph
}

pub struct Renderer {
    gl: Arc<glow::Context>,
    bg_program: glow::Program,
    glyph_program: glow::Program,
    vao: glow::VertexArray,
    _quad_vbo: glow::Buffer,
    bg_instance_vbo: glow::Buffer,
    glyph_instance_vbo: glow::Buffer,
    atlas_tex: glow::Texture,
    shelf_y: i32,
    shelf_cursor_x: i32,
    shelf_h: i32,
    glyph_cache: HashMap<(char, FontStyle), AtlasEntry>,

    // Uniform locations.
    bg_u_cell_size: glow::UniformLocation,
    bg_u_viewport: glow::UniformLocation,
    glyph_u_cell_size: glow::UniformLocation,
    glyph_u_viewport: glow::UniformLocation,
    glyph_u_atlas: glow::UniformLocation,

    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
}

impl Renderer {
    pub fn new(gl: Arc<glow::Context>, font: &FontContext) -> Self {
        unsafe {
            let bg_program = compile_program(&gl, BG_VERT, BG_FRAG, &[]);
            let glyph_program = compile_program(&gl, GLYPH_VERT, GLYPH_FRAG, &[]);

            // VAO layout:
            //   binding 0: quad_vbo         → location 0 (vec2 in_pos)
            //   binding 1: instance_vbo     → locations 1..N, divisor 1
            // We re-bind the instance VBO between the two passes because the
            // two instance layouts differ in stride and attribute count.
            let vao = gl.create_vertex_array().expect("vao");
            gl.bind_vertex_array(Some(vao));

            let quad_vbo = gl.create_buffer().expect("quad vbo");
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad_vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast_f32(&QUAD_VERTS),
                glow::STATIC_DRAW,
            );
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.enable_vertex_attrib_array(0);

            let bg_instance_vbo = gl.create_buffer().expect("bg instance vbo");
            let glyph_instance_vbo = gl.create_buffer().expect("glyph instance vbo");

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            // Atlas — RGBA8, starts cleared to 0.
            let atlas_tex = gl.create_texture().expect("atlas texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(atlas_tex));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let zero = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                ATLAS_SIZE,
                ATLAS_SIZE,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&zero)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            let bg_u_cell_size = gl.get_uniform_location(bg_program, "u_cell_size").unwrap();
            let bg_u_viewport = gl.get_uniform_location(bg_program, "u_viewport").unwrap();
            let glyph_u_cell_size = gl
                .get_uniform_location(glyph_program, "u_cell_size")
                .unwrap();
            let glyph_u_viewport = gl
                .get_uniform_location(glyph_program, "u_viewport")
                .unwrap();
            let glyph_u_atlas = gl.get_uniform_location(glyph_program, "u_atlas").unwrap();

            Self {
                gl,
                bg_program,
                glyph_program,
                vao,
                _quad_vbo: quad_vbo,
                bg_instance_vbo,
                glyph_instance_vbo,
                atlas_tex,
                shelf_y: 0,
                shelf_cursor_x: 0,
                shelf_h: 0,
                glyph_cache: HashMap::new(),
                bg_u_cell_size,
                bg_u_viewport,
                glyph_u_cell_size,
                glyph_u_viewport,
                glyph_u_atlas,
                cell_w: font.cell_width(),
                cell_h: font.cell_height(),
                // FreeType's descender is negative; baseline-from-top = line_height + descent.
                ascent: font.cell_height() + font.metrics.descent,
            }
        }
    }

    /// Look up a glyph, rasterizing + uploading to atlas on cache miss.
    fn get_or_insert_glyph(
        &mut self,
        c: char,
        style: FontStyle,
        font: &mut FontContext,
    ) -> AtlasEntry {
        let key = (c, style);
        if let Some(entry) = self.glyph_cache.get(&key) {
            return *entry;
        }

        let raster = match font.rasterize(c, style) {
            Ok(g) => g,
            Err(_) => {
                let empty = AtlasEntry {
                    uv_rect: [0.0; 4],
                    size_px: [0.0; 2],
                    offset_px: [0.0; 2],
                    empty: true,
                };
                self.glyph_cache.insert(key, empty);
                return empty;
            }
        };

        if raster.width <= 0 || raster.height <= 0 {
            let empty = AtlasEntry {
                uv_rect: [0.0; 4],
                size_px: [0.0; 2],
                offset_px: [0.0; 2],
                empty: true,
            };
            self.glyph_cache.insert(key, empty);
            return empty;
        }

        // Shelf allocator.
        let w = raster.width;
        let h = raster.height;
        if self.shelf_cursor_x + w > ATLAS_SIZE {
            self.shelf_y += self.shelf_h;
            self.shelf_cursor_x = 0;
            self.shelf_h = 0;
        }
        if self.shelf_y + h > ATLAS_SIZE {
            // Atlas full — for phase 2, clear it.
            self.reset_atlas();
        }
        if h > self.shelf_h {
            self.shelf_h = h;
        }
        let x = self.shelf_cursor_x;
        let y = self.shelf_y;
        self.shelf_cursor_x += w;

        // Normalize source buffer to RGBA8 with alpha = average of RGB.
        let rgba = to_rgba(&raster.buffer, w as usize, h as usize);
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas_tex));
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&rgba)),
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }

        let entry = AtlasEntry {
            uv_rect: [
                x as f32 / ATLAS_SIZE as f32,
                y as f32 / ATLAS_SIZE as f32,
                (x + w) as f32 / ATLAS_SIZE as f32,
                (y + h) as f32 / ATLAS_SIZE as f32,
            ],
            size_px: [w as f32, h as f32],
            // freetype top = bearing above baseline (+up). Our cell origin is
            // top-left of the cell; glyph's top-left in cell = ascent - top, left = left.
            offset_px: [raster.left as f32, self.ascent - raster.top as f32],
            empty: false,
        };
        self.glyph_cache.insert(key, entry);
        entry
    }

    /// Swap in a new font. Clears the atlas (all glyphs belong to the old size)
    /// and updates cell metrics. Call this when the user changes font family or size.
    pub fn reload_font(&mut self, font: &FontContext) {
        self.reset_atlas();
        self.cell_w = font.cell_width();
        self.cell_h = font.cell_height();
        self.ascent = font.cell_height() + font.metrics.descent;
    }

    fn reset_atlas(&mut self) {
        self.glyph_cache.clear();
        self.shelf_y = 0;
        self.shelf_cursor_x = 0;
        self.shelf_h = 0;
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas_tex));
            let zero = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                ATLAS_SIZE,
                ATLAS_SIZE,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&zero)),
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    pub fn paint(
        &mut self,
        viewport_px: (f32, f32),
        bg_instances: &[BgInstance],
        glyph_instances: &[GlyphInstance],
    ) {
        unsafe {
            let gl = self.gl.clone();
            gl.disable(glow::DEPTH_TEST);
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE, glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.bind_vertex_array(Some(self.vao));

            // ---- BG pass ----
            if !bg_instances.is_empty() {
                gl.use_program(Some(self.bg_program));
                gl.uniform_2_f32(Some(&self.bg_u_cell_size), self.cell_w, self.cell_h);
                gl.uniform_2_f32(Some(&self.bg_u_viewport), viewport_px.0, viewport_px.1);

                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.bg_instance_vbo));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck_cast_bg(bg_instances),
                    glow::STREAM_DRAW,
                );
                // Layout: ivec2 cell(0), vec4 color(8), vec2 offset(24), vec2 size(32). stride=40.
                let stride: i32 = 40;
                gl.vertex_attrib_pointer_i32(1, 2, glow::INT, stride, 0);
                gl.vertex_attrib_pointer_f32(2, 4, glow::FLOAT, false, stride, 8);
                gl.vertex_attrib_pointer_f32(3, 2, glow::FLOAT, false, stride, 24);
                gl.vertex_attrib_pointer_f32(4, 2, glow::FLOAT, false, stride, 32);
                for loc in 1..=4 {
                    gl.vertex_attrib_divisor(loc, 1);
                    gl.enable_vertex_attrib_array(loc);
                }

                gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, bg_instances.len() as i32);

                for loc in 1..=4 {
                    gl.disable_vertex_attrib_array(loc);
                }
            }

            // ---- Glyph pass ----
            if !glyph_instances.is_empty() {
                // Dual-source blending for subpixel AA.
                gl.blend_func_separate(
                    glow::SRC1_COLOR,
                    glow::ONE_MINUS_SRC1_COLOR,
                    glow::SRC1_ALPHA,
                    glow::ONE_MINUS_SRC1_ALPHA,
                );
                gl.use_program(Some(self.glyph_program));
                gl.uniform_2_f32(Some(&self.glyph_u_cell_size), self.cell_w, self.cell_h);
                gl.uniform_2_f32(Some(&self.glyph_u_viewport), viewport_px.0, viewport_px.1);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas_tex));
                gl.uniform_1_i32(Some(&self.glyph_u_atlas), 0);

                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.glyph_instance_vbo));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck_cast_glyph(glyph_instances),
                    glow::STREAM_DRAW,
                );
                // Layout: ivec2 cell(0), vec4 color(8), vec4 uv_rect(24),
                //         vec2 size_px(40), vec2 offset_px(48). stride=56.
                let stride: i32 = 56;
                gl.vertex_attrib_pointer_i32(1, 2, glow::INT, stride, 0);
                gl.vertex_attrib_pointer_f32(2, 4, glow::FLOAT, false, stride, 8);
                gl.vertex_attrib_pointer_f32(3, 4, glow::FLOAT, false, stride, 24);
                gl.vertex_attrib_pointer_f32(4, 2, glow::FLOAT, false, stride, 40);
                gl.vertex_attrib_pointer_f32(5, 2, glow::FLOAT, false, stride, 48);
                for loc in 1..=5 {
                    gl.vertex_attrib_divisor(loc, 1);
                    gl.enable_vertex_attrib_array(loc);
                }

                gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, glyph_instances.len() as i32);

                for loc in 1..=5 {
                    gl.disable_vertex_attrib_array(loc);
                }
                gl.bind_texture(glow::TEXTURE_2D, None);

                gl.blend_func_separate(
                    glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA,
                    glow::ONE, glow::ONE_MINUS_SRC_ALPHA,
                );
            }

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.use_program(None);
        }
    }

    /// Build a glyph instance for character `c` at `(col, row)` with color `fg`.
    /// Returns `None` for glyphs that should not emit a draw (e.g. spaces).
    pub fn build_glyph_instance(
        &mut self,
        c: char,
        style: FontStyle,
        col: i32,
        row: i32,
        fg: [f32; 4],
        font: &mut FontContext,
    ) -> Option<GlyphInstance> {
        let entry = self.get_or_insert_glyph(c, style, font);
        if entry.empty {
            return None;
        }
        Some(GlyphInstance {
            cell: [col, row],
            color: fg,
            uv_rect: entry.uv_rect,
            size_px: entry.size_px,
            offset_px: entry.offset_px,
        })
    }
}

fn to_rgba(buf: &BitmapBuffer, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    match buf {
        BitmapBuffer::Rgb(src) => {
            for i in 0..(w * h) {
                out[i * 4] = src[i * 3];
                out[i * 4 + 1] = src[i * 3 + 1];
                out[i * 4 + 2] = src[i * 3 + 2];
                out[i * 4 + 3] = 255;
            }
        }
        BitmapBuffer::Rgba(src) => {
            out.copy_from_slice(src);
        }
    }
    out
}

// Small local casts so we don't pull in bytemuck as a dep.
fn bytemuck_cast_f32(s: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}
fn bytemuck_cast_bg(s: &[BgInstance]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}
fn bytemuck_cast_glyph(s: &[GlyphInstance]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

unsafe fn compile_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
    frag_data_bindings: &[(&str, u32, u32)],
) -> glow::Program {
    unsafe {
        let program = gl.create_program().expect("program");
        let vert = gl.create_shader(glow::VERTEX_SHADER).expect("vert shader");
        gl.shader_source(vert, vert_src);
        gl.compile_shader(vert);
        if !gl.get_shader_compile_status(vert) {
            panic!("vert shader compile: {}", gl.get_shader_info_log(vert));
        }
        let frag = gl
            .create_shader(glow::FRAGMENT_SHADER)
            .expect("frag shader");
        gl.shader_source(frag, frag_src);
        gl.compile_shader(frag);
        if !gl.get_shader_compile_status(frag) {
            panic!("frag shader compile: {}", gl.get_shader_info_log(frag));
        }
        gl.attach_shader(program, vert);
        gl.attach_shader(program, frag);
        // Bind dual-source fragment outputs before linking.
        for (name, color, _) in frag_data_bindings {
            gl.bind_frag_data_location(program, *color, name);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            panic!("program link: {}", gl.get_program_info_log(program));
        }
        gl.detach_shader(program, vert);
        gl.detach_shader(program, frag);
        gl.delete_shader(vert);
        gl.delete_shader(frag);
        program
    }
}

const BG_VERT: &str = r#"#version 330 core
layout(location = 0) in vec2 in_pos;
layout(location = 1) in ivec2 in_cell;
layout(location = 2) in vec4 in_color;
layout(location = 3) in vec2 in_offset_cells;
layout(location = 4) in vec2 in_size_cells;

uniform vec2 u_cell_size;
uniform vec2 u_viewport;

out vec4 v_color;

void main() {
    vec2 origin = vec2(in_cell) + in_offset_cells;
    vec2 px = (origin + in_pos * in_size_cells) * u_cell_size;
    vec2 clip = (px / u_viewport) * 2.0 - 1.0;
    clip.y = -clip.y;
    gl_Position = vec4(clip, 0.0, 1.0);
    v_color = in_color;
}
"#;

const BG_FRAG: &str = r#"#version 330 core
in vec4 v_color;
out vec4 o_color;
void main() {
    o_color = v_color;
}
"#;

const GLYPH_VERT: &str = r#"#version 330 core
layout(location = 0) in vec2 in_pos;
layout(location = 1) in ivec2 in_cell;
layout(location = 2) in vec4 in_color;
layout(location = 3) in vec4 in_uv_rect;
layout(location = 4) in vec2 in_size_px;
layout(location = 5) in vec2 in_offset_px;

uniform vec2 u_cell_size;
uniform vec2 u_viewport;

out vec2 v_uv;
out vec4 v_color;

void main() {
    vec2 cell_origin = vec2(in_cell) * u_cell_size;
    vec2 px = cell_origin + in_offset_px + in_pos * in_size_px;
    vec2 clip = (px / u_viewport) * 2.0 - 1.0;
    clip.y = -clip.y;
    gl_Position = vec4(clip, 0.0, 1.0);

    v_uv = mix(in_uv_rect.xy, in_uv_rect.zw, in_pos);
    v_color = in_color;
}
"#;

const GLYPH_FRAG: &str = r#"#version 330 core

in vec2 v_uv;
in vec4 v_color;
uniform sampler2D u_atlas;

layout(location = 0, index = 0) out vec4 o_color;
layout(location = 0, index = 1) out vec4 o_mask;

// Subpixel AA via dual-source blending:
//   out = o_color * o_mask + dst * (1 - o_mask)
// where o_mask has per-channel coverage from the LCD-filtered glyph bitmap.
void main() {
    vec3 mask = texture(u_atlas, v_uv).rgb;
    float avg = (mask.r + mask.g + mask.b) / 3.0;
    o_color = vec4(v_color.rgb, 1.0);
    o_mask = vec4(mask * v_color.a, avg * v_color.a);
}
"#;
