//! Layout engine — walks the parsed block tree and produces positioned draw
//! items in absolute document coordinates. Also handles the sidebar.
//!
//! Two output buckets:
//!   - content_items: scrollable (the main document).
//!   - pinned_items:  drawn at fixed screen positions (sidebar, chrome).
//!
//! hit_targets are in screen coordinates (for pinned UI) — click handlers
//! compare mouse position directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fontdue::Font;

use crate::font::Fonts;
use crate::images::{CachedImage, ImageCache};
use crate::math::{self, MathBox};
use crate::md::{Align, Block, Inline};
use crate::theme::{Rgba, Theme};
use crate::tree::{TreeEntry, TreeKind};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FontId {
    Body,
    Bold,
    Italic,
    BoldItalic,
    Mono,
    Emoji,
}

#[derive(Debug)]
pub enum Placed {
    Glyph {
        ch: char,
        font: FontId,
        size: f32,
        x: f32,
        baseline: f32,
        color: Rgba,
        /// False for "chrome" glyphs like the code block's copy button.
        /// The selection hit-test skips these so dragging across the
        /// top of a code block doesn't snap to the button's letters.
        selectable: bool,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgba,
    },
    Underline {
        x: f32,
        y: f32,
        w: f32,
        color: Rgba,
    },
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        data: Arc<CachedImage>,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        thickness: f32,
        color: Rgba,
    },
    Triangle {
        p1: (f32, f32),
        p2: (f32, f32),
        p3: (f32, f32),
        color: Rgba,
    },
}

#[derive(Debug, Clone)]
pub enum HitAction {
    Open(PathBuf),
    Toggle(PathBuf),
    OpenUrl(String),
    CopyCode(String),
    /// Scroll the content panel to this document y-coord. Used by the
    /// sidebar outline entries.
    ScrollTo(f32),
    /// Change the sidebar tree's root directory. Emitted by the ".."
    /// row and by a double-click on any directory.
    SetRoot(PathBuf),
}

/// A heading captured during content layout, used to build the sidebar
/// outline under the active file.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub level: u8,
    pub text: String,
    /// Document y-coord where the heading begins.
    pub doc_y: f32,
}

#[derive(Debug, Clone)]
pub struct HitTarget {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: HitAction,
}

/// A rectangular region in document coords that, when right-clicked, offers
/// a "Copy …" menu item — no visible button, just a hover-revealed
/// affordance via the context menu. Produced for code blocks and tables.
#[derive(Debug, Clone)]
pub struct CopyZone {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub kind: CopyKind,
    /// The text that goes on the clipboard (raw code for Code, RFC-4180
    /// for Csv).
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    Code,
    Csv,
}

pub struct Layout {
    pub content_items: Vec<Placed>,
    pub pinned_items: Vec<Placed>,
    /// Hit targets pinned to screen coords (sidebar rows). y is screen y.
    pub hit_targets: Vec<HitTarget>,
    /// Hit targets in document coords (links inside content). y needs
    /// `- scroll` to translate to screen coords at hit-test time.
    pub content_hit_targets: Vec<HitTarget>,
    /// Regions in document coords that offer a "Copy …" item when
    /// right-clicked. Separate from hit_targets so clicks don't trigger
    /// copy — only the context menu does.
    pub copy_zones: Vec<CopyZone>,
    pub doc_height: f32,
    pub sidebar_width: f32,
    /// Total pixel height needed to render every tree row. Used to clamp
    /// sidebar scroll and decide whether to draw the sidebar scrollbar.
    pub sidebar_content_height: f32,
    /// Headings discovered during content layout, in document order.
    pub outline: Vec<OutlineEntry>,
}

#[derive(Copy, Clone)]
struct Style {
    bold: bool,
    italic: bool,
    mono: bool,
    size: f32,
    color: Rgba,
    underline: bool,
}

impl Style {
    fn font_id(self) -> FontId {
        if self.mono {
            FontId::Mono
        } else if self.bold && self.italic {
            FontId::BoldItalic
        } else if self.bold {
            FontId::Bold
        } else if self.italic {
            FontId::Italic
        } else {
            FontId::Body
        }
    }
}

pub fn pick_font(fonts: &Fonts, id: FontId) -> &Font {
    match id {
        FontId::Body => &fonts.body,
        FontId::Bold => &fonts.bold,
        FontId::Italic => &fonts.italic,
        FontId::BoldItalic => &fonts.bold_italic,
        FontId::Mono => &fonts.mono,
        FontId::Emoji => &fonts.emoji,
    }
}

pub const SIDEBAR_WIDTH_DEFAULT: f32 = 260.0;

pub struct LayoutInput<'a> {
    pub blocks: &'a [Block],
    pub tree: Option<&'a [TreeEntry]>,
    pub active_path: Option<&'a Path>,
    pub base_dir: Option<&'a Path>,
    pub viewport_w: u32,
    pub viewport_h: u32,
    pub theme: &'a Theme,
    pub fonts: &'a Fonts,
    /// Width of the file-tree sidebar. 0 → hidden.
    pub sidebar_width: f32,
    /// Scroll offset inside the sidebar. 0 → top.
    pub sidebar_scroll: f32,
    /// Font zoom for the main content panel (multiplier; 1.0 = default).
    pub content_zoom: f32,
    /// Font zoom for the sidebar tree panel (multiplier; 1.0 = default).
    pub sidebar_zoom: f32,
}

pub fn layout(input: LayoutInput, images: &mut ImageCache) -> Layout {
    // Reserve sidebar space whenever a width is set — even when `tree`
    // is None (e.g. `measure()` which skips sidebar drawing). Otherwise
    // measurement uses a wider content column than rendering and the
    // reported doc_height is too small, which makes max-scroll clamp
    // before the real bottom of the document.
    let sidebar_width = if input.sidebar_width > 0.0 {
        input.sidebar_width
    } else {
        0.0
    };

    let content_left = sidebar_width + input.theme.margin_x;
    let content_right = (input.viewport_w as f32) - input.theme.margin_x;

    // Content panel uses a zoomed copy of the theme. Margins, colors, and
    // line-height multiplier stay fixed so the page geometry doesn't jump
    // when the user zooms.
    let content_theme = Theme {
        body_size: input.theme.body_size * input.content_zoom,
        heading_size: input.theme.heading_size * input.content_zoom,
        mono_size: input.theme.mono_size * input.content_zoom,
        ..input.theme.clone()
    };

    let mut content_items: Vec<Placed> = Vec::new();
    let mut content_hit_targets: Vec<HitTarget> = Vec::new();
    let mut copy_zones: Vec<CopyZone> = Vec::new();
    let mut outline: Vec<OutlineEntry> = Vec::new();
    let doc_height = {
        let mut ctx = Ctx {
            items: &mut content_items,
            content_hits: &mut content_hit_targets,
            copy_zones: &mut copy_zones,
            outline: &mut outline,
            y: content_theme.margin_y,
            content_left,
            content_right,
            theme: &content_theme,
            fonts: input.fonts,
            base_dir: input.base_dir,
            images,
        };
        for b in input.blocks {
            ctx.block(b, 0.0);
        }
        ctx.y + content_theme.margin_y
    };

    let mut pinned_items = Vec::new();
    let mut hit_targets = Vec::new();
    let mut sidebar_content_height = 0.0;
    if let (Some(tree), true) = (input.tree, sidebar_width > 0.0) {
        sidebar_content_height = layout_sidebar(
            tree,
            input.active_path,
            &outline,
            sidebar_width,
            input.viewport_h as f32,
            input.sidebar_scroll,
            input.sidebar_zoom,
            input.theme,
            input.fonts,
            &mut pinned_items,
            &mut hit_targets,
        );
    }

    // Resolve internal anchor links (href="#slug") against the outline so
    // clicking one scrolls instead of trying to xdg-open "#slug". Slugs
    // follow the common GitHub convention: lowercase, non-alphanumerics
    // collapsed to single hyphens.
    let mut slug_y: std::collections::HashMap<String, f32> =
        std::collections::HashMap::with_capacity(outline.len());
    for o in &outline {
        slug_y.entry(slugify(&o.text)).or_insert(o.doc_y);
    }
    for hit in &mut content_hit_targets {
        if let HitAction::OpenUrl(href) = &hit.action {
            if let Some(slug) = href.strip_prefix('#') {
                let s = slugify(slug);
                if let Some(y) = slug_y.get(&s) {
                    hit.action = HitAction::ScrollTo(*y);
                }
            }
        }
    }

    Layout {
        content_items,
        pinned_items,
        hit_targets,
        content_hit_targets,
        copy_zones,
        doc_height,
        sidebar_width,
        sidebar_content_height,
        outline,
    }
}

/// GitHub-style slug: lowercase, drop non-alphanumerics (keeping hyphens/
/// underscores), runs of separators collapse to one hyphen. Good enough
/// to match most hand-written link fragments against heading text.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_sep = false;
        } else if !prev_sep {
            out.push('-');
            prev_sep = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ───── sidebar layout ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn layout_sidebar(
    tree: &[TreeEntry],
    active: Option<&Path>,
    outline: &[OutlineEntry],
    width: f32,
    height: f32,
    sidebar_scroll: f32,
    sidebar_zoom: f32,
    theme: &Theme,
    fonts: &Fonts,
    items: &mut Vec<Placed>,
    hits: &mut Vec<HitTarget>,
) -> f32 {
    let sidebar_bg = theme.sidebar_bg;
    let border = theme.muted;

    // Background panel.
    items.push(Placed::Rect { x: 0.0, y: 0.0, w: width, h: height, color: sidebar_bg });
    // Right border.
    items.push(Placed::Rect { x: width - 1.0, y: 0.0, w: 1.0, h: height, color: border });

    let size = theme.body_size * 0.82 * sidebar_zoom;
    let row_h = size * 1.5;
    let top_pad = theme.margin_y * 0.5;
    let content_h = top_pad * 2.0 + row_h * tree.len() as f32;
    let mut y = top_pad - sidebar_scroll;

    // Keep names clear of the scrollbar strip when one is visible. Computed
    // once — depends on total content height, not per row.
    let right_inset = if content_h > height + 1.0 {
        crate::render::SIDEBAR_SB_HIT_W + crate::render::SIDEBAR_SB_RIGHT_PAD
    } else {
        0.0
    };

    for entry in tree {
        let row_y = y;
        y += row_h;
        // Cull rows fully outside the sidebar viewport; still advance `y`.
        if row_y + row_h < 0.0 || row_y > height {
            continue;
        }
        let rel_name = match entry.kind {
            TreeKind::Parent => "..".to_string(),
            _ => match entry.path.file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => entry.path.display().to_string(),
            },
        };
        let indent = 10.0 + (entry.depth as f32) * 14.0;
        let is_active = active.map(|a| a == entry.path.as_path()).unwrap_or(false);
        if is_active {
            items.push(Placed::Rect {
                x: 2.0,
                y: row_y,
                w: width - 4.0,
                h: row_h,
                color: theme.sidebar_active_bg,
            });
        }

        let marker = match entry.kind {
            TreeKind::Folder => if entry.expanded { "▾" } else { "▸" },
            TreeKind::Markdown => " ",
            TreeKind::Parent => "↑",
        };
        let baseline = row_y + row_h * 0.5 + size * 0.35;
        let muted = theme.muted;
        let fg = theme.fg;

        let mut x = indent;
        for ch in marker.chars() {
            let m = fonts.body.metrics(ch, size);
            items.push(Placed::Glyph {
                ch,
                font: FontId::Body,
                size,
                x,
                baseline,
                color: muted,
                selectable: true,
            });
            x += m.advance_width;
        }
        x += size * 0.25;

        let font_id = match entry.kind {
            TreeKind::Folder => FontId::Bold,
            _ => FontId::Body,
        };
        let color = match entry.kind {
            TreeKind::Folder => fg,
            TreeKind::Parent => muted,
            TreeKind::Markdown if is_active => theme.accent,
            TreeKind::Markdown => fg,
        };
        let font = pick_font(fonts, font_id);
        // Truncate name to fit; reserve the scrollbar strip so the ellipsis
        // doesn't overdraw the thumb.
        let max_name_x = width - 8.0 - right_inset;
        for ch in rel_name.chars() {
            let (glyph_font, fid) = if crate::font::is_emoji(ch) {
                (&fonts.emoji, FontId::Emoji)
            } else {
                (font, font_id)
            };
            let m = glyph_font.metrics(ch, size);
            if x + m.advance_width > max_name_x {
                let e = font.metrics('…', size);
                items.push(Placed::Glyph {
                    ch: '…',
                    font: font_id,
                    size,
                    x,
                    baseline,
                    color,
                    selectable: true,
                });
                x += e.advance_width;
                break;
            }
            items.push(Placed::Glyph {
                ch,
                font: fid,
                size,
                x,
                baseline,
                color,
                selectable: true,
            });
            x += m.advance_width;
        }

        let action = match entry.kind {
            TreeKind::Folder => HitAction::Toggle(entry.path.clone()),
            TreeKind::Markdown => HitAction::Open(entry.path.clone()),
            TreeKind::Parent => HitAction::SetRoot(entry.path.clone()),
        };
        // Clip hit-target to the visible strip so a partial row doesn't grab
        // clicks outside the sidebar. If there's a scrollbar, shrink the
        // row's right edge so clicks on the scrollbar don't fall through
        // to the tree row underneath.
        let clipped_top = row_y.max(0.0);
        let clipped_bot = (row_y + row_h).min(height);
        let row_w = (width - right_inset).max(1.0);
        if clipped_bot > clipped_top {
            hits.push(HitTarget {
                x: 0.0,
                y: clipped_top,
                w: row_w,
                h: clipped_bot - clipped_top,
                action,
            });
        }

    }

    // Thin scrollbar stripe on the sidebar's right edge. Geometry comes
    // from `render::sidebar_scrollbar_geom` so hit-testing and drawing
    // stay in sync.
    if let Some(g) = crate::render::sidebar_scrollbar_geom(width, height, sidebar_scroll, content_h) {
        let track_color = [theme.muted[0], theme.muted[1], theme.muted[2], 30];
        let thumb_color = [theme.muted[0], theme.muted[1], theme.muted[2], 170];
        items.push(Placed::Rect {
            x: g.track_x, y: g.track_y, w: g.track_w, h: g.track_h,
            color: track_color,
        });
        items.push(Placed::Rect {
            x: g.track_x, y: g.thumb_y, w: g.track_w, h: g.thumb_h,
            color: thumb_color,
        });
    }

    content_h
}

// ───── main content layout ──────────────────────────────────────────────────

struct Ctx<'a> {
    items: &'a mut Vec<Placed>,
    content_hits: &'a mut Vec<HitTarget>,
    copy_zones: &'a mut Vec<CopyZone>,
    outline: &'a mut Vec<OutlineEntry>,
    y: f32,
    content_left: f32,
    content_right: f32,
    theme: &'a Theme,
    fonts: &'a Fonts,
    base_dir: Option<&'a Path>,
    images: &'a mut ImageCache,
}

impl<'a> Ctx<'a> {
    fn block(&mut self, block: &Block, indent: f32) {
        match block {
            Block::Heading { level, inlines } => {
                self.y += self.theme.body_size * 0.8;
                let heading_start_y = self.y;
                let size = heading_size(self.theme.heading_size, *level);
                let style = Style {
                    bold: true,
                    italic: false,
                    mono: false,
                    size,
                    color: self.theme.fg,
                    underline: false,
                };
                self.paragraph(inlines, style, indent);
                self.y += size * 0.15;
                self.outline.push(OutlineEntry {
                    level: *level,
                    text: inlines_to_plain(inlines),
                    doc_y: heading_start_y,
                });
            }
            Block::Paragraph(inlines) => {
                if let Some((alt, src)) = single_image(inlines) {
                    if self.emit_block_image(alt, src, indent) {
                        self.y += self.theme.body_size * 0.55;
                        return;
                    }
                }
                let style = body_style(self.theme);
                self.paragraph(inlines, style, indent);
                self.y += self.theme.body_size * 0.55;
            }
            Block::CodeBlock { text, .. } => {
                let pad = 12.0;
                let size = self.theme.mono_size;
                let lh = size * self.theme.line_height_mult;
                let mut lines: Vec<&str> = text.lines().collect();
                if lines.is_empty() {
                    lines.push("");
                }
                let start_y = self.y;
                let rect_x = self.content_left + indent;
                let rect_w = (self.content_right - rect_x).max(1.0);
                let rect_h = lines.len() as f32 * lh + pad * 2.0;
                self.items.push(Placed::Rect {
                    x: rect_x,
                    y: start_y,
                    w: rect_w,
                    h: rect_h,
                    color: self.theme.code_bg,
                });
                let mut baseline = start_y + pad + size;
                for line in &lines {
                    let mut x = rect_x + pad;
                    for ch in line.chars() {
                        let m = self.fonts.mono.metrics(ch, size);
                        self.items.push(Placed::Glyph {
                            ch,
                            font: FontId::Mono,
                            size,
                            x,
                            baseline,
                            color: self.theme.accent,
                            selectable: true,
                        });
                        x += m.advance_width;
                    }
                    baseline += lh;
                }

                // Right-click anywhere over the block to copy — no visible
                // button. The context menu picks it up via copy_zones.
                self.copy_zones.push(CopyZone {
                    x: rect_x,
                    y: start_y,
                    w: rect_w,
                    h: rect_h,
                    kind: CopyKind::Code,
                    text: text.clone(),
                });

                self.y = start_y + rect_h + self.theme.body_size * 0.5;
            }
            Block::List { ordered, items } => {
                let indent_step = 28.0;
                let size = self.theme.body_size;
                let lh = size * self.theme.line_height_mult;
                for (idx, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}.", idx + 1)
                    } else {
                        "•".to_string()
                    };
                    let marker_x = self.content_left + indent;
                    let baseline = self.y + size;
                    let mut mx = marker_x;
                    for ch in marker.chars() {
                        let m = self.fonts.body.metrics(ch, size);
                        self.items.push(Placed::Glyph {
                            ch,
                            font: FontId::Body,
                            size,
                            x: mx,
                            baseline,
                            color: self.theme.muted,
                            selectable: true,
                        });
                        mx += m.advance_width;
                    }
                    let style = body_style(self.theme);
                    let before_y = self.y;
                    self.paragraph(item, style, indent + indent_step);
                    if self.y <= before_y {
                        self.y = before_y + lh;
                    }
                    self.y += size * 0.12;
                }
                self.y += size * 0.4;
            }
            Block::BlockQuote(inner) => {
                let start_y = self.y;
                for b in inner {
                    self.block(b, indent + 20.0);
                }
                let end_y = self.y;
                self.items.push(Placed::Rect {
                    x: self.content_left + indent + 4.0,
                    y: start_y,
                    w: 3.0,
                    h: (end_y - start_y).max(1.0),
                    color: self.theme.muted,
                });
            }
            Block::Table { header, align, rows } => {
                self.table(header, align, rows, indent);
            }
            Block::Mermaid(src) => {
                let avail = (self.content_right - self.content_left - indent).max(1.0);
                let Some(mut r) = crate::mermaid::render(src, avail, self.theme, self.fonts) else {
                    // parse failed — fall back to a plain code block.
                    self.block(
                        &Block::CodeBlock { lang: Some("mermaid".into()), text: src.clone() },
                        indent,
                    );
                    return;
                };
                // Padding around the diagram and horizontal centering.
                let pad = 16.0;
                let outer_w = r.width + pad * 2.0;
                let x0 = self.content_left + indent + ((avail - outer_w) / 2.0).max(0.0);
                let y0 = self.y + pad;
                self.items.push(Placed::Rect {
                    x: x0,
                    y: self.y,
                    w: outer_w.min(avail),
                    h: r.height + pad * 2.0,
                    color: self.theme.code_bg,
                });
                for item in r.items.drain(..) {
                    self.items.push(shift_placed(item, x0 + pad, y0));
                }
                self.y = y0 + r.height + pad + self.theme.body_size * 0.5;
            }
            Block::DisplayMath(src) => {
                let size = self.theme.body_size * 1.15;
                let b = math::layout(src, size, self.fonts);
                let avail = (self.content_right - self.content_left - indent).max(1.0);
                let scale = if b.width > avail { avail / b.width } else { 1.0 };
                let w = b.width * scale;
                let x0 = self.content_left + indent + (avail - w) / 2.0;
                let baseline = self.y + b.ascent * scale + self.theme.body_size * 0.4;
                for g in &b.glyphs {
                    self.items.push(Placed::Glyph {
                        ch: g.ch,
                        font: g.font,
                        size: g.size * scale,
                        x: x0 + g.x * scale,
                        baseline: baseline + g.y * scale,
                        color: self.theme.fg,
                        selectable: true,
                    });
                }
                for r in &b.rules {
                    self.items.push(Placed::Rect {
                        x: x0 + r.x * scale,
                        y: baseline + r.y * scale,
                        w: r.w * scale,
                        h: r.h * scale,
                        color: self.theme.fg,
                    });
                }
                self.y = baseline + b.descent * scale + self.theme.body_size * 0.6;
            }
            Block::ThematicBreak => {
                let y = self.y + self.theme.body_size * 0.6;
                self.items.push(Placed::Rect {
                    x: self.content_left,
                    y,
                    w: (self.content_right - self.content_left).max(1.0),
                    h: 1.0,
                    color: self.theme.muted,
                });
                self.y = y + 1.0 + self.theme.body_size * 0.8;
            }
        }
    }

    fn table(
        &mut self,
        header: &[Vec<Inline>],
        align: &[Align],
        rows: &[Vec<Vec<Inline>>],
        indent: f32,
    ) {
        if header.is_empty() {
            return;
        }
        let cols = header.len();
        let pad_x = 12.0;
        let pad_y = 8.0;
        let line_color = self.theme.muted;
        // Header fill follows the theme so the table styling survives
        // the dark-mode switch. code_bg is the "slightly recessed" color in
        // both palettes — same role we want here.
        let header_bg: Rgba = self.theme.code_bg;
        let stroke: Rgba = [line_color[0], line_color[1], line_color[2], 180];

        // Natural width for each column = max of (header, body) single-line widths.
        let base = body_style(self.theme);
        let mut natural: Vec<f32> = vec![0.0; cols];
        for c in 0..cols {
            let h_style = Style { bold: true, ..base };
            natural[c] = natural[c].max(measure_inline_run(&header[c], h_style, self.fonts, self.theme));
            for row in rows {
                if let Some(cell) = row.get(c) {
                    natural[c] = natural[c].max(
                        measure_inline_run(cell, base, self.fonts, self.theme),
                    );
                }
            }
            natural[c] += pad_x * 2.0;
        }

        // Fit into available width. Cap any single column so super-wide cells
        // don't starve the rest.
        let avail = (self.content_right - self.content_left - indent).max(1.0);
        let cap = avail * 0.6;
        for w in natural.iter_mut() {
            *w = w.min(cap.max(80.0));
        }
        let total: f32 = natural.iter().sum();
        let col_w: Vec<f32> = if total > avail {
            let scale = avail / total;
            natural.iter().map(|w| w * scale).collect()
        } else if total > 0.0 && total < avail {
            // Distribute extra width proportionally, up to the available.
            let scale = avail / total;
            let scale = scale.min(1.4); // don't balloon tiny tables
            natural.iter().map(|w| w * scale).collect()
        } else {
            natural.clone()
        };

        let table_w: f32 = col_w.iter().sum();
        let x0 = self.content_left + indent;

        let header_style = Style { bold: true, ..base };
        let y_start = self.y + self.theme.body_size * 0.2;
        let mut y = y_start;

        // ── header row ──
        let header_h = self.row_height(header, align, &col_w, header_style, pad_y);
        self.items.push(Placed::Rect {
            x: x0, y, w: table_w, h: header_h, color: header_bg,
        });
        self.draw_row(header, align, &col_w, header_style, x0, y, header_h, pad_x, pad_y);
        y += header_h;
        // border under header
        self.items.push(Placed::Rect { x: x0, y, w: table_w, h: 1.5, color: stroke });
        y += 1.5;

        // ── body rows ──
        for row in rows {
            let row_h = self.row_height(row, align, &col_w, base, pad_y);
            self.draw_row(row, align, &col_w, base, x0, y, row_h, pad_x, pad_y);
            y += row_h;
            self.items.push(Placed::Rect { x: x0, y, w: table_w, h: 1.0, color: [stroke[0], stroke[1], stroke[2], 90] });
            y += 1.0;
        }

        // ── vertical dividers + outer border ──
        let col_thin: Rgba = [stroke[0], stroke[1], stroke[2], 70];
        let mut cx = x0;
        self.items.push(Placed::Rect { x: cx, y: y_start, w: 1.0, h: y - y_start, color: col_thin });
        for w in &col_w {
            cx += *w;
            self.items.push(Placed::Rect { x: cx - 0.5, y: y_start, w: 1.0, h: y - y_start, color: col_thin });
        }

        // Right-click anywhere over the table to copy as CSV — the
        // context menu picks this up via copy_zones.
        self.copy_zones.push(CopyZone {
            x: x0,
            y: y_start,
            w: table_w,
            h: y - y_start,
            kind: CopyKind::Csv,
            text: table_to_csv(header, rows),
        });

        self.y = y + self.theme.body_size * 0.4;
    }

    fn row_height(
        &self,
        row: &[Vec<Inline>],
        _align: &[Align],
        col_w: &[f32],
        style: Style,
        pad_y: f32,
    ) -> f32 {
        let mut max_h: f32 = style.size * self.theme.line_height_mult;
        for (c, cell) in row.iter().enumerate() {
            let w = col_w.get(c).copied().unwrap_or(0.0);
            let inner_w = (w - 24.0).max(40.0);
            let lines = count_wrapped_lines(cell, style, inner_w, self.fonts, self.theme);
            let h = (lines as f32) * style.size * self.theme.line_height_mult;
            if h > max_h {
                max_h = h;
            }
        }
        max_h + pad_y * 2.0
    }

    fn draw_row(
        &mut self,
        row: &[Vec<Inline>],
        align: &[Align],
        col_w: &[f32],
        style: Style,
        x0: f32,
        y: f32,
        row_h: f32,
        pad_x: f32,
        pad_y: f32,
    ) {
        let mut cell_x = x0;
        for (c, cell) in row.iter().enumerate() {
            let Some(&w) = col_w.get(c) else { break };
            let cell_w_inner = (w - pad_x * 2.0).max(1.0);
            let a = align.get(c).copied().unwrap_or(Align::Left);
            let cell_top = y + pad_y;
            self.draw_cell_inlines(cell, style, cell_x + pad_x, cell_w_inner, cell_top, a);
            cell_x += w;
            let _ = row_h;
        }
    }

    fn draw_cell_inlines(
        &mut self,
        inlines: &[Inline],
        base: Style,
        left: f32,
        width: f32,
        y_top: f32,
        align: Align,
    ) {
        let mut collector = WordCollector::new(base);
        for i in inlines {
            collector.walk(i, base, self.fonts, self.theme);
        }
        let words = collector.finish();
        if words.is_empty() {
            return;
        }

        // Wrap into lines greedily.
        let mut lines: Vec<Vec<usize>> = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        let mut cur_w = 0.0;
        for (i, w) in words.iter().enumerate() {
            let sw = match &w.payload {
                WordPayload::Text { style, .. } => space_advance(self.fonts, *style),
                WordPayload::Math(_) => self.theme.body_size * 0.3,
            };
            let lead = !cur.is_empty() && w.leading_space;
            let gap = if lead { sw } else { 0.0 };
            if !cur.is_empty() && cur_w + gap + w.width > width {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
            if !cur.is_empty() && w.leading_space {
                cur_w += sw;
            }
            cur.push(i);
            cur_w += w.width;
        }
        if !cur.is_empty() {
            lines.push(cur);
        }

        // Place each line with alignment.
        let lh = base.size * self.theme.line_height_mult;
        let mut baseline = y_top + base.size;
        for line in &lines {
            let line_w = line_width(&words, line, self.fonts);
            let x_start = match align {
                Align::Left => left,
                Align::Center => left + (width - line_w) / 2.0,
                Align::Right => left + width - line_w,
            };
            let mut pen = x_start;
            for (pos, &idx) in line.iter().enumerate() {
                let word = &words[idx];
                if pos > 0 && word.leading_space {
                    let sw = match &word.payload {
                        WordPayload::Text { style, .. } => space_advance(self.fonts, *style),
                        WordPayload::Math(_) => self.theme.body_size * 0.3,
                    };
                    pen += sw;
                }
                match &word.payload {
                    WordPayload::Text { glyphs, style } => {
                        let start_x = pen;
                        // Emit a link hit-target for this word so clicks /
                        // hovers inside table cells work just like in
                        // paragraphs. Use the line's y_top (ascent/descent
                        // aren't tracked here, approximate via base.size).
                        let line_top = baseline - base.size * 0.85;
                        let line_h = base.size * 1.1;
                        if let Some(href) = &word.link_href {
                            self.content_hits.push(HitTarget {
                                x: start_x,
                                y: line_top,
                                w: word.width,
                                h: line_h,
                                action: HitAction::OpenUrl(href.as_ref().to_string()),
                            });
                        }
                        for g in glyphs {
                            let fid = g.font.unwrap_or_else(|| style.font_id());
                            self.items.push(Placed::Glyph {
                                ch: g.ch,
                                font: fid,
                                size: style.size,
                                x: pen,
                                baseline,
                                color: style.color,
                                selectable: true,
                            });
                            pen += g.advance;
                        }
                        if style.underline {
                            self.items.push(Placed::Underline {
                                x: start_x, y: baseline + 2.0, w: word.width, color: style.color,
                            });
                        }
                    }
                    WordPayload::Math(mb) => {
                        for g in &mb.glyphs {
                            self.items.push(Placed::Glyph {
                                ch: g.ch, font: g.font, size: g.size,
                                x: pen + g.x, baseline: baseline + g.y,
                                color: self.theme.fg,
                                selectable: true,
                            });
                        }
                        for r in &mb.rules {
                            self.items.push(Placed::Rect {
                                x: pen + r.x, y: baseline + r.y,
                                w: r.w, h: r.h, color: self.theme.fg,
                            });
                        }
                        pen += mb.width;
                    }
                }
            }
            baseline += lh;
        }
    }

    /// Try to place an image as a block element. Returns true if placed;
    /// false if the image could not be loaded (caller falls back to alt text).
    fn emit_block_image(&mut self, alt: &str, src: &str, indent: f32) -> bool {
        let Some(resolved) = ImageCache::resolve(src, self.base_dir) else {
            return false;
        };
        let Some(data) = self.images.get_or_load(&resolved) else {
            return false;
        };
        let avail_w = (self.content_right - self.content_left - indent).max(1.0);
        let (nat_w, nat_h) = (data.width as f32, data.height as f32);
        let (w, h) = if nat_w > avail_w {
            let scale = avail_w / nat_w;
            (avail_w, nat_h * scale)
        } else {
            (nat_w, nat_h)
        };
        let x = self.content_left + indent;
        let y = self.y;
        self.items.push(Placed::Image { x, y, w, h, data });
        self.y = y + h;
        let _ = alt; // alt is used for accessibility / fallback only.
        true
    }

    fn paragraph(&mut self, inlines: &[Inline], base: Style, indent: f32) {
        let left = self.content_left + indent;
        let right = self.content_right;
        let avail = (right - left).max(1.0);

        let mut collector = WordCollector::new(base);
        for i in inlines {
            collector.walk(i, base, self.fonts, self.theme);
        }
        let words = collector.finish();
        if words.is_empty() {
            return;
        }

        // One line at a time. Collect (word, x_offset) pairs, then at flush
        // time compute line_ascent/line_descent (so math can be taller than
        // body text) and emit Placed items with the right baseline.
        let mut line: Vec<(usize, f32)> = Vec::new(); // (word index, x offset from left)
        let mut pen = 0.0f32;
        let mut y_top = self.y;

        let emit_line = |ctx: &mut Ctx,
                         line: &mut Vec<(usize, f32)>,
                         words: &[Word],
                         left: f32,
                         y_top: f32|
         -> f32 {
            if line.is_empty() {
                return y_top;
            }
            let ascent = line
                .iter()
                .map(|(idx, _)| words[*idx].ascent)
                .fold(0.0f32, f32::max);
            let descent = line
                .iter()
                .map(|(idx, _)| words[*idx].descent)
                .fold(0.0f32, f32::max);
            let baseline = y_top + ascent;
            for (idx, ox) in line.iter() {
                let word = &words[*idx];
                let wx = left + *ox;
                if let Some(href) = &word.link_href {
                    ctx.content_hits.push(HitTarget {
                        x: wx,
                        y: y_top,
                        w: word.width,
                        h: ascent + descent,
                        action: HitAction::OpenUrl(href.as_ref().to_string()),
                    });
                }
                match &word.payload {
                    WordPayload::Text { glyphs, style } => {
                        let mut gx = wx;
                        for g in glyphs {
                            let fid = g.font.unwrap_or_else(|| style.font_id());
                            ctx.items.push(Placed::Glyph {
                                ch: g.ch,
                                font: fid,
                                size: style.size,
                                x: gx,
                                baseline,
                                color: style.color,
                                selectable: true,
                            });
                            gx += g.advance;
                        }
                        if style.underline {
                            ctx.items.push(Placed::Underline {
                                x: wx,
                                y: baseline + 2.0,
                                w: word.width,
                                color: style.color,
                            });
                        }
                    }
                    WordPayload::Math(mb) => {
                        for g in &mb.glyphs {
                            ctx.items.push(Placed::Glyph {
                                ch: g.ch,
                                font: g.font,
                                size: g.size,
                                x: wx + g.x,
                                baseline: baseline + g.y,
                                color: ctx.theme.fg,
                                selectable: true,
                            });
                        }
                        for r in &mb.rules {
                            ctx.items.push(Placed::Rect {
                                x: wx + r.x,
                                y: baseline + r.y,
                                w: r.w,
                                h: r.h,
                                color: ctx.theme.fg,
                            });
                        }
                    }
                }
            }
            line.clear();
            let line_gap = ctx.theme.body_size * (ctx.theme.line_height_mult - 1.0) * 0.6;
            baseline + descent + line_gap
        };

        for (idx, word) in words.iter().enumerate() {
            let sw = match &word.payload {
                WordPayload::Text { style, .. } => space_advance(self.fonts, *style),
                WordPayload::Math(_) => self.theme.body_size * 0.3,
            };
            let needs_space = !line.is_empty() && word.leading_space;
            let gap = if needs_space { sw } else { 0.0 };
            let projected = pen + gap + word.width;
            if !line.is_empty() && projected > avail {
                y_top = emit_line(self, &mut line, &words, left, y_top);
                pen = 0.0;
            } else if needs_space {
                pen += sw;
            }
            line.push((idx, pen));
            pen += word.width;
        }
        y_top = emit_line(self, &mut line, &words, left, y_top);
        self.y = y_top;
    }
}

/// Flatten an inline tree to plain text (for the outline / accessibility).
/// Render a table as RFC 4180 CSV. Each cell is flattened to plain text;
/// commas, quotes, and newlines force quoting with doubled-quote escape.
fn table_to_csv(header: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> String {
    fn escape(s: &str) -> String {
        if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for c in s.chars() {
                if c == '"' { out.push('"'); }
                out.push(c);
            }
            out.push('"');
            out
        } else {
            s.to_string()
        }
    }
    fn row_csv(cells: &[Vec<Inline>]) -> String {
        cells.iter()
            .map(|c| escape(inlines_to_plain(c).trim()))
            .collect::<Vec<_>>()
            .join(",")
    }
    let mut out = String::new();
    out.push_str(&row_csv(header));
    for row in rows {
        out.push('\n');
        out.push_str(&row_csv(row));
    }
    out
}

fn inlines_to_plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(s) => out.push_str(s),
            Inline::Code(s) => out.push_str(s),
            Inline::Math(s) => out.push_str(s),
            Inline::Bold(inner) | Inline::Italic(inner) => {
                out.push_str(&inlines_to_plain(inner));
            }
            Inline::Link { text, .. } => {
                out.push_str(&inlines_to_plain(text));
            }
            Inline::Image { alt, .. } => out.push_str(alt),
        }
    }
    out
}

/// If `inlines` reduces to a single Image (optionally surrounded by
/// whitespace-only Text runs), return (alt, src). Otherwise None.
fn single_image(inlines: &[Inline]) -> Option<(&str, &str)> {
    let mut found: Option<(&str, &str)> = None;
    for i in inlines {
        match i {
            Inline::Image { alt, src } => {
                if found.is_some() {
                    return None;
                }
                found = Some((alt.as_str(), src.as_str()));
            }
            Inline::Text(t) if t.trim().is_empty() => {}
            _ => return None,
        }
    }
    found
}

fn shift_placed(p: Placed, dx: f32, dy: f32) -> Placed {
    match p {
        Placed::Glyph { ch, font, size, x, baseline, color, selectable } => Placed::Glyph {
            ch, font, size,
            x: x + dx,
            baseline: baseline + dy,
            color,
            selectable,
        },
        Placed::Rect { x, y, w, h, color } => Placed::Rect { x: x + dx, y: y + dy, w, h, color },
        Placed::Underline { x, y, w, color } => Placed::Underline { x: x + dx, y: y + dy, w, color },
        Placed::Image { x, y, w, h, data } => Placed::Image { x: x + dx, y: y + dy, w, h, data },
        Placed::Line { x1, y1, x2, y2, thickness, color } => Placed::Line {
            x1: x1 + dx, y1: y1 + dy,
            x2: x2 + dx, y2: y2 + dy,
            thickness, color,
        },
        Placed::Triangle { p1, p2, p3, color } => Placed::Triangle {
            p1: (p1.0 + dx, p1.1 + dy),
            p2: (p2.0 + dx, p2.1 + dy),
            p3: (p3.0 + dx, p3.1 + dy),
            color,
        },
    }
}

fn heading_size(base: f32, level: u8) -> f32 {
    match level {
        1 => base,
        2 => base * 0.80,
        3 => base * 0.66,
        4 => base * 0.58,
        5 => base * 0.52,
        _ => base * 0.48,
    }
}

fn body_style(theme: &Theme) -> Style {
    Style {
        bold: false,
        italic: false,
        mono: false,
        size: theme.body_size,
        color: theme.fg,
        underline: false,
    }
}

// ───── word collection ──────────────────────────────────────────────────────

struct Glyph {
    ch: char,
    advance: f32,
    /// Per-glyph font override. If None, falls back to the word's
    /// style font. Emoji codepoints get routed here to FontId::Emoji
    /// regardless of the surrounding style.
    font: Option<FontId>,
}

pub enum WordPayload {
    Text { glyphs: Vec<Glyph>, style: Style },
    Math(MathBox),
}

struct Word {
    payload: WordPayload,
    width: f32,
    ascent: f32,
    descent: f32,
    leading_space: bool,
    /// If set, every glyph in this word is part of a markdown link and
    /// clicking anywhere on the word should open this URL.
    link_href: Option<std::rc::Rc<str>>,
}

struct WordCollector {
    out: Vec<Word>,
    cur: Vec<Glyph>,
    cur_width: f32,
    cur_style: Style,
    cur_leading_space: bool,
    pending_space: bool,
    /// Active link URL while walking inside an `Inline::Link`.
    cur_link: Option<std::rc::Rc<str>>,
}

impl WordCollector {
    fn new(base: Style) -> Self {
        Self {
            out: Vec::new(),
            cur: Vec::new(),
            cur_width: 0.0,
            cur_style: base,
            cur_leading_space: false,
            pending_space: false,
            cur_link: None,
        }
    }

    fn finish(mut self) -> Vec<Word> {
        self.flush();
        self.out
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            let size = self.cur_style.size;
            self.out.push(Word {
                payload: WordPayload::Text {
                    glyphs: std::mem::take(&mut self.cur),
                    style: self.cur_style,
                },
                width: std::mem::take(&mut self.cur_width),
                ascent: size * 0.85,
                descent: size * 0.25,
                leading_space: self.cur_leading_space,
                link_href: self.cur_link.clone(),
            });
            self.cur_leading_space = false;
        }
    }

    fn ensure_style(&mut self, style: Style) {
        if !self.cur.is_empty()
            && (self.cur_style.font_id() != style.font_id()
                || self.cur_style.size != style.size
                || self.cur_style.color != style.color
                || self.cur_style.underline != style.underline)
        {
            self.flush();
        }
        self.cur_style = style;
    }

    fn emit_text(&mut self, s: &str, style: Style, fonts: &Fonts) {
        self.ensure_style(style);
        for ch in s.chars() {
            if ch == ' ' || ch == '\t' || ch == '\n' {
                self.flush();
                self.pending_space = true;
                continue;
            }
            if self.cur.is_empty() {
                self.cur_leading_space = self.pending_space;
                self.pending_space = false;
                self.cur_style = style;
            }
            let (g_font, fid) = if crate::font::is_emoji(ch) {
                (&fonts.emoji, Some(FontId::Emoji))
            } else {
                (pick_font(fonts, style.font_id()), None)
            };
            let m = g_font.metrics(ch, style.size);
            self.cur.push(Glyph {
                ch,
                advance: m.advance_width,
                font: fid,
            });
            self.cur_width += m.advance_width;
        }
    }

    fn walk(&mut self, inline: &Inline, base: Style, fonts: &Fonts, theme: &Theme) {
        match inline {
            Inline::Text(s) => self.emit_text(s, base, fonts),
            Inline::Bold(inner) => {
                let s = Style { bold: true, ..base };
                for ii in inner { self.walk(ii, s, fonts, theme); }
            }
            Inline::Italic(inner) => {
                let s = Style { italic: true, ..base };
                for ii in inner { self.walk(ii, s, fonts, theme); }
            }
            Inline::Code(s) => {
                let st = Style {
                    mono: true,
                    color: theme.accent,
                    size: base.size * 0.92,
                    underline: false,
                    ..base
                };
                self.emit_text(s, st, fonts);
            }
            Inline::Link { text, href } => {
                // Flush anything pending so the word preceding the link
                // doesn't accidentally inherit link_href.
                self.flush();
                let prev_link = self.cur_link.take();
                self.cur_link = Some(std::rc::Rc::from(href.as_str()));
                let s = Style { color: theme.accent, underline: true, ..base };
                for ii in text { self.walk(ii, s, fonts, theme); }
                self.flush();
                self.cur_link = prev_link;
            }
            Inline::Image { alt, .. } => {
                let label = format!("[image: {}]", alt);
                let s = Style { italic: true, color: theme.muted, underline: false, ..base };
                self.emit_text(&label, s, fonts);
            }
            Inline::Math(src) => {
                self.flush();
                let size = base.size;
                let mb = math::layout(src, size, fonts);
                let width = mb.width;
                let ascent = mb.ascent;
                let descent = mb.descent;
                let leading_space = self.pending_space;
                self.pending_space = false;
                self.out.push(Word {
                    payload: WordPayload::Math(mb),
                    width,
                    ascent,
                    descent,
                    leading_space,
                    link_href: self.cur_link.clone(),
                });
            }
        }
    }
}

fn space_advance(fonts: &Fonts, style: Style) -> f32 {
    pick_font(fonts, style.font_id()).metrics(' ', style.size).advance_width
}

/// Measure the single-line natural width of an inline sequence in the given
/// base style. Used for table column sizing.
fn measure_inline_run(inlines: &[Inline], base: Style, fonts: &Fonts, theme: &Theme) -> f32 {
    let mut collector = WordCollector::new(base);
    for i in inlines {
        collector.walk(i, base, fonts, theme);
    }
    let words = collector.finish();
    let mut total = 0.0;
    for (i, w) in words.iter().enumerate() {
        if i > 0 && w.leading_space {
            let sw = match &w.payload {
                WordPayload::Text { style, .. } => space_advance(fonts, *style),
                WordPayload::Math(_) => theme.body_size * 0.3,
            };
            total += sw;
        }
        total += w.width;
    }
    total
}

/// Count how many lines a cell's inlines wrap to at the given width.
fn count_wrapped_lines(
    inlines: &[Inline],
    base: Style,
    width: f32,
    fonts: &Fonts,
    theme: &Theme,
) -> usize {
    let mut collector = WordCollector::new(base);
    for i in inlines {
        collector.walk(i, base, fonts, theme);
    }
    let words = collector.finish();
    if words.is_empty() {
        return 1;
    }
    let mut lines = 1;
    let mut cur_w: f32 = 0.0;
    let mut first_on_line = true;
    for w in &words {
        let sw = match &w.payload {
            WordPayload::Text { style, .. } => space_advance(fonts, *style),
            WordPayload::Math(_) => theme.body_size * 0.3,
        };
        let gap = if !first_on_line && w.leading_space { sw } else { 0.0 };
        if !first_on_line && cur_w + gap + w.width > width {
            lines += 1;
            cur_w = 0.0;
            first_on_line = true;
        }
        if !first_on_line && w.leading_space {
            cur_w += sw;
        }
        cur_w += w.width;
        first_on_line = false;
    }
    lines
}

fn line_width(words: &[Word], indices: &[usize], fonts: &Fonts) -> f32 {
    let mut w: f32 = 0.0;
    for (pos, &idx) in indices.iter().enumerate() {
        let word = &words[idx];
        if pos > 0 && word.leading_space {
            let sw = match &word.payload {
                WordPayload::Text { style, .. } => space_advance(fonts, *style),
                WordPayload::Math(_) => 6.0,
            };
            w += sw;
        }
        w += word.width;
    }
    w
}
