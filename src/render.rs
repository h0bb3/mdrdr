//! The pure render core.

use std::path::Path;

use crate::font::Fonts;
use crate::images::ImageCache;
use crate::layout::{layout, pick_font, HitTarget, Layout, LayoutInput, Placed};
use crate::md::parse;
use crate::theme::{Rgba, Theme};
use crate::tree::TreeEntry;

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.width as i32);
        let y1 = (y + h).min(self.height as i32);
        for py in y0..y1 {
            for px in x0..x1 {
                self.blend(px, py, color, 255);
            }
        }
    }
}

pub struct RenderInput<'a> {
    pub source: &'a str,
    pub viewport: Viewport,
    pub scroll: f32,
    pub theme: &'a Theme,
    pub fonts: &'a Fonts,
    pub tree: Option<&'a [TreeEntry]>,
    pub active_path: Option<&'a Path>,
    pub base_dir: Option<&'a Path>,
    pub sidebar_width: f32,
}

pub fn render(input: &RenderInput, images: &mut ImageCache) -> Framebuffer {
    let blocks = parse(input.source);
    let lay = layout(
        LayoutInput {
            blocks: &blocks,
            tree: input.tree,
            active_path: input.active_path,
            base_dir: input.base_dir,
            viewport_w: input.viewport.width,
            viewport_h: input.viewport.height,
            theme: input.theme,
            fonts: input.fonts,
            sidebar_width: input.sidebar_width,
        },
        images,
    );
    draw(&lay, input.viewport, input.scroll, input.theme, input.fonts)
}

pub fn measure(
    source: &str,
    viewport_w: u32,
    viewport_h: u32,
    base_dir: Option<&Path>,
    sidebar_width: f32,
    theme: &Theme,
    fonts: &Fonts,
    images: &mut ImageCache,
) -> f32 {
    let blocks = parse(source);
    let lay = layout(
        LayoutInput {
            blocks: &blocks,
            tree: None,
            active_path: None,
            base_dir,
            viewport_w,
            viewport_h,
            theme,
            fonts,
            sidebar_width,
        },
        images,
    );
    lay.doc_height
}

pub fn hit_test<'a>(targets: &'a [HitTarget], x: f32, y: f32) -> Option<&'a HitTarget> {
    targets
        .iter()
        .find(|t| x >= t.x && x <= t.x + t.w && y >= t.y && y <= t.y + t.h)
}

pub fn compute_hit_targets(input: &RenderInput, images: &mut ImageCache) -> Vec<HitTarget> {
    let blocks = parse(input.source);
    let lay = layout(
        LayoutInput {
            blocks: &blocks,
            tree: input.tree,
            active_path: input.active_path,
            base_dir: input.base_dir,
            viewport_w: input.viewport.width,
            viewport_h: input.viewport.height,
            theme: input.theme,
            fonts: input.fonts,
            sidebar_width: input.sidebar_width,
        },
        images,
    );
    lay.hit_targets
}

fn draw(
    lay: &Layout,
    viewport: Viewport,
    scroll: f32,
    theme: &Theme,
    fonts: &Fonts,
) -> Framebuffer {
    let mut fb = Framebuffer::new(viewport.width, viewport.height, theme.bg);
    draw_items(&mut fb, &lay.content_items, scroll, viewport, fonts);
    draw_items(&mut fb, &lay.pinned_items, 0.0, viewport, fonts);
    draw_scrollbar(&mut fb, viewport, scroll, lay.doc_height, theme);
    fb
}

fn draw_scrollbar(
    fb: &mut Framebuffer,
    viewport: Viewport,
    scroll: f32,
    doc_height: f32,
    theme: &Theme,
) {
    let vh = viewport.height as f32;
    let vw = viewport.width as i32;
    if doc_height <= vh + 1.0 {
        return;
    }
    let track_w: i32 = 8;
    let track_x = vw - track_w - 2;
    let track_color: [u8; 4] = [theme.muted[0], theme.muted[1], theme.muted[2], 40];
    fb.fill_rect(track_x, 0, track_w, viewport.height as i32, track_color);

    let thumb_h = ((vh / doc_height) * vh).max(40.0);
    let max_scroll = (doc_height - vh).max(1.0);
    let frac = (scroll / max_scroll).clamp(0.0, 1.0);
    let thumb_y = frac * (vh - thumb_h);
    let thumb_color: [u8; 4] = [theme.muted[0], theme.muted[1], theme.muted[2], 180];
    fb.fill_rect(track_x, thumb_y as i32, track_w, thumb_h as i32, thumb_color);
}

fn draw_items(
    fb: &mut Framebuffer,
    items: &[Placed],
    scroll: f32,
    viewport: Viewport,
    fonts: &Fonts,
) {
    let vh = viewport.height as f32;
    for item in items {
        match item {
            Placed::Glyph { ch, font, size, x, baseline, color } => {
                let screen_baseline = *baseline - scroll;
                if screen_baseline < -(*size) || screen_baseline - size > vh {
                    continue;
                }
                let f = pick_font(fonts, *font);
                let (metrics, bitmap) = f.rasterize(*ch, *size);
                if metrics.width == 0 || metrics.height == 0 {
                    continue;
                }
                let left = *x as i32 + metrics.xmin;
                let top = screen_baseline as i32 - (metrics.height as i32 + metrics.ymin);
                for gy in 0..metrics.height {
                    for gx in 0..metrics.width {
                        let a = bitmap[gy * metrics.width + gx];
                        if a > 0 {
                            fb.blend(left + gx as i32, top + gy as i32, *color, a);
                        }
                    }
                }
            }
            Placed::Rect { x, y, w, h, color } => {
                let screen_y = *y - scroll;
                if screen_y + *h < 0.0 || screen_y > vh {
                    continue;
                }
                fb.fill_rect(*x as i32, screen_y as i32, *w as i32, *h as i32, *color);
            }
            Placed::Underline { x, y, w, color } => {
                let screen_y = *y - scroll;
                if screen_y + 1.0 < 0.0 || screen_y > vh {
                    continue;
                }
                fb.fill_rect(*x as i32, screen_y as i32, *w as i32, 1, *color);
            }
            Placed::Image { x, y, w, h, data } => {
                let screen_y = *y - scroll;
                if screen_y + *h < 0.0 || screen_y > vh {
                    continue;
                }
                blit_image_scaled(fb, *x, screen_y, *w, *h, data.width, data.height, &data.rgba);
            }
            Placed::Line { x1, y1, x2, y2, thickness, color } => {
                let s_y1 = *y1 - scroll;
                let s_y2 = *y2 - scroll;
                if s_y1.max(s_y2) < 0.0 || s_y1.min(s_y2) > vh {
                    continue;
                }
                draw_line(fb, *x1, s_y1, *x2, s_y2, *thickness, *color);
            }
            Placed::Triangle { p1, p2, p3, color } => {
                let min_y = p1.1.min(p2.1).min(p3.1) - scroll;
                let max_y = p1.1.max(p2.1).max(p3.1) - scroll;
                if max_y < 0.0 || min_y > vh {
                    continue;
                }
                let p1s = (p1.0, p1.1 - scroll);
                let p2s = (p2.0, p2.1 - scroll);
                let p3s = (p3.0, p3.1 - scroll);
                fill_triangle(fb, p1s, p2s, p3s, *color);
            }
        }
    }
}

fn draw_line(fb: &mut Framebuffer, x1: f32, y1: f32, x2: f32, y2: f32, thickness: f32, color: Rgba) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    let steps = len.ceil() as i32;
    let r = (thickness * 0.5).max(0.5);
    let r_int = r.ceil() as i32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let cx = x1 + dx * t;
        let cy = y1 + dy * t;
        let cxi = cx.round() as i32;
        let cyi = cy.round() as i32;
        for oy in -r_int..=r_int {
            for ox in -r_int..=r_int {
                let d = ((ox as f32).powi(2) + (oy as f32).powi(2)).sqrt();
                if d <= r + 0.5 {
                    let alpha = if d <= r - 0.5 {
                        255u8
                    } else {
                        // soft edge over half a pixel
                        ((1.0 - (d - (r - 0.5))).clamp(0.0, 1.0) * 255.0) as u8
                    };
                    fb.blend(cxi + ox, cyi + oy, color, alpha);
                }
            }
        }
    }
}

fn fill_triangle(fb: &mut Framebuffer, a: (f32, f32), b: (f32, f32), c: (f32, f32), color: Rgba) {
    let min_x = a.0.min(b.0).min(c.0).floor() as i32;
    let max_x = a.0.max(b.0).max(c.0).ceil() as i32;
    let min_y = a.1.min(b.1).min(c.1).floor() as i32;
    let max_y = a.1.max(b.1).max(c.1).ceil() as i32;
    let edge = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| -> f32 {
        (r.0 - p.0) * (q.1 - p.1) - (r.1 - p.1) * (q.0 - p.0)
    };
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let w0 = edge(b, c, p);
            let w1 = edge(c, a, p);
            let w2 = edge(a, b, p);
            let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
            if inside {
                fb.blend(x, y, color, 255);
            }
        }
    }
}

/// Nearest-neighbor scaled blit. Source is RGBA8, `sw` × `sh`.
fn blit_image_scaled(
    fb: &mut Framebuffer,
    dst_x: f32,
    dst_y: f32,
    dst_w: f32,
    dst_h: f32,
    sw: u32,
    sh: u32,
    src: &[u8],
) {
    let dw = dst_w.round() as i32;
    let dh = dst_h.round() as i32;
    let dx = dst_x.round() as i32;
    let dy = dst_y.round() as i32;
    if dw <= 0 || dh <= 0 || sw == 0 || sh == 0 {
        return;
    }
    for py in 0..dh {
        let sy = (py as u64 * sh as u64 / dh as u64).min(sh as u64 - 1) as u32;
        let row = (sy as usize) * (sw as usize) * 4;
        for px in 0..dw {
            let sx = (px as u64 * sw as u64 / dw as u64).min(sw as u64 - 1) as u32;
            let idx = row + (sx as usize) * 4;
            let r = src[idx];
            let g = src[idx + 1];
            let b = src[idx + 2];
            let a = src[idx + 3];
            fb.blend(dx + px, dy + py, [r, g, b, 255], a);
        }
    }
}
