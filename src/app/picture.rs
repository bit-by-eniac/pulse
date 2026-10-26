//! The video picture. Frames come from the decoder as NV12 (1.5 bytes a pixel)
//! and a small shader turns them into colour as they are drawn, so the CPU only
//! copies them. Where the graphics driver is too old for that shader (OpenGL
//! before 3, OpenGL ES 2), the frames are converted here instead.

use crate::engine::Frame;
use eframe::egui::{self, pos2, Color32, Rect, TextureHandle, TextureOptions};
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Picture {
    shared: Arc<Mutex<Shared>>,
    /// The size of the frame on show.
    size: Option<[usize; 2]>,
    /// Converted frames, on drivers without the shader.
    texture: Option<TextureHandle>,
}

/// Between the UI and the paint callback, which runs while egui draws.
#[derive(Default)]
struct Shared {
    /// The newest frame, not yet on the graphics chip.
    pending: Option<Frame>,
    /// Buffers it has taken, to go back to the decoder.
    spent: Vec<Vec<u8>>,
    gl: Option<Gl>,
    /// The driver cannot run the shader.
    unsupported: bool,
}

impl Picture {
    pub fn size(&self) -> Option<[usize; 2]> {
        self.size
    }

    /// Shows `frame` from the next paint on. Hands back buffers done with.
    pub fn set(&mut self, ctx: &egui::Context, frame: Frame) -> Vec<Vec<u8>> {
        let mut shared = self.shared.lock().unwrap();
        let mut done = std::mem::take(&mut shared.spent);
        self.size = Some([frame.width, frame.height]);
        if shared.unsupported {
            let image = to_rgba(&frame);
            match &mut self.texture {
                Some(tex) => tex.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("video", image, TextureOptions::LINEAR)),
            }
            done.push(frame.pixels);
        } else if let Some(old) = shared.pending.replace(frame) {
            done.push(old.pixels);
        }
        done
    }

    /// Nothing to show (another track, audio mode). Hands back buffers done with.
    pub fn clear(&mut self) -> Vec<Vec<u8>> {
        self.size = None;
        self.texture = None;
        let mut shared = self.shared.lock().unwrap();
        let mut done = std::mem::take(&mut shared.spent);
        done.extend(shared.pending.take().map(|f| f.pixels));
        done
    }

    pub fn paint(&self, painter: &egui::Painter, rect: Rect) {
        if let Some(tex) = &self.texture {
            painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            return;
        }
        let shared = self.shared.clone();
        let callback = egui_glow::CallbackFn::new(move |_info, painter| {
            let mut shared = shared.lock().unwrap();
            let shared = &mut *shared;
            if shared.unsupported {
                return;
            }
            let gl = painter.gl();
            if shared.gl.is_none() {
                // SAFETY: called by egui_glow while its context is current.
                match unsafe { Gl::new(gl) } {
                    Ok(made) => shared.gl = Some(made),
                    Err(e) => {
                        eprintln!("video shader: {e}; converting frames on the CPU");
                        shared.unsupported = true;
                        shared.spent.extend(shared.pending.take().map(|f| f.pixels));
                        return;
                    }
                }
            }
            let Some(state) = &mut shared.gl else { return };
            // SAFETY: as above; the textures and program are this context's.
            unsafe {
                if let Some(frame) = shared.pending.take() {
                    state.upload(gl, &frame);
                    shared.spent.push(frame.pixels);
                }
                state.draw(gl);
            }
        });
        painter.add(egui::PaintCallback { rect, callback: Arc::new(callback) });
    }
}

/// The shader, its two textures (brightness; colour at half size) and the empty
/// vertex array a core-profile context needs to draw at all.
struct Gl {
    program: glow::Program,
    vertices: glow::VertexArray,
    luma: glow::Texture,
    chroma: glow::Texture,
    size: [usize; 2],
}

/// Draws the picture over the whole viewport, which egui sets to the callback's
/// rectangle; the four corners come from the vertex number alone.
const VERTEX: &str = r"
out vec2 v_uv;
void main() {
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    v_uv = vec2(corner.x, 1.0 - corner.y);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}";

/// BT.709, limited range, into the gamma-encoded colour egui draws with.
const FRAGMENT: &str = r"
precision mediump float;
uniform sampler2D u_luma;
uniform sampler2D u_chroma;
in vec2 v_uv;
out vec4 out_color;
void main() {
    float y = (texture(u_luma, v_uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    vec2 c = (texture(u_chroma, v_uv).rg - 128.0 / 255.0) * (255.0 / 224.0);
    vec3 rgb = vec3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    out_color = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}";

impl Gl {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        if !version.is_new_shader_interface() {
            return Err(format!("{version:?} is too old"));
        }
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, &format!("{}\n{source}", version.version_declaration()));
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            gl.use_program(Some(program));
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_luma").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_chroma").as_ref(), 1);
            let texture = || -> Result<glow::Texture, String> {
                let t = gl.create_texture()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (key, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, key, value as i32);
                }
                Ok(t)
            };
            Ok(Gl { program, vertices: gl.create_vertex_array()?, luma: texture()?, chroma: texture()?, size: [0, 0] })
        }
    }

    unsafe fn upload(&mut self, gl: &glow::Context, frame: &Frame) {
        let [w, h] = [frame.width, frame.height];
        let (luma, chroma) = frame.pixels.split_at(w * h);
        let planes = [(self.luma, glow::R8, glow::RED, w, h, luma), (self.chroma, glow::RG8, glow::RG, w / 2, h / 2, chroma)];
        unsafe {
            // Rows are as long as they are, not padded to 4 bytes.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for (texture, inner, format, pw, ph, data) in planes {
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                let data = glow::PixelUnpackData::Slice(Some(data));
                if self.size == [w, h] {
                    gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, pw as i32, ph as i32, format, glow::UNSIGNED_BYTE, data);
                } else {
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, inner as i32, pw as i32, ph as i32, 0, format, glow::UNSIGNED_BYTE, data);
                }
            }
        }
        self.size = [w, h];
    }

    unsafe fn draw(&self, gl: &glow::Context) {
        if self.size == [0, 0] {
            return;
        }
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertices));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.chroma));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.luma));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

/// The shader's sums on the CPU, for drivers without it.
fn to_rgba(frame: &Frame) -> egui::ColorImage {
    let [w, h] = [frame.width, frame.height];
    let (luma, chroma) = frame.pixels.split_at(w * h);
    let mut pixels = Vec::with_capacity(w * h);
    for row in 0..h {
        let colour = &chroma[row / 2 * w..][..w];
        for (col, &l) in luma[row * w..][..w].iter().enumerate() {
            let y = (l as f32 - 16.0) * (255.0 / 219.0);
            let u = (colour[col & !1] as f32 - 128.0) * (255.0 / 224.0);
            let v = (colour[col | 1] as f32 - 128.0) * (255.0 / 224.0);
            let byte = |x: f32| x.clamp(0.0, 255.0) as u8;
            pixels.push(Color32::from_rgb(byte(y + 1.5748 * v), byte(y - 0.1873 * u - 0.4681 * v), byte(y + 1.8556 * u)));
        }
    }
    egui::ColorImage::new([w, h], pixels)
}
