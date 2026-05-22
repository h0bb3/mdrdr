//! Minimal Mermaid flowchart. Supported:
//!   graph TD|TB|LR|RL
//!   node shapes: A, A[label], A(label), A((label))
//!   edges:      A --> B, A --- B, A -->|label| B, chained: A --> B --> C
//!   dotted:     A -.-> B, A -.- B (and bi-/reverse forms)
//!   grouping:   subgraph ID["Label"] … end  (nested ok)
//!   styling:    style ID fill:#rrggbb,color:#rrggbb  (works on nodes and subgraphs)
//!
//! Layout is longest-path layering with fixed row/col sizes. Good enough for
//! the trees and pipelines that actually show up in notes. Subgraphs are
//! laid out internally first, then placed as super-nodes in the outer graph.

use std::collections::{HashMap, HashSet};

use fontdue::Font;

use crate::font::Fonts;
use crate::layout::{pick_font, FontId, Placed};
use crate::theme::{Rgba, Theme};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Top → bottom. `graph TD` / `graph TB` / no header suffix.
    TopBottom,
    /// Bottom → top. `graph BT`.
    BottomTop,
    /// Left → right. `graph LR`.
    LeftRight,
    /// Right → left. `graph RL`.
    RightLeft,
    /// Diagonal matrix. Nodes sit on the main diagonal of an N×N grid;
    /// forward edges route through the upper-right triangle (right then
    /// down), back edges through the lower-left triangle (left then up).
    /// Not part of mermaid's grammar — an mdrdr extension.
    Diagonal,
}

impl Direction {
    /// Is this direction laid out top-to-bottom (TB / BT)? The layout code
    /// only distinguishes two fundamental axes; the BT/RL variants flip
    /// coordinates at the end.
    fn is_vertical(self) -> bool {
        matches!(self, Direction::TopBottom | Direction::BottomTop)
    }
    fn is_flipped(self) -> bool {
        matches!(self, Direction::BottomTop | Direction::RightLeft)
    }
    pub fn label(self) -> &'static str {
        match self {
            Direction::TopBottom => "TB",
            Direction::BottomTop => "BT",
            Direction::LeftRight => "LR",
            Direction::RightLeft => "RL",
            Direction::Diagonal => "DG",
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum Shape {
    Rect,
    Rounded,
    Circle,
    Diamond,
}

#[derive(Debug)]
struct Node {
    id: String,
    label: String,
    shape: Shape,
    // Layout output
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum EdgeKind {
    /// a --> b        — head on the `to` side
    Arrow,
    /// a <-- b        — head on the `from` side
    ReverseArrow,
    /// a <--> b       — heads on both ends
    BiArrow,
    /// a --- b        — plain line
    Line,
}

#[derive(Debug)]
struct Edge {
    from: String,
    to: String,
    label: Option<String>,
    kind: EdgeKind,
    /// `-.->`, `-.-`, etc. Drawn dashed.
    dotted: bool,
}

#[derive(Debug, Default, Clone, Copy)]
struct StyleOverride {
    fill: Option<Rgba>,
    text: Option<Rgba>,
}

#[derive(Debug)]
struct Subgraph {
    id: String,
    label: String,
    /// Direct child node ids (declaration order).
    child_nodes: Vec<String>,
    /// Indices into `Graph.subgraphs` of direct child subgraphs.
    child_groups: Vec<usize>,
    /// Index of parent subgraph, if any.
    parent: Option<usize>,
    // Computed by layout (final absolute coordinates):
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Header (label strip) height — children sit below this within the container.
    header_h: f32,
}

struct Graph {
    direction: Direction,
    nodes: HashMap<String, Node>,
    order: Vec<String>,
    edges: Vec<Edge>,
    subgraphs: Vec<Subgraph>,
    /// node id → index into `subgraphs`
    node_to_group: HashMap<String, usize>,
    /// subgraph id → index into `subgraphs`
    group_id_to_index: HashMap<String, usize>,
    /// id (node or subgraph) → style overrides
    styles: HashMap<String, StyleOverride>,
}

pub struct MermaidRender {
    pub items: Vec<Placed>,
    pub width: f32,
    pub height: f32,
}

pub fn render(src: &str, max_width: f32, theme: &Theme, fonts: &Fonts) -> Option<MermaidRender> {
    render_with(src, max_width, theme, fonts, None)
}

/// Same as `render`, but lets the caller replace the direction declared
/// by the source's `flowchart ...` header. Used by the per-diagram layout
/// override the right-click context menu offers. The override is ignored
/// for sequence diagrams — they don't have a TB/LR concept.
pub fn render_with(
    src: &str,
    max_width: f32,
    theme: &Theme,
    fonts: &Fonts,
    override_dir: Option<Direction>,
) -> Option<MermaidRender> {
    if let Some(seq) = parse_sequence(src) {
        return Some(layout_sequence(seq, max_width, theme, fonts));
    }
    let mut graph = parse(src)?;
    if let Some(d) = override_dir {
        graph.direction = d;
    }
    Some(layout(graph, max_width, theme, fonts))
}

// ───── parse ────────────────────────────────────────────────────────────────

fn parse(src: &str) -> Option<Graph> {
    let mut direction = Direction::TopBottom;
    let mut nodes: HashMap<String, Node> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut subgraphs: Vec<Subgraph> = Vec::new();
    let mut group_id_to_index: HashMap<String, usize> = HashMap::new();
    let mut node_to_group: HashMap<String, usize> = HashMap::new();
    let mut styles: HashMap<String, StyleOverride> = HashMap::new();
    let mut sg_stack: Vec<usize> = Vec::new();

    let mut saw_header = false;
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !saw_header {
            if let Some(dir) = parse_header(line) {
                direction = dir;
                saw_header = true;
                continue;
            } else {
                // No explicit graph/flowchart header → not a mermaid block we
                // can handle. Caller falls back to rendering as a code block.
                return None;
            }
        }

        let lower_first = line
            .split(|c: char| c.is_whitespace() || c == '[' || c == '"')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        // Block-structure keywords.
        if lower_first == "subgraph" {
            let rest = line["subgraph".len()..].trim();
            let (id, label) = parse_subgraph_header(rest);
            let parent = sg_stack.last().copied();
            let idx = subgraphs.len();
            if let Some(p) = parent {
                subgraphs[p].child_groups.push(idx);
            }
            subgraphs.push(Subgraph {
                id: id.clone(),
                label,
                child_nodes: Vec::new(),
                child_groups: Vec::new(),
                parent,
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                header_h: 0.0,
            });
            group_id_to_index.insert(id, idx);
            sg_stack.push(idx);
            continue;
        }
        if lower_first == "end" {
            sg_stack.pop();
            continue;
        }
        if lower_first == "style" {
            parse_style_line(&line["style".len()..], &mut styles);
            continue;
        }
        // Ignore directives we don't visualise; keep them from being parsed
        // as nodes.
        if matches!(
            lower_first.as_str(),
            "classdef" | "class" | "linkstyle" | "click" | "direction"
        ) {
            continue;
        }

        let new_ids = parse_line(line, &mut nodes, &mut order, &mut edges);
        if let Some(&current) = sg_stack.last() {
            for id in new_ids {
                // Don't assign subgraph-id pseudo-nodes (e.g. cross-subgraph
                // edge endpoints) to a group — they'll be filtered out below.
                if !group_id_to_index.contains_key(&id) {
                    node_to_group.entry(id.clone()).or_insert(current);
                    if !subgraphs[current].child_nodes.contains(&id) {
                        subgraphs[current].child_nodes.push(id);
                    }
                }
            }
        }
    }
    if !saw_header {
        return None;
    }

    // Edge endpoints that name a subgraph (e.g. `EMSX <--> NovaCore`) end up
    // as bogus nodes in `nodes` / `order`. Strip them; the renderer routes
    // those edges through the subgraph container instead.
    let group_ids: HashSet<String> = group_id_to_index.keys().cloned().collect();
    nodes.retain(|id, _| !group_ids.contains(id));
    order.retain(|id| !group_ids.contains(id));

    Some(Graph {
        direction,
        nodes,
        order,
        edges,
        subgraphs,
        node_to_group,
        group_id_to_index,
        styles,
    })
}

/// `ID["Label"]`, `ID[Label]`, `ID`, `"Label"`. Whitespace-trimmed.
fn parse_subgraph_header(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if rest.is_empty() {
        // Mermaid allows anonymous subgraphs; synthesise an id.
        return (String::from("__anon"), String::new());
    }
    if let Some(open) = rest.find('[') {
        let id = rest[..open].trim().to_string();
        let close = rest.rfind(']').unwrap_or(rest.len());
        let inner = rest[open + 1..close].trim();
        let unquoted = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(inner);
        let label = normalise_label(unquoted);
        let id = if id.is_empty() { label.clone() } else { id };
        return (id, label);
    }
    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(end) = stripped.find('"') {
            let label = stripped[..end].to_string();
            return (label.clone(), label);
        }
    }
    // Bare id.
    let id = rest.to_string();
    (id.clone(), id)
}

fn parse_style_line(rest: &str, styles: &mut HashMap<String, StyleOverride>) {
    let rest = rest.trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let Some(id) = parts.next() else { return };
    let id = id.trim().to_string();
    if id.is_empty() {
        return;
    }
    let props = parts.next().unwrap_or("");
    let mut style = StyleOverride::default();
    for prop in props.split(',') {
        let prop = prop.trim();
        if let Some(v) = prop.strip_prefix("fill:") {
            style.fill = parse_hex_color(v.trim());
        } else if let Some(v) = prop.strip_prefix("color:") {
            style.text = parse_hex_color(v.trim());
        }
    }
    styles.insert(id, style);
}

fn parse_hex_color(s: &str) -> Option<Rgba> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some([r, g, b, 255])
        }
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
            Some([r, g, b, 255])
        }
        _ => None,
    }
}

fn parse_header(line: &str) -> Option<Direction> {
    let lower = line.to_ascii_lowercase();
    for kw in ["graph", "flowchart"] {
        if let Some(rest) = lower.strip_prefix(kw) {
            let r = rest.trim();
            return Some(match r {
                "td" | "tb" | "" => Direction::TopBottom,
                "bt" => Direction::BottomTop,
                "lr" => Direction::LeftRight,
                "rl" => Direction::RightLeft,
                "dg" | "diagonal" | "diag" => Direction::Diagonal,
                _ => Direction::TopBottom,
            });
        }
    }
    None
}

/// Returns the node ids that were newly created (so the caller can assign
/// them to the currently-open subgraph). Ids that already existed are
/// not returned — they keep their original subgraph membership.
fn parse_line(
    line: &str,
    nodes: &mut HashMap<String, Node>,
    order: &mut Vec<String>,
    edges: &mut Vec<Edge>,
) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut prev: Option<String> = None;
    let mut pending_arrow: Option<(EdgeKind, bool, Option<String>)> = None;
    let mut new_ids: Vec<String> = Vec::new();

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Try to parse an arrow.
        if let Some((kind, dotted, after)) = read_arrow(bytes, i) {
            let mut j = after;
            // Optional |label|
            let mut label: Option<String> = None;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() { j += 1; }
            if j < bytes.len() && bytes[j] == b'|' {
                j += 1;
                let start = j;
                while j < bytes.len() && bytes[j] != b'|' { j += 1; }
                label = Some(normalise_label(&line[start..j]));
                if j < bytes.len() { j += 1; }
            }
            pending_arrow = Some((kind, dotted, label));
            i = j;
            continue;
        }

        // Otherwise parse a node.
        if let Some((id, shape, end)) = read_node(bytes, i) {
            let label = shape.1;
            let shape_kind = shape.0;
            if !nodes.contains_key(&id) {
                nodes.insert(
                    id.clone(),
                    Node {
                        id: id.clone(),
                        label: label.clone().unwrap_or_else(|| id.clone()),
                        shape: shape_kind,
                        x: 0.0, y: 0.0, w: 0.0, h: 0.0,
                    },
                );
                order.push(id.clone());
                new_ids.push(id.clone());
            } else if let Some(l) = label {
                // Upgrade label / shape if re-declared with content.
                if let Some(n) = nodes.get_mut(&id) {
                    n.label = l;
                    n.shape = shape_kind;
                }
            }
            if let (Some(p), Some((kind, dotted, lab))) = (prev.as_ref(), pending_arrow.take()) {
                edges.push(Edge { from: p.clone(), to: id.clone(), label: lab, kind, dotted });
            }
            prev = Some(id);
            i = end;
            continue;
        }

        // Nothing matched — skip one byte to avoid infinite loop.
        i += 1;
    }
    new_ids
}

fn read_arrow(b: &[u8], i: usize) -> Option<(EdgeKind, bool, usize)> {
    // Longest first so `<-->` isn't shortened to `-->`, and dotted variants
    // (which contain `.`) aren't shortened to their solid cousins.
    let m = |pat: &[u8]| -> Option<usize> {
        if b.len() >= i + pat.len() && &b[i..i + pat.len()] == pat {
            Some(i + pat.len())
        } else {
            None
        }
    };
    // Dotted: `-.->` and `<-.->`/`<-.-`/`-.-`. Mermaid also allows extra
    // dots (`-..->`, `-...->`) for visual length; treat all as the same
    // shape so the dot-count doesn't change semantics.
    for (pat, kind) in [
        (b"<-...->".as_slice(), EdgeKind::BiArrow),
        (b"<-..->".as_slice(), EdgeKind::BiArrow),
        (b"<-.->".as_slice(), EdgeKind::BiArrow),
        (b"-...->".as_slice(), EdgeKind::Arrow),
        (b"-..->".as_slice(), EdgeKind::Arrow),
        (b"-.->".as_slice(), EdgeKind::Arrow),
        (b"<-...-".as_slice(), EdgeKind::ReverseArrow),
        (b"<-..-".as_slice(), EdgeKind::ReverseArrow),
        (b"<-.-".as_slice(), EdgeKind::ReverseArrow),
        (b"-...-".as_slice(), EdgeKind::Line),
        (b"-..-".as_slice(), EdgeKind::Line),
        (b"-.-".as_slice(), EdgeKind::Line),
    ] {
        if let Some(end) = m(pat) {
            return Some((kind, true, end));
        }
    }
    // Solid: longest first.
    for (pat, kind) in [
        (b"<-->".as_slice(), EdgeKind::BiArrow),
        (b"<--".as_slice(), EdgeKind::ReverseArrow),
        (b"-->".as_slice(), EdgeKind::Arrow),
        (b"---".as_slice(), EdgeKind::Line),
    ] {
        if let Some(end) = m(pat) {
            return Some((kind, false, end));
        }
    }
    None
}

fn read_node(b: &[u8], i: usize) -> Option<(String, (Shape, Option<String>), usize)> {
    let mut j = i;
    let start = j;
    while j < b.len() && is_id_byte(b[j]) { j += 1; }
    if j == start {
        return None;
    }
    let id = std::str::from_utf8(&b[start..j]).ok()?.to_string();

    // Optional shape + label
    if j < b.len() {
        match b[j] {
            b'[' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b']' { k += 1; }
                let label = clean_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Rect, Some(label)), (k + 1).min(b.len())));
            }
            b'{' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b'}' { k += 1; }
                let label = clean_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Diamond, Some(label)), (k + 1).min(b.len())));
            }
            b'(' if j + 1 < b.len() && b[j + 1] == b'(' => {
                let lab_start = j + 2;
                let mut k = lab_start;
                while k + 1 < b.len() && !(b[k] == b')' && b[k + 1] == b')') { k += 1; }
                let label = clean_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Circle, Some(label)), (k + 2).min(b.len())));
            }
            b'(' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b')' { k += 1; }
                let label = clean_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Rounded, Some(label)), (k + 1).min(b.len())));
            }
            _ => {}
        }
    }
    Some((id, (Shape::Rect, None), j))
}

/// Mermaid allows wrapping a label in double quotes so it can contain
/// brackets/parens/spaces (`A["Step (one)"]`). The quotes are syntax, not
/// content — strip them before normalising line breaks.
fn clean_label(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    normalise_label(unquoted)
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Mermaid labels can contain `<br>` / `<br/>` / `<br />` as line breaks.
/// Normalise all three to `\n` so downstream layout can just split().
fn normalise_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Case-insensitive <br ...> match ending at the next `>`.
        if bytes[i] == b'<'
            && i + 3 <= bytes.len()
            && bytes[i + 1].eq_ignore_ascii_case(&b'b')
            && bytes[i + 2].eq_ignore_ascii_case(&b'r')
        {
            // Find the closing `>` within a short window (`<br/>` or `<br />`).
            let mut j = i + 3;
            let limit = (i + 8).min(bytes.len());
            while j < limit && bytes[j] != b'>' {
                j += 1;
            }
            if j < limit && bytes[j] == b'>' {
                out.push('\n');
                i = j + 1;
                continue;
            }
        }
        // Safe because we advance exactly one byte and `raw` is UTF-8 —
        // but only push a char boundary. Find the next char boundary.
        let ch_end = (i + 1..=raw.len()).find(|e| raw.is_char_boundary(*e)).unwrap_or(raw.len());
        out.push_str(&raw[i..ch_end]);
        i = ch_end;
    }
    out
}

// ───── layout ───────────────────────────────────────────────────────────────

fn measure_label(text: &str, size: f32, font: &Font) -> (f32, f32) {
    // Multi-line labels (mermaid's `<br/>` normalised to `\n`): width =
    // widest line, height = line-count * line-height.
    let mut max_w = 0.0f32;
    let mut lines = 0;
    for line in text.split('\n') {
        let w: f32 = line.chars().map(|c| font.metrics(c, size).advance_width).sum();
        if w > max_w { max_w = w; }
        lines += 1;
    }
    let h = (lines.max(1) as f32) * size * 1.25;
    (max_w, h)
}

/// Layered placement of a flat set of units. Computes (x, y) positions and
/// layer indices. Unscaled. Used for both each subgraph's internal layout
/// and for the top-level layout (treating each subgraph as a super-node).
fn place_layered(
    unit_ids: &[String],
    sizes: &HashMap<String, (f32, f32)>,
    edges: &[&Edge],
    direction: Direction,
    font: &Font,
    label_size: f32,
    h_gap: f32,
    v_gap: f32,
) -> (HashMap<String, (f32, f32)>, HashMap<String, usize>, f32, f32) {
    // Layer assignment: longest path from sources using only edges that go
    // forward in declaration order (back-edges don't influence layers).
    let mut idx_of: HashMap<&str, usize> = HashMap::new();
    for (i, id) in unit_ids.iter().enumerate() {
        idx_of.insert(id.as_str(), i);
    }
    let mut preds: HashMap<String, Vec<String>> = HashMap::new();
    for id in unit_ids {
        preds.insert(id.clone(), Vec::new());
    }
    for e in edges {
        let (Some(&fi), Some(&ti)) = (idx_of.get(e.from.as_str()), idx_of.get(e.to.as_str())) else { continue };
        if fi < ti {
            preds.entry(e.to.clone()).or_default().push(e.from.clone());
        }
    }
    let mut layer: HashMap<String, usize> = HashMap::new();
    let mut changed = true;
    let mut guard = 0;
    while changed && guard < unit_ids.len() * 2 + 5 {
        changed = false;
        for id in unit_ids {
            let l = preds
                .get(id)
                .map(|ps| ps.iter().map(|p| *layer.get(p).unwrap_or(&0) + 1).max().unwrap_or(0))
                .unwrap_or(0);
            let cur = *layer.get(id).unwrap_or(&0);
            if l != cur && (cur == 0 || l > cur) {
                layer.insert(id.clone(), l);
                changed = true;
            }
        }
        guard += 1;
    }
    for id in unit_ids {
        layer.entry(id.clone()).or_insert(0);
    }

    // Group by layer in declaration order.
    let mut per_layer: Vec<Vec<String>> = Vec::new();
    for id in unit_ids {
        let l = *layer.get(id).unwrap_or(&0);
        while per_layer.len() <= l {
            per_layer.push(Vec::new());
        }
        per_layer[l].push(id.clone());
    }

    let mut positions: HashMap<String, (f32, f32)> = HashMap::new();
    let (mut total_w, mut total_h) = (0.0f32, 0.0f32);

    match direction {
        Direction::TopBottom | Direction::BottomTop => {
            let row_heights: Vec<f32> = per_layer
                .iter()
                .map(|row| row.iter().map(|id| sizes.get(id).map(|s| s.1).unwrap_or(0.0)).fold(0.0f32, f32::max))
                .collect();
            let row_widths: Vec<f32> = per_layer
                .iter()
                .map(|row| {
                    let ws: f32 = row.iter().map(|id| sizes.get(id).map(|s| s.0).unwrap_or(0.0)).sum();
                    let gaps = h_gap * (row.len().saturating_sub(1)) as f32;
                    ws + gaps
                })
                .collect();
            let max_w = row_widths.iter().fold(0.0f32, |a, &b| a.max(b));
            total_w = max_w.max(1.0);

            let gap_n = per_layer.len().saturating_sub(1);
            let mut gap_h: Vec<f32> = vec![v_gap; gap_n];
            let lbl_size = label_size * 0.75;
            let chip_pad = 4.0;
            let breathing = 28.0;
            for e in edges {
                let Some(lab) = e.label.as_deref() else { continue };
                let (_lw, lh) = measure_label(lab, lbl_size, font);
                let from_layer = *layer.get(&e.from).unwrap_or(&0);
                let to_layer = *layer.get(&e.to).unwrap_or(&0);
                if to_layer > from_layer && from_layer < gap_h.len() {
                    let need = lh + chip_pad * 2.0 + breathing * 2.0;
                    if need > gap_h[from_layer] {
                        gap_h[from_layer] = need;
                    }
                }
            }

            let mut y = 0.0;
            for (i, row) in per_layer.iter().enumerate() {
                let row_w = row_widths[i];
                let mut x = (total_w - row_w) / 2.0;
                let row_h = row_heights[i];
                for id in row {
                    let (uw, uh) = sizes.get(id).copied().unwrap_or((0.0, 0.0));
                    positions.insert(id.clone(), (x, y + (row_h - uh) / 2.0));
                    x += uw + h_gap;
                }
                y += row_h;
                if i + 1 < per_layer.len() {
                    y += gap_h[i];
                }
            }
            total_h = y;
        }
        Direction::LeftRight | Direction::RightLeft => {
            let col_widths: Vec<f32> = per_layer
                .iter()
                .map(|col| col.iter().map(|id| sizes.get(id).map(|s| s.0).unwrap_or(0.0)).fold(0.0f32, f32::max))
                .collect();
            let col_heights: Vec<f32> = per_layer
                .iter()
                .map(|col| {
                    let hs: f32 = col.iter().map(|id| sizes.get(id).map(|s| s.1).unwrap_or(0.0)).sum();
                    let gaps = v_gap * (col.len().saturating_sub(1)) as f32;
                    hs + gaps
                })
                .collect();
            let max_h = col_heights.iter().fold(0.0f32, |a, &b| a.max(b));
            total_h = max_h.max(1.0);

            let gap_n = per_layer.len().saturating_sub(1);
            let mut gap_w: Vec<f32> = vec![h_gap; gap_n];
            let lbl_size = label_size * 0.75;
            let chip_pad = 4.0;
            let breathing = 28.0;
            for e in edges {
                let Some(lab) = e.label.as_deref() else { continue };
                let (lw, _lh) = measure_label(lab, lbl_size, font);
                let from_layer = *layer.get(&e.from).unwrap_or(&0);
                let to_layer = *layer.get(&e.to).unwrap_or(&0);
                if to_layer > from_layer && from_layer < gap_w.len() {
                    let need = lw + chip_pad * 2.0 + breathing * 2.0;
                    if need > gap_w[from_layer] {
                        gap_w[from_layer] = need;
                    }
                }
            }

            let mut x = 0.0;
            for (i, col) in per_layer.iter().enumerate() {
                let col_h = col_heights[i];
                let mut y = (total_h - col_h) / 2.0;
                let col_w = col_widths[i];
                for id in col {
                    let (uw, uh) = sizes.get(id).copied().unwrap_or((0.0, 0.0));
                    positions.insert(id.clone(), (x + (col_w - uw) / 2.0, y));
                    y += uh + v_gap;
                }
                x += col_w;
                if i + 1 < per_layer.len() {
                    x += gap_w[i];
                }
            }
            total_w = x;
        }
        Direction::Diagonal => {
            // Tight diagonal: each node's top-left sits at the running sum of
            // the previous nodes' sizes plus a small bend gap, so adjacent
            // boxes nearly touch corner-to-corner instead of all sharing a
            // max-of-everyone cell. The gap exists to host the L-elbow of
            // the edge connecting them — wide enough to read, narrow enough
            // not to waste a wedge of canvas per cell.
            let diag_gap: f32 = 8.0;
            let mut x_acc = 0.0f32;
            let mut y_acc = 0.0f32;
            for id in unit_ids {
                let (uw, uh) = sizes.get(id).copied().unwrap_or((0.0, 0.0));
                positions.insert(id.clone(), (x_acc, y_acc));
                x_acc += uw + diag_gap;
                y_acc += uh + diag_gap;
            }
            total_w = (x_acc - diag_gap).max(0.0);
            total_h = (y_acc - diag_gap).max(0.0);
        }
    }

    (positions, layer, total_w, total_h)
}

/// Outgoing anchor on a bbox `(bx, by, bw, bh)`, scaled by `scale`.
fn anchor_bbox_out(bx: f32, by: f32, bw: f32, bh: f32, dir: &Direction, scale: f32) -> (f32, f32) {
    match dir {
        Direction::TopBottom => ((bx + bw / 2.0) * scale, (by + bh) * scale),
        Direction::BottomTop => ((bx + bw / 2.0) * scale, by * scale),
        Direction::LeftRight => ((bx + bw) * scale, (by + bh / 2.0) * scale),
        Direction::RightLeft => (bx * scale, (by + bh / 2.0) * scale),
        Direction::Diagonal => ((bx + bw / 2.0) * scale, (by + bh / 2.0) * scale),
    }
}

fn anchor_bbox_in(bx: f32, by: f32, bw: f32, bh: f32, dir: &Direction, scale: f32) -> (f32, f32) {
    match dir {
        Direction::TopBottom => ((bx + bw / 2.0) * scale, by * scale),
        Direction::BottomTop => ((bx + bw / 2.0) * scale, (by + bh) * scale),
        Direction::LeftRight => (bx * scale, (by + bh / 2.0) * scale),
        Direction::RightLeft => ((bx + bw) * scale, (by + bh / 2.0) * scale),
        Direction::Diagonal => ((bx + bw / 2.0) * scale, (by + bh / 2.0) * scale),
    }
}

/// Approximate dashed line at an arbitrary angle. Splits the segment into
/// short dash+gap runs along its direction vector.
fn emit_dashed_seg(
    items: &mut Vec<Placed>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    thickness: f32,
    color: Rgba,
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.001 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let dash: f32 = 6.0;
    let gap: f32 = 4.0;
    let mut t = 0.0f32;
    while t < len {
        let end = (t + dash).min(len);
        items.push(Placed::Line {
            x1: x0 + ux * t,
            y1: y0 + uy * t,
            x2: x0 + ux * end,
            y2: y0 + uy * end,
            thickness,
            color,
        });
        t = end + gap;
    }
}

fn layout(mut graph: Graph, max_width: f32, theme: &Theme, fonts: &Fonts) -> MermaidRender {
    let pad_x = 14.0;
    let pad_y = 10.0;
    let min_w = 80.0;
    let label_size = theme.body_size * 0.9;
    let sg_label_size = theme.body_size;
    let sg_pad = 16.0;
    let h_gap = 40.0;
    let v_gap = 50.0;
    let font = &fonts.body;

    // 1. Measure nodes.
    for id in &graph.order {
        if let Some(n) = graph.nodes.get_mut(id) {
            let (lw, lh) = measure_label(&n.label, label_size, font);
            match n.shape {
                Shape::Circle => {
                    let d = (lw.max(lh) + pad_x * 2.0).max(min_w);
                    n.w = d;
                    n.h = d;
                }
                _ => {
                    n.w = (lw + pad_x * 2.0).max(min_w);
                    n.h = lh + pad_y * 2.0;
                }
            }
        }
    }

    // 2. For each subgraph (deepest first), compute its internal layout and
    //    container bbox. Child positions are stored as offsets *relative to
    //    the subgraph's inner origin* in `internal_pos`; we'll translate to
    //    absolute coords after the top-level layout fixes each subgraph's
    //    position.
    // Subgraphs inherit the outer flow direction. Diagonal cascades through so
    // children of a diagonal flowchart also tile on the diagonal of their
    // container; BT/RL flip only at the top level (children stay TB/LR — the
    // BT/RL flip is applied to top-level units after placement, not to the
    // intra-subgraph layout).
    let inner_dir = match graph.direction {
        Direction::Diagonal => Direction::Diagonal,
        Direction::BottomTop | Direction::TopBottom => Direction::TopBottom,
        Direction::LeftRight | Direction::RightLeft => Direction::LeftRight,
    };

    let mut depths: Vec<(usize, usize)> = (0..graph.subgraphs.len())
        .map(|i| {
            let mut d = 0;
            let mut cur = graph.subgraphs[i].parent;
            while let Some(p) = cur {
                d += 1;
                cur = graph.subgraphs[p].parent;
            }
            (d, i)
        })
        .collect();
    depths.sort_by(|a, b| b.0.cmp(&a.0));

    let mut internal_pos: HashMap<String, (f32, f32)> = HashMap::new();
    let mut internal_extent: HashMap<usize, (f32, f32)> = HashMap::new();

    for (_, sg_idx) in &depths {
        let sg_idx = *sg_idx;
        let mut unit_ids: Vec<String> = Vec::new();
        let mut sizes: HashMap<String, (f32, f32)> = HashMap::new();
        for nid in graph.subgraphs[sg_idx].child_nodes.clone() {
            if let Some(n) = graph.nodes.get(&nid) {
                unit_ids.push(nid.clone());
                sizes.insert(nid.clone(), (n.w, n.h));
            }
        }
        let child_groups = graph.subgraphs[sg_idx].child_groups.clone();
        for cgi in child_groups {
            let cg = &graph.subgraphs[cgi];
            unit_ids.push(cg.id.clone());
            sizes.insert(cg.id.clone(), (cg.w, cg.h));
        }

        let unit_set: HashSet<String> = unit_ids.iter().cloned().collect();
        let internal_edges: Vec<&Edge> = graph
            .edges
            .iter()
            .filter(|e| unit_set.contains(&e.from) && unit_set.contains(&e.to))
            .collect();

        let (positions, _layers, inner_w, inner_h) =
            place_layered(&unit_ids, &sizes, &internal_edges, inner_dir, font, label_size, h_gap, v_gap);

        let header_h = if graph.subgraphs[sg_idx].label.is_empty() {
            sg_pad
        } else {
            sg_label_size * 1.3 + 12.0
        };
        let (lbl_w, _) = measure_label(&graph.subgraphs[sg_idx].label, sg_label_size, font);
        let min_outer_w = lbl_w + sg_pad * 4.0;
        let outer_w = (inner_w + sg_pad * 2.0).max(min_outer_w).max(min_w * 1.5);
        let outer_h = header_h + inner_h + sg_pad * 2.0;

        graph.subgraphs[sg_idx].w = outer_w;
        graph.subgraphs[sg_idx].h = outer_h;
        graph.subgraphs[sg_idx].header_h = header_h;
        internal_extent.insert(sg_idx, (inner_w, inner_h));

        // Centre the inner layout horizontally inside the container when
        // the container ended up wider than the strict content width (e.g.,
        // because the label needed more room).
        let extra_left = ((outer_w - sg_pad * 2.0) - inner_w) / 2.0;
        for (id, (px, py)) in positions {
            internal_pos.insert(id, (px + extra_left, py));
        }
    }

    // 3. Top-level units = top-level subgraphs + nodes that aren't inside
    //    any subgraph. Order follows declaration order from `graph.order`,
    //    falling back to `graph.subgraphs` for top-level subgraphs whose
    //    children don't appear in `graph.order` (rare).
    let top_level_of = |id: &str, g: &Graph| -> Option<String> {
        if let Some(&sg_idx) = g.node_to_group.get(id) {
            let mut cur = sg_idx;
            while let Some(p) = g.subgraphs[cur].parent {
                cur = p;
            }
            Some(g.subgraphs[cur].id.clone())
        } else if let Some(&sg_idx) = g.group_id_to_index.get(id) {
            let mut cur = sg_idx;
            while let Some(p) = g.subgraphs[cur].parent {
                cur = p;
            }
            Some(g.subgraphs[cur].id.clone())
        } else if g.nodes.contains_key(id) {
            Some(id.to_string())
        } else {
            None
        }
    };

    let mut top_unit_ids: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for id in &graph.order {
        if let Some(tl) = top_level_of(id, &graph) {
            if seen.insert(tl.clone()) {
                top_unit_ids.push(tl);
            }
        }
    }
    for sg in &graph.subgraphs {
        if sg.parent.is_none() && seen.insert(sg.id.clone()) {
            top_unit_ids.push(sg.id.clone());
        }
    }

    let mut top_sizes: HashMap<String, (f32, f32)> = HashMap::new();
    for id in &top_unit_ids {
        if let Some(&sg_idx) = graph.group_id_to_index.get(id) {
            let sg = &graph.subgraphs[sg_idx];
            top_sizes.insert(id.clone(), (sg.w, sg.h));
        } else if let Some(n) = graph.nodes.get(id) {
            top_sizes.insert(id.clone(), (n.w, n.h));
        }
    }

    // Project edges to the top level: each endpoint becomes its top-level
    // container id. Edges that collapse to the same top-level unit (i.e.,
    // both endpoints live inside the same subgraph) don't influence the
    // top-level layout.
    let top_set: HashSet<String> = top_unit_ids.iter().cloned().collect();
    let projected_edges: Vec<Edge> = graph
        .edges
        .iter()
        .filter_map(|e| {
            let tlf = top_level_of(&e.from, &graph)?;
            let tlt = top_level_of(&e.to, &graph)?;
            if tlf == tlt || !top_set.contains(&tlf) || !top_set.contains(&tlt) {
                return None;
            }
            Some(Edge {
                from: tlf,
                to: tlt,
                label: e.label.clone(),
                kind: e.kind,
                dotted: e.dotted,
            })
        })
        .collect();
    let projected_refs: Vec<&Edge> = projected_edges.iter().collect();

    let (mut top_positions, top_layers, total_w, total_h) = place_layered(
        &top_unit_ids,
        &top_sizes,
        &projected_refs,
        graph.direction,
        font,
        label_size,
        h_gap,
        v_gap,
    );

    // BT/RL flip — mirror unit positions along the flow axis (only at the
    // top level; children inside subgraphs use the inner direction which
    // never flips, so their relative positions stay valid).
    if graph.direction.is_flipped() {
        for (id, p) in top_positions.iter_mut() {
            let (uw, uh) = top_sizes.get(id).copied().unwrap_or((0.0, 0.0));
            if graph.direction.is_vertical() {
                p.1 = total_h - p.1 - uh;
            } else {
                p.0 = total_w - p.0 - uw;
            }
        }
    }

    // 4. Commit top-level positions to graph (unscaled absolute coords).
    for (id, &(x, y)) in &top_positions {
        if let Some(&sg_idx) = graph.group_id_to_index.get(id) {
            graph.subgraphs[sg_idx].x = x;
            graph.subgraphs[sg_idx].y = y;
        } else if let Some(n) = graph.nodes.get_mut(id) {
            n.x = x;
            n.y = y;
        }
    }

    // 5. Translate each subgraph's children to absolute coords (root-first
    //    so a parent's absolute position is set before we touch its kids).
    let mut root_first: Vec<usize> = depths.iter().map(|(_, i)| *i).collect();
    root_first.reverse();
    for sg_idx in root_first {
        let parent_x = graph.subgraphs[sg_idx].x;
        let parent_y = graph.subgraphs[sg_idx].y;
        let header_h = graph.subgraphs[sg_idx].header_h;
        let inner_x = parent_x + sg_pad;
        let inner_y = parent_y + header_h;
        let child_nodes = graph.subgraphs[sg_idx].child_nodes.clone();
        let child_groups = graph.subgraphs[sg_idx].child_groups.clone();
        for nid in &child_nodes {
            if let Some(&(rx, ry)) = internal_pos.get(nid) {
                if let Some(n) = graph.nodes.get_mut(nid) {
                    n.x = inner_x + rx;
                    n.y = inner_y + ry;
                }
            }
        }
        for cgi in &child_groups {
            let cid = graph.subgraphs[*cgi].id.clone();
            if let Some(&(rx, ry)) = internal_pos.get(&cid) {
                graph.subgraphs[*cgi].x = inner_x + rx;
                graph.subgraphs[*cgi].y = inner_y + ry;
            }
        }
    }

    // 6. Scale to fit the available width.
    let target = max_width * 0.92;
    let scale = if max_width <= 0.0 || total_w <= 0.0 {
        1.0
    } else if total_w > max_width {
        max_width / total_w
    } else if total_w < target {
        (target / total_w).min(1.6)
    } else {
        1.0
    };

    // 7. Back-edge lanes at the top level. Internal subgraph edges don't
    //    use the lane; if there are back-edges *inside* a subgraph, they
    //    just clip straight through (consistent with the rest of the
    //    layout's "good enough" approach for the common case).
    let stroke: Rgba = theme.fg;
    let fill: Rgba = theme.code_bg;
    let mut items: Vec<Placed> = Vec::new();

    let lane_gap = 18.0 * scale;
    // Edges that must detour around the side instead of running straight
    // between layers: either they go *backwards* (target above source) or
    // they *skip* a layer (target more than one layer past source). A
    // straight line for either would cut through the subgraph(s) sitting in
    // between. Each gets its own stacked lane so multiple detours don't
    // overlap. (Adjacent forward edges — the common case — stay straight.)
    let mut side_edges: Vec<(usize, f32)> = Vec::new();
    {
        let mut lane_idx = 0usize;
        for (ei, e) in projected_edges.iter().enumerate() {
            let from_layer = *top_layers.get(&e.from).unwrap_or(&0);
            let to_layer = *top_layers.get(&e.to).unwrap_or(&0);
            if from_layer > to_layer || to_layer > from_layer + 1 {
                side_edges.push((ei, lane_idx as f32));
                lane_idx += 1;
            }
        }
    }
    let back_lane_depth = side_edges.len() as f32 * lane_gap
        + if side_edges.is_empty() { 0.0 } else { lane_gap * 2.0 };
    let content_h = total_h * scale;
    let content_w = total_w * scale;
    let total_render_h = content_h
        + match graph.direction {
            Direction::LeftRight | Direction::RightLeft | Direction::TopBottom | Direction::BottomTop => {
                back_lane_depth
            }
            Direction::Diagonal => 0.0,
        };

    // Map original edge index → projected edge index (so we can look up
    // each original edge's back-edge lane).
    let mut orig_to_projected: HashMap<usize, usize> = HashMap::new();
    {
        let mut pj = 0;
        for (oi, e) in graph.edges.iter().enumerate() {
            let tlf = top_level_of(&e.from, &graph);
            let tlt = top_level_of(&e.to, &graph);
            if let (Some(f), Some(t)) = (tlf, tlt) {
                if f != t && top_set.contains(&f) && top_set.contains(&t) {
                    orig_to_projected.insert(oi, pj);
                    pj += 1;
                }
            }
        }
    }

    // 8. Emit subgraph containers (root-first so children paint on top of
    //    nested parents).
    let mut sg_render_order: Vec<usize> = depths.iter().map(|(_, i)| *i).collect();
    sg_render_order.reverse();
    for sg_idx in &sg_render_order {
        let sg = &graph.subgraphs[*sg_idx];
        let style = graph.styles.get(&sg.id).copied().unwrap_or_default();
        // Default background: a subtle tint based on the code bg so the
        // container is visible even without an explicit `style`.
        let bg = style.fill.unwrap_or([fill[0], fill[1], fill[2], 70]);
        let txt = style.text.unwrap_or(stroke);
        let sx = sg.x * scale;
        let sy = sg.y * scale;
        let sw = sg.w * scale;
        let sh = sg.h * scale;
        let header_h = sg.header_h * scale;
        // Background fill.
        items.push(Placed::Rect { x: sx, y: sy, w: sw, h: sh, color: bg });
        // Border.
        let t = 1.5 * scale;
        items.push(Placed::Rect { x: sx, y: sy, w: sw, h: t, color: stroke });
        items.push(Placed::Rect { x: sx, y: sy + sh - t, w: sw, h: t, color: stroke });
        items.push(Placed::Rect { x: sx, y: sy, w: t, h: sh, color: stroke });
        items.push(Placed::Rect { x: sx + sw - t, y: sy, w: t, h: sh, color: stroke });
        // Title in the header band.
        if !sg.label.is_empty() {
            emit_label_lines(
                &mut items,
                &sg.label,
                font,
                sg_label_size * scale,
                sx + sw / 2.0,
                sy + header_h / 2.0,
                txt,
            );
        }
    }

    let bbox_of = |id: &str, g: &Graph| -> Option<(f32, f32, f32, f32)> {
        if let Some(&sg_idx) = g.group_id_to_index.get(id) {
            let sg = &g.subgraphs[sg_idx];
            Some((sg.x, sg.y, sg.w, sg.h))
        } else if let Some(n) = g.nodes.get(id) {
            Some((n.x, n.y, n.w, n.h))
        } else {
            None
        }
    };

    // 9. Edge lines + arrowheads. We pick the anchor direction by where the
    //    edge's endpoints live: cross-container edges use the top-level
    //    direction; edges inside a subgraph use the inner direction.
    for (ei, e) in graph.edges.iter().enumerate() {
        let (Some((ax, ay, aw, ah)), Some((bx, by, bw, bh))) =
            (bbox_of(&e.from, &graph), bbox_of(&e.to, &graph))
        else {
            continue;
        };
        let from_tl = top_level_of(&e.from, &graph);
        let to_tl = top_level_of(&e.to, &graph);
        let cross_container = from_tl != to_tl;
        let edge_dir = if cross_container { graph.direction } else { inner_dir };
        let (is_back, is_skip) = if cross_container {
            let fl = top_layers
                .get(from_tl.as_deref().unwrap_or(""))
                .copied()
                .unwrap_or(0);
            let tl = top_layers
                .get(to_tl.as_deref().unwrap_or(""))
                .copied()
                .unwrap_or(0);
            (fl > tl, tl > fl + 1)
        } else {
            (false, false)
        };
        // Both backwards and layer-skipping edges detour around the side.
        let is_side = is_back || is_skip;
        let head_at_end = matches!(e.kind, EdgeKind::Arrow | EdgeKind::BiArrow);
        let head_at_start = matches!(e.kind, EdgeKind::ReverseArrow | EdgeKind::BiArrow);
        let thick = 1.5 * scale;
        let dotted = e.dotted;

        let line_emit = |items: &mut Vec<Placed>, x1: f32, y1: f32, x2: f32, y2: f32| {
            if dotted {
                emit_dashed_seg(items, x1, y1, x2, y2, thick, stroke);
            } else {
                items.push(Placed::Line { x1, y1, x2, y2, thickness: thick, color: stroke });
            }
        };

        // Diagonal L-paths apply wherever the chosen direction is Diagonal —
        // both cross-container edges (top-level diagonal flow) and edges
        // internal to a subgraph that inherited Diagonal from its parent.
        if matches!(edge_dir, Direction::Diagonal) {
            let head_h_px = 9.0 * scale;
            let head_w_px = 6.0 * scale;
            if !is_back {
                let sx = (ax + aw) * scale;
                let sy = (ay + ah / 2.0) * scale;
                let tx = (bx + bw / 2.0) * scale;
                let ty = by * scale;
                line_emit(&mut items, sx, sy, tx, sy);
                line_emit(&mut items, tx, sy, tx, ty);
                if head_at_end {
                    let tip = (tx, ty);
                    let base = (tx, ty - head_h_px);
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base.0 + head_w_px, base.1),
                        p3: (base.0 - head_w_px, base.1),
                        color: stroke,
                    });
                }
                if head_at_start {
                    // Bidirectional convention: head_at_start points INTO
                    // the source, base is on the line side (toward end).
                    // Forward L exits source rightward, so base sits to the
                    // right of the tip.
                    let tip = (sx, sy);
                    let base = (sx + head_h_px, sy);
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base.0, base.1 + head_w_px),
                        p3: (base.0, base.1 - head_w_px),
                        color: stroke,
                    });
                }
            } else {
                let sx = ax * scale;
                let sy = (ay + ah / 2.0) * scale;
                let tx = (bx + bw / 2.0) * scale;
                let ty = (by + bh) * scale;
                line_emit(&mut items, sx, sy, tx, sy);
                line_emit(&mut items, tx, sy, tx, ty);
                if head_at_end {
                    let tip = (tx, ty);
                    let base = (tx, ty + head_h_px);
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base.0 + head_w_px, base.1),
                        p3: (base.0 - head_w_px, base.1),
                        color: stroke,
                    });
                }
                if head_at_start {
                    // Back L exits source leftward — base sits to the LEFT
                    // of the tip so the arrow still points INTO source.
                    let tip = (sx, sy);
                    let base = (sx - head_h_px, sy);
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base.0, base.1 + head_w_px),
                        p3: (base.0, base.1 - head_w_px),
                        color: stroke,
                    });
                }
            }
            continue;
        }

        if is_side {
            let lane_n = orig_to_projected
                .get(&ei)
                .and_then(|pj| side_edges.iter().find(|(i, _)| *i == *pj))
                .map(|(_, n)| *n)
                .unwrap_or(0.0);
            let lane_off = lane_gap * (lane_n + 1.5);
            let head_h_px = 9.0 * scale;
            let head_w_px = 6.0 * scale;
            match graph.direction {
                Direction::LeftRight | Direction::RightLeft => {
                    // Detour through a lane *below* all columns: exit the
                    // source's bottom, run along the lane, rise into the
                    // target's bottom. Symmetric in x, so it serves both
                    // back- and skip-edges.
                    let sx = (ax + aw / 2.0) * scale;
                    let sy = (ay + ah) * scale;
                    let tx = (bx + bw / 2.0) * scale;
                    let ty = (by + bh) * scale;
                    let lane_y = content_h + lane_off;
                    line_emit(&mut items, sx, sy, sx, lane_y);
                    line_emit(&mut items, sx, lane_y, tx, lane_y);
                    line_emit(&mut items, tx, lane_y, tx, ty);
                    if head_at_end {
                        let tip = (tx, ty);
                        let base = (tx, ty + head_h_px);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0 + head_w_px, base.1),
                            p3: (base.0 - head_w_px, base.1),
                            color: stroke,
                        });
                    }
                    if head_at_start {
                        let tip = (sx, sy);
                        let base = (sx, sy + head_h_px);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0 + head_w_px, base.1),
                            p3: (base.0 - head_w_px, base.1),
                            color: stroke,
                        });
                    }
                }
                Direction::TopBottom | Direction::BottomTop => {
                    // Detour through a lane to the *right* of all rows: exit
                    // the source's right edge, run out to the lane, travel
                    // vertically to the target's row, come back into the
                    // target's right edge. Symmetric in y, so it serves both
                    // back-edges (target above) and skip-edges (target more
                    // than one row below).
                    let sx = (ax + aw) * scale;
                    let sy = (ay + ah / 2.0) * scale;
                    let tx = (bx + bw) * scale;
                    let ty = (by + bh / 2.0) * scale;
                    let lane_x = content_w + lane_off;
                    line_emit(&mut items, sx, sy, lane_x, sy);
                    line_emit(&mut items, lane_x, sy, lane_x, ty);
                    line_emit(&mut items, lane_x, ty, tx, ty);
                    if head_at_end {
                        let tip = (tx, ty);
                        let base = (tx + head_h_px, ty);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0, base.1 + head_w_px),
                            p3: (base.0, base.1 - head_w_px),
                            color: stroke,
                        });
                    }
                    if head_at_start {
                        let tip = (sx, sy);
                        let base = (sx + head_h_px, sy);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0, base.1 + head_w_px),
                            p3: (base.0, base.1 - head_w_px),
                            color: stroke,
                        });
                    }
                }
                Direction::Diagonal => unreachable!(),
            }
            continue;
        }

        // Forward edge — straight line between anchor points.
        let start = anchor_bbox_out(ax, ay, aw, ah, &edge_dir, scale);
        let end = anchor_bbox_in(bx, by, bw, bh, &edge_dir, scale);
        line_emit(&mut items, start.0, start.1, end.0, end.1);
        if head_at_end || head_at_start {
            let head_h_px = 9.0 * scale;
            let head_w_px = 6.0 * scale;
            let dx = end.0 - start.0;
            let dy = end.1 - start.1;
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            let ux = dx / len;
            let uy = dy / len;
            let px = -uy;
            let py = ux;
            if head_at_end {
                let tip = (end.0, end.1);
                let base_cx = end.0 - ux * head_h_px;
                let base_cy = end.1 - uy * head_h_px;
                items.push(Placed::Triangle {
                    p1: tip,
                    p2: (base_cx + px * head_w_px, base_cy + py * head_w_px),
                    p3: (base_cx - px * head_w_px, base_cy - py * head_w_px),
                    color: stroke,
                });
            }
            if head_at_start {
                let tip = (start.0, start.1);
                let base_cx = start.0 + ux * head_h_px;
                let base_cy = start.1 + uy * head_h_px;
                items.push(Placed::Triangle {
                    p1: tip,
                    p2: (base_cx + px * head_w_px, base_cy + py * head_w_px),
                    p3: (base_cx - px * head_w_px, base_cy - py * head_w_px),
                    color: stroke,
                });
            }
        }
    }

    // 10. Node boxes + their labels. Style overrides win over the default
    //     code-block fill / foreground stroke.
    for id in &graph.order {
        if let Some(n) = graph.nodes.get(id) {
            let style = graph.styles.get(id).copied().unwrap_or_default();
            let node_fill = style.fill.unwrap_or(fill);
            let node_text = style.text.unwrap_or(stroke);
            let (nx, ny, nw, nh) = (n.x * scale, n.y * scale, n.w * scale, n.h * scale);
            match n.shape {
                Shape::Rect => {
                    items.push(Placed::Rect { x: nx, y: ny, w: nw, h: nh, color: node_fill });
                    let t = 1.5 * scale;
                    items.push(Placed::Rect { x: nx, y: ny, w: nw, h: t, color: stroke });
                    items.push(Placed::Rect { x: nx, y: ny + nh - t, w: nw, h: t, color: stroke });
                    items.push(Placed::Rect { x: nx, y: ny, w: t, h: nh, color: stroke });
                    items.push(Placed::Rect { x: nx + nw - t, y: ny, w: t, h: nh, color: stroke });
                }
                Shape::Rounded => {
                    let t = 1.5 * scale;
                    let radius = nh.min(nw) * 0.22;
                    items.push(Placed::RoundRect { x: nx, y: ny, w: nw, h: nh, radius, color: stroke });
                    items.push(Placed::RoundRect {
                        x: nx + t,
                        y: ny + t,
                        w: (nw - 2.0 * t).max(0.0),
                        h: (nh - 2.0 * t).max(0.0),
                        radius: (radius - t).max(0.0),
                        color: node_fill,
                    });
                }
                Shape::Circle => {
                    let t = 1.5 * scale;
                    let cx = nx + nw / 2.0;
                    let cy = ny + nh / 2.0;
                    items.push(Placed::Ellipse { cx, cy, rx: nw / 2.0, ry: nh / 2.0, color: stroke });
                    items.push(Placed::Ellipse {
                        cx,
                        cy,
                        rx: (nw / 2.0 - t).max(0.0),
                        ry: (nh / 2.0 - t).max(0.0),
                        color: node_fill,
                    });
                }
                Shape::Diamond => {
                    let cx = nx + nw / 2.0;
                    let cy = ny + nh / 2.0;
                    let top = (cx, ny);
                    let right = (nx + nw, cy);
                    let bottom = (cx, ny + nh);
                    let left = (nx, cy);
                    items.push(Placed::Triangle { p1: top, p2: right, p3: bottom, color: node_fill });
                    items.push(Placed::Triangle { p1: top, p2: bottom, p3: left, color: node_fill });
                    let t = 1.5 * scale;
                    items.push(Placed::Line { x1: top.0, y1: top.1, x2: right.0, y2: right.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: right.0, y1: right.1, x2: bottom.0, y2: bottom.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: bottom.0, y1: bottom.1, x2: left.0, y2: left.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: left.0, y1: left.1, x2: top.0, y2: top.1, thickness: t, color: stroke });
                }
            }
            emit_label_lines(
                &mut items,
                &n.label,
                font,
                label_size * scale,
                nx + nw / 2.0,
                ny + nh / 2.0,
                node_text,
            );
        }
    }

    // 11. Edge labels — drawn last so they sit on top of lines and any
    //     stray crossings.
    for (ei, e) in graph.edges.iter().enumerate() {
        let Some(lab) = &e.label else { continue };
        let (Some((ax, ay, aw, ah)), Some((bx, by, bw, bh))) =
            (bbox_of(&e.from, &graph), bbox_of(&e.to, &graph))
        else {
            continue;
        };
        let from_tl = top_level_of(&e.from, &graph);
        let to_tl = top_level_of(&e.to, &graph);
        let cross_container = from_tl != to_tl;
        let edge_dir = if cross_container { graph.direction } else { inner_dir };
        let (is_back, is_skip) = if cross_container {
            let fl = top_layers
                .get(from_tl.as_deref().unwrap_or(""))
                .copied()
                .unwrap_or(0);
            let tl = top_layers
                .get(to_tl.as_deref().unwrap_or(""))
                .copied()
                .unwrap_or(0);
            (fl > tl, tl > fl + 1)
        } else {
            (false, false)
        };
        let is_side = is_back || is_skip;
        let lsize = label_size * 0.75 * scale;
        let (lw, _lh) = measure_label(lab, lsize, font);
        let pad = 4.0 * scale;

        let (mid_x, mid_y) = if cross_container && matches!(graph.direction, Direction::Diagonal) {
            let sy_mid = (ay + ah / 2.0) * scale;
            let (sx, tx_mid) = if !is_back {
                ((ax + aw) * scale, (bx + bw / 2.0) * scale)
            } else {
                (ax * scale, (bx + bw / 2.0) * scale)
            };
            let bend_clear = 10.0 * scale;
            let half = lw / 2.0 + pad;
            let cx = if !is_back {
                let want = tx_mid - half - bend_clear;
                want.max(sx + half + bend_clear)
            } else {
                let want = tx_mid + half + bend_clear;
                want.min(sx - half - bend_clear)
            };
            (cx, sy_mid)
        } else if is_side {
            let lane_n = orig_to_projected
                .get(&ei)
                .and_then(|pj| side_edges.iter().find(|(i, _)| *i == *pj))
                .map(|(_, n)| *n)
                .unwrap_or(0.0);
            let lane_off = lane_gap * (lane_n + 1.5);
            match graph.direction {
                Direction::LeftRight | Direction::RightLeft => {
                    let sx = (ax + aw / 2.0) * scale;
                    let tx = (bx + bw / 2.0) * scale;
                    ((sx + tx) / 2.0, content_h + lane_off)
                }
                Direction::TopBottom | Direction::BottomTop => {
                    let sy = (ay + ah / 2.0) * scale;
                    let ty = (by + bh / 2.0) * scale;
                    (content_w + lane_off, (sy + ty) / 2.0)
                }
                Direction::Diagonal => unreachable!(),
            }
        } else {
            let start = anchor_bbox_out(ax, ay, aw, ah, &edge_dir, scale);
            let end = anchor_bbox_in(bx, by, bw, bh, &edge_dir, scale);
            ((start.0 + end.0) / 2.0, (start.1 + end.1) / 2.0)
        };
        emit_label_lines(&mut items, lab, font, lsize, mid_x, mid_y, theme.muted);
    }

    let render_w = match graph.direction {
        Direction::LeftRight | Direction::RightLeft => content_w,
        Direction::TopBottom | Direction::BottomTop => content_w + back_lane_depth,
        Direction::Diagonal => content_w,
    };
    MermaidRender { items, width: render_w, height: total_render_h }
}


/// Emit glyph items for a possibly multi-line label, centered on
/// `(cx, cy)`. Each `\n`-separated line is horizontally centered
/// independently; lines stack with a fixed 1.25-em line height.
fn emit_label_lines(
    items: &mut Vec<Placed>,
    text: &str,
    font: &Font,
    size: f32,
    cx: f32,
    cy: f32,
    color: Rgba,
) {
    let line_h = size * 1.25;
    let lines: Vec<&str> = text.split('\n').collect();
    let total_h = lines.len() as f32 * line_h;
    let first_baseline = cy - total_h / 2.0 + line_h * 0.75;
    for (li, line) in lines.iter().enumerate() {
        let line_w: f32 = line.chars().map(|c| font.metrics(c, size).advance_width).sum();
        let mut lx = cx - line_w / 2.0;
        let ly = first_baseline + li as f32 * line_h;
        for ch in line.chars() {
            let m = font.metrics(ch, size);
            items.push(Placed::Glyph {
                ch,
                font: FontId::Body,
                size,
                x: lx,
                baseline: ly,
                color,
                selectable: true,
            });
            lx += m.advance_width;
        }
    }
}


// ───── sequence diagrams ───────────────────────────────────────────────────
//
// Minimal subset of mermaid sequence diagrams. Supports:
//   sequenceDiagram
//   participant ID
//   participant ID as Display Label
//   X->>Y: msg            — solid arrow
//   X-->>Y: msg           — dashed arrow (typical "response")
//   X->Y: msg / X-->Y: msg — also accepted
//   X->>X: msg            — self-call rendered as a small loop on X's lifeline
//
// Out of scope: alt/loop/opt/par blocks, Note over/left of/right of,
// activate/deactivate, autonumber. Falls back to a code block on parse
// failure (handled by the caller).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeqLineKind {
    Solid,
    Dashed,
}

#[derive(Debug)]
struct SeqMsg {
    from: usize,
    to: usize,
    kind: SeqLineKind,
    label: String,
}

#[derive(Debug)]
struct SeqParticipant {
    /// Display label — what's drawn in the header. Defaults to the id.
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeqBlockKind {
    Loop,
    Alt,
    Opt,
    Par,
    Rect,
    Critical,
    Break,
}

impl SeqBlockKind {
    fn keyword(self) -> &'static str {
        match self {
            SeqBlockKind::Loop => "loop",
            SeqBlockKind::Alt => "alt",
            SeqBlockKind::Opt => "opt",
            SeqBlockKind::Par => "par",
            SeqBlockKind::Rect => "rect",
            SeqBlockKind::Critical => "critical",
            SeqBlockKind::Break => "break",
        }
    }
}

#[derive(Debug)]
enum SeqEvent {
    Msg(SeqMsg),
    /// Note over a range of lanes [from..=to]. Multi-line via `\n`.
    Note { from: usize, to: usize, text: String },
    BlockStart { kind: SeqBlockKind, label: String },
    /// Mid-block divider (`else <label>` inside an `alt`).
    Else { label: String },
    BlockEnd,
}

struct Sequence {
    participants: Vec<SeqParticipant>,
    events: Vec<SeqEvent>,
}

fn parse_sequence(src: &str) -> Option<Sequence> {
    let mut it = src.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("%%"));
    let header = it.next()?;
    if !header.eq_ignore_ascii_case("sequenceDiagram") {
        return None;
    }
    let mut ids: Vec<String> = Vec::new();
    let mut participants: Vec<SeqParticipant> = Vec::new();
    let mut events: Vec<SeqEvent> = Vec::new();

    fn ensure(ids: &mut Vec<String>, participants: &mut Vec<SeqParticipant>, id: &str) -> usize {
        if let Some(i) = ids.iter().position(|x| x == id) {
            return i;
        }
        ids.push(id.to_string());
        participants.push(SeqParticipant { label: id.to_string() });
        ids.len() - 1
    }

    fn unbreak(s: &str) -> String {
        // Mermaid uses `<br/>` (and sometimes `<br>`) inside note text and
        // labels for line breaks. Normalise to `\n`.
        s.replace("<br/>", "\n").replace("<br>", "\n")
    }

    fn parse_block_keyword(line: &str) -> Option<(SeqBlockKind, String)> {
        for (kw, kind) in [
            ("loop ", SeqBlockKind::Loop),
            ("alt ", SeqBlockKind::Alt),
            ("opt ", SeqBlockKind::Opt),
            ("par ", SeqBlockKind::Par),
            ("rect ", SeqBlockKind::Rect),
            ("critical ", SeqBlockKind::Critical),
            ("break ", SeqBlockKind::Break),
        ] {
            if let Some(rest) = line.strip_prefix(kw) {
                return Some((kind, rest.trim().to_string()));
            }
        }
        // Bare keywords with no label.
        for (kw, kind) in [
            ("loop", SeqBlockKind::Loop),
            ("alt", SeqBlockKind::Alt),
            ("opt", SeqBlockKind::Opt),
            ("par", SeqBlockKind::Par),
            ("rect", SeqBlockKind::Rect),
            ("critical", SeqBlockKind::Critical),
            ("break", SeqBlockKind::Break),
        ] {
            if line == kw {
                return Some((kind, String::new()));
            }
        }
        None
    }

    for line in it {
        if let Some(rest) = line.strip_prefix("participant ").or_else(|| line.strip_prefix("actor ")) {
            let rest = rest.trim();
            // "ID" or "ID as Label" — split on " as " (literal).
            let (id, label) = if let Some(idx) = rest.find(" as ") {
                (rest[..idx].trim(), rest[idx + 4..].trim())
            } else {
                (rest, rest)
            };
            if let Some(i) = ids.iter().position(|x| x == id) {
                participants[i].label = label.to_string();
            } else {
                ids.push(id.to_string());
                participants.push(SeqParticipant { label: label.to_string() });
            }
            continue;
        }
        // autonumber / activate / deactivate are silently accepted but
        // not visualised — the diagram still draws everything else.
        if line == "autonumber"
            || line.starts_with("autonumber ")
            || line.starts_with("activate ")
            || line.starts_with("deactivate ")
        {
            continue;
        }
        if line == "end" {
            events.push(SeqEvent::BlockEnd);
            continue;
        }
        if let Some(rest) = line.strip_prefix("else ") {
            events.push(SeqEvent::Else { label: rest.trim().to_string() });
            continue;
        }
        if line == "else" {
            events.push(SeqEvent::Else { label: String::new() });
            continue;
        }
        if let Some((kind, label)) = parse_block_keyword(line) {
            events.push(SeqEvent::BlockStart { kind, label });
            continue;
        }
        if let Some(rest) = line.strip_prefix("Note over ").or_else(|| line.strip_prefix("note over ")) {
            // "X[,Y]: text"
            let (targets, text) = rest.split_once(':').unwrap_or((rest, ""));
            let targets = targets.trim();
            let text = unbreak(text.trim());
            let (from_id, to_id) = match targets.split_once(',') {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (targets, targets),
            };
            let from = ensure(&mut ids, &mut participants, from_id);
            let to = ensure(&mut ids, &mut participants, to_id);
            let (lo, hi) = (from.min(to), from.max(to));
            events.push(SeqEvent::Note { from: lo, to: hi, text });
            continue;
        }
        // "Note left of X: text" / "Note right of X: text" — render same
        // as "Note over X: text"; simpler than tracking placement and
        // good enough at this resolution.
        for prefix in ["Note left of ", "Note right of ", "note left of ", "note right of "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let (target, text) = rest.split_once(':').unwrap_or((rest, ""));
                let target = target.trim();
                let text = unbreak(text.trim());
                let i = ensure(&mut ids, &mut participants, target);
                events.push(SeqEvent::Note { from: i, to: i, text });
                break;
            }
        }
        if let Some((arrow_start, arrow_end, kind)) = find_seq_arrow(line) {
            let from_str = line[..arrow_start].trim();
            let after = &line[arrow_end..];
            let (to_str, label) = match after.split_once(':') {
                Some((t, l)) => (t.trim(), unbreak(l.trim())),
                None => (after.trim(), String::new()),
            };
            if from_str.is_empty() || to_str.is_empty() {
                continue;
            }
            let from = ensure(&mut ids, &mut participants, from_str);
            let to = ensure(&mut ids, &mut participants, to_str);
            events.push(SeqEvent::Msg(SeqMsg { from, to, kind, label }));
        }
    }
    if participants.is_empty() {
        return None;
    }
    Some(Sequence { participants, events })
}

/// Match the longest arrow form first so `-->>` doesn't get parsed as `-->`.
fn find_seq_arrow(line: &str) -> Option<(usize, usize, SeqLineKind)> {
    for (pat, kind) in [
        ("-->>", SeqLineKind::Dashed),
        ("->>", SeqLineKind::Solid),
        ("-->", SeqLineKind::Dashed),
        ("->", SeqLineKind::Solid),
    ] {
        if let Some(start) = line.find(pat) {
            return Some((start, start + pat.len(), kind));
        }
    }
    None
}

fn layout_sequence(seq: Sequence, max_width: f32, theme: &Theme, fonts: &Fonts) -> MermaidRender {
    let n = seq.participants.len();
    let head_size = theme.body_size;
    let msg_size = theme.body_size * 0.92;
    let block_label_size = theme.body_size * 0.78;
    let note_size = msg_size;
    let head_font = pick_font(fonts, FontId::Body);
    let msg_font = pick_font(fonts, FontId::Body);
    let block_font = pick_font(fonts, FontId::Italic);

    let pad_x = 14.0;
    let head_pad_y = 8.0;
    let head_h = head_size + head_pad_y * 2.0;
    let row_h = msg_size * 2.0;
    let self_row_h = msg_size * 3.0;
    let block_header_h = block_label_size * 1.6 + 6.0;
    let block_inner_pad = 6.0;
    let note_pad_x = 10.0;
    let note_pad_y = 6.0;
    let top_margin = 4.0;
    let bottom_margin = 4.0;

    // ── width pass ────────────────────────────────────────────────────────
    let header_w: Vec<f32> = seq
        .participants
        .iter()
        .map(|p| measure_label(&p.label, head_size, head_font).0 + pad_x * 2.0)
        .collect();
    let min_gap_pad: f32 = 24.0;
    let mut gap: Vec<f32> = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..n.saturating_sub(1) {
        gap.push((header_w[i] + header_w[i + 1]) / 2.0 + min_gap_pad);
    }
    let bump_span = |gap: &mut Vec<f32>, a: usize, b: usize, need: f32| {
        if a >= b {
            return;
        }
        let cur: f32 = gap[a..b].iter().sum();
        if need > cur {
            let extra = need - cur;
            let per = extra / (b - a) as f32;
            for g in &mut gap[a..b] {
                *g += per;
            }
        }
    };
    let loop_w = 36.0_f32;
    let mut right_extra = 0.0_f32;
    for ev in &seq.events {
        match ev {
            SeqEvent::Msg(m) if m.from != m.to => {
                let (a, b) = (m.from.min(m.to), m.from.max(m.to));
                let need = measure_label(&m.label, msg_size, msg_font).0 + 24.0;
                bump_span(&mut gap, a, b, need);
            }
            SeqEvent::Msg(m) => {
                let label_w = if m.label.is_empty() {
                    0.0
                } else {
                    measure_label(&m.label, msg_size, msg_font).0
                };
                let extent = loop_w + 8.0 + label_w + 8.0;
                if m.from + 1 == n {
                    right_extra = right_extra.max(extent);
                } else if let Some(g) = gap.get_mut(m.from) {
                    let need = header_w[m.from] / 2.0 + extent;
                    if need > *g {
                        *g = need;
                    }
                }
            }
            SeqEvent::Note { from, to, text } => {
                let text_w = measure_label(text, note_size, msg_font).0;
                let need = text_w + note_pad_x * 2.0;
                if from == to {
                    // Single-lane note centres over the lane and overflows
                    // into the gaps on either side. Bump each side by
                    // half-overflow.
                    let half_overflow = ((need - header_w[*from]) * 0.5).max(0.0);
                    if *from > 0 {
                        let g = &mut gap[*from - 1];
                        let want = header_w[*from] / 2.0 + half_overflow + 6.0;
                        if want > *g {
                            *g = want;
                        }
                    }
                    if *from + 1 < n {
                        let g = &mut gap[*from];
                        let want = header_w[*from] / 2.0 + half_overflow + 6.0;
                        if want > *g {
                            *g = want;
                        }
                    }
                    if *from + 1 == n && *from == 0 {
                        // Lone-participant diagrams: nothing to bump; let
                        // the right margin take the slack.
                        right_extra = right_extra.max(half_overflow);
                    }
                } else {
                    bump_span(&mut gap, *from, *to, need);
                }
            }
            _ => {}
        }
    }
    let left_margin = header_w.first().copied().unwrap_or(0.0) / 2.0 + 8.0;
    let right_margin = header_w.last().copied().unwrap_or(0.0) / 2.0 + 8.0 + right_extra;
    let mut center_x: Vec<f32> = Vec::with_capacity(n);
    center_x.push(left_margin);
    for i in 1..n {
        center_x.push(center_x[i - 1] + gap[i - 1]);
    }
    let mut total_w = center_x.last().copied().unwrap_or(0.0) + right_margin;
    if total_w > max_width && total_w > 0.0 {
        let scale = max_width / total_w;
        for c in center_x.iter_mut() {
            *c *= scale;
        }
        total_w = max_width;
    }

    // ── height pass: walk events sequentially, tracking nested blocks ────
    let mut items: Vec<Placed> = Vec::new();
    let stroke = theme.muted;
    let body_fg = theme.fg;
    let head_bg = theme.bg;
    let head_border = stroke;
    let block_tag_bg: Rgba = [stroke[0], stroke[1], stroke[2], 220];
    // Notes need to occlude the dashed lifelines that would otherwise
    // show through their fill — alpha ≈ 90 reads as a clear tinted
    // panel without going so dark that the text contrast suffers.
    let note_bg: Rgba = [theme.accent[0], theme.accent[1], theme.accent[2], 90];
    let note_border = stroke;

    /// Pending block on the layout stack. `top_y` is where the frame
    /// begins (before the tag); `kind`/`label` drive the tag rendering.
    struct PendingBlock {
        kind: SeqBlockKind,
        label: String,
        top_y: f32,
    }
    let mut block_stack: Vec<PendingBlock> = Vec::new();

    let life_top = top_margin + head_h;
    let mut y = life_top + 8.0;

    // We need to know the lifeline bottom *before* drawing the lifelines,
    // but the bottom depends on how tall the messages/blocks/notes turn
    // out to be. So: walk events into deferred draw closures, advancing
    // `y` as we go, then emit lifelines at the end.
    enum Deferred {
        Msg { msg: SeqMsg, y: f32, is_self: bool },
        Note { from: usize, to: usize, text: String, y: f32, h: f32 },
        Block { kind: SeqBlockKind, label: String, top_y: f32, bottom_y: f32, depth: usize, dividers: Vec<(f32, String)> },
    }
    let mut deferred: Vec<Deferred> = Vec::new();
    // Track the most recently opened block index (in `deferred`) per
    // stack depth so an `else` event can append a divider to it.
    let mut block_idx_stack: Vec<usize> = Vec::new();

    for ev in seq.events {
        match ev {
            SeqEvent::Msg(m) => {
                let is_self = m.from == m.to;
                let h = if is_self { self_row_h } else { row_h };
                deferred.push(Deferred::Msg { msg: m, y, is_self });
                y += h;
            }
            SeqEvent::Note { from, to, text } => {
                let lines = text.split('\n').count().max(1) as f32;
                let h = lines * note_size * 1.25 + note_pad_y * 2.0;
                deferred.push(Deferred::Note { from, to, text, y, h });
                y += h + 6.0;
            }
            SeqEvent::BlockStart { kind, label } => {
                let top_y = y;
                let depth = block_stack.len();
                block_stack.push(PendingBlock { kind, label: label.clone(), top_y });
                // Emit a placeholder; we'll fill in bottom_y at BlockEnd.
                let idx = deferred.len();
                deferred.push(Deferred::Block {
                    kind,
                    label,
                    top_y,
                    bottom_y: top_y,
                    depth,
                    dividers: Vec::new(),
                });
                block_idx_stack.push(idx);
                y += block_header_h;
            }
            SeqEvent::Else { label } => {
                // Add the divider y to the innermost open block. Render-
                // order ensures it sits between the rows above and below.
                if let Some(&idx) = block_idx_stack.last() {
                    if let Deferred::Block { dividers, .. } = &mut deferred[idx] {
                        dividers.push((y, label));
                    }
                }
                y += block_header_h * 0.7;
            }
            SeqEvent::BlockEnd => {
                if let (Some(_), Some(idx)) = (block_stack.pop(), block_idx_stack.pop()) {
                    y += block_inner_pad;
                    if let Deferred::Block { bottom_y, .. } = &mut deferred[idx] {
                        *bottom_y = y;
                    }
                    y += 4.0;
                }
            }
        }
    }
    // Unclosed blocks — terminate them at the current y so the frame
    // still draws (sources missing `end` shouldn't lose the frame).
    while let (Some(_), Some(idx)) = (block_stack.pop(), block_idx_stack.pop()) {
        y += block_inner_pad;
        if let Deferred::Block { bottom_y, .. } = &mut deferred[idx] {
            *bottom_y = y;
        }
        y += 4.0;
    }

    let footer_y = y + 8.0;
    let life_bottom = footer_y;
    let total_h = footer_y + head_h + bottom_margin;

    // Lifelines first so messages/notes/blocks paint over them.
    for &cx in &center_x {
        emit_dashed_v(&mut items, cx, life_top, life_bottom, stroke);
    }

    // Block frames go before notes/messages so their borders sit behind.
    for d in &deferred {
        if let Deferred::Block { kind, label, top_y, bottom_y, depth, dividers } = d {
            // Inset nested blocks so each level's frame sits visibly
            // inside its parent rather than stacking edge-to-edge.
            let inset = (*depth as f32) * 6.0;
            let bx = inset;
            let by = *top_y;
            let bw = (total_w - inset * 2.0).max(1.0);
            let bh = bottom_y - top_y;
            // Top, bottom, left, right strokes.
            items.push(Placed::Rect { x: bx, y: by, w: bw, h: 1.0, color: stroke });
            items.push(Placed::Rect { x: bx, y: by + bh - 1.0, w: bw, h: 1.0, color: stroke });
            items.push(Placed::Rect { x: bx, y: by, w: 1.0, h: bh, color: stroke });
            items.push(Placed::Rect { x: bx + bw - 1.0, y: by, w: 1.0, h: bh, color: stroke });

            // Tag in top-left: filled darker rect with keyword + label.
            let kw = kind.keyword();
            let tag_text = if label.is_empty() {
                kw.to_string()
            } else {
                format!("{kw}  {label}")
            };
            let tw = measure_label(&tag_text, block_label_size, block_font).0 + 14.0;
            let th = block_label_size + 6.0;
            items.push(Placed::Rect {
                x: bx + 1.0,
                y: by + 1.0,
                w: tw,
                h: th,
                color: block_tag_bg,
            });
            // Wrap the keyword in brackets visually by drawing the label
            // centred inside the tag, with the body bg colour so it pops
            // against the dark tag fill.
            emit_label_lines(
                &mut items,
                &tag_text,
                block_font,
                block_label_size,
                bx + 1.0 + tw / 2.0,
                by + 1.0 + th / 2.0,
                head_bg,
            );

            for (dy, dlabel) in dividers {
                // Dashed horizontal divider across the frame.
                emit_dashed_h(&mut items, bx + 1.0, bx + bw - 1.0, *dy, stroke);
                if !dlabel.is_empty() {
                    let dtag = format!("else  {dlabel}");
                    let tw2 = measure_label(&dtag, block_label_size, block_font).0 + 14.0;
                    let th2 = block_label_size + 6.0;
                    items.push(Placed::Rect {
                        x: bx + 1.0,
                        y: dy + 2.0,
                        w: tw2,
                        h: th2,
                        color: block_tag_bg,
                    });
                    emit_label_lines(
                        &mut items,
                        &dtag,
                        block_font,
                        block_label_size,
                        bx + 1.0 + tw2 / 2.0,
                        dy + 2.0 + th2 / 2.0,
                        head_bg,
                    );
                }
            }
        }
    }

    // Notes and messages on top.
    for d in deferred {
        match d {
            Deferred::Msg { msg, y, is_self } => {
                if is_self {
                    let cx = center_x[msg.from];
                    let y0 = y + msg_size * 0.6;
                    let y1 = y + self_row_h - msg_size * 0.8;
                    items.push(Placed::Rect { x: cx, y: y0, w: loop_w, h: 1.0, color: stroke });
                    items.push(Placed::Rect { x: cx + loop_w, y: y0, w: 1.0, h: y1 - y0, color: stroke });
                    if msg.kind == SeqLineKind::Dashed {
                        emit_dashed_h(&mut items, cx, cx + loop_w, y1, stroke);
                    } else {
                        items.push(Placed::Rect { x: cx, y: y1, w: loop_w, h: 1.0, color: stroke });
                    }
                    emit_arrow_head(&mut items, cx, y1, false, stroke);
                    if !msg.label.is_empty() {
                        emit_label_lines(
                            &mut items,
                            &msg.label,
                            msg_font,
                            msg_size,
                            cx + loop_w + 6.0 + measure_label(&msg.label, msg_size, msg_font).0 / 2.0,
                            (y0 + y1) / 2.0,
                            body_fg,
                        );
                    }
                } else {
                    let x_from = center_x[msg.from];
                    let x_to = center_x[msg.to];
                    let arrow_y = y + row_h * 0.62;
                    let going_right = x_to > x_from;
                    let (lo, hi) = if going_right { (x_from, x_to) } else { (x_to, x_from) };
                    if msg.kind == SeqLineKind::Dashed {
                        emit_dashed_h(&mut items, lo, hi, arrow_y, stroke);
                    } else {
                        items.push(Placed::Rect { x: lo, y: arrow_y, w: hi - lo, h: 1.0, color: stroke });
                    }
                    emit_arrow_head(&mut items, x_to, arrow_y, going_right, stroke);
                    if !msg.label.is_empty() {
                        let cx = (lo + hi) / 2.0;
                        let cy = arrow_y - msg_size * 0.85;
                        emit_label_lines(&mut items, &msg.label, msg_font, msg_size, cx, cy, body_fg);
                    }
                }
            }
            Deferred::Note { from, to, text, y, h } => {
                let text_w = measure_label(&text, note_size, msg_font).0;
                let lo_cx = center_x[from];
                let hi_cx = center_x[to];
                let span_centre = (lo_cx + hi_cx) / 2.0;
                let need_w = text_w + note_pad_x * 2.0;
                let nat_span = (hi_cx - lo_cx) + 60.0;
                let note_w = need_w.max(nat_span);
                let note_x = span_centre - note_w / 2.0;
                items.push(Placed::Rect { x: note_x, y, w: note_w, h, color: note_bg });
                // Border (1px outline)
                items.push(Placed::Rect { x: note_x, y, w: note_w, h: 1.0, color: note_border });
                items.push(Placed::Rect { x: note_x, y: y + h - 1.0, w: note_w, h: 1.0, color: note_border });
                items.push(Placed::Rect { x: note_x, y, w: 1.0, h, color: note_border });
                items.push(Placed::Rect { x: note_x + note_w - 1.0, y, w: 1.0, h, color: note_border });
                emit_label_lines(
                    &mut items,
                    &text,
                    msg_font,
                    note_size,
                    span_centre,
                    y + h / 2.0,
                    body_fg,
                );
            }
            Deferred::Block { .. } => {}
        }
    }

    // Participant headers (top + bottom) — drawn last so they sit on
    // top of any frame borders that pass through their rects.
    for (i, p) in seq.participants.iter().enumerate() {
        let cx = center_x[i];
        let label_w = measure_label(&p.label, head_size, head_font).0;
        let used_w = (label_w + pad_x * 2.0).max(40.0);
        let x = cx - used_w / 2.0;
        for &y_top in &[top_margin, footer_y] {
            items.push(Placed::RoundRect {
                x,
                y: y_top,
                w: used_w,
                h: head_h,
                radius: 6.0,
                color: head_border,
            });
            items.push(Placed::RoundRect {
                x: x + 1.0,
                y: y_top + 1.0,
                w: used_w - 2.0,
                h: head_h - 2.0,
                radius: 5.0,
                color: head_bg,
            });
            emit_label_lines(
                &mut items,
                &p.label,
                head_font,
                head_size,
                cx,
                y_top + head_h / 2.0,
                body_fg,
            );
        }
    }

    MermaidRender {
        items,
        width: total_w,
        height: total_h,
    }
}

/// Draw a dashed vertical line as a column of short rect segments.
fn emit_dashed_v(items: &mut Vec<Placed>, x: f32, y0: f32, y1: f32, color: Rgba) {
    let dash: f32 = 4.0;
    let gap: f32 = 4.0;
    let mut y = y0;
    while y < y1 {
        let h = dash.min(y1 - y);
        items.push(Placed::Rect { x: x - 0.5, y, w: 1.0, h, color });
        y += dash + gap;
    }
}

fn emit_dashed_h(items: &mut Vec<Placed>, x0: f32, x1: f32, y: f32, color: Rgba) {
    let dash: f32 = 5.0;
    let gap: f32 = 4.0;
    let mut x = x0;
    while x < x1 {
        let w = dash.min(x1 - x);
        items.push(Placed::Rect { x, y: y - 0.5, w, h: 1.0, color });
        x += dash + gap;
    }
}

/// Filled triangle pointing into `(x, y)`. `right` chooses the direction
/// the arrow flies (true → arrow pointing rightwards, head sits at x).
fn emit_arrow_head(items: &mut Vec<Placed>, x: f32, y: f32, right: bool, color: Rgba) {
    let h = 6.0;
    let w = 8.0;
    let (tail_x, tip_x) = if right { (x - w, x) } else { (x + w, x) };
    items.push(Placed::Triangle {
        p1: (tip_x, y),
        p2: (tail_x, y - h * 0.5),
        p3: (tail_x, y + h * 0.5),
        color,
    });
}
