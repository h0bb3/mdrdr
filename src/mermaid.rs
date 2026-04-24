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

#[derive(Copy, Clone, Debug)]
enum Direction {
    TopDown,
    LeftRight,
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
    Arrow,
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
    let graph = parse(src)?;
    Some(layout(graph, max_width, theme, fonts))
}

// ───── parse ────────────────────────────────────────────────────────────────

fn parse(src: &str) -> Option<Graph> {
    let mut direction = Direction::TopDown;
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
                "td" | "tb" | "" => Direction::TopDown,
                "lr" | "rl" => Direction::LeftRight,
                _ => Direction::TopDown,
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
                label = Some(line[start..j].to_string());
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
    // --> or -->
    if b.len() >= i + 3 && &b[i..i + 3] == b"-->" {
        return Some((EdgeKind::Arrow, i + 3));
    }
    if b.len() >= i + 3 && &b[i..i + 3] == b"---" {
        return Some((EdgeKind::Line, i + 3));
    }
    // -- ... --> (longer link form) — for M7 we only support --- and -->.
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
                let label = std::str::from_utf8(&b[lab_start..k]).ok()?.to_string();
                return Some((id, (Shape::Rect, Some(label)), (k + 1).min(b.len())));
            }
            b'{' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b'}' { k += 1; }
                let label = std::str::from_utf8(&b[lab_start..k]).ok()?.to_string();
                return Some((id, (Shape::Diamond, Some(label)), (k + 1).min(b.len())));
            }
            b'(' if j + 1 < b.len() && b[j + 1] == b'(' => {
                let lab_start = j + 2;
                let mut k = lab_start;
                while k + 1 < b.len() && !(b[k] == b')' && b[k + 1] == b')') { k += 1; }
                let label = std::str::from_utf8(&b[lab_start..k]).ok()?.to_string();
                return Some((id, (Shape::Circle, Some(label)), (k + 2).min(b.len())));
            }
            b'(' => {
                let lab_start = j + 1;
                let mut k = lab_start;
                while k < b.len() && b[k] != b')' { k += 1; }
                let label = std::str::from_utf8(&b[lab_start..k]).ok()?.to_string();
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

// ───── layout ───────────────────────────────────────────────────────────────

fn measure_label(text: &str, size: f32, font: &Font) -> (f32, f32) {
    let mut w = 0.0;
    for ch in text.chars() {
        w += font.metrics(ch, size).advance_width;
    }
    let h = size * 1.25;
    (w, h)
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
        Direction::TopDown => {
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
                    y += v_gap;
                }
            }
            total_h = y;
        }
        Direction::LeftRight => {
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
            let lbl_size = label_size * 0.85;
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

    // Rects / circles for nodes.
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
                    // Fill + smaller fill inset by the stroke thickness =
                    // a bordered rounded rect without needing a dedicated
                    // stroke primitive.
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
                    // True ellipse (circle when w == h). Outer = stroke,
                    // inner = fill inset by the stroke thickness.
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
            // Label centered.
            let (lw, _lh) = measure_label(&n.label, label_size * scale, font);
            let mut lx = nx + (nw - lw) / 2.0;
            let ly = ny + nh / 2.0 + label_size * scale * 0.35;
            for ch in n.label.chars() {
                let m = font.metrics(ch, label_size * scale);
                items.push(Placed::Glyph {
                    ch,
                    font: FontId::Body,
                    size: label_size * scale,
                    x: lx,
                    baseline: ly,
                    color: stroke,
                    selectable: true,
                });
                lx += m.advance_width;
            }
        }
    }

    // Edges.
    for e in &graph.edges {
        let (Some(a), Some(b)) = (graph.nodes.get(&e.from), graph.nodes.get(&e.to)) else { continue };
        let start = anchor_out(a, &graph.direction, scale);
        let end = anchor_in(b, &graph.direction, scale);
        items.push(Placed::Line {
            x1: start.0, y1: start.1,
            x2: end.0, y2: end.1,
            thickness: 1.5 * scale,
            color: stroke,
        });
        if e.kind == EdgeKind::Arrow {
            let ah = 9.0 * scale;
            let aw = 6.0 * scale;
            let dx = end.0 - start.0;
            let dy = end.1 - start.1;
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            let ux = dx / len;
            let uy = dy / len;
            let px = -uy;
            let py = ux;
            let tip = (end.0, end.1);
            let base_cx = end.0 - ux * ah;
            let base_cy = end.1 - uy * ah;
            let p2 = (base_cx + px * aw, base_cy + py * aw);
            let p3 = (base_cx - px * aw, base_cy - py * aw);
            items.push(Placed::Triangle { p1: tip, p2, p3, color: stroke });
        }

        // Edge label midway.
        if let Some(lab) = &e.label {
            let mid_x = (start.0 + end.0) / 2.0;
            let mid_y = (start.1 + end.1) / 2.0;
            let (lw, lh) = measure_label(lab, label_size * 0.85 * scale, font);
            // background chip
            let pad = 4.0 * scale;
            let bx = mid_x - lw / 2.0 - pad;
            let by = mid_y - lh / 2.0 - pad;
            items.push(Placed::Rect {
                x: bx, y: by, w: lw + pad * 2.0, h: lh + pad * 2.0,
                color: theme.bg,
            });
            let mut lx = mid_x - lw / 2.0;
            let ly = mid_y + label_size * 0.85 * scale * 0.3;
            for ch in lab.chars() {
                let m = font.metrics(ch, label_size * 0.85 * scale);
                items.push(Placed::Glyph {
                    ch,
                    font: FontId::Body,
                    size: label_size * 0.85 * scale,
                    x: lx,
                    baseline: ly,
                    color: theme.muted,
                    selectable: true,
                });
                lx += m.advance_width;
            }
        }
    }

    let render_w = total_w * scale;
    let render_h = total_h * scale;
    MermaidRender { items, width: render_w, height: render_h }
}

fn assign_layers(graph: &Graph) -> HashMap<String, usize> {
    let mut preds: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &graph.order {
        preds.insert(id.as_str(), Vec::new());
    }
    for e in &graph.edges {
        if graph.nodes.contains_key(&e.from) && graph.nodes.contains_key(&e.to) {
            preds.entry(e.to.as_str()).or_default().push(e.from.as_str());
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
    match dir {
        Direction::TopDown => ((n.x + n.w / 2.0) * scale, (n.y + n.h) * scale),
        Direction::LeftRight => ((n.x + n.w) * scale, (n.y + n.h / 2.0) * scale),
    }
}

fn anchor_in(n: &Node, dir: &Direction, scale: f32) -> (f32, f32) {
    match dir {
        Direction::TopDown => ((n.x + n.w / 2.0) * scale, n.y * scale),
        Direction::LeftRight => (n.x * scale, (n.y + n.h / 2.0) * scale),
    }
}
