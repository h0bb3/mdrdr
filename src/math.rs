//! Minimal LaTeX-ish math layout. Produces a MathBox (glyph + rule list with
//! bounding box) so the main layout can treat math as a single inline unit.
//!
//! Supported subset:
//!   - single chars (letters italic, digits + operators upright)
//!   - \cmd expansion to Unicode (Greek, operators, big ops)
//!   - { ... } grouping
//!   - a^b  a_b  with single char or group
//!   - \frac{num}{den}     — stacked with a rule
//!   - \sqrt{x}            — radical sign + overline
//!
//! Ample room for growth later. Good enough for typical notes.

use fontdue::Font;

use crate::font::Fonts;
use crate::layout::{pick_font, FontId};

pub struct MathBox {
    pub glyphs: Vec<MathGlyph>,
    pub rules: Vec<MathRule>,
    pub width: f32,
    pub ascent: f32,   // positive, distance from baseline up to top
    pub descent: f32,  // positive, distance from baseline down to bottom
}

pub struct MathGlyph {
    pub ch: char,
    pub x: f32,
    pub y: f32,       // offset from baseline (negative = above)
    pub size: f32,
    pub font: FontId,
}

pub struct MathRule {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub fn layout(src: &str, size: f32, fonts: &Fonts) -> MathBox {
    let tokens = tokenize(src);
    let node = parse_row(&mut Parser::new(&tokens));
    layout_node(&node, size, fonts)
}

// ───── tokenize ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Tok {
    Char(char),
    Command(String),
    Open,
    Close,
    Caret,
    Underscore,
    // Whitespace is dropped at the tokenization stage.
}

fn tokenize(src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = src.chars().peekable();
    while let Some(&c) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        if c == '\\' {
            it.next();
            let mut name = String::new();
            while let Some(&nc) = it.peek() {
                if nc.is_ascii_alphabetic() {
                    name.push(nc);
                    it.next();
                } else {
                    break;
                }
            }
            if name.is_empty() {
                // \<non-letter> — escape that single char
                if let Some(nc) = it.next() {
                    out.push(Tok::Char(nc));
                }
            } else {
                out.push(Tok::Command(name));
            }
            continue;
        }
        match c {
            '{' => { it.next(); out.push(Tok::Open); }
            '}' => { it.next(); out.push(Tok::Close); }
            '^' => { it.next(); out.push(Tok::Caret); }
            '_' => { it.next(); out.push(Tok::Underscore); }
            _   => { it.next(); out.push(Tok::Char(c)); }
        }
    }
    out
}

// ───── parse ────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Node {
    Atom { ch: char, italic: bool },
    Row(Vec<Node>),
    Frac(Box<Node>, Box<Node>),
    Sqrt(Box<Node>),
    Sup { base: Box<Node>, exp: Box<Node> },
    Sub { base: Box<Node>, sub: Box<Node> },
    SubSup { base: Box<Node>, sub: Box<Node>, sup: Box<Node> },
}

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self { Self { toks, i: 0 } }
    fn peek(&self) -> Option<&Tok> { self.toks.get(self.i) }
    fn bump(&mut self) -> Option<&Tok> { let t = self.toks.get(self.i); self.i += 1; t }
}

fn parse_row(p: &mut Parser) -> Node {
    let mut nodes: Vec<Node> = Vec::new();
    while let Some(t) = p.peek() {
        if matches!(t, Tok::Close) { break; }
        let mut base = parse_atom(p);
        // Attach ^ and/or _ to the preceding atom.
        let mut sup: Option<Node> = None;
        let mut sub: Option<Node> = None;
        loop {
            match p.peek() {
                Some(Tok::Caret) => {
                    p.bump();
                    sup = Some(parse_atom(p));
                }
                Some(Tok::Underscore) => {
                    p.bump();
                    sub = Some(parse_atom(p));
                }
                _ => break,
            }
        }
        base = match (sub, sup) {
            (Some(a), Some(b)) => Node::SubSup { base: Box::new(base), sub: Box::new(a), sup: Box::new(b) },
            (Some(a), None)    => Node::Sub   { base: Box::new(base), sub: Box::new(a) },
            (None, Some(b))    => Node::Sup   { base: Box::new(base), exp: Box::new(b) },
            (None, None)       => base,
        };
        nodes.push(base);
    }
    if nodes.len() == 1 {
        nodes.remove(0)
    } else {
        Node::Row(nodes)
    }
}

fn parse_atom(p: &mut Parser) -> Node {
    match p.bump().cloned() {
        Some(Tok::Char(c)) => Node::Atom { ch: c, italic: is_math_italic(c) },
        Some(Tok::Open) => {
            let inner = parse_row(p);
            // consume matching Close
            if matches!(p.peek(), Some(Tok::Close)) { p.bump(); }
            inner
        }
        Some(Tok::Command(cmd)) => expand_command(&cmd, p),
        Some(Tok::Close) | Some(Tok::Caret) | Some(Tok::Underscore) | None => {
            Node::Atom { ch: ' ', italic: false }
        }
    }
}

fn expand_command(name: &str, p: &mut Parser) -> Node {
    match name {
        "frac" => {
            let n = parse_atom(p);
            let d = parse_atom(p);
            Node::Frac(Box::new(n), Box::new(d))
        }
        "sqrt" => {
            let inner = parse_atom(p);
            Node::Sqrt(Box::new(inner))
        }
        _ => {
            if let Some(c) = command_to_char(name) {
                Node::Atom { ch: c, italic: false }
            } else {
                // Unknown command — render as `\name` in upright.
                let mut row = Vec::with_capacity(name.len() + 1);
                row.push(Node::Atom { ch: '\\', italic: false });
                for ch in name.chars() {
                    row.push(Node::Atom { ch, italic: false });
                }
                Node::Row(row)
            }
        }
    }
}

fn command_to_char(name: &str) -> Option<char> {
    Some(match name {
        // Greek lowercase
        "alpha" => 'α', "beta" => 'β', "gamma" => 'γ', "delta" => 'δ', "epsilon" => 'ε',
        "zeta" => 'ζ', "eta" => 'η', "theta" => 'θ', "iota" => 'ι', "kappa" => 'κ',
        "lambda" => 'λ', "mu" => 'μ', "nu" => 'ν', "xi" => 'ξ', "omicron" => 'ο',
        "pi" => 'π', "rho" => 'ρ', "sigma" => 'σ', "tau" => 'τ', "upsilon" => 'υ',
        "phi" => 'φ', "chi" => 'χ', "psi" => 'ψ', "omega" => 'ω',
        // Greek uppercase
        "Gamma" => 'Γ', "Delta" => 'Δ', "Theta" => 'Θ', "Lambda" => 'Λ', "Xi" => 'Ξ',
        "Pi" => 'Π', "Sigma" => 'Σ', "Upsilon" => 'Υ', "Phi" => 'Φ', "Psi" => 'Ψ',
        "Omega" => 'Ω',
        // Operators / relations
        "pm" => '±', "mp" => '∓', "times" => '×', "cdot" => '·', "div" => '÷',
        "le" => '≤', "leq" => '≤', "ge" => '≥', "geq" => '≥', "ne" => '≠', "neq" => '≠',
        "approx" => '≈', "equiv" => '≡', "sim" => '∼', "propto" => '∝',
        "to" => '→', "rightarrow" => '→', "leftarrow" => '←', "leftrightarrow" => '↔',
        "Rightarrow" => '⇒', "Leftarrow" => '⇐',
        "in" => '∈', "notin" => '∉', "subset" => '⊂', "supset" => '⊃', "cup" => '∪', "cap" => '∩',
        // Big operators
        "sum" => '∑', "prod" => '∏', "int" => '∫', "oint" => '∮',
        // Misc
        "infty" => '∞', "partial" => '∂', "nabla" => '∇', "forall" => '∀', "exists" => '∃',
        "emptyset" => '∅', "hbar" => 'ℏ', "ell" => 'ℓ', "Re" => 'ℜ', "Im" => 'ℑ',
        "neg" => '¬', "land" => '∧', "lor" => '∨',
        "ldots" => '…', "cdots" => '⋯', "dots" => '…',
        _ => return None,
    })
}

fn is_math_italic(c: char) -> bool {
    c.is_ascii_alphabetic()
}

// ───── layout ───────────────────────────────────────────────────────────────

fn layout_node(node: &Node, size: f32, fonts: &Fonts) -> MathBox {
    match node {
        Node::Atom { ch, italic } => layout_atom(*ch, *italic, size, fonts),
        Node::Row(items) => layout_row_items(items, size, fonts),
        Node::Frac(n, d) => layout_frac(n, d, size, fonts),
        Node::Sqrt(inner) => layout_sqrt(inner, size, fonts),
        Node::Sup { base, exp } => layout_sup(base, exp, size, fonts),
        Node::Sub { base, sub } => layout_sub(base, sub, size, fonts),
        Node::SubSup { base, sub, sup } => layout_subsup(base, sub, sup, size, fonts),
    }
}

fn layout_atom(ch: char, italic: bool, size: f32, fonts: &Fonts) -> MathBox {
    let font_id = if italic { FontId::Italic } else { FontId::Body };
    let font: &Font = pick_font(fonts, font_id);
    let m = font.metrics(ch, size);
    // Derive ascent/descent from glyph metrics — fall back to font line metrics if empty.
    let lm = font.horizontal_line_metrics(size);
    let (a, d) = if m.height == 0 {
        (lm.map(|l| l.ascent).unwrap_or(size * 0.7), lm.map(|l| -l.descent).unwrap_or(size * 0.2))
    } else {
        let top = -(m.ymin as f32 + m.height as f32);  // top above baseline
        let bottom = -(m.ymin as f32);                 // bottom; if negative, goes below baseline
        // ascent = how far above baseline: -top (since top is negative)
        let asc = (-top).max(0.0);
        let desc = (-bottom).max(0.0);
        // guard with font line metrics
        let font_asc = lm.map(|l| l.ascent).unwrap_or(size * 0.7);
        let font_desc = lm.map(|l| -l.descent).unwrap_or(size * 0.2);
        (asc.max(font_asc * 0.6), desc.max(font_desc * 0.2))
    };
    MathBox {
        glyphs: vec![MathGlyph { ch, x: 0.0, y: 0.0, size, font: font_id }],
        rules: vec![],
        width: m.advance_width,
        ascent: a,
        descent: d,
    }
}

fn layout_row_items(items: &[Node], size: f32, fonts: &Fonts) -> MathBox {
    let mut glyphs = Vec::new();
    let mut rules = Vec::new();
    let mut x = 0.0;
    let mut asc: f32 = 0.0;
    let mut desc: f32 = 0.0;
    for (i, node) in items.iter().enumerate() {
        let mut b = layout_node(node, size, fonts);
        // Optional spacing between ops and operands — skip for now; LaTeX's mu-spacing is intricate.
        if i > 0 && needs_kern(&items[i - 1], node) {
            x += size * 0.12;
        }
        for g in &mut b.glyphs {
            g.x += x;
        }
        for r in &mut b.rules {
            r.x += x;
        }
        glyphs.extend(b.glyphs);
        rules.extend(b.rules);
        x += b.width;
        asc = asc.max(b.ascent);
        desc = desc.max(b.descent);
    }
    MathBox { glyphs, rules, width: x, ascent: asc, descent: desc }
}

fn needs_kern(prev: &Node, cur: &Node) -> bool {
    // Thin space around operators and before/after \cdot etc.
    let is_op = |n: &Node| matches!(
        n,
        Node::Atom { ch, .. } if matches!(*ch, '+' | '−' | '-' | '=' | '<' | '>' | '≤' | '≥' | '≠' | '±' | '×' | '·' | '→' | '≈' | '≡')
    );
    is_op(prev) || is_op(cur)
}

fn layout_frac(n: &Node, d: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let num = layout_node(n, size * 0.92, fonts);
    let den = layout_node(d, size * 0.92, fonts);
    let pad = size * 0.25;
    let bar_h = (size * 0.06).max(1.0);
    let inner_w = num.width.max(den.width);
    let width = inner_w + pad * 2.0;

    // bar y = 0 (at baseline)
    let gap_top = size * 0.10;
    let gap_bot = size * 0.14;

    let mut glyphs = Vec::new();
    let mut rules = Vec::new();

    let num_center = pad + inner_w / 2.0;
    let num_x_shift = num_center - num.width / 2.0;
    // num baseline sits above the bar, so its baseline_y = -(bar_h/2 + gap_top + num.descent)
    let num_baseline = -(bar_h / 2.0 + gap_top + num.descent);
    for g in &num.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + num_x_shift, y: g.y + num_baseline, size: g.size, font: g.font });
    }
    for r in &num.rules {
        rules.push(MathRule { x: r.x + num_x_shift, y: r.y + num_baseline, w: r.w, h: r.h });
    }

    let den_center = pad + inner_w / 2.0;
    let den_x_shift = den_center - den.width / 2.0;
    let den_baseline = bar_h / 2.0 + gap_bot + den.ascent;
    for g in &den.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + den_x_shift, y: g.y + den_baseline, size: g.size, font: g.font });
    }
    for r in &den.rules {
        rules.push(MathRule { x: r.x + den_x_shift, y: r.y + den_baseline, w: r.w, h: r.h });
    }

    // bar
    rules.push(MathRule {
        x: pad * 0.5,
        y: -bar_h / 2.0,
        w: width - pad,
        h: bar_h,
    });

    let ascent = -num_baseline + num.ascent;
    let descent = den_baseline + den.descent;
    MathBox { glyphs, rules, width, ascent, descent }
}

fn layout_sqrt(inner: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let i_box = layout_node(inner, size, fonts);
    let rad_w = size * 0.55;
    let pad_right = size * 0.1;
    let overline_h = (size * 0.05).max(1.0);
    let width = rad_w + i_box.width + pad_right;
    // Draw a '√' glyph as the radical, then overline spanning the inner.
    let glyph = MathGlyph {
        ch: '√',
        x: 0.0,
        y: 0.0,
        size: size * 1.15,
        font: FontId::Body,
    };
    let mut glyphs = vec![glyph];
    let mut rules = vec![MathRule {
        x: rad_w - overline_h * 0.2,
        y: -(i_box.ascent + overline_h * 1.2),
        w: i_box.width + pad_right,
        h: overline_h,
    }];
    for g in &i_box.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + rad_w, y: g.y, size: g.size, font: g.font });
    }
    for r in &i_box.rules {
        rules.push(MathRule { x: r.x + rad_w, y: r.y, w: r.w, h: r.h });
    }
    MathBox {
        glyphs,
        rules,
        width,
        ascent: i_box.ascent + overline_h * 1.8,
        descent: i_box.descent,
    }
}

fn layout_sup(base: &Node, exp: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let e = layout_node(exp, size * 0.7, fonts);
    let shift_up = size * 0.45;
    let mut glyphs = b.glyphs;
    let mut rules = b.rules;
    for g in &e.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width, y: g.y - shift_up, size: g.size, font: g.font });
    }
    for r in &e.rules {
        rules.push(MathRule { x: r.x + b.width, y: r.y - shift_up, w: r.w, h: r.h });
    }
    MathBox {
        glyphs,
        rules,
        width: b.width + e.width,
        ascent: b.ascent.max(e.ascent + shift_up),
        descent: b.descent,
    }
}

fn layout_sub(base: &Node, sub: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let s = layout_node(sub, size * 0.7, fonts);
    let shift_down = size * 0.18;
    let mut glyphs = b.glyphs;
    let mut rules = b.rules;
    for g in &s.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width, y: g.y + shift_down, size: g.size, font: g.font });
    }
    for r in &s.rules {
        rules.push(MathRule { x: r.x + b.width, y: r.y + shift_down, w: r.w, h: r.h });
    }
    MathBox {
        glyphs,
        rules,
        width: b.width + s.width,
        ascent: b.ascent,
        descent: b.descent.max(s.descent + shift_down),
    }
}

fn layout_subsup(base: &Node, sub: &Node, sup: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let sp = layout_node(sup, size * 0.7, fonts);
    let sb = layout_node(sub, size * 0.7, fonts);
    let shift_up = size * 0.45;
    let shift_down = size * 0.18;
    let sx_w = sp.width.max(sb.width);
    let mut glyphs = b.glyphs;
    let mut rules = b.rules;
    for g in &sp.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width, y: g.y - shift_up, size: g.size, font: g.font });
    }
    for r in &sp.rules {
        rules.push(MathRule { x: r.x + b.width, y: r.y - shift_up, w: r.w, h: r.h });
    }
    for g in &sb.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width, y: g.y + shift_down, size: g.size, font: g.font });
    }
    for r in &sb.rules {
        rules.push(MathRule { x: r.x + b.width, y: r.y + shift_down, w: r.w, h: r.h });
    }
    MathBox {
        glyphs,
        rules,
        width: b.width + sx_w,
        ascent: b.ascent.max(sp.ascent + shift_up),
        descent: b.descent.max(sb.descent + shift_down),
    }
}
