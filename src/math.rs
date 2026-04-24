//! Minimal LaTeX-ish math layout. Produces a MathBox (glyph + rule list with
//! bounding box) so the main layout can treat math as a single inline unit.
//!
//! Supported subset:
//!   - single chars (letters italic, digits + operators upright)
//!   - \cmd expansion to Unicode (Greek, operators, big ops)
//!   - { ... } grouping
//!   - a^b  a_b  with single char or group
//!   - \frac{num}{den}                — stacked with a rule
//!   - \sqrt{x}                       — radical sign + overline
//!   - \hat, \bar, \tilde, \vec, \dot, \ddot — accent above a base
//!   - \max \min \log \ln \sin ...    — named operators in upright roman
//!   - \text{…}, \mathrm{…}           — upright run
//!   - \mathbb{…}                     — blackboard-bold via U+1D5xx
//!   - \left<d> … \right<d>           — auto-sized delimiters
//!   - \big \Big \bigg \Bigg <d>      — fixed-size enlarged delimiters
//!   - \quad \qquad                   — horizontal spacing
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
    /// True if the rightmost baseline-resident glyph is italic. Lets a
    /// following superscript apply italic correction — even when the ink
    /// order has an accent glyph (ˆ, ¯, …) appended after the italic base.
    pub italic_tail: bool,
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
    /// \hat, \bar, \tilde, …
    Accent { kind: AccentKind, base: Box<Node> },
    /// Named operator rendered in upright roman: max, min, log, sin, …
    Op(String),
    /// \text{…} / \mathrm{…} — upright content; letters are NOT italicised.
    Text(String),
    /// \mathbb{…} — letters/digits remapped to blackboard-bold Unicode points.
    Mathbb(String),
    /// Horizontal space measured in ems (\quad = 1, \qquad = 2).
    Space(f32),
    /// \left<d> … \right<d> — the outer glyphs stretch with the content.
    /// Either delimiter may be `None` for \left.  /  \right.
    Delim { left: Option<char>, right: Option<char>, inner: Box<Node> },
    /// \big / \Big / \bigg / \Bigg <delim> — fixed-size enlarged delimiter.
    BigDelim { ch: char, scale: f32 },
}

#[derive(Debug, Copy, Clone)]
enum AccentKind { Hat, Bar, Tilde, Vec, Dot, Ddot }

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
        // \right terminates a row begun by \left — the outer \left handler
        // consumes it. Peek-only; do not bump here.
        if let Tok::Command(s) = t {
            if s == "right" { break; }
        }
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
        "hat"   => Node::Accent { kind: AccentKind::Hat,   base: Box::new(parse_atom(p)) },
        "bar"   => Node::Accent { kind: AccentKind::Bar,   base: Box::new(parse_atom(p)) },
        "tilde" => Node::Accent { kind: AccentKind::Tilde, base: Box::new(parse_atom(p)) },
        "vec"   => Node::Accent { kind: AccentKind::Vec,   base: Box::new(parse_atom(p)) },
        "dot"   => Node::Accent { kind: AccentKind::Dot,   base: Box::new(parse_atom(p)) },
        "ddot"  => Node::Accent { kind: AccentKind::Ddot,  base: Box::new(parse_atom(p)) },

        // Named upright operators. Render as roman glyphs in a row.
        "max" | "min" | "log" | "ln" | "exp" | "sin" | "cos" | "tan"
        | "sec" | "csc" | "cot" | "sinh" | "cosh" | "tanh"
        | "det" | "dim" | "ker" | "arg" | "gcd" | "lim" | "deg" | "mod"
        | "Pr"  => Node::Op(name.to_string()),

        // Upright runs. \mathrm is treated identically to \text for this minimal
        // subset; \mathbf would be bold but we don't have a bold body font path
        // here — treat as upright too rather than render '\mathbf' literally.
        "text" | "mathrm" | "mathbf" | "operatorname" => {
            Node::Text(parse_text_group(p))
        }
        "mathit" => parse_atom(p),

        "mathbb" | "mathds" => Node::Mathbb(parse_text_group(p)),

        // Horizontal spacing.
        "quad"   => Node::Space(1.0),
        "qquad"  => Node::Space(2.0),
        "thinspace" => Node::Space(0.167),
        "thickspace" | "medspace" => Node::Space(0.28),

        // Auto-sized delimiters.
        "left"  => parse_left_right(p),
        // A stray \right (no matching \left) — treat the following delim as a
        // plain atom so we don't swallow tokens.
        "right" => match parse_delim(p) {
            Some(ch) => Node::Atom { ch, italic: false },
            None => Node::Atom { ch: ' ', italic: false },
        },

        // Fixed-size big delimiters. LaTeX step sizes.
        "big"  => big_delim(p, 1.2),
        "Big"  => big_delim(p, 1.5),
        "bigg" => big_delim(p, 1.8),
        "Bigg" => big_delim(p, 2.2),

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

/// Parse `<delim> … \right<delim>` after `\left` has been consumed.
fn parse_left_right(p: &mut Parser) -> Node {
    let left = parse_delim(p);
    let inner = parse_row(p);
    // Consume the terminating \right (if present) and its delim.
    let right = if matches!(p.peek(), Some(Tok::Command(s)) if s == "right") {
        p.bump();
        parse_delim(p)
    } else {
        None
    };
    Node::Delim { left, right, inner: Box::new(inner) }
}

fn big_delim(p: &mut Parser, scale: f32) -> Node {
    let ch = parse_delim(p).unwrap_or(' ');
    Node::BigDelim { ch, scale }
}

/// Read one delimiter. Accepts a literal char, `{` / `}`, or a named
/// delimiter command. `\left.` / `\right.` means no delimiter — returns None.
fn parse_delim(p: &mut Parser) -> Option<char> {
    match p.bump().cloned()? {
        Tok::Char('.') => None,
        Tok::Char(c)   => Some(c),
        Tok::Open      => Some('{'),
        Tok::Close     => Some('}'),
        Tok::Command(s) => match s.as_str() {
            "lbrace" => Some('{'),
            "rbrace" => Some('}'),
            "lvert"  | "vert" | "mid" => Some('|'),
            "lVert"  | "Vert" => Some('‖'),
            "langle" => Some('⟨'),
            "rangle" => Some('⟩'),
            "lceil"  => Some('⌈'),
            "rceil"  => Some('⌉'),
            "lfloor" => Some('⌊'),
            "rfloor" => Some('⌋'),
            _ => None,
        },
        _ => None,
    }
}

/// Consume a `{ … }` group and concatenate its inner characters into a string.
/// Inner commands are resolved: named commands mapping to a single character
/// are emitted as that character; anything else falls back to its name.
fn parse_text_group(p: &mut Parser) -> String {
    let mut out = String::new();
    if matches!(p.peek(), Some(Tok::Open)) {
        p.bump();
    } else {
        // No group — consume a single atom-equivalent char.
        if let Some(Tok::Char(c)) = p.bump().cloned() { out.push(c); }
        return out;
    }
    let mut depth: usize = 1;
    while let Some(tok) = p.bump().cloned() {
        match tok {
            Tok::Close => {
                depth -= 1;
                if depth == 0 { break; }
                out.push('}');
            }
            Tok::Open => { depth += 1; out.push('{'); }
            Tok::Char(c) => out.push(c),
            Tok::Caret => out.push('^'),
            Tok::Underscore => out.push('_'),
            Tok::Command(s) => {
                if let Some(c) = command_to_char(&s) { out.push(c); }
                else { out.push('\\'); out.push_str(&s); }
            }
        }
    }
    out
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
        Node::Accent { kind, base } => layout_accent(*kind, base, size, fonts),
        Node::Op(name) => layout_upright_run(name, size, fonts),
        Node::Text(s) => layout_upright_run(s, size, fonts),
        Node::Mathbb(s) => {
            let mapped: String = s.chars().map(mathbb_char).collect();
            layout_upright_run(&mapped, size, fonts)
        }
        Node::Space(em) => MathBox {
            glyphs: vec![], rules: vec![], width: em * size, ascent: 0.0, descent: 0.0,
            italic_tail: false,
        },
        Node::Delim { left, right, inner } => layout_delim(*left, *right, inner, size, fonts),
        Node::BigDelim { ch, scale } => layout_big_delim(*ch, *scale, size, fonts),
    }
}

fn layout_upright_run(s: &str, size: f32, fonts: &Fonts) -> MathBox {
    let font_id = FontId::Body;
    let f = pick_font(fonts, font_id);
    let mut glyphs = Vec::with_capacity(s.chars().count());
    let mut x = 0.0f32;
    let mut asc: f32 = 0.0;
    let mut desc: f32 = 0.0;
    let lm = f.horizontal_line_metrics(size);
    let line_asc = lm.map(|l| l.ascent).unwrap_or(size * 0.7);
    let line_desc = lm.map(|l| -l.descent).unwrap_or(size * 0.2);
    for ch in s.chars() {
        let m = f.metrics(ch, size);
        glyphs.push(MathGlyph { ch, x, y: 0.0, size, font: font_id });
        x += m.advance_width;
        let glyph_top = (m.ymin as f32 + m.height as f32).max(0.0);
        let glyph_bot = (-(m.ymin as f32)).max(0.0);
        asc = asc.max(glyph_top).max(line_asc * 0.6);
        desc = desc.max(glyph_bot).max(line_desc * 0.2);
    }
    MathBox { glyphs, rules: vec![], width: x, ascent: asc, descent: desc, italic_tail: false }
}

fn layout_accent(kind: AccentKind, base: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let mut glyphs = b.glyphs;
    let rules = b.rules;

    // Two families of mark glyph:
    //
    //  * modifier letters (ˆ ˜ ¯ ˙ ¨) — their bitmap already sits above their
    //    own baseline (ymin > 0). Rendering them at y=0 lands them roughly at
    //    cap-height. We only shift upward when a *tall* base (P, f, ℓ…) would
    //    otherwise collide with the mark's bitmap bottom.
    //
    //  * the arrow → for \vec — a normal-body glyph that sits on its baseline,
    //    so we lift it explicitly above the base's ascent.
    let (ch, asize, is_modifier) = match kind {
        AccentKind::Hat   => ('ˆ', size * 1.0, true),
        AccentKind::Tilde => ('˜', size * 1.0, true),
        AccentKind::Bar   => ('¯', size * 1.0, true),
        AccentKind::Dot   => ('˙', size * 1.1, true),
        AccentKind::Ddot  => ('¨', size * 1.1, true),
        AccentKind::Vec   => ('→', size * 0.7,  false),
    };
    let font_id = FontId::Body;
    let f = pick_font(fonts, font_id);
    let m = f.metrics(ch, asize);

    // Centre the mark on the base's advance width. (For modifier letters the
    // advance width tracks the ink extent reasonably well.)
    let cx = (b.width - m.advance_width) / 2.0;

    let gap = size * 0.05;
    let cy = if is_modifier {
        // Modifier's bitmap bottom sits at (−ymin) in math coords at cy=0.
        // Lift only if base pokes above that line; otherwise the mark is
        // already comfortably seated above cap-height.
        let bitmap_bottom_at_zero = -(m.ymin as f32);
        let target_bottom = -(b.ascent + gap);
        (target_bottom - bitmap_bottom_at_zero).min(0.0)
    } else {
        -(b.ascent + gap)
    };

    // Glyph bitmap top in math coords (negative = up). Used for ascent.
    let glyph_top_math_y = cy - (m.ymin as f32 + m.height as f32);
    glyphs.push(MathGlyph { ch, x: cx, y: cy, size: asize, font: font_id });

    MathBox {
        glyphs,
        rules,
        width: b.width,
        ascent: b.ascent.max(-glyph_top_math_y),
        descent: b.descent,
        italic_tail: b.italic_tail,
    }
}

/// Auto-sized delimiters. Not a real piece-wise assembly — we just scale the
/// delimiter glyph so its height matches (roughly) the inner content.
fn layout_delim(
    left: Option<char>,
    right: Option<char>,
    inner: &Node,
    size: f32,
    fonts: &Fonts,
) -> MathBox {
    let b = layout_node(inner, size, fonts);
    let total_h = (b.ascent + b.descent).max(size);
    // Grow the delim glyph enough to cover the content; clamp so tiny content
    // doesn't give tiny parens.
    let delim_size = (total_h * 1.1).max(size).min(size * 4.0);

    let font_id = FontId::Body;
    let f = pick_font(fonts, font_id);

    // Shift the inner content right to make room for the opening delim.
    let (l_w, l_glyph) = match left {
        Some(ch) => {
            let m = f.metrics(ch, delim_size);
            (m.advance_width, Some((ch, m)))
        }
        None => (0.0, None),
    };
    let (r_w, r_glyph) = match right {
        Some(ch) => {
            let m = f.metrics(ch, delim_size);
            (m.advance_width, Some((ch, m)))
        }
        None => (0.0, None),
    };

    let mut glyphs = Vec::with_capacity(b.glyphs.len() + 2);
    for g in b.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + l_w, y: g.y, size: g.size, font: g.font });
    }
    let rules: Vec<MathRule> = b.rules.into_iter().map(|r| MathRule {
        x: r.x + l_w, y: r.y, w: r.w, h: r.h,
    }).collect();

    // Centre each delim on the content's vertical midline. A paren glyph's
    // natural visual centre (drawn at y=0) sits at math-y = -(ymin+height/2)
    // — i.e. well above the baseline. For a tall fraction whose content is
    // symmetric about the baseline, drawing the paren at y=0 leaves the
    // inner equation visibly low inside the delim. Shifting the paren
    // baseline down by (ymin + height/2 + axis_y) lines the paren's centre
    // up with the content's centre.
    let axis_y = (b.descent - b.ascent) * 0.5;

    fn centre_y(m: &fontdue::Metrics, axis_y: f32) -> f32 {
        axis_y + m.ymin as f32 + m.height as f32 * 0.5
    }

    let mut asc = b.ascent;
    let mut desc = b.descent;
    if let Some((ch, ref m)) = l_glyph {
        let cy = centre_y(m, axis_y);
        // Glyph extents in math-y terms (negative = above baseline).
        let top_math_y = cy - (m.ymin as f32 + m.height as f32);
        let bot_math_y = cy - m.ymin as f32;
        asc = asc.max(-top_math_y);
        desc = desc.max(bot_math_y);
        glyphs.insert(0, MathGlyph { ch, x: 0.0, y: cy, size: delim_size, font: font_id });
    }
    if let Some((ch, ref m)) = r_glyph {
        let cy = centre_y(m, axis_y);
        let top_math_y = cy - (m.ymin as f32 + m.height as f32);
        let bot_math_y = cy - m.ymin as f32;
        asc = asc.max(-top_math_y);
        desc = desc.max(bot_math_y);
        glyphs.push(MathGlyph { ch, x: l_w + b.width, y: cy, size: delim_size, font: font_id });
    }

    MathBox {
        glyphs,
        rules,
        width: l_w + b.width + r_w,
        ascent: asc,
        descent: desc,
        italic_tail: false,
    }
}

fn layout_big_delim(ch: char, scale: f32, size: f32, fonts: &Fonts) -> MathBox {
    let font_id = FontId::Body;
    let f = pick_font(fonts, font_id);
    let dsize = size * scale;
    let m = f.metrics(ch, dsize);
    let glyph = MathGlyph { ch, x: 0.0, y: 0.0, size: dsize, font: font_id };
    MathBox {
        glyphs: vec![glyph],
        rules: vec![],
        width: m.advance_width,
        ascent: dsize * 0.75,
        descent: dsize * 0.25,
        italic_tail: false,
    }
}

/// Map an ASCII letter/digit to its blackboard-bold Unicode equivalent, if
/// one exists. The special letters C H N P Q R Z have dedicated double-struck
/// codepoints (ℂ, ℍ, …) outside the normal 𝔸–𝕫 block.
fn mathbb_char(c: char) -> char {
    match c {
        '0' => '𝟘', '1' => '𝟙', '2' => '𝟚', '3' => '𝟛', '4' => '𝟜',
        '5' => '𝟝', '6' => '𝟞', '7' => '𝟟', '8' => '𝟠', '9' => '𝟡',
        'C' => 'ℂ', 'H' => 'ℍ', 'N' => 'ℕ', 'P' => 'ℙ', 'Q' => 'ℚ', 'R' => 'ℝ', 'Z' => 'ℤ',
        'A'..='Z' => std::char::from_u32(0x1D538 + (c as u32 - 'A' as u32)).unwrap_or(c),
        'a'..='z' => std::char::from_u32(0x1D552 + (c as u32 - 'a' as u32)).unwrap_or(c),
        other => other,
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
        italic_tail: italic,
    }
}

fn layout_row_items(items: &[Node], size: f32, fonts: &Fonts) -> MathBox {
    let mut glyphs = Vec::new();
    let mut rules = Vec::new();
    let mut x = 0.0;
    let mut asc: f32 = 0.0;
    let mut desc: f32 = 0.0;
    let mut italic_tail = false;
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
        italic_tail = b.italic_tail;  // only the last item matters
    }
    MathBox { glyphs, rules, width: x, ascent: asc, descent: desc, italic_tail }
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
    MathBox { glyphs, rules, width, ascent, descent, italic_tail: false }
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
        italic_tail: false,
    }
}

// Script size vs base size — LaTeX's `scriptstyle` is ~0.7; bumping it down
// a notch makes sub/sup feel like exponents instead of crowding the base.
const SCRIPT_SCALE: f32 = 0.66;
// Vertical shifts. Chosen so a sup with descenders (p, g, y) clears a sub
// with ascenders (h, l, b, d) by ~0.15em — no more touching.
const SUP_SHIFT: f32 = 0.55;
const SUB_SHIFT: f32 = 0.26;

/// Italic letters (f, q, y, …) slant to the right at the top, so a
/// superscript glued to the right edge of the advance box runs into the
/// base's slant. LaTeX solves this with an "italic correction" kern before
/// the superscript. We apply the same nudge whenever the base's italic_tail
/// flag is set — which survives through accents like \hat{f} (where the
/// *drawn* last glyph is the accent mark, not the italic base).
fn italic_correction(b: &MathBox, size: f32) -> f32 {
    if b.italic_tail { size * 0.14 } else { 0.0 }
}

fn layout_sup(base: &Node, exp: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let e = layout_node(exp, size * SCRIPT_SCALE, fonts);
    let shift_up = size * SUP_SHIFT;
    let ic = italic_correction(&b, size);
    let mut glyphs = b.glyphs;
    let mut rules = b.rules;
    for g in &e.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width + ic, y: g.y - shift_up, size: g.size, font: g.font });
    }
    for r in &e.rules {
        rules.push(MathRule { x: r.x + b.width + ic, y: r.y - shift_up, w: r.w, h: r.h });
    }
    MathBox {
        glyphs,
        rules,
        width: b.width + ic + e.width,
        ascent: b.ascent.max(e.ascent + shift_up),
        descent: b.descent,
        italic_tail: false,
    }
}

fn layout_sub(base: &Node, sub: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let s = layout_node(sub, size * SCRIPT_SCALE, fonts);
    let shift_down = size * SUB_SHIFT;
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
        italic_tail: false,
    }
}

fn layout_subsup(base: &Node, sub: &Node, sup: &Node, size: f32, fonts: &Fonts) -> MathBox {
    let b = layout_node(base, size, fonts);
    let sp = layout_node(sup, size * SCRIPT_SCALE, fonts);
    let sb = layout_node(sub, size * SCRIPT_SCALE, fonts);
    let shift_up = size * SUP_SHIFT;
    let shift_down = size * SUB_SHIFT;
    // Italic correction only applies to the sup (which climbs into the
    // italic slant); the sub sits below the baseline, out of the way.
    let ic = italic_correction(&b, size);
    let sx_w = (sp.width + ic).max(sb.width);
    let mut glyphs = b.glyphs;
    let mut rules = b.rules;
    for g in &sp.glyphs {
        glyphs.push(MathGlyph { ch: g.ch, x: g.x + b.width + ic, y: g.y - shift_up, size: g.size, font: g.font });
    }
    for r in &sp.rules {
        rules.push(MathRule { x: r.x + b.width + ic, y: r.y - shift_up, w: r.w, h: r.h });
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
        italic_tail: false,
    }
}
