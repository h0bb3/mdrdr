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
use crate::md::{Block, Inline};
use crate::theme::{Rgba, Theme};
use crate::tree::{TreeEntry, TreeKind};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FontId {
    Body,
    Bold,
    Italic,
    BoldItalic,
    Mono,
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
}

#[derive(Debug, Clone)]
pub struct HitTarget {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: HitAction,
}

pub struct Layout {
    pub content_items: Vec<Placed>,
    pub pinned_items: Vec<Placed>,
    pub hit_targets: Vec<HitTarget>,
    pub doc_height: f32,
    pub sidebar_width: f32,
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
}

pub fn layout(input: LayoutInput, images: &mut ImageCache) -> Layout {
    let sidebar_width = if input.tree.is_some() && input.sidebar_width > 0.0 {
        input.sidebar_width
    } else {
        0.0
    };

    let content_left = sidebar_width + input.theme.margin_x;
    let content_right = (input.viewport_w as f32) - input.theme.margin_x;

    let mut content_items: Vec<Placed> = Vec::new();
    let doc_height = {
        let mut ctx = Ctx {
            items: &mut content_items,
            y: input.theme.margin_y,
            content_left,
            content_right,
            theme: input.theme,
            fonts: input.fonts,
            base_dir: input.base_dir,
            images,
        };
        for b in input.blocks {
            ctx.block(b, 0.0);
        }
        ctx.y + input.theme.margin_y
    };

    let mut pinned_items = Vec::new();
    let mut hit_targets = Vec::new();
    if let (Some(tree), true) = (input.tree, sidebar_width > 0.0) {
        layout_sidebar(
            tree,
            input.active_path,
            sidebar_width,
            input.viewport_h as f32,
            input.theme,
            input.fonts,
            &mut pinned_items,
            &mut hit_targets,
        );
    }

    Layout {
        content_items,
        pinned_items,
        hit_targets,
        doc_height,
        sidebar_width,
    }
}

// ───── sidebar layout ───────────────────────────────────────────────────────

fn layout_sidebar(
    tree: &[TreeEntry],
    active: Option<&Path>,
    width: f32,
    height: f32,
    theme: &Theme,
    fonts: &Fonts,
    items: &mut Vec<Placed>,
    hits: &mut Vec<HitTarget>,
) {
    let sidebar_bg = [0xf3, 0xef, 0xe5, 0xff];
    let border = theme.muted;

    // Background panel.
    items.push(Placed::Rect { x: 0.0, y: 0.0, w: width, h: height, color: sidebar_bg });
    // Right border.
    items.push(Placed::Rect { x: width - 1.0, y: 0.0, w: 1.0, h: height, color: border });

    let size = theme.body_size * 0.82;
    let row_h = size * 1.5;
    let mut y = theme.margin_y * 0.5;

    for entry in tree {
        let rel_name = match entry.path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => entry.path.display().to_string(),
        };
        let indent = 10.0 + (entry.depth as f32) * 14.0;
        let is_active = active.map(|a| a == entry.path.as_path()).unwrap_or(false);

        let row_y = y;
        if is_active {
            items.push(Placed::Rect {
                x: 2.0,
                y: row_y,
                w: width - 4.0,
                h: row_h,
                color: [0xe3, 0xdc, 0xc9, 0xff],
            });
        }

        let marker = match entry.kind {
            TreeKind::Folder => if entry.expanded { "▾" } else { "▸" },
            TreeKind::Markdown => " ",
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
            });
            x += m.advance_width;
        }
        x += size * 0.25;

        let font_id = if entry.kind == TreeKind::Folder {
            FontId::Bold
        } else {
            FontId::Body
        };
        let color = match entry.kind {
            TreeKind::Folder => fg,
            TreeKind::Markdown if is_active => theme.accent,
            TreeKind::Markdown => fg,
        };
        let font = pick_font(fonts, font_id);
        // Truncate name to fit.
        let max_name_x = width - 8.0;
        for ch in rel_name.chars() {
            let m = font.metrics(ch, size);
            if x + m.advance_width > max_name_x {
                // draw ellipsis and stop
                let e = font.metrics('…', size);
                items.push(Placed::Glyph {
                    ch: '…',
                    font: font_id,
                    size,
                    x,
                    baseline,
                    color,
                });
                x += e.advance_width;
                break;
            }
            items.push(Placed::Glyph {
                ch,
                font: font_id,
                size,
                x,
                baseline,
                color,
            });
            x += m.advance_width;
        }

        let action = match entry.kind {
            TreeKind::Folder => HitAction::Toggle(entry.path.clone()),
            TreeKind::Markdown => HitAction::Open(entry.path.clone()),
        };
        hits.push(HitTarget {
            x: 0.0,
            y: row_y,
            w: width,
            h: row_h,
            action,
        });

        y += row_h;
        if y > height {
            break; // don't render rows below viewport for M3
        }
    }
}

// ───── main content layout ──────────────────────────────────────────────────

struct Ctx<'a> {
    items: &'a mut Vec<Placed>,
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
                        });
                        x += m.advance_width;
                    }
                    baseline += lh;
                }
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
                    color: [0xf6, 0xf2, 0xe9, 0xff],
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
                match &word.payload {
                    WordPayload::Text { glyphs, style } => {
                        let mut gx = wx;
                        for g in glyphs {
                            ctx.items.push(Placed::Glyph {
                                ch: g.ch,
                                font: style.font_id(),
                                size: style.size,
                                x: gx,
                                baseline,
                                color: style.color,
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
        Placed::Glyph { ch, font, size, x, baseline, color } => Placed::Glyph {
            ch, font, size,
            x: x + dx,
            baseline: baseline + dy,
            color,
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
}

struct WordCollector {
    out: Vec<Word>,
    cur: Vec<Glyph>,
    cur_width: f32,
    cur_style: Style,
    cur_leading_space: bool,
    pending_space: bool,
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
        let font = pick_font(fonts, style.font_id());
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
            let m = font.metrics(ch, style.size);
            self.cur.push(Glyph { ch, advance: m.advance_width });
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
            Inline::Link { text, href: _ } => {
                let s = Style { color: theme.accent, underline: true, ..base };
                for ii in text { self.walk(ii, s, fonts, theme); }
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
                });
            }
        }
    }
}

fn space_advance(fonts: &Fonts, style: Style) -> f32 {
    pick_font(fonts, style.font_id()).metrics(' ', style.size).advance_width
}
