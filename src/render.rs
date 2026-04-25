//! The pure render core.

use std::path::Path;

use crate::font::Fonts;
use crate::images::ImageCache;
use crate::layout::{layout, pick_font, CopyZone, HitTarget, Layout, LayoutInput, OutlineEntry, Placed};
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

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
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

    /// Rasterize one glyph into the framebuffer. `baseline` is the text
    /// baseline y; we use fontdue's `ymin` to place the bitmap above it.
    pub fn draw_glyph(
        &mut self,
        f: &fontdue::Font,
        ch: char,
        size: f32,
        x: f32,
        baseline: f32,
        color: Rgba,
    ) {
        let (metrics, bitmap) = f.rasterize(ch, size);
        if metrics.width == 0 || metrics.height == 0 {
            return;
        }
        let left = x as i32 + metrics.xmin;
        let top = baseline as i32 - (metrics.height as i32 + metrics.ymin);
        for gy in 0..metrics.height {
            for gx in 0..metrics.width {
                let a = bitmap[gy * metrics.width + gx];
                if a > 0 {
                    self.blend(left + gx as i32, top + gy as i32, color, a);
                }
            }
        }
    }
}

/// Sum of glyph advance widths for `text` at `size`, in the given font.
pub fn measure_text_width(f: &fontdue::Font, text: &str, size: f32) -> f32 {
    let mut w = 0.0;
    for ch in text.chars() {
        w += f.metrics(ch, size).advance_width;
    }
    w
}

/// A substring match in the document — expressed in terms of the glyph
/// indices inside `content_items`, plus the first glyph's doc-y (for
/// scroll) and its x-bounds (for drawing a highlight rect).
#[derive(Debug, Clone)]
pub struct ContentMatch {
    pub glyph_start: usize,
    pub glyph_end: usize,  // inclusive
    pub doc_y: f32,
}

/// Cheap fingerprint over a layout's content items + query. Used to short-
/// circuit `find_content_matches` when the same search has already been
/// computed for the same layout. Item count plus sampled glyph coordinates
/// uniquely identify a layout result for our purposes — anything that
/// reflows (zoom, viewport, source edit) changes either the count or the
/// sample positions.
fn layout_fingerprint(items: &[Placed], query: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    items.len().hash(&mut h);
    query.hash(&mut h);
    let n = items.len();
    let probes = [0, n / 4, n / 2, (n * 3) / 4, n.saturating_sub(1)];
    for i in probes {
        if let Some(Placed::Glyph { ch, x, baseline, size, .. }) = items.get(i) {
            ch.hash(&mut h);
            x.to_bits().hash(&mut h);
            baseline.to_bits().hash(&mut h);
            size.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

/// ASCII case-insensitive substring search across the laid-out content.
/// Walks glyphs in order, synthesising spaces at word gaps and newlines at
/// baseline changes so multi-word queries find their target. Non-selectable
/// "chrome" glyphs (e.g. the code-block "copy" button glyphs — now gone
/// but the selectable flag is still honoured) are skipped.
///
/// Memoised per-thread by `(items fingerprint, query)`. A naive 100k-glyph
/// document re-scanned at paint rate (~60 Hz) plus once per search-box
/// keystroke is enough work to back up the compositor's request queue and
/// trigger an EPIPE on the wayland/X11 socket. The cache makes the second
/// and later calls for the same layout+query free.
pub fn find_content_matches(items: &[Placed], query: &str, fonts: &Fonts) -> Vec<ContentMatch> {
    if query.is_empty() {
        return Vec::new();
    }

    thread_local! {
        static CACHE: std::cell::RefCell<Option<(u64, Vec<ContentMatch>)>> =
            const { std::cell::RefCell::new(None) };
    }
    let key = layout_fingerprint(items, query);
    if let Some(hit) = CACHE.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    }) {
        return hit;
    }

    // Build a parallel char/glyph-index stream. Synthetic gaps (space/newline)
    // carry `None` so a match span can skip them when locating the start/end
    // glyph. Uses real font metrics to compute each glyph's advance — an
    // earlier size*0.5 heuristic over-estimated the gap between adjacent
    // narrow glyphs (i, l, t, …) and inserted spurious spaces, which made
    // every multi-char query fail.
    let mut seq: Vec<(char, Option<usize>)> = Vec::new();
    let mut last_baseline: Option<f32> = None;
    let mut last_xend: Option<f32> = None;
    for (i, item) in items.iter().enumerate() {
        let Placed::Glyph { ch, font, size, x, baseline, selectable: true, .. } = item else { continue };
        if let Some(bl) = last_baseline {
            if (baseline - bl).abs() > 2.0 {
                seq.push(('\n', None));
                last_xend = None;
            } else if let Some(xe) = last_xend {
                // A gap of ~0.25em-ish between words; anything smaller is
                // inter-letter kerning.
                if x - xe > *size * 0.15 {
                    seq.push((' ', None));
                }
            }
        }
        seq.push((ch.to_ascii_lowercase(), Some(i)));
        let f = pick_font(fonts, *font);
        let adv = f.metrics(*ch, *size).advance_width;
        last_baseline = Some(*baseline);
        last_xend = Some(*x + adv);
    }

    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    if q.len() > seq.len() {
        return Vec::new();
    }

    let mut out: Vec<ContentMatch> = Vec::new();
    let mut i = 0;
    while i + q.len() <= seq.len() {
        let window = &seq[i..i + q.len()];
        let eq = window.iter().zip(&q).all(|((c, _), qc)| c == qc);
        if eq {
            let first = window.iter().find_map(|(_, g)| *g);
            let last = window.iter().rev().find_map(|(_, g)| *g);
            if let (Some(f), Some(l)) = (first, last) {
                if let Placed::Glyph { baseline, .. } = items[f] {
                    out.push(ContentMatch { glyph_start: f, glyph_end: l, doc_y: baseline });
                }
            }
            i += q.len().max(1);
        } else {
            i += 1;
        }
    }
    CACHE.with(|c| *c.borrow_mut() = Some((key, out.clone())));
    out
}

/// Bounding rects (one per visual line) for a glyph range, so a match can
/// be highlighted by filling them. Same algorithm as `selection_rects` but
/// fed an index range instead of document anchor points.
pub fn match_rects(items: &[Placed], m: &ContentMatch, fonts: &Fonts) -> Vec<(f32, f32, f32, f32)> {
    let mut out: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut cur_baseline: Option<f32> = None;
    let mut cur_x0: f32 = 0.0;
    let mut cur_x1: f32 = 0.0;
    let mut cur_size: f32 = 0.0;

    for i in m.glyph_start..=m.glyph_end {
        let Some(Placed::Glyph { ch, font, size, x, baseline, selectable: true, .. }) = items.get(i) else {
            continue;
        };
        let f = pick_font(fonts, *font);
        let advance = f.metrics(*ch, *size).advance_width;
        let gx0 = *x;
        let gx1 = *x + advance;
        match cur_baseline {
            Some(bl) if (bl - baseline).abs() < 2.0 => {
                cur_x1 = cur_x1.max(gx1);
                cur_size = cur_size.max(*size);
            }
            Some(bl) => {
                let top = bl - cur_size * 0.95;
                let bot = bl + cur_size * 0.25;
                out.push((cur_x0, top, (cur_x1 - cur_x0).max(1.0), bot - top));
                cur_baseline = Some(*baseline);
                cur_x0 = gx0;
                cur_x1 = gx1;
                cur_size = *size;
            }
            None => {
                cur_baseline = Some(*baseline);
                cur_x0 = gx0;
                cur_x1 = gx1;
                cur_size = *size;
            }
        }
    }
    if let Some(bl) = cur_baseline {
        let top = bl - cur_size * 0.95;
        let bot = bl + cur_size * 0.25;
        out.push((cur_x0, top, (cur_x1 - cur_x0).max(1.0), bot - top));
    }
    out
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
    pub sidebar_scroll: f32,
    pub content_zoom: f32,
    pub sidebar_zoom: f32,
    /// (anchor, head) in document coordinates. None = no selection.
    pub selection: Option<((f32, f32), (f32, f32))>,
    /// Mouse position in screen coords. Used to paint a hover highlight on
    /// the hit-target under the cursor (tree row / link).
    pub hover_pos: Option<(f32, f32)>,
    /// Active in-document search. When set, every match is drawn with a
    /// muted highlight and the current match gets the accent colour.
    pub search: Option<SearchHighlights<'a>>,
    /// Per-mermaid-block layout overrides, keyed by the block's 0-based
    /// index in document order. View-only.
    pub mermaid_overrides: Option<&'a std::collections::HashMap<usize, crate::mermaid::Direction>>,
    /// Maximum width of the narrow text reading column. Code, tables,
    /// images and diagrams ignore it.
    pub text_column_width: f32,
    /// Horizontal offset of the text column from the content area's left
    /// edge.
    pub text_column_offset_x: f32,
}

/// What the renderer needs to paint search highlights. Computed by the
/// caller from the already-laid-out items and the live query.
pub struct SearchHighlights<'a> {
    pub query: &'a str,
    pub current: Option<usize>,
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
            sidebar_scroll: input.sidebar_scroll,
            content_zoom: input.content_zoom,
            sidebar_zoom: input.sidebar_zoom,
            mermaid_overrides: input.mermaid_overrides,
            text_column_width: input.text_column_width,
            text_column_offset_x: input.text_column_offset_x,
        },
        images,
    );
    draw(
        &lay,
        input.viewport,
        input.scroll,
        input.theme,
        input.fonts,
        input.selection,
        input.hover_pos,
        input.search.as_ref(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn measure(
    source: &str,
    viewport_w: u32,
    viewport_h: u32,
    base_dir: Option<&Path>,
    sidebar_width: f32,
    content_zoom: f32,
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
            sidebar_scroll: 0.0,
            content_zoom,
            sidebar_zoom: 1.0,
            mermaid_overrides: None,
            text_column_width: f32::INFINITY,
            text_column_offset_x: 0.0,
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

/// Scrollbar geometry. Shared between the draw pass and mouse hit-testing so
/// "what the thumb shows" and "what the user grabs" are the same values.
///
/// Coords are in screen space. `max_scroll` is the maximum valid scroll
/// position (doc_height - viewport_height). Returns None when the doc fits.
#[derive(Debug, Clone, Copy)]
pub struct SbGeom {
    pub track_x: f32,
    pub track_y: f32,
    pub track_w: f32,
    pub track_h: f32,
    pub thumb_y: f32,
    pub thumb_h: f32,
    pub max_scroll: f32,
}

pub const SB_VISIBLE_W: f32 = 8.0;
pub const SB_HIT_W: f32 = 14.0;
pub const SB_RIGHT_PAD: f32 = 2.0;
pub const SB_MIN_THUMB: f32 = 40.0;

pub fn scrollbar_geom(viewport: Viewport, scroll: f32, doc_height: f32) -> Option<SbGeom> {
    let vh = viewport.height as f32;
    let vw = viewport.width as f32;
    if doc_height <= vh + 1.0 {
        return None;
    }
    let max_scroll = (doc_height - vh).max(1.0);
    let thumb_h = ((vh / doc_height) * vh).max(SB_MIN_THUMB).min(vh);
    let frac = (scroll / max_scroll).clamp(0.0, 1.0);
    let thumb_y = frac * (vh - thumb_h);
    Some(SbGeom {
        track_x: vw - SB_VISIBLE_W - SB_RIGHT_PAD,
        track_y: 0.0,
        track_w: SB_VISIBLE_W,
        track_h: vh,
        thumb_y,
        thumb_h,
        max_scroll,
    })
}

/// Is (x, y) inside the scrollbar hit strip (slightly wider than the visible
/// track so users can grab it)?
pub fn in_scrollbar_strip(g: &SbGeom, viewport: Viewport, x: f32, y: f32) -> bool {
    let strip_left = viewport.width as f32 - SB_HIT_W - SB_RIGHT_PAD;
    let strip_right = viewport.width as f32;
    x >= strip_left && x < strip_right && y >= g.track_y && y < g.track_y + g.track_h
}

/// Width of the sidebar scrollbar. Shared by layout (draw) and window
/// (hit test).
pub const SIDEBAR_SB_W: f32 = 6.0;
pub const SIDEBAR_SB_RIGHT_PAD: f32 = 2.0;
pub const SIDEBAR_SB_HIT_W: f32 = 14.0;
pub const SIDEBAR_SB_MIN_THUMB: f32 = 32.0;

/// Geometry for the sidebar's internal scrollbar. None when the tree fits.
pub fn sidebar_scrollbar_geom(
    sidebar_width: f32,
    viewport_h: f32,
    sidebar_scroll: f32,
    content_h: f32,
) -> Option<SbGeom> {
    if content_h <= viewport_h + 1.0 {
        return None;
    }
    let max_scroll = (content_h - viewport_h).max(1.0);
    let thumb_h = ((viewport_h / content_h) * viewport_h)
        .max(SIDEBAR_SB_MIN_THUMB)
        .min(viewport_h);
    let frac = (sidebar_scroll / max_scroll).clamp(0.0, 1.0);
    let thumb_y = frac * (viewport_h - thumb_h);
    Some(SbGeom {
        track_x: sidebar_width - SIDEBAR_SB_W - SIDEBAR_SB_RIGHT_PAD,
        track_y: 0.0,
        track_w: SIDEBAR_SB_W,
        track_h: viewport_h,
        thumb_y,
        thumb_h,
        max_scroll,
    })
}

/// Is (x, y) inside the sidebar scrollbar hit strip?
pub fn in_sidebar_scrollbar_strip(g: &SbGeom, sidebar_width: f32, x: f32, y: f32) -> bool {
    let strip_right = sidebar_width;
    let strip_left = sidebar_width - SIDEBAR_SB_HIT_W - SIDEBAR_SB_RIGHT_PAD;
    x >= strip_left && x < strip_right && y >= g.track_y && y < g.track_y + g.track_h
}

/// Total content height of the sidebar tree plus outline rows. Pure formula
/// — no layout pass needed. Used by callers that need to clamp
/// sidebar_scroll without re-laying out the whole document.
pub fn sidebar_content_height(
    theme: &Theme,
    tree_len: usize,
    outline_len: usize,
    sidebar_zoom: f32,
) -> f32 {
    let size = theme.body_size * 0.82 * sidebar_zoom;
    let row_h = size * 1.5;
    let top_pad = theme.margin_y * 0.5;
    top_pad * 2.0 + row_h * (tree_len + outline_len) as f32
}

/// Returns (pinned_hits, content_hits). Pinned hits (sidebar tree rows) are
/// in screen coordinates. Content hits (links) are in document coordinates
/// and need `- scroll` applied before hit-testing.
pub fn compute_all_hit_targets(
    input: &RenderInput,
    images: &mut ImageCache,
) -> (Vec<HitTarget>, Vec<HitTarget>) {
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
            sidebar_scroll: input.sidebar_scroll,
            content_zoom: input.content_zoom,
            sidebar_zoom: input.sidebar_zoom,
            mermaid_overrides: input.mermaid_overrides,
            text_column_width: input.text_column_width,
            text_column_offset_x: input.text_column_offset_x,
        },
        images,
    );
    (lay.hit_targets, lay.content_hit_targets)
}

/// Document-coord zones that offer a contextual "Copy …" item when the
/// cursor is over them at right-click time. y is doc space — caller must
/// add `scroll` to the cursor's screen-y before comparing.
pub fn compute_copy_zones(input: &RenderInput, images: &mut ImageCache) -> Vec<CopyZone> {
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
            sidebar_scroll: input.sidebar_scroll,
            content_zoom: input.content_zoom,
            sidebar_zoom: input.sidebar_zoom,
            mermaid_overrides: input.mermaid_overrides,
            text_column_width: input.text_column_width,
            text_column_offset_x: input.text_column_offset_x,
        },
        images,
    );
    lay.copy_zones
}

/// Heading outline of the current document. Computed by the same layout
/// pass that the render pipeline uses, so doc_y values line up with the
/// current viewport.
pub fn compute_outline(input: &RenderInput, images: &mut ImageCache) -> Vec<OutlineEntry> {
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
            sidebar_scroll: input.sidebar_scroll,
            content_zoom: input.content_zoom,
            sidebar_zoom: input.sidebar_zoom,
            mermaid_overrides: input.mermaid_overrides,
            text_column_width: input.text_column_width,
            text_column_offset_x: input.text_column_offset_x,
        },
        images,
    );
    lay.outline
}

fn draw(
    lay: &Layout,
    viewport: Viewport,
    scroll: f32,
    theme: &Theme,
    fonts: &Fonts,
    selection: Option<((f32, f32), (f32, f32))>,
    hover_pos: Option<(f32, f32)>,
    search: Option<&SearchHighlights>,
) -> Framebuffer {
    let mut fb = Framebuffer::new(viewport.width, viewport.height, theme.bg);

    draw_items(&mut fb, &lay.content_items, scroll, viewport, fonts);

    // Search highlights: all matches tinted in muted, the current match in
    // accent. Drawn after content so code-block bgs don't hide them.
    if let Some(s) = search {
        let matches = find_content_matches(&lay.content_items, s.query, fonts);
        let vh = viewport.height as f32;
        let base: Rgba = [theme.muted[0], theme.muted[1], theme.muted[2], 80];
        let current: Rgba = [theme.accent[0], theme.accent[1], theme.accent[2], 140];
        for (i, m) in matches.iter().enumerate() {
            let rects = match_rects(&lay.content_items, m, fonts);
            let color = if Some(i) == s.current { current } else { base };
            for (x, y, w, rh) in rects {
                let sy = y - scroll;
                if sy + rh < 0.0 || sy > vh { continue; }
                fb.fill_rect(x as i32, sy as i32, w.ceil() as i32, rh.ceil() as i32, color);
            }
        }
    }

    // Selection highlight goes *after* content items so opaque block
    // backgrounds (code blocks, etc.) don't bury it. alpha keeps glyphs
    // readable through the tint.
    if let Some((a, h)) = selection {
        let rects = selection_rects(&lay.content_items, a, h, fonts);
        let hl: Rgba = [theme.accent[0], theme.accent[1], theme.accent[2], 80];
        let vh = viewport.height as f32;
        for (x, y, w, rh) in rects {
            let sy = y - scroll;
            if sy + rh < 0.0 || sy > vh {
                continue;
            }
            fb.fill_rect(x as i32, sy as i32, w.ceil() as i32, rh.ceil() as i32, hl);
        }
    }

    draw_items(&mut fb, &lay.pinned_items, 0.0, viewport, fonts);

    // Hover highlight goes *after* pinned_items so the sidebar's opaque
    // background doesn't bury it. alpha is low enough to leave glyphs
    // readable through the tint.
    if let Some((hx, hy)) = hover_pos {
        if let Some(t) = hit_test(&lay.hit_targets, hx, hy) {
            let hl: Rgba = [theme.muted[0], theme.muted[1], theme.muted[2], 45];
            fb.fill_rect(t.x as i32, t.y as i32, t.w.ceil() as i32, t.h.ceil() as i32, hl);
        } else if let Some(t) = hit_test(&lay.content_hit_targets, hx, hy + scroll) {
            let sy = t.y - scroll;
            let hl: Rgba = [theme.accent[0], theme.accent[1], theme.accent[2], 45];
            fb.fill_rect(t.x as i32, sy as i32, t.w.ceil() as i32, t.h.ceil() as i32, hl);
        }
    }

    draw_scrollbar(&mut fb, viewport, scroll, lay.doc_height, theme);
    fb
}

/// Map a document point to the nearest glyph index in `items`.
/// Returns 0 if the items list has no glyphs.
fn glyph_index_at(items: &[Placed], doc_x: f32, doc_y: f32) -> usize {
    // First: pick the line (baseline) closest to doc_y. Skip chrome
    // glyphs (selectable: false) so, e.g., the code block's "copy"
    // button doesn't get picked when the user aims at the first line.
    let mut best_bl: Option<f32> = None;
    let mut best_bld = f32::MAX;
    for item in items {
        if let Placed::Glyph { baseline, selectable: true, .. } = item {
            let d = (baseline - doc_y).abs();
            if d < best_bld {
                best_bld = d;
                best_bl = Some(*baseline);
            }
        }
    }
    let Some(target) = best_bl else { return 0 };

    // On that line, pick the glyph whose x is closest to doc_x.
    let mut best_idx = 0;
    let mut best_dx = f32::MAX;
    for (i, item) in items.iter().enumerate() {
        if let Placed::Glyph { baseline, x, selectable: true, .. } = item {
            if (baseline - target).abs() < 2.0 {
                let d = (*x - doc_x).abs();
                if d < best_dx {
                    best_dx = d;
                    best_idx = i;
                }
            }
        }
    }
    best_idx
}

/// For each line in the selection range, produce a merged highlight rect.
fn selection_rects(
    items: &[Placed],
    anchor: (f32, f32),
    head: (f32, f32),
    fonts: &Fonts,
) -> Vec<(f32, f32, f32, f32)> {
    if items.is_empty() {
        return vec![];
    }
    let a_idx = glyph_index_at(items, anchor.0, anchor.1);
    let h_idx = glyph_index_at(items, head.0, head.1);
    let (start, end) = if a_idx <= h_idx { (a_idx, h_idx) } else { (h_idx, a_idx) };

    let mut out: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut cur_baseline: Option<f32> = None;
    let mut cur_x0: f32 = 0.0;
    let mut cur_x1: f32 = 0.0;
    let mut cur_size: f32 = 0.0;

    for i in start..=end {
        let Some(Placed::Glyph { ch, font, size, x, baseline, selectable: true, .. }) = items.get(i) else {
            continue;
        };
        let f = pick_font(fonts, *font);
        let advance = f.metrics(*ch, *size).advance_width;
        let gx0 = *x;
        let gx1 = *x + advance;

        match cur_baseline {
            Some(bl) if (bl - baseline).abs() < 2.0 => {
                cur_x1 = cur_x1.max(gx1);
                cur_size = cur_size.max(*size);
            }
            Some(bl) => {
                out.push(line_rect(bl, cur_x0, cur_x1, cur_size));
                cur_baseline = Some(*baseline);
                cur_x0 = gx0;
                cur_x1 = gx1;
                cur_size = *size;
                let _ = bl;
            }
            None => {
                cur_baseline = Some(*baseline);
                cur_x0 = gx0;
                cur_x1 = gx1;
                cur_size = *size;
            }
        }
    }
    if let Some(bl) = cur_baseline {
        out.push(line_rect(bl, cur_x0, cur_x1, cur_size));
    }
    out
}

fn line_rect(baseline: f32, x0: f32, x1: f32, size: f32) -> (f32, f32, f32, f32) {
    let top = baseline - size * 0.95;
    let bot = baseline + size * 0.25;
    (x0, top, (x1 - x0).max(1.0), bot - top)
}

/// Extract the selected text as plaintext. Used by /copy and Ctrl+C.
pub fn extract_selection(
    input: &RenderInput,
    images: &mut ImageCache,
) -> Option<String> {
    let (anchor, head) = input.selection?;
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
            sidebar_scroll: input.sidebar_scroll,
            content_zoom: input.content_zoom,
            sidebar_zoom: input.sidebar_zoom,
            mermaid_overrides: input.mermaid_overrides,
            text_column_width: input.text_column_width,
            text_column_offset_x: input.text_column_offset_x,
        },
        images,
    );
    Some(build_selection_text(&lay.content_items, anchor, head, input.fonts))
}

fn build_selection_text(
    items: &[Placed],
    anchor: (f32, f32),
    head: (f32, f32),
    fonts: &Fonts,
) -> String {
    if items.is_empty() {
        return String::new();
    }
    let a_idx = glyph_index_at(items, anchor.0, anchor.1);
    let h_idx = glyph_index_at(items, head.0, head.1);
    let (start, end) = if a_idx <= h_idx { (a_idx, h_idx) } else { (h_idx, a_idx) };

    let mut out = String::new();
    let mut last_baseline: Option<f32> = None;
    let mut last_xend: Option<f32> = None;

    for i in start..=end {
        let Some(Placed::Glyph { ch, font, size, x, baseline, selectable: true, .. }) = items.get(i) else {
            continue;
        };
        if let Some(bl) = last_baseline {
            if (baseline - bl).abs() > 2.0 {
                out.push('\n');
                last_xend = None;
            } else if let Some(xe) = last_xend {
                if x - xe > 1.0 {
                    out.push(' ');
                }
            }
        }
        out.push(*ch);
        let f = pick_font(fonts, *font);
        let adv = f.metrics(*ch, *size).advance_width;
        last_baseline = Some(*baseline);
        last_xend = Some(*x + adv);
    }
    out
}

fn draw_scrollbar(
    fb: &mut Framebuffer,
    viewport: Viewport,
    scroll: f32,
    doc_height: f32,
    theme: &Theme,
) {
    let Some(g) = scrollbar_geom(viewport, scroll, doc_height) else { return };
    let track_color: [u8; 4] = [theme.muted[0], theme.muted[1], theme.muted[2], 40];
    fb.fill_rect(
        g.track_x as i32,
        g.track_y as i32,
        g.track_w as i32,
        g.track_h as i32,
        track_color,
    );
    let thumb_color: [u8; 4] = [theme.muted[0], theme.muted[1], theme.muted[2], 180];
    fb.fill_rect(
        g.track_x as i32,
        g.thumb_y as i32,
        g.track_w as i32,
        g.thumb_h.ceil() as i32,
        thumb_color,
    );
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
            Placed::Glyph { ch, font, size, x, baseline, color, .. } => {
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
            Placed::Ellipse { cx, cy, rx, ry, color } => {
                let screen_cy = *cy - scroll;
                if screen_cy + *ry < 0.0 || screen_cy - *ry > vh {
                    continue;
                }
                fill_ellipse(fb, *cx, screen_cy, *rx, *ry, *color);
            }
            Placed::RoundRect { x, y, w, h, radius, color } => {
                let screen_y = *y - scroll;
                if screen_y + *h < 0.0 || screen_y > vh {
                    continue;
                }
                fill_round_rect(fb, *x, screen_y, *w, *h, *radius, *color);
            }
        }
    }
}

/// Filled ellipse with 4× subpixel AA on the edge pixels.
fn fill_ellipse(fb: &mut Framebuffer, cx: f32, cy: f32, rx: f32, ry: f32, color: Rgba) {
    let rx = rx.max(0.001);
    let ry = ry.max(0.001);
    let x0 = (cx - rx).floor() as i32;
    let x1 = (cx + rx).ceil() as i32;
    let y0 = (cy - ry).floor() as i32;
    let y1 = (cy + ry).ceil() as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let mut hits = 0u16;
            for sy in 0..2 {
                for sx in 0..2 {
                    let x = px as f32 + 0.25 + 0.5 * sx as f32;
                    let y = py as f32 + 0.25 + 0.5 * sy as f32;
                    let dx = (x - cx) / rx;
                    let dy = (y - cy) / ry;
                    if dx * dx + dy * dy <= 1.0 {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let a = (hits * 255 / 4) as u8;
                fb.blend(px, py, color, a);
            }
        }
    }
}

/// Filled rounded rectangle. Radius is clamped to min(w, h) / 2 so a
/// radius of w/2 yields a proper capsule. 4× subpixel AA on corners.
fn fill_round_rect(fb: &mut Framebuffer, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Rgba) {
    let r = radius.clamp(0.0, w.min(h) * 0.5);
    let x0 = x.floor() as i32;
    let x1 = (x + w).ceil() as i32;
    let y0 = y.floor() as i32;
    let y1 = (y + h).ceil() as i32;
    // Fast path: interior band — no corner work needed.
    let inner_top = (y + r).ceil() as i32;
    let inner_bot = (y + h - r).floor() as i32;
    for py in y0..=y1 {
        let in_inner_band = py >= inner_top && py < inner_bot;
        for px in x0..=x1 {
            if in_inner_band {
                // Fully inside the body; only the left/right straight edges
                // need clipping against the outer rect.
                if (px as f32) + 1.0 > x && (px as f32) < x + w {
                    fb.blend(px, py, color, 255);
                }
                continue;
            }
            let mut hits = 0u16;
            for sy in 0..2 {
                for sx in 0..2 {
                    let sxf = px as f32 + 0.25 + 0.5 * sx as f32;
                    let syf = py as f32 + 0.25 + 0.5 * sy as f32;
                    if point_in_round_rect(sxf, syf, x, y, w, h, r) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let a = (hits * 255 / 4) as u8;
                fb.blend(px, py, color, a);
            }
        }
    }
}

fn point_in_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if px < x || px > x + w || py < y || py > y + h {
        return false;
    }
    // Inside main body (one of the straight bands)?
    if (px >= x + r && px <= x + w - r) || (py >= y + r && py <= y + h - r) {
        return true;
    }
    // In a corner — test distance to the corner circle's centre.
    let cx = if px < x + r { x + r } else { x + w - r };
    let cy = if py < y + r { y + r } else { y + h - r };
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
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
