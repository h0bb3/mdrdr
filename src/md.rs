//! Markdown parser. A pragmatic CommonMark subset — enough for typical notes.
//!
//! Block level:  ATX headings (# .. ######), paragraphs, fenced code (```),
//!               bullet lists (-, *, +), ordered lists (1.), blockquotes (>),
//!               thematic breaks (---, ***, ___).
//! Inline level: **bold**, *italic*, `code`, [text](url), ![alt](src),
//!               backslash escapes.
//!
//! Not handled yet: nested lists, setext headings, reference-style links,
//! HTML passthrough, tables. They land as we need them.

#[derive(Debug, Clone)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Code(String),
    Link { text: Vec<Inline>, href: String },
    Image { alt: String, src: String },
    Math(String),
}

#[derive(Debug, Clone)]
pub enum Block {
    Heading { level: u8, inlines: Vec<Inline> },
    Paragraph(Vec<Inline>),
    CodeBlock { lang: Option<String>, text: String },
    List { ordered: bool, items: Vec<Vec<Inline>> },
    BlockQuote(Vec<Block>),
    ThematicBreak,
    DisplayMath(String),
    Mermaid(String),
}

pub fn parse(src: &str) -> Vec<Block> {
    let lines: Vec<&str> = src.lines().collect();
    parse_lines(&lines)
}

fn parse_lines(lines: &[&str]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trim = line.trim_start();

        if trim.starts_with("$$") {
            let mut body = String::new();
            // Same-line close: $$ a + b $$
            let first = &trim[2..];
            if let Some(close) = first.find("$$") {
                out.push(Block::DisplayMath(first[..close].trim().to_string()));
                i += 1;
                continue;
            }
            if !first.is_empty() {
                body.push_str(first);
                body.push('\n');
            }
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if let Some(close) = l.find("$$") {
                    body.push_str(&l[..close]);
                    i += 1;
                    break;
                }
                body.push_str(l);
                body.push('\n');
                i += 1;
            }
            out.push(Block::DisplayMath(body.trim().to_string()));
            continue;
        }

        if trim.starts_with("```") {
            let lang = trim.trim_start_matches('`').trim();
            let lang_opt = if lang.is_empty() { None } else { Some(lang.to_string()) };
            let mut code = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push_str(lines[i]);
                code.push('\n');
                i += 1;
            }
            if i < lines.len() { i += 1; }
            if lang.eq_ignore_ascii_case("mermaid") {
                out.push(Block::Mermaid(code));
            } else {
                out.push(Block::CodeBlock { lang: lang_opt, text: code });
            }
            continue;
        }

        if let Some(level) = heading_level(trim) {
            let rest = trim[(level as usize)..].trim_start().trim_end_matches('#').trim();
            out.push(Block::Heading { level, inlines: parse_inlines(rest) });
            i += 1;
            continue;
        }

        if is_thematic_break(trim) {
            out.push(Block::ThematicBreak);
            i += 1;
            continue;
        }

        if trim.starts_with('>') {
            let mut inner_lines: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if t.starts_with('>') {
                    let stripped = t.trim_start_matches('>').trim_start_matches(' ').to_string();
                    inner_lines.push(stripped);
                    i += 1;
                } else if lines[i].trim().is_empty() {
                    break;
                } else {
                    inner_lines.push(lines[i].to_string());
                    i += 1;
                }
            }
            let refs: Vec<&str> = inner_lines.iter().map(|s| s.as_str()).collect();
            out.push(Block::BlockQuote(parse_lines(&refs)));
            continue;
        }

        if let Some((ordered, _)) = list_marker(trim) {
            let mut items: Vec<Vec<Inline>> = Vec::new();
            while i < lines.len() {
                let ln = lines[i];
                let tln = ln.trim_start();
                if let Some((o, me)) = list_marker(tln) {
                    if o != ordered { break; }
                    items.push(parse_inlines(&tln[me..]));
                    i += 1;
                } else if ln.trim().is_empty() {
                    // tolerate one blank line between items
                    if i + 1 < lines.len() && list_marker(lines[i + 1].trim_start()).is_some() {
                        i += 1;
                        continue;
                    }
                    break;
                } else {
                    break;
                }
            }
            out.push(Block::List { ordered, items });
            continue;
        }

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Paragraph — soak up contiguous lines.
        let mut buf = String::new();
        while i < lines.len() {
            let ln = lines[i];
            if ln.trim().is_empty() { break; }
            let t = ln.trim_start();
            if heading_level(t).is_some()
                || t.starts_with("```")
                || is_thematic_break(t)
                || t.starts_with('>')
                || list_marker(t).is_some()
            {
                break;
            }
            if !buf.is_empty() { buf.push('\n'); }
            buf.push_str(ln);
            i += 1;
        }
        if !buf.is_empty() {
            out.push(Block::Paragraph(parse_inlines(&buf)));
        }
    }
    out
}

fn heading_level(s: &str) -> Option<u8> {
    let b = s.as_bytes();
    let mut n = 0;
    while n < b.len() && b[n] == b'#' { n += 1; }
    if (1..=6).contains(&n) && n < b.len() && b[n] == b' ' {
        Some(n as u8)
    } else {
        None
    }
}

fn is_thematic_break(s: &str) -> bool {
    let collapsed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if collapsed.len() < 3 { return false; }
    let first = collapsed.chars().next().unwrap();
    if !matches!(first, '-' | '*' | '_') { return false; }
    collapsed.chars().all(|c| c == first)
}

fn list_marker(s: &str) -> Option<(bool, usize)> {
    let b = s.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' {
        return Some((false, 2));
    }
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() { i += 1; }
    if i > 0 && i + 1 < b.len() && b[i] == b'.' && b[i + 1] == b' ' {
        return Some((true, i + 2));
    }
    None
}

// ───── inline parser ─────────────────────────────────────────────────────────

pub fn parse_inlines(src: &str) -> Vec<Inline> {
    let mut p = InlineParser { src: src.as_bytes(), i: 0 };
    p.parse_until(None)
}

struct InlineParser<'a> {
    src: &'a [u8],
    i: usize,
}

impl<'a> InlineParser<'a> {
    fn parse_until(&mut self, closer: Option<&[u8]>) -> Vec<Inline> {
        let mut out: Vec<Inline> = Vec::new();
        let mut buf = String::new();

        let flush = |buf: &mut String, out: &mut Vec<Inline>| {
            if !buf.is_empty() {
                out.push(Inline::Text(std::mem::take(buf)));
            }
        };

        while self.i < self.src.len() {
            if let Some(c) = closer {
                if self.peek(c) {
                    flush(&mut buf, &mut out);
                    return out;
                }
            }
            let b = self.src[self.i];

            if b == b'\\' && self.i + 1 < self.src.len() {
                let nxt = self.src[self.i + 1];
                if matches!(nxt, b'*' | b'_' | b'`' | b'[' | b']' | b'(' | b')' | b'!' | b'\\' | b'$') {
                    buf.push(nxt as char);
                    self.i += 2;
                    continue;
                }
            }

            match b {
                b'`' => {
                    let mut fence = 0;
                    while self.i + fence < self.src.len() && self.src[self.i + fence] == b'`' {
                        fence += 1;
                    }
                    let needle: Vec<u8> = vec![b'`'; fence];
                    let search_from = self.i + fence;
                    if let Some(pos) = find_seq(&self.src[search_from..], &needle) {
                        let raw = &self.src[search_from..search_from + pos];
                        let s = std::str::from_utf8(raw).unwrap_or("").trim().to_string();
                        flush(&mut buf, &mut out);
                        out.push(Inline::Code(s));
                        self.i = search_from + pos + fence;
                    } else {
                        buf.push('`');
                        self.i += 1;
                    }
                }
                b'*' | b'_' => {
                    let ch = b;
                    let mut run = 0;
                    while self.i + run < self.src.len() && self.src[self.i + run] == ch {
                        run += 1;
                    }
                    // Only open if followed by a non-space.
                    let after = self.i + run;
                    let ok_open = after < self.src.len() && !is_space(self.src[after]);
                    if !ok_open {
                        for _ in 0..run { buf.push(ch as char); }
                        self.i += run;
                        continue;
                    }
                    let delim = if run >= 2 { 2 } else { 1 };
                    let seq: Vec<u8> = vec![ch; delim];
                    self.i += delim;
                    flush(&mut buf, &mut out);
                    let inner = self.parse_until(Some(&seq));
                    if self.peek(&seq) {
                        self.i += delim;
                    }
                    if delim == 2 {
                        out.push(Inline::Bold(inner));
                    } else {
                        out.push(Inline::Italic(inner));
                    }
                }
                b'!' if self.i + 1 < self.src.len() && self.src[self.i + 1] == b'[' => {
                    if let Some((alt, src, end)) = self.parse_bracket_paren(self.i + 2) {
                        flush(&mut buf, &mut out);
                        out.push(Inline::Image { alt, src });
                        self.i = end;
                    } else {
                        buf.push('!');
                        self.i += 1;
                    }
                }
                b'[' => {
                    if let Some((text_str, href, end)) = self.parse_bracket_paren(self.i + 1) {
                        flush(&mut buf, &mut out);
                        out.push(Inline::Link { text: parse_inlines(&text_str), href });
                        self.i = end;
                    } else {
                        buf.push('[');
                        self.i += 1;
                    }
                }
                b'$' => {
                    // Inline math: $...$ — find next un-escaped $.
                    let start = self.i + 1;
                    let mut j = start;
                    let mut found = None;
                    while j < self.src.len() {
                        if self.src[j] == b'\\' && j + 1 < self.src.len() {
                            j += 2;
                            continue;
                        }
                        if self.src[j] == b'$' {
                            found = Some(j);
                            break;
                        }
                        j += 1;
                    }
                    if let Some(end) = found {
                        let raw = &self.src[start..end];
                        let s = std::str::from_utf8(raw).unwrap_or("").to_string();
                        flush(&mut buf, &mut out);
                        out.push(Inline::Math(s));
                        self.i = end + 1;
                    } else {
                        buf.push('$');
                        self.i += 1;
                    }
                }
                b'\n' => {
                    buf.push(' ');
                    self.i += 1;
                }
                _ => {
                    let n = utf8_len(b);
                    let end = (self.i + n).min(self.src.len());
                    if let Ok(s) = std::str::from_utf8(&self.src[self.i..end]) {
                        buf.push_str(s);
                    }
                    self.i = end;
                }
            }
        }

        if !buf.is_empty() { out.push(Inline::Text(buf)); }
        out
    }

    /// Parse `text](url)` starting at the given index (right after `[`).
    /// Returns (text, url, end_idx) on success.
    fn parse_bracket_paren(&self, start: usize) -> Option<(String, String, usize)> {
        let end_bracket = start + find_u8(&self.src[start..], b']')?;
        let text = std::str::from_utf8(&self.src[start..end_bracket]).ok()?.to_string();
        let after = end_bracket + 1;
        if after >= self.src.len() || self.src[after] != b'(' {
            return None;
        }
        let url_start = after + 1;
        let end_paren = url_start + find_u8(&self.src[url_start..], b')')?;
        let url = std::str::from_utf8(&self.src[url_start..end_paren]).ok()?.to_string();
        Some((text, url, end_paren + 1))
    }

    fn peek(&self, needle: &[u8]) -> bool {
        self.i + needle.len() <= self.src.len()
            && &self.src[self.i..self.i + needle.len()] == needle
    }
}

fn find_u8(slice: &[u8], b: u8) -> Option<usize> {
    slice.iter().position(|&x| x == b)
}

fn find_seq(slice: &[u8], target: &[u8]) -> Option<usize> {
    if target.is_empty() { return Some(0); }
    slice.windows(target.len()).position(|w| w == target)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}
