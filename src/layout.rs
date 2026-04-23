//! Layout engine — walks the parsed block tree and produces a flat list of
//! positioned draw items in absolute document coordinates. The render pass
//! applies scroll and clips; nothing in here cares about scroll or viewport
//! height (only width).

use fontdue::Font;

use crate::font::Fonts;
use crate::md::{Block, Inline};
use crate::theme::{Rgba, Theme};

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
}

pub struct Layout {
    pub items: Vec<Placed>,
    pub doc_height: f32,
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

pub fn layout(blocks: &[Block], viewport_w: u32, theme: &Theme, fonts: &Fonts) -> Layout {
    let mut ctx = Ctx {
        items: Vec::new(),
        y: theme.margin_y,
        viewport_w: viewport_w as f32,
        theme,
        fonts,
    };
    for b in blocks {
        ctx.block(b, 0.0);
    }
    let doc_height = ctx.y + theme.margin_y;
    Layout { items: ctx.items, doc_height }
}

struct Ctx<'a> {
    items: Vec<Placed>,
    y: f32,
    viewport_w: f32,
    theme: &'a Theme,
    fonts: &'a Fonts,
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
                let rect_x = self.theme.margin_x + indent;
                let rect_w = (self.viewport_w - self.theme.margin_x * 2.0 - indent).max(1.0);
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
                    let marker_x = self.theme.margin_x + indent;
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
                    x: self.theme.margin_x + indent + 4.0,
                    y: start_y,
                    w: 3.0,
                    h: (end_y - start_y).max(1.0),
                    color: self.theme.muted,
                });
            }
            Block::ThematicBreak => {
                let y = self.y + self.theme.body_size * 0.6;
                self.items.push(Placed::Rect {
                    x: self.theme.margin_x,
                    y,
                    w: (self.viewport_w - self.theme.margin_x * 2.0).max(1.0),
                    h: 1.0,
                    color: self.theme.muted,
                });
                self.y = y + 1.0 + self.theme.body_size * 0.8;
            }
        }
    }

    /// Layout `inlines` as a flowing paragraph. Word-wrap, accumulate placed
    /// glyphs, advance self.y by the lines consumed.
    fn paragraph(&mut self, inlines: &[Inline], base: Style, indent: f32) {
        let left = self.theme.margin_x + indent;
        let right = self.viewport_w - self.theme.margin_x;
        let avail = (right - left).max(1.0);

        let mut collector = WordCollector::new(base);
        for i in inlines {
            collector.walk(i, base, self.fonts, self.theme);
        }
        let words = collector.finish();
        if words.is_empty() {
            return;
        }

        let mut pen_x = left;
        let mut line_width: f32 = 0.0;
        let mut line_items: Vec<Placed> = Vec::new();
        let mut line_max_size = base.size;
        let mut baseline = self.y + base.size;

        for word in &words {
            let needs_space = !line_items.is_empty() && word.leading_space;
            let sw = if needs_space {
                space_advance(self.fonts, word.style)
            } else {
                0.0
            };
            let projected = line_width + sw + word.width;
            if !line_items.is_empty() && projected > avail {
                self.items.extend(line_items.drain(..));
                baseline += line_max_size * self.theme.line_height_mult;
                pen_x = left;
                line_width = 0.0;
                line_max_size = word.style.size;
                // On a fresh line, drop the leading space.
                // Fall through to placing the word at pen_x.
                line_items.clear();
                // Force no-space for the first word on this line.
            } else if needs_space {
                pen_x += sw;
                line_width += sw;
            }
            line_max_size = line_max_size.max(word.style.size);
            let word_start_x = pen_x;
            for g in &word.glyphs {
                line_items.push(Placed::Glyph {
                    ch: g.ch,
                    font: word.style.font_id(),
                    size: word.style.size,
                    x: pen_x,
                    baseline,
                    color: word.style.color,
                });
                pen_x += g.advance;
            }
            line_width += word.width;
            if word.style.underline {
                line_items.push(Placed::Underline {
                    x: word_start_x,
                    y: baseline + 2.0,
                    w: word.width,
                    color: word.style.color,
                });
            }
        }
        self.items.extend(line_items.drain(..));
        self.y = baseline;
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

struct Word {
    glyphs: Vec<Glyph>,
    width: f32,
    style: Style,
    /// True if whitespace preceded this word in the source. Wrapping honors
    /// this to avoid phantom spaces before punctuation like "`code`,".
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
            self.out.push(Word {
                glyphs: std::mem::take(&mut self.cur),
                width: std::mem::take(&mut self.cur_width),
                style: self.cur_style,
                leading_space: self.cur_leading_space,
            });
            // Next word defaults to no leading space until we see one.
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
        }
    }
}

fn space_advance(fonts: &Fonts, style: Style) -> f32 {
    pick_font(fonts, style.font_id()).metrics(' ', style.size).advance_width
}
