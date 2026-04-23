//! The pure render core.
//!
//! `render(...)` is the single entry point every mode funnels through.
//! Window mode pushes the output to softbuffer; headless mode writes PNG;
//! API mode ships it over HTTP. All three see the exact same pixels.

use fontdue::Font;

use crate::font::Fonts;
use crate::theme::{Rgba, Theme};

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8, row-major
}

impl Framebuffer {
    pub fn new(width: u32, height: u32, bg: Rgba) -> Self {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&bg);
        }
        Self { width, height, pixels }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, color: Rgba, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let idx = ((y as u32 * self.width + x as u32) * 4) as usize;
        let a = (color[3] as u16 * alpha as u16) / 255;
        let inv = 255 - a as u16;
        let dst = &mut self.pixels[idx..idx + 4];
        dst[0] = ((dst[0] as u16 * inv + color[0] as u16 * a) / 255) as u8;
        dst[1] = ((dst[1] as u16 * inv + color[1] as u16 * a) / 255) as u8;
        dst[2] = ((dst[2] as u16 * inv + color[2] as u16 * a) / 255) as u8;
        dst[3] = 255;
    }
}

/// Draw a single line of text at `baseline` coordinates, returns the advance width used.
fn draw_text(
    fb: &mut Framebuffer,
    text: &str,
    font: &Font,
    size: f32,
    x: f32,
    baseline_y: f32,
    color: Rgba,
) -> f32 {
    let mut pen = x;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        if metrics.width > 0 && metrics.height > 0 {
            let left = pen as i32 + metrics.xmin;
            let top = baseline_y as i32 - (metrics.height as i32 + metrics.ymin);
            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let alpha = bitmap[gy * metrics.width + gx];
                    if alpha > 0 {
                        fb.blend(left + gx as i32, top + gy as i32, color, alpha);
                    }
                }
            }
        }
        pen += metrics.advance_width;
    }
    pen - x
}

/// Fill a solid rect.
fn fill_rect(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
    for py in y..(y + h) {
        for px in x..(x + w) {
            fb.blend(px, py, color, 255);
        }
    }
}

/// Milestone 1 render: a hardcoded sample page that exercises every font style
/// plus a solid-color swatch. Once the markdown parser lands in M2, this is
/// replaced with an AST walk.
pub fn render(_source: &str, viewport: Viewport, _scroll: f32, theme: &Theme, fonts: &Fonts) -> Framebuffer {
    let mut fb = Framebuffer::new(viewport.width, viewport.height, theme.bg);

    let x = theme.margin_x;
    let mut y = theme.margin_y + theme.heading_size; // baseline of first line

    // H1
    draw_text(&mut fb, "mdrdr", &fonts.bold, theme.heading_size, x, y, theme.fg);
    y += theme.heading_size * theme.line_height_mult;

    // subtitle
    draw_text(
        &mut fb,
        "a from-scratch markdown viewer — milestone 1",
        &fonts.italic,
        theme.body_size,
        x,
        y,
        theme.muted,
    );
    y += theme.body_size * theme.line_height_mult * 1.5;

    // body paragraph
    draw_text(
        &mut fb,
        "This image was rendered by the mdrdr pure render core.",
        &fonts.body,
        theme.body_size,
        x,
        y,
        theme.fg,
    );
    y += theme.body_size * theme.line_height_mult;
    draw_text(
        &mut fb,
        "Pipeline: fontdue rasterizes glyphs, we alpha-blend into an RGBA",
        &fonts.body,
        theme.body_size,
        x,
        y,
        theme.fg,
    );
    y += theme.body_size * theme.line_height_mult;
    draw_text(
        &mut fb,
        "framebuffer, the same buffer ships to window / PNG / HTTP /screenshot.",
        &fonts.body,
        theme.body_size,
        x,
        y,
        theme.fg,
    );
    y += theme.body_size * theme.line_height_mult * 1.5;

    // code block swatch
    let code_x = x;
    let code_y = y - theme.mono_size; // rect starts above baseline
    let code_w = (viewport.width as f32 - theme.margin_x * 2.0) as i32;
    let code_h = (theme.mono_size * theme.line_height_mult * 1.4) as i32;
    fill_rect(&mut fb, code_x as i32, code_y as i32, code_w, code_h, theme.code_bg);
    draw_text(
        &mut fb,
        "fn main() { println!(\"hello, mdrdr\"); }",
        &fonts.mono,
        theme.mono_size,
        x + 12.0,
        y + theme.mono_size * 0.2,
        theme.accent,
    );
    y += theme.mono_size * theme.line_height_mult * 2.0;

    // footer
    draw_text(
        &mut fb,
        "Greek sample: α β γ δ ε ζ η θ ι κ λ μ ν ξ ο π ρ σ τ υ φ χ ψ ω",
        &fonts.body,
        theme.body_size,
        x,
        y,
        theme.fg,
    );

    fb
}
