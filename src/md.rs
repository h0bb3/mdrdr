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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone)]
pub enum Block {
    Heading { level: u8, inlines: Vec<Inline> },
    Paragraph(Vec<Inline>),
    CodeBlock { lang: Option<String>, text: String },
    List { ordered: bool, items: Vec<ListItem> },
    BlockQuote(Vec<Block>),
    ThematicBreak,
    DisplayMath(String),
    Mermaid(String),
    Table {
        header: Vec<Vec<Inline>>,
        align: Vec<Align>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// One row in a bulleted or ordered list. `task` is `Some` when the item
/// starts with a GFM task-list marker (`[ ]` / `[x]`), and carries the byte
/// offset of the `[` in the original source so a click on the checkbox can
/// patch the file in-place.
#[derive(Debug, Clone)]
pub struct ListItem {
    pub inlines: Vec<Inline>,
    pub task: Option<TaskState>,
}

#[derive(Debug, Clone, Copy)]
pub struct TaskState {
    pub checked: bool,
    /// Byte offset in the source where the `[` of the checkbox lives.
    /// Click handler writes `x` or ` ` to `box_byte + 1` to toggle.
    pub box_byte: usize,
}

/// Cheap pass to count top-level headings for sidebar-outline sizing,
/// without constructing the full Block vec.
pub fn count_headings(src: &str) -> usize {
    let mut n = 0;
    let mut in_fence = false;
    for line in src.lines() {
        let trim = line.trim_start();
        if trim.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trim.starts_with('#') {
            let hashes = trim.chars().take_while(|c| *c == '#').count();
            if hashes >= 1
                && hashes <= 6
                && trim.as_bytes().get(hashes).copied() == Some(b' ')
            {
                n += 1;
            }
        }
    }
    n
}

pub fn parse(src: &str) -> Vec<Block> {
    parse_with_lines(src).0
}

/// Same as `parse`, but also returns a 1-based inclusive `(start, end)`
/// source-line span for each top-level block, in the same order as the
/// returned `Vec<Block>`. Used to map a rendered selection back to exact
/// source lines so an external editor (Claude Code) can target them.
pub fn parse_with_lines(src: &str) -> (Vec<Block>, Vec<(u32, u32)>) {
    // Walk source line-by-line *with* byte offsets so task-list parsing can
    // later patch the original file at the exact `[` of the checkbox.
    let mut lines: Vec<&str> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut pos = 0;
    for chunk in src.split_inclusive('\n') {
        starts.push(pos);
        let trimmed = chunk.trim_end_matches(|c: char| c == '\n' || c == '\r');
        lines.push(trimmed);
        pos += chunk.len();
    }
    let mut spans: Vec<(u32, u32)> = Vec::new();
    let blocks = parse_lines(&lines, Some(&starts), Some(&mut spans));
    (blocks, spans)
}

/// `line_starts` is `Some` only when the `lines` slice refers back to the
/// original source — then it can supply absolute byte offsets for task
/// list detection. Recursive callers that re-slice owned strings pass
/// `None`, which disables toggle-in-place for tasks inside blockquotes.
///
/// `spans`, when `Some`, collects a 1-based `(start_line, end_line)` for
/// every block pushed at this level (top-level only — nested blockquote
/// content passes `None`). Each loop iteration emits at most one block and
/// advances `i` to one-past-the-block, so `(blk_start+1, i)` is the block's
/// inclusive 1-based line span at every `continue`/fall-through point.
fn parse_lines(
    lines: &[&str],
    line_starts: Option<&[usize]>,
    mut spans: Option<&mut Vec<(u32, u32)>>,
) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let blk_start = i;
        let len0 = out.len();
        macro_rules! rec {
            () => {
                if let Some(sp) = spans.as_deref_mut() {
                    if out.len() > len0 {
                        sp.push((blk_start as u32 + 1, i as u32));
                    }
                }
            };
        }
        let line = lines[i];
        let trim = line.trim_start();

        if trim.starts_with("$$") {
            let mut body = String::new();
            // Same-line close: $$ a + b $$
            let first = &trim[2..];
            if let Some(close) = first.find("$$") {
                out.push(Block::DisplayMath(first[..close].trim().to_string()));
                i += 1;
                rec!();
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
            rec!();
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
            rec!();
            continue;
        }

        if let Some(level) = heading_level(trim) {
            let rest = trim[(level as usize)..].trim_start().trim_end_matches('#').trim();
            out.push(Block::Heading { level, inlines: parse_inlines(rest) });
            i += 1;
            rec!();
            continue;
        }

        if is_thematic_break(trim) {
            out.push(Block::ThematicBreak);
            i += 1;
            rec!();
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
            // Inner refs are owned Strings — offsets don't map back to the
            // original source, so task detection (and span tracking) is
            // disabled here.
            out.push(Block::BlockQuote(parse_lines(&refs, None, None)));
            rec!();
            continue;
        }

        if let Some((ordered, _)) = list_marker(trim) {
            let mut items: Vec<ListItem> = Vec::new();
            while i < lines.len() {
                let ln = lines[i];
                let tln = ln.trim_start();
                if let Some((o, me)) = list_marker(tln) {
                    if o != ordered { break; }
                    let rest = &tln[me..];
                    // GFM task-list marker: `[ ]` or `[x]` / `[X]` right
                    // after the list marker, followed by whitespace.
                    let task = parse_task_prefix(rest).and_then(|checked| {
                        let line_start = line_starts?.get(i).copied()?;
                        let indent = ln.len().saturating_sub(tln.len());
                        Some(TaskState {
                            checked,
                            box_byte: line_start + indent + me,
                        })
                    });
                    let inline_src = if task.is_some() { &rest[4..] } else { rest };
                    items.push(ListItem {
                        inlines: parse_inlines(inline_src),
                        task,
                    });
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
            rec!();
            continue;
        }

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Table detection: current line looks like a pipe row, and the next
        // line is a separator of dashes-with-optional-colons.
        if looks_like_table_row(line) && i + 1 < lines.len() {
            if let Some(align) = parse_table_separator(lines[i + 1]) {
                let header = split_pipe_row(line);
                if header.len() == align.len() {
                    let header_inlines: Vec<Vec<Inline>> =
                        header.iter().map(|c| parse_inlines(c)).collect();
                    i += 2;
                    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
                    while i < lines.len() && looks_like_table_row(lines[i]) {
                        let cells = split_pipe_row(lines[i]);
                        let mut row: Vec<Vec<Inline>> =
                            cells.iter().map(|c| parse_inlines(c)).collect();
                        while row.len() < align.len() {
                            row.push(Vec::new());
                        }
                        row.truncate(align.len());
                        rows.push(row);
                        i += 1;
                    }
                    out.push(Block::Table { header: header_inlines, align, rows });
                    rec!();
                    continue;
                }
            }
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
        rec!();
    }
    out
}

fn looks_like_table_row(line: &str) -> bool {
    let t = line.trim();
    t.contains('|') && t.chars().any(|c| c == '|')
        && t.trim_start().starts_with('|')
        && t.trim_end().ends_with('|')
}

fn parse_table_separator(line: &str) -> Option<Vec<Align>> {
    let t = line.trim();
    if !t.starts_with('|') || !t.ends_with('|') {
        return None;
    }
    let inner = &t[1..t.len() - 1];
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    let mut aligns = Vec::with_capacity(cells.len());
    for c in cells {
        if c.is_empty() {
            return None;
        }
        let starts = c.starts_with(':');
        let ends = c.ends_with(':');
        let body = c.trim_matches(':');
        if body.is_empty() || !body.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(match (starts, ends) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    if aligns.is_empty() {
        None
    } else {
        Some(aligns)
    }
}

fn split_pipe_row(line: &str) -> Vec<String> {
    // Split on unescaped '|', then trim. Leading/trailing empties (from the
    // outer pipes) are dropped.
    let bytes = line.trim().as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            cur.push('|');
            i += 2;
            continue;
        }
        if b == b'|' {
            out.push(cur.trim().to_string());
            cur = String::new();
            i += 1;
            continue;
        }
        let ch_len = match b {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        let end = (i + ch_len).min(bytes.len());
        if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
            cur.push_str(s);
        }
        i = end;
    }
    out.push(cur.trim().to_string());
    // drop empty leading/trailing from surrounding pipes
    if out.first().map(|s| s.is_empty()).unwrap_or(false) {
        out.remove(0);
    }
    if out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
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

/// Returns `Some(checked)` if `s` starts with a GFM task-list marker
/// (`[ ]`, `[x]`, or `[X]` followed by at least one space). `None`
/// otherwise.
fn parse_task_prefix(s: &str) -> Option<bool> {
    let b = s.as_bytes();
    if b.len() >= 4 && b[0] == b'[' && b[2] == b']' && b[3] == b' ' {
        match b[1] {
            b' ' => Some(false),
            b'x' | b'X' => Some(true),
            _ => None,
        }
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_line_spans_match_source() {
        // 1:# Title 2:blank 3-4:paragraph 5:blank 6-8:code 9:blank 10-11:list
        let src = "# Title\n\npara one\nstill para\n\n```\ncode\n```\n\n- a\n- b\n";
        let (blocks, spans) = parse_with_lines(src);
        assert_eq!(blocks.len(), spans.len(), "one span per block");
        assert_eq!(spans[0], (1, 1), "heading");
        assert_eq!(spans[1], (3, 4), "paragraph spans both lines");
        assert_eq!(spans[2], (6, 8), "code fence incl. both ``` lines");
        assert_eq!(spans[3], (10, 11), "list spans both items");
    }

    #[test]
    fn spans_disabled_by_default_parse() {
        // parse() must still work and produce the same blocks.
        let src = "# A\n\nb\n";
        let blocks = parse(src);
        let (blocks2, _) = parse_with_lines(src);
        assert_eq!(blocks.len(), blocks2.len());
    }
}
