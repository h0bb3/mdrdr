//! The pure render core.

use std::path::Path;

use crate::font::Fonts;
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
    pub pixels: Vec<u8>, // RGBA8 row-major
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
}

pub fn render(input: &RenderInput) -> Framebuffer {
    let blocks = parse(input.source);
    let lay = layout(LayoutInput {
        blocks: &blocks,
        tree: input.tree,
        active_path: input.active_path,
        viewport_w: input.viewport.width,
        viewport_h: input.viewport.height,
        theme: input.theme,
        fonts: input.fonts,
    });
    draw(&lay, input.viewport, input.scroll, input.theme, input.fonts)
}

pub fn measure(
    source: &str,
    viewport_w: u32,
    viewport_h: u32,
    sidebar_x_offset: u32,
    theme: &Theme,
    fonts: &Fonts,
) -> f32 {
    let _ = sidebar_x_offset;
    let blocks = parse(source);
    let lay = layout(LayoutInput {
        blocks: &blocks,
        tree: None,
        active_path: None,
        viewport_w,
        viewport_h,
        theme,
        fonts,
    });
    lay.doc_height
}

/// Hit-test a point (in screen coords) against the sidebar. Returns the
/// click action if any target contains (x,y).
pub fn hit_test<'a>(targets: &'a [HitTarget], x: f32, y: f32) -> Option<&'a HitTarget> {
    targets
        .iter()
        .find(|t| x >= t.x && x <= t.x + t.w && y >= t.y && y <= t.y + t.h)
}

fn draw(
    lay: &Layout,
    viewport: Viewport,
    scroll: f32,
    theme: &Theme,
    fonts: &Fonts,
) -> Framebuffer {
    let mut fb = Framebuffer::new(viewport.width, viewport.height, theme.bg);

    // Content (scrolled).
    draw_items(&mut fb, &lay.content_items, scroll, viewport, fonts);
    // Pinned UI on top.
    draw_items(&mut fb, &lay.pinned_items, 0.0, viewport, fonts);

    // suppress unused warning
    let _ = theme;
    fb
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
        }
    }
}

/// Compute hit targets for the current state without drawing.
pub fn compute_hit_targets(input: &RenderInput) -> Vec<HitTarget> {
    let blocks = parse(input.source);
    let lay = layout(LayoutInput {
        blocks: &blocks,
        tree: input.tree,
        active_path: input.active_path,
        viewport_w: input.viewport.width,
        viewport_h: input.viewport.height,
        theme: input.theme,
        fonts: input.fonts,
    });
    lay.hit_targets
}
