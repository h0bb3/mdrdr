//! Minimal Mermaid flowchart. Supported:
//!   graph TD|TB|LR|RL
//!   node shapes: A, A[label], A(label), A((label))
//!   edges:      A --> B, A --- B, A -->|label| B, chained: A --> B --> C
//!
//! Layout is longest-path layering with fixed row/col sizes. Good enough for
//! the trees and pipelines that actually show up in notes.

use std::collections::HashMap;

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
}

struct Graph {
    direction: Direction,
    nodes: HashMap<String, Node>,
    order: Vec<String>,
    edges: Vec<Edge>,
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
/// override the right-click context menu offers.
pub fn render_with(
    src: &str,
    max_width: f32,
    theme: &Theme,
    fonts: &Fonts,
    override_dir: Option<Direction>,
) -> Option<MermaidRender> {
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
        parse_line(line, &mut nodes, &mut order, &mut edges);
    }
    if !saw_header {
        return None;
    }
    Some(Graph { direction, nodes, order, edges })
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

fn parse_line(
    line: &str,
    nodes: &mut HashMap<String, Node>,
    order: &mut Vec<String>,
    edges: &mut Vec<Edge>,
) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut prev: Option<String> = None;
    let mut pending_arrow: Option<(EdgeKind, Option<String>)> = None;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Try to parse an arrow.
        if let Some((kind, after)) = read_arrow(bytes, i) {
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
            pending_arrow = Some((kind, label));
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
            } else if let Some(l) = label {
                // Upgrade label / shape if re-declared with content.
                if let Some(n) = nodes.get_mut(&id) {
                    n.label = l;
                    n.shape = shape_kind;
                }
            }
            if let (Some(p), Some((kind, lab))) = (prev.as_ref(), pending_arrow.take()) {
                edges.push(Edge { from: p.clone(), to: id.clone(), label: lab, kind });
            }
            prev = Some(id);
            i = end;
            continue;
        }

        // Nothing matched — skip one byte to avoid infinite loop.
        i += 1;
    }
}

fn read_arrow(b: &[u8], i: usize) -> Option<(EdgeKind, usize)> {
    // Longest first so `<-->` isn't shortened to `-->`.
    if b.len() >= i + 4 && &b[i..i + 4] == b"<-->" {
        return Some((EdgeKind::BiArrow, i + 4));
    }
    if b.len() >= i + 3 && &b[i..i + 3] == b"<--" {
        return Some((EdgeKind::ReverseArrow, i + 3));
    }
    if b.len() >= i + 3 && &b[i..i + 3] == b"-->" {
        return Some((EdgeKind::Arrow, i + 3));
    }
    if b.len() >= i + 3 && &b[i..i + 3] == b"---" {
        return Some((EdgeKind::Line, i + 3));
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
                let label = normalise_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Rect, Some(label)), (k + 1).min(b.len())));
            }
            b'{' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b'}' { k += 1; }
                let label = normalise_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Diamond, Some(label)), (k + 1).min(b.len())));
            }
            b'(' if j + 1 < b.len() && b[j + 1] == b'(' => {
                let lab_start = j + 2;
                let mut k = lab_start;
                while k + 1 < b.len() && !(b[k] == b')' && b[k + 1] == b')') { k += 1; }
                let label = normalise_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Circle, Some(label)), (k + 2).min(b.len())));
            }
            b'(' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b')' { k += 1; }
                let label = normalise_label(std::str::from_utf8(&b[lab_start..k]).ok()?);
                return Some((id, (Shape::Rounded, Some(label)), (k + 1).min(b.len())));
            }
            _ => {}
        }
    }
    Some((id, (Shape::Rect, None), j))
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

fn layout(mut graph: Graph, max_width: f32, theme: &Theme, fonts: &Fonts) -> MermaidRender {
    let pad_x = 14.0;
    let pad_y = 10.0;
    let min_w = 80.0;
    let label_size = theme.body_size * 0.9;
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

    // 2. Layers via longest-path from sources.
    let layers = assign_layers(&graph);

    // 3. Group nodes by layer in insertion order.
    let mut per_layer: Vec<Vec<String>> = Vec::new();
    for id in &graph.order {
        let l = *layers.get(id).unwrap_or(&0);
        while per_layer.len() <= l {
            per_layer.push(Vec::new());
        }
        per_layer[l].push(id.clone());
    }

    // 4. Place.
    let h_gap = 40.0;
    let v_gap = 50.0;

    let (mut total_w, mut total_h) = (0.0f32, 0.0f32);
    match graph.direction {
        Direction::TopBottom | Direction::BottomTop => {
            // Each layer is a row. Compute each row's total width and row height.
            let row_heights: Vec<f32> = per_layer
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|id| graph.nodes.get(id).map(|n| n.h).unwrap_or(0.0))
                        .fold(0.0f32, f32::max)
                })
                .collect();
            let row_widths: Vec<f32> = per_layer
                .iter()
                .map(|row| {
                    let ws: f32 = row
                        .iter()
                        .map(|id| graph.nodes.get(id).map(|n| n.w).unwrap_or(0.0))
                        .sum();
                    let gaps = h_gap * (row.len().saturating_sub(1)) as f32;
                    ws + gaps
                })
                .collect();
            let max_w = row_widths.iter().fold(0.0f32, |a, &b| a.max(b));
            total_w = max_w.max(1.0);

            // Inter-row gaps: widen to fit the tallest label (multi-line
            // labels can exceed the default v_gap) plus breathing room so
            // the chip doesn't butt against the arrowhead or node.
            let gap_n = per_layer.len().saturating_sub(1);
            let mut gap_h: Vec<f32> = vec![v_gap; gap_n];
            let lbl_size = label_size * 0.75;
            let chip_pad = 4.0;
            let breathing = 28.0;
            for e in &graph.edges {
                let Some(lab) = e.label.as_deref() else { continue };
                let (_lw, lh) = measure_label(lab, lbl_size, font);
                let from_layer = *layers.get(&e.from).unwrap_or(&0);
                let to_layer = *layers.get(&e.to).unwrap_or(&0);
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
                    if let Some(n) = graph.nodes.get_mut(id) {
                        n.x = x;
                        n.y = y + (row_h - n.h) / 2.0;
                        x += n.w + h_gap;
                    }
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
                .map(|col| col.iter().map(|id| graph.nodes.get(id).map(|n| n.w).unwrap_or(0.0)).fold(0.0f32, f32::max))
                .collect();
            let col_heights: Vec<f32> = per_layer
                .iter()
                .map(|col| {
                    let hs: f32 = col.iter().map(|id| graph.nodes.get(id).map(|n| n.h).unwrap_or(0.0)).sum();
                    let gaps = v_gap * (col.len().saturating_sub(1)) as f32;
                    hs + gaps
                })
                .collect();
            let max_h = col_heights.iter().fold(0.0f32, |a, &b| a.max(b));
            total_h = max_h.max(1.0);

            // Each inter-column gap gets widened to fit the max label width
            // of any labelled edge that starts in that column, so the arrow
            // line is long enough to host the label without clipping the
            // next node. Unlabelled edges keep the default gap.
            let gap_n = per_layer.len().saturating_sub(1);
            let mut gap_w: Vec<f32> = vec![h_gap; gap_n];
            let lbl_size = label_size * 0.75;
            let chip_pad = 4.0;
            // Arrow has to span: node edge → chip start → chip → chip end →
            // arrowhead + node. We want generous breathing room on both sides
            // of the chip so the arrowhead and source node don't crowd the
            // label.
            let breathing = 28.0;
            for e in &graph.edges {
                let Some(lab) = e.label.as_deref() else { continue };
                let (lw, _lh) = measure_label(lab, lbl_size, font);
                let from_layer = *layers.get(&e.from).unwrap_or(&0);
                let to_layer = *layers.get(&e.to).unwrap_or(&0);
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
                    if let Some(n) = graph.nodes.get_mut(id) {
                        n.x = x + (col_w - n.w) / 2.0;
                        n.y = y;
                        y += n.h + v_gap;
                    }
                }
                x += col_w;
                if i + 1 < per_layer.len() {
                    x += gap_w[i];
                }
            }
            total_w = x;
        }
        Direction::Diagonal => {
            // N nodes in declaration order sit on the main diagonal of an
            // N×N grid. Each cell is a uniform max-of-all-nodes box so
            // forward (upper-right) and back (lower-left) edges route
            // cleanly through the triangles without crossing.
            let cell_w = graph.nodes.values().map(|n| n.w).fold(0.0f32, f32::max);
            let cell_h = graph.nodes.values().map(|n| n.h).fold(0.0f32, f32::max);
            let n = graph.order.len().max(1);
            let step_x = cell_w + h_gap;
            let step_y = cell_h + v_gap;
            for (i, id) in graph.order.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(id) {
                    node.x = i as f32 * step_x + (cell_w - node.w) / 2.0;
                    node.y = i as f32 * step_y + (cell_h - node.h) / 2.0;
                }
            }
            total_w = n as f32 * step_x - h_gap;
            total_h = n as f32 * step_y - v_gap;
        }
    }

    // BT / RL are just TB / LR with the long axis mirrored. Mirroring the
    // *node positions* here (not the emitted primitives later) keeps labels
    // readable — each node's internal text still stacks top-to-bottom and
    // fits inside its rect.
    if graph.direction.is_flipped() {
        if graph.direction.is_vertical() {
            for (_, n) in graph.nodes.iter_mut() {
                n.y = total_h - n.y - n.h;
            }
        } else {
            for (_, n) in graph.nodes.iter_mut() {
                n.x = total_w - n.x - n.w;
            }
        }
    }

    // Fit the diagram to the available width.
    //   - If it's too wide, scale down so it exactly fits.
    //   - If it's too narrow, scale up proportionally (capped at 1.6x) so
    //     the graph actually uses the space. A small graph in a wide window
    //     looked cramped otherwise.
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

    // 5. Emit primitives.
    let stroke: Rgba = theme.fg;
    let fill: Rgba = theme.code_bg;
    let mut items: Vec<Placed> = Vec::new();

    // Back edges (source layer > target layer) route around the node row
    // through a lane on the far side, so they don't cut through intermediate
    // nodes. Multiple back edges stack in lanes 1, 2, 3… so they don't
    // overlap each other.
    let lane_gap = 18.0 * scale;
    let mut back_edges: Vec<(usize, f32)> = Vec::new(); // (edge_index, lane_y_or_x)
    {
        let mut lane_idx = 0usize;
        for (ei, e) in graph.edges.iter().enumerate() {
            let from_layer = *layers.get(&e.from).unwrap_or(&0);
            let to_layer = *layers.get(&e.to).unwrap_or(&0);
            if from_layer > to_layer {
                back_edges.push((ei, lane_idx as f32));
                lane_idx += 1;
            }
        }
    }
    let back_lane_depth = back_edges.len() as f32 * lane_gap
        + if back_edges.is_empty() { 0.0 } else { lane_gap * 2.0 };
    let content_h = total_h * scale;
    let content_w = total_w * scale;
    let total_render_h = content_h + match graph.direction {
        Direction::LeftRight | Direction::RightLeft | Direction::TopBottom | Direction::BottomTop => back_lane_depth,
        // Diagonal routes back edges through the lower-left triangle
        // itself, so no extra lane height is needed.
        Direction::Diagonal => 0.0,
    };

    // Pass 1: edge lines + arrowheads. Forward edges draw straight;
    // back edges take a polyline around the node row.
    for (ei, e) in graph.edges.iter().enumerate() {
        let (Some(a), Some(b)) = (graph.nodes.get(&e.from), graph.nodes.get(&e.to)) else { continue };
        let from_layer = *layers.get(&e.from).unwrap_or(&0);
        let to_layer = *layers.get(&e.to).unwrap_or(&0);
        let is_back = from_layer > to_layer;
        let head_at_end = matches!(e.kind, EdgeKind::Arrow | EdgeKind::BiArrow);
        let head_at_start = matches!(e.kind, EdgeKind::ReverseArrow | EdgeKind::BiArrow);
        let thick = 1.5 * scale;

        // Diagonal: L-shaped edges through the appropriate triangle. Forward
        // edges exit the source's right, run rightward to the target's
        // column, then drop down into the target's top. Back edges exit
        // left, run to the target's column, then rise into the target's
        // bottom. Zero crossings, arrows always land on a node face.
        if matches!(graph.direction, Direction::Diagonal) {
            let ah = 9.0 * scale;
            let aw = 6.0 * scale;
            if !is_back {
                let sx = (a.x + a.w) * scale;
                let sy = (a.y + a.h / 2.0) * scale;
                let tx = (b.x + b.w / 2.0) * scale;
                let ty = b.y * scale;
                items.push(Placed::Line { x1: sx, y1: sy, x2: tx, y2: sy, thickness: thick, color: stroke });
                items.push(Placed::Line { x1: tx, y1: sy, x2: tx, y2: ty, thickness: thick, color: stroke });
                if head_at_end {
                    let tip = (tx, ty);
                    let base = (tx, ty - ah);
                    items.push(Placed::Triangle {
                        p1: tip, p2: (base.0 + aw, base.1), p3: (base.0 - aw, base.1),
                        color: stroke,
                    });
                }
                if head_at_start {
                    let tip = (sx, sy);
                    let base = (sx - ah, sy);
                    items.push(Placed::Triangle {
                        p1: tip, p2: (base.0, base.1 + aw), p3: (base.0, base.1 - aw),
                        color: stroke,
                    });
                }
            } else {
                let sx = a.x * scale;
                let sy = (a.y + a.h / 2.0) * scale;
                let tx = (b.x + b.w / 2.0) * scale;
                let ty = (b.y + b.h) * scale;
                items.push(Placed::Line { x1: sx, y1: sy, x2: tx, y2: sy, thickness: thick, color: stroke });
                items.push(Placed::Line { x1: tx, y1: sy, x2: tx, y2: ty, thickness: thick, color: stroke });
                if head_at_end {
                    let tip = (tx, ty);
                    let base = (tx, ty + ah);
                    items.push(Placed::Triangle {
                        p1: tip, p2: (base.0 + aw, base.1), p3: (base.0 - aw, base.1),
                        color: stroke,
                    });
                }
                if head_at_start {
                    let tip = (sx, sy);
                    let base = (sx + ah, sy);
                    items.push(Placed::Triangle {
                        p1: tip, p2: (base.0, base.1 + aw), p3: (base.0, base.1 - aw),
                        color: stroke,
                    });
                }
            }
            continue;
        }

        if is_back {
            let lane_n = back_edges
                .iter()
                .find(|(i, _)| *i == ei)
                .map(|(_, n)| *n)
                .unwrap_or(0.0);
            let lane_off = lane_gap * (lane_n + 1.5);
            match graph.direction {
                Direction::LeftRight | Direction::RightLeft => {
                    // Exit bottom of source, run left along a lane below all
                    // nodes, come up into bottom of target.
                    let sx = (a.x + a.w / 2.0) * scale;
                    let sy = (a.y + a.h) * scale;
                    let tx = (b.x + b.w / 2.0) * scale;
                    let ty = (b.y + b.h) * scale;
                    let lane_y = content_h + lane_off;
                    items.push(Placed::Line { x1: sx, y1: sy, x2: sx, y2: lane_y, thickness: thick, color: stroke });
                    items.push(Placed::Line { x1: sx, y1: lane_y, x2: tx, y2: lane_y, thickness: thick, color: stroke });
                    items.push(Placed::Line { x1: tx, y1: lane_y, x2: tx, y2: ty, thickness: thick, color: stroke });
                    if head_at_end {
                        // arrowhead pointing up into target's bottom edge
                        let ah = 9.0 * scale;
                        let aw = 6.0 * scale;
                        let tip = (tx, ty);
                        let base = (tx, ty + ah);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0 + aw, base.1),
                            p3: (base.0 - aw, base.1),
                            color: stroke,
                        });
                    }
                    if head_at_start {
                        let ah = 9.0 * scale;
                        let aw = 6.0 * scale;
                        let tip = (sx, sy);
                        let base = (sx, sy + ah);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0 + aw, base.1),
                            p3: (base.0 - aw, base.1),
                            color: stroke,
                        });
                    }
                }
                Direction::Diagonal => unreachable!("diagonal back-edges handled above"),
                Direction::TopBottom | Direction::BottomTop => {
                    // Mirror for TD: route around the right side.
                    let sx = (a.x + a.w) * scale;
                    let sy = (a.y + a.h / 2.0) * scale;
                    let tx = (b.x + b.w) * scale;
                    let ty = (b.y + b.h / 2.0) * scale;
                    let lane_x = content_w + lane_off;
                    items.push(Placed::Line { x1: sx, y1: sy, x2: lane_x, y2: sy, thickness: thick, color: stroke });
                    items.push(Placed::Line { x1: lane_x, y1: sy, x2: lane_x, y2: ty, thickness: thick, color: stroke });
                    items.push(Placed::Line { x1: lane_x, y1: ty, x2: tx, y2: ty, thickness: thick, color: stroke });
                    if head_at_end {
                        let ah = 9.0 * scale;
                        let aw = 6.0 * scale;
                        let tip = (tx, ty);
                        let base = (tx + ah, ty);
                        items.push(Placed::Triangle {
                            p1: tip,
                            p2: (base.0, base.1 + aw),
                            p3: (base.0, base.1 - aw),
                            color: stroke,
                        });
                    }
                }
            }
        } else {
            let start = anchor_out(a, &graph.direction, scale);
            let end = anchor_in(b, &graph.direction, scale);
            items.push(Placed::Line {
                x1: start.0, y1: start.1,
                x2: end.0, y2: end.1,
                thickness: thick,
                color: stroke,
            });
            if head_at_end || head_at_start {
                let ah = 9.0 * scale;
                let aw = 6.0 * scale;
                let dx = end.0 - start.0;
                let dy = end.1 - start.1;
                let len = (dx * dx + dy * dy).sqrt().max(0.001);
                let ux = dx / len;
                let uy = dy / len;
                let px = -uy;
                let py = ux;
                if head_at_end {
                    let tip = (end.0, end.1);
                    let base_cx = end.0 - ux * ah;
                    let base_cy = end.1 - uy * ah;
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base_cx + px * aw, base_cy + py * aw),
                        p3: (base_cx - px * aw, base_cy - py * aw),
                        color: stroke,
                    });
                }
                if head_at_start {
                    let tip = (start.0, start.1);
                    let base_cx = start.0 + ux * ah;
                    let base_cy = start.1 + uy * ah;
                    items.push(Placed::Triangle {
                        p1: tip,
                        p2: (base_cx + px * aw, base_cy + py * aw),
                        p3: (base_cx - px * aw, base_cy - py * aw),
                        color: stroke,
                    });
                }
            }
        }
    }

    // Pass 2: node boxes + their labels. Drawn after edge lines so any
    // back-edge polyline stub that touches a node is covered cleanly.
    for id in &graph.order {
        if let Some(n) = graph.nodes.get(id) {
            let (nx, ny, nw, nh) = (n.x * scale, n.y * scale, n.w * scale, n.h * scale);
            match n.shape {
                Shape::Rect => {
                    items.push(Placed::Rect { x: nx, y: ny, w: nw, h: nh, color: fill });
                    let t = 1.5 * scale;
                    items.push(Placed::Rect { x: nx, y: ny, w: nw, h: t, color: stroke });
                    items.push(Placed::Rect { x: nx, y: ny + nh - t, w: nw, h: t, color: stroke });
                    items.push(Placed::Rect { x: nx, y: ny, w: t, h: nh, color: stroke });
                    items.push(Placed::Rect { x: nx + nw - t, y: ny, w: t, h: nh, color: stroke });
                }
                Shape::Rounded => {
                    let t = 1.5 * scale;
                    let radius = nh.min(nw) * 0.22;
                    items.push(Placed::RoundRect {
                        x: nx, y: ny, w: nw, h: nh, radius, color: stroke,
                    });
                    items.push(Placed::RoundRect {
                        x: nx + t, y: ny + t,
                        w: (nw - 2.0 * t).max(0.0), h: (nh - 2.0 * t).max(0.0),
                        radius: (radius - t).max(0.0),
                        color: fill,
                    });
                }
                Shape::Circle => {
                    let t = 1.5 * scale;
                    let cx = nx + nw / 2.0;
                    let cy = ny + nh / 2.0;
                    items.push(Placed::Ellipse {
                        cx, cy, rx: nw / 2.0, ry: nh / 2.0, color: stroke,
                    });
                    items.push(Placed::Ellipse {
                        cx, cy,
                        rx: (nw / 2.0 - t).max(0.0),
                        ry: (nh / 2.0 - t).max(0.0),
                        color: fill,
                    });
                }
                Shape::Diamond => {
                    let cx = nx + nw / 2.0;
                    let cy = ny + nh / 2.0;
                    let top = (cx, ny);
                    let right = (nx + nw, cy);
                    let bottom = (cx, ny + nh);
                    let left = (nx, cy);
                    items.push(Placed::Triangle { p1: top, p2: right, p3: bottom, color: fill });
                    items.push(Placed::Triangle { p1: top, p2: bottom, p3: left, color: fill });
                    let t = 1.5 * scale;
                    items.push(Placed::Line { x1: top.0, y1: top.1, x2: right.0, y2: right.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: right.0, y1: right.1, x2: bottom.0, y2: bottom.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: bottom.0, y1: bottom.1, x2: left.0, y2: left.1, thickness: t, color: stroke });
                    items.push(Placed::Line { x1: left.0, y1: left.1, x2: top.0, y2: top.1, thickness: t, color: stroke });
                }
            }
            emit_label_lines(
                &mut items, &n.label, font,
                label_size * scale,
                nx + nw / 2.0, ny + nh / 2.0,
                stroke,
            );
        }
    }

    // Pass 3: edge labels — drawn last so they always sit on top of both
    // edge lines and any stray crossings. Forward labels go on the line
    // midpoint; back-edge labels sit on the lane.
    for (ei, e) in graph.edges.iter().enumerate() {
        let Some(lab) = &e.label else { continue };
        let (Some(a), Some(b)) = (graph.nodes.get(&e.from), graph.nodes.get(&e.to)) else { continue };
        let from_layer = *layers.get(&e.from).unwrap_or(&0);
        let to_layer = *layers.get(&e.to).unwrap_or(&0);
        let is_back = from_layer > to_layer;
        let lsize = label_size * 0.75 * scale;
        let (lw, _lh) = measure_label(lab, lsize, font);
        let pad = 4.0 * scale;

        let (mid_x, mid_y) = if matches!(graph.direction, Direction::Diagonal) {
            // Diagonal labels sit on the horizontal segment of the L, hugging
            // the 90° bend. The bend is where edges cluster together (each L
            // turns at its target's column), so anchoring the chip there
            // keeps multiple edges' labels visually tied to their own corner
            // instead of piling up at segment midpoints.
            let sy_mid = (a.y + a.h / 2.0) * scale;
            let (sx, tx_mid) = if !is_back {
                ((a.x + a.w) * scale, (b.x + b.w / 2.0) * scale)
            } else {
                (a.x * scale, (b.x + b.w / 2.0) * scale)
            };
            let bend_clear = 10.0 * scale;
            let half = lw / 2.0 + pad;
            let cx = if !is_back {
                // horizontal runs sx → tx_mid (left-to-right); bend at tx_mid.
                let want = tx_mid - half - bend_clear;
                want.max(sx + half + bend_clear)
            } else {
                // horizontal runs sx → tx_mid (right-to-left); bend at tx_mid.
                let want = tx_mid + half + bend_clear;
                want.min(sx - half - bend_clear)
            };
            (cx, sy_mid)
        } else if is_back {
            let lane_n = back_edges
                .iter()
                .find(|(i, _)| *i == ei)
                .map(|(_, n)| *n)
                .unwrap_or(0.0);
            let lane_off = lane_gap * (lane_n + 1.5);
            match graph.direction {
                Direction::LeftRight | Direction::RightLeft => {
                    let sx = (a.x + a.w / 2.0) * scale;
                    let tx = (b.x + b.w / 2.0) * scale;
                    ((sx + tx) / 2.0, content_h + lane_off)
                }
                Direction::TopBottom | Direction::BottomTop => {
                    let sy = (a.y + a.h / 2.0) * scale;
                    let ty = (b.y + b.h / 2.0) * scale;
                    (content_w + lane_off, (sy + ty) / 2.0)
                }
                Direction::Diagonal => unreachable!(),
            }
        } else {
            let start = anchor_out(a, &graph.direction, scale);
            let end = anchor_in(b, &graph.direction, scale);
            ((start.0 + end.0) / 2.0, (start.1 + end.1) / 2.0)
        };
        emit_label_lines(&mut items, lab, font, lsize, mid_x, mid_y, theme.muted);
    }

    let render_w = match graph.direction {
        Direction::LeftRight | Direction::RightLeft => content_w,
        Direction::TopBottom | Direction::BottomTop => content_w + back_lane_depth,
        Direction::Diagonal => content_w,
    };
    let render_h = total_render_h;
    MermaidRender { items, width: render_w, height: render_h }
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

fn assign_layers(graph: &Graph) -> HashMap<String, usize> {
    let mut idx_of: HashMap<&str, usize> = HashMap::new();
    for (i, id) in graph.order.iter().enumerate() {
        idx_of.insert(id.as_str(), i);
    }
    let mut preds: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &graph.order {
        preds.insert(id.as_str(), Vec::new());
    }
    // Only forward edges (by declaration order) contribute to layering. Back
    // edges still render, but are ignored here so cycles don't push layers
    // to runaway values.
    for e in &graph.edges {
        if graph.nodes.contains_key(&e.from) && graph.nodes.contains_key(&e.to) {
            let fi = idx_of.get(e.from.as_str()).copied().unwrap_or(0);
            let ti = idx_of.get(e.to.as_str()).copied().unwrap_or(0);
            if fi < ti {
                preds.entry(e.to.as_str()).or_default().push(e.from.as_str());
            }
        }
    }
    let mut layer: HashMap<String, usize> = HashMap::new();
    let mut changed = true;
    let mut guard = 0;
    while changed && guard < graph.order.len() * 2 + 5 {
        changed = false;
        for id in &graph.order {
            let ps = preds.get(id.as_str()).cloned().unwrap_or_default();
            let l = ps
                .iter()
                .map(|p| *layer.get(*p).unwrap_or(&0) + 1)
                .max()
                .unwrap_or(0);
            let cur = *layer.get(id).unwrap_or(&0);
            if l != cur && (cur == 0 || l > cur) {
                layer.insert(id.clone(), l);
                changed = true;
            }
        }
        guard += 1;
    }
    for id in &graph.order {
        layer.entry(id.clone()).or_insert(0);
    }
    layer
}

fn anchor_out(n: &Node, dir: &Direction, scale: f32) -> (f32, f32) {
    // Outgoing side = the edge that faces the *next* layer in visual flow.
    // For flipped directions (BT / RL), the flow reverses so the outgoing
    // side flips with it. Diagonal is unused by this helper (edges route
    // via their own L-path) but included so the match stays exhaustive.
    match dir {
        Direction::TopBottom => ((n.x + n.w / 2.0) * scale, (n.y + n.h) * scale),
        Direction::BottomTop => ((n.x + n.w / 2.0) * scale, n.y * scale),
        Direction::LeftRight => ((n.x + n.w) * scale, (n.y + n.h / 2.0) * scale),
        Direction::RightLeft => (n.x * scale, (n.y + n.h / 2.0) * scale),
        Direction::Diagonal => ((n.x + n.w / 2.0) * scale, (n.y + n.h / 2.0) * scale),
    }
}

fn anchor_in(n: &Node, dir: &Direction, scale: f32) -> (f32, f32) {
    match dir {
        Direction::TopBottom => ((n.x + n.w / 2.0) * scale, n.y * scale),
        Direction::BottomTop => ((n.x + n.w / 2.0) * scale, (n.y + n.h) * scale),
        Direction::LeftRight => (n.x * scale, (n.y + n.h / 2.0) * scale),
        Direction::RightLeft => ((n.x + n.w) * scale, (n.y + n.h / 2.0) * scale),
        Direction::Diagonal => ((n.x + n.w / 2.0) * scale, (n.y + n.h / 2.0) * scale),
    }
}
