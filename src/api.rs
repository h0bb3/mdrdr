//! Tiny HTTP/1.1 server — just enough to drive the window from `curl`.
//!
//! Endpoints:
//!   GET  /state
//!   GET  /screenshot
//!   GET  /tree
//!   POST /scroll?y=N | ?dy=N
//!   POST /resize?w=N&h=N
//!   POST /open?path=URL_ENCODED
//!   POST /click?x=X&y=Y
//!   POST /quit

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use winit::event_loop::EventLoopProxy;

use crate::render::{measure, render, RenderInput, Viewport};
use crate::tree::{TreeEntry, TreeKind};
use crate::window::{click_at, Shared, SynthKey, UserEvent};

/// Spawn the HTTP control API on `127.0.0.1:port`. Port `0` picks an
/// ephemeral one. Returns the actually-bound port, or an `io::Error` if
/// the port is taken — callers typically log-and-continue rather than
/// crash the window.
pub fn spawn(
    shared: Arc<Shared>,
    proxy: EventLoopProxy<UserEvent>,
    port: u16,
) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr().unwrap().port();

    std::thread::Builder::new()
        .name("mdrdr-api".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, shared, proxy);
                });
            }
        })
        .expect("spawn api thread");

    Ok(port)
}

fn handle(
    mut stream: TcpStream,
    shared: Arc<Shared>,
    proxy: EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    let req = std::str::from_utf8(&buf[..n]).unwrap_or("");

    let first_line = req.lines().next().unwrap_or("");
    let mut parts = first_line.split(' ');
    let method = parts.next().unwrap_or("");
    let path_query = parts.next().unwrap_or("");

    let (path, qs) = match path_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_query, ""),
    };
    let q = parse_query(qs);

    match (method, path) {
        ("GET", "/state") => ok_json(&mut stream, &state_json(&shared)),
        ("GET", "/screenshot") => screenshot(&mut stream, &shared, &q),
        ("GET", "/hits") => do_hits(&mut stream, &shared),
        ("POST", "/zoom") => do_zoom(&mut stream, &shared, &proxy, &q),
        ("POST", "/theme") => do_theme(&mut stream, &shared, &proxy, &q),
        ("GET", "/tree") => ok_json(&mut stream, &tree_json(&shared)),
        ("POST", "/scroll") => do_scroll(&mut stream, &shared, &proxy, &q),
        ("POST", "/resize") => do_resize(&mut stream, &shared, &proxy, &q),
        ("POST", "/open") => do_open(&mut stream, &shared, &proxy, &q),
        ("POST", "/click") => do_click(&mut stream, &shared, &proxy, &q),
        ("POST", "/sidebar") => do_sidebar(&mut stream, &shared, &proxy, &q),
        ("POST", "/select") => do_select(&mut stream, &shared, &proxy, &q),
        ("POST", "/select/clear") => do_select_clear(&mut stream, &shared, &proxy),
        ("POST", "/copy") | ("GET", "/selection") => do_copy(&mut stream, &shared),
        ("POST", "/quit") => do_quit(&mut stream, &proxy),
        // Test-driver hooks. Synthesize keystrokes through the same
        // dispatch path that real keyboard events use, so the API can
        // exercise the search / quick-open input keyboard model end-to-end.
        ("POST", "/key") => do_key(&mut stream, &proxy, &q),
        ("POST", "/type") => do_type(&mut stream, &proxy, &q),
        ("POST", "/search/open") => do_search_open(&mut stream, &shared, &proxy),
        ("POST", "/search/close") => do_search_close(&mut stream, &shared, &proxy),
        ("GET", "/search") => ok_json(&mut stream, &search_json(&shared)),
        ("POST", "/quickopen/open") => do_quickopen_open(&mut stream, &shared, &proxy),
        ("POST", "/quickopen/close") => do_quickopen_close(&mut stream, &shared, &proxy),
        ("GET", "/quickopen") => ok_json(&mut stream, &quickopen_json(&shared)),
        ("GET", "/") => ok_text(&mut stream, INDEX),
        _ => not_found(&mut stream),
    }
}

// ───── endpoint handlers ─────────────────────────────────────────────────────

fn state_json(shared: &Arc<Shared>) -> String {
    let s = shared.state.lock().unwrap();
    let path = s.source_path.as_ref().map(|p| p.display().to_string());
    let path_json = match path {
        Some(p) => format!("\"{}\"", json_escape(&p)),
        None => "null".into(),
    };
    let root_json = s
        .tree
        .as_ref()
        .map(|t| format!("\"{}\"", json_escape(&t.root.display().to_string())))
        .unwrap_or_else(|| "null".to_string());
    format!(
        "{{\"source_path\":{path_json},\"scroll\":{:.3},\"viewport\":{{\"w\":{},\"h\":{}}},\"source_len\":{},\"tree_root\":{root_json},\"sidebar_width\":{:.1},\"sidebar_scroll\":{:.3},\"content_zoom\":{:.3},\"sidebar_zoom\":{:.3},\"dark\":{}}}",
        s.scroll,
        s.viewport.width,
        s.viewport.height,
        s.source.len(),
        s.sidebar_width,
        s.sidebar_scroll,
        s.content_zoom,
        s.sidebar_zoom,
        s.dark,
    )
}

fn tree_json(shared: &Arc<Shared>) -> String {
    let s = shared.state.lock().unwrap();
    let Some(tree) = s.tree.as_ref() else {
        return "null".into();
    };
    let flat = tree.flatten();
    let active = s.source_path.clone();
    let mut out = String::from("[");
    for (i, e) in flat.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&entry_json(e, active.as_deref()));
    }
    out.push(']');
    out
}

fn entry_json(e: &TreeEntry, active: Option<&std::path::Path>) -> String {
    let kind = match e.kind {
        TreeKind::Folder => "folder",
        TreeKind::Markdown => "file",
        TreeKind::Parent => "parent",
    };
    let is_active = active.map(|a| a == e.path.as_path()).unwrap_or(false);
    format!(
        "{{\"path\":\"{}\",\"depth\":{},\"kind\":\"{}\",\"expanded\":{},\"active\":{}}}",
        json_escape(&e.path.display().to_string()),
        e.depth,
        kind,
        e.expanded,
        is_active
    )
}

fn screenshot(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let snap = shared.snapshot();
    let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let hover_pos = match (
        q.get("hx").and_then(|v| v.parse::<f32>().ok()),
        q.get("hy").and_then(|v| v.parse::<f32>().ok()),
    ) {
        (Some(hx), Some(hy)) => Some((hx, hy)),
        _ => None,
    };
    let mut images = shared.images.lock().unwrap();
    let mut fb = render(
        &RenderInput {
            source: &snap.source,
            viewport: snap.viewport,
            scroll: snap.scroll,
            theme: &snap.theme,
            fonts: &shared.fonts,
            tree: snap.tree_flat.as_deref(),
            active_path: snap.source_path.as_deref(),
            base_dir: base_dir.as_deref(),
            sidebar_width: snap.sidebar_width,
            sidebar_scroll: snap.sidebar_scroll,
            content_zoom: snap.content_zoom,
            sidebar_zoom: snap.sidebar_zoom,
            selection: snap.selection,
            hover_pos,
            search: snap.search.as_ref().filter(|s| !s.query.is_empty()).map(|s| crate::render::SearchHighlights {
                query: &s.query,
                current: if s.match_count > 0 { Some(s.current) } else { None },
            }),
            mermaid_overrides: Some(&snap.mermaid_overrides),
        },
        &mut images,
    );
    drop(images);

    // Draw overlay panels last so the test API screenshots match what the
    // user actually sees in the live window.
    if let Some(su) = &snap.search {
        crate::window::draw_search_ui(&mut fb, &snap.theme, &shared.fonts, su, hover_pos);
    }
    if let Some(qo) = &snap.quick_open {
        crate::window::draw_quick_open_ui(&mut fb, &snap.theme, &shared.fonts, qo, snap.viewport, hover_pos);
    }
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(fb.width, fb.height, fb.pixels).expect("fb size mismatch");
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        png.len()
    )?;
    stream.write_all(&png)?;
    Ok(())
}

fn do_scroll(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let max_scroll = compute_max_scroll(shared);
    {
        let mut s = shared.state.lock().unwrap();
        if let Some(y) = q.get("y").and_then(|v| v.parse::<f32>().ok()) {
            s.scroll = y.clamp(0.0, max_scroll);
        } else if let Some(dy) = q.get("dy").and_then(|v| v.parse::<f32>().ok()) {
            s.scroll = (s.scroll + dy).clamp(0.0, max_scroll);
        }
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn compute_max_scroll(shared: &Arc<Shared>) -> f32 {
    let snap = shared.snapshot();
    let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let mut images = shared.images.lock().unwrap();
    let doc_h = measure(
        &snap.source,
        snap.viewport.width,
        snap.viewport.height,
        base_dir.as_deref(),
        snap.sidebar_width,
        snap.content_zoom,
        &snap.theme,
        &shared.fonts,
        &mut images,
    );
    (doc_h - snap.viewport.height as f32).max(0.0)
}

fn do_resize(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let w = q.get("w").and_then(|v| v.parse::<u32>().ok());
    let h = q.get("h").and_then(|v| v.parse::<u32>().ok());
    if let (Some(w), Some(h)) = (w, h) {
        let mut s = shared.state.lock().unwrap();
        s.viewport = Viewport { width: w.max(1), height: h.max(1) };
    }
    // Re-clamp in case the resize shrank the doc / scroll became past-bottom.
    let max_scroll = compute_max_scroll(shared);
    {
        let mut s = shared.state.lock().unwrap();
        if s.scroll > max_scroll {
            s.scroll = max_scroll;
        }
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn do_open(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let Some(path) = q.get("path") else {
        return bad_request(stream, "missing ?path=");
    };
    let path = url_decode(path);
    let source = std::fs::read_to_string(&path).unwrap_or_default();
    {
        let mut s = shared.state.lock().unwrap();
        s.source = source;
        s.source_path = Some(PathBuf::from(path));
        s.scroll = 0.0;
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn do_click(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let Some(x) = q.get("x").and_then(|v| v.parse::<f32>().ok()) else {
        return bad_request(stream, "missing ?x=");
    };
    let Some(y) = q.get("y").and_then(|v| v.parse::<f32>().ok()) else {
        return bad_request(stream, "missing ?y=");
    };
    let action = click_at(shared, x, y);
    let _ = proxy.send_event(UserEvent::Redraw);
    let body = match action {
        Some(a) => format!("{{\"hit\":true,\"action\":{}}}", action_json(&a)),
        None => "{\"hit\":false}".to_string(),
    };
    ok_json(stream, &body)
}

fn action_json(a: &crate::layout::HitAction) -> String {
    use crate::layout::HitAction::*;
    match a {
        Open(p) => format!("{{\"kind\":\"open\",\"path\":\"{}\"}}", json_escape(&p.display().to_string())),
        Toggle(p) => format!("{{\"kind\":\"toggle\",\"path\":\"{}\"}}", json_escape(&p.display().to_string())),
        OpenUrl(u) => format!("{{\"kind\":\"url\",\"url\":\"{}\"}}", json_escape(u)),
        CopyCode(_) => "{\"kind\":\"copy_code\"}".to_string(),
        ScrollTo(y) => format!("{{\"kind\":\"scroll_to\",\"y\":{:.3}}}", y),
        SetRoot(p) => format!("{{\"kind\":\"set_root\",\"path\":\"{}\"}}", json_escape(&p.display().to_string())),
        ToggleTask { box_byte, now_checked } => format!(
            "{{\"kind\":\"toggle_task\",\"byte\":{},\"checked\":{}}}",
            box_byte, now_checked
        ),
    }
}

fn do_sidebar(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    {
        let mut s = shared.state.lock().unwrap();
        if let Some(w) = q.get("w").and_then(|v| v.parse::<f32>().ok()) {
            let w = w.clamp(0.0, 1000.0);
            s.sidebar_width = w;
            if w > 0.0 {
                s.sidebar_width_restore = w;
            }
        }
        if let Some(v) = q.get("visible") {
            let on = v == "1" || v.eq_ignore_ascii_case("true");
            if on {
                s.sidebar_width = if s.sidebar_width_restore > 0.0 {
                    s.sidebar_width_restore
                } else {
                    260.0
                };
            } else {
                if s.sidebar_width > 0.0 {
                    s.sidebar_width_restore = s.sidebar_width;
                }
                s.sidebar_width = 0.0;
            }
        }
        if let Some(y) = q.get("scroll").and_then(|v| v.parse::<f32>().ok()) {
            s.sidebar_scroll = y.max(0.0);
        } else if let Some(dy) = q.get("dscroll").and_then(|v| v.parse::<f32>().ok()) {
            s.sidebar_scroll = (s.sidebar_scroll + dy).max(0.0);
        }
    }
    clamp_sidebar_scroll(shared);
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn do_zoom(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    {
        let mut s = shared.state.lock().unwrap();
        if let Some(v) = q.get("content").and_then(|v| v.parse::<f32>().ok()) {
            s.content_zoom = v.clamp(0.5, 3.0);
        }
        if let Some(v) = q.get("sidebar").and_then(|v| v.parse::<f32>().ok()) {
            s.sidebar_zoom = v.clamp(0.5, 3.0);
        }
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn do_theme(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    {
        let mut s = shared.state.lock().unwrap();
        if let Some(v) = q.get("dark") {
            s.dark = v == "1" || v.eq_ignore_ascii_case("true");
        } else {
            // no param → toggle
            s.dark = !s.dark;
        }
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
}

fn do_hits(stream: &mut TcpStream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let snap = shared.snapshot();
    let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let (pinned, content) = {
        let mut images = shared.images.lock().unwrap();
        crate::render::compute_all_hit_targets(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &shared.fonts,
                tree: snap.tree_flat.as_deref(),
                active_path: snap.source_path.as_deref(),
                base_dir: base_dir.as_deref(),
                sidebar_width: snap.sidebar_width,
                sidebar_scroll: snap.sidebar_scroll,
                content_zoom: snap.content_zoom,
                sidebar_zoom: snap.sidebar_zoom,
                selection: None,
                hover_pos: None,
                search: None,
                mermaid_overrides: None,
            },
            &mut images,
        )
    };
    let mut body = String::from("{\"pinned\":[");
    for (i, t) in pinned.iter().enumerate() {
        if i > 0 { body.push(','); }
        body.push_str(&format!(
            "{{\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1}}}",
            t.x, t.y, t.w, t.h
        ));
    }
    body.push_str("],\"content\":[");
    for (i, t) in content.iter().enumerate() {
        if i > 0 { body.push(','); }
        body.push_str(&format!(
            "{{\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1}}}",
            t.x, t.y, t.w, t.h
        ));
    }
    body.push_str("]}");
    ok_json(stream, &body)
}

fn clamp_sidebar_scroll(shared: &Arc<Shared>) {
    let snap = shared.snapshot();
    let Some(tree) = snap.tree_flat.as_deref() else { return };
    if snap.sidebar_width <= 0.0 {
        return;
    }
    let outline_len = if snap.source_path.is_some() {
        crate::md::count_headings(&snap.source)
    } else {
        0
    };
    let content_h = crate::render::sidebar_content_height(
        &snap.theme,
        tree.len(),
        outline_len,
        snap.sidebar_zoom,
    );
    let max_scroll = (content_h - snap.viewport.height as f32).max(0.0);
    let mut s = shared.state.lock().unwrap();
    if s.sidebar_scroll > max_scroll {
        s.sidebar_scroll = max_scroll;
    }
    if s.sidebar_scroll < 0.0 {
        s.sidebar_scroll = 0.0;
    }
}

fn do_select(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let gf = |k: &str| q.get(k).and_then(|v| v.parse::<f32>().ok());
    let (Some(x1), Some(y1), Some(x2), Some(y2)) = (gf("x1"), gf("y1"), gf("x2"), gf("y2")) else {
        return bad_request(stream, "need ?x1=&y1=&x2=&y2= (screen coords)");
    };
    {
        let mut s = shared.state.lock().unwrap();
        let sc = s.scroll;
        s.sel_anchor = Some((x1, y1 + sc));
        s.sel_head = Some((x2, y2 + sc));
        s.is_selecting = false;
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, "{\"ok\":true}")
}

fn do_select_clear(
    stream: &mut TcpStream,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    {
        let mut s = shared.state.lock().unwrap();
        s.sel_anchor = None;
        s.sel_head = None;
        s.is_selecting = false;
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, "{\"ok\":true}")
}

fn do_copy(stream: &mut TcpStream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let snap = shared.snapshot();
    if snap.selection.is_none() {
        return ok_json(stream, "{\"selected\":false,\"text\":\"\"}");
    }
    let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let text = {
        let mut images = shared.images.lock().unwrap();
        crate::render::extract_selection(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &shared.fonts,
                tree: snap.tree_flat.as_deref(),
                active_path: snap.source_path.as_deref(),
                base_dir: base_dir.as_deref(),
                sidebar_width: snap.sidebar_width,
                sidebar_scroll: snap.sidebar_scroll,
                content_zoom: snap.content_zoom,
                sidebar_zoom: snap.sidebar_zoom,
                selection: snap.selection,
                hover_pos: None,
                search: None,
                mermaid_overrides: None,
            },
            &mut images,
        )
        .unwrap_or_default()
    };
    let copied = !text.is_empty() && crate::clipboard::copy(&text);
    let body = format!(
        "{{\"selected\":true,\"copied\":{},\"text\":\"{}\"}}",
        copied,
        json_escape(&text)
    );
    ok_json(stream, &body)
}

fn do_quit(stream: &mut TcpStream, proxy: &EventLoopProxy<UserEvent>) -> std::io::Result<()> {
    let _ = proxy.send_event(UserEvent::Quit);
    ok_text(stream, "bye\n")
}

// ───── HTTP plumbing ─────────────────────────────────────────────────────────

fn ok_json(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn ok_text(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn not_found(stream: &mut TcpStream) -> std::io::Result<()> {
    let body = "not found\n";
    write!(
        stream,
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn bad_request(stream: &mut TcpStream, msg: &str) -> std::io::Result<()> {
    let body = format!("{msg}\n");
    write!(
        stream,
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

// ───── tiny parsers ──────────────────────────────────────────────────────────

fn parse_query(qs: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if qs.is_empty() {
        return map;
    }
    for pair in qs.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(url_decode(k), url_decode(v));
    }
    map
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex(bytes[i + 1]);
                let lo = hex(bytes[i + 2]);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4) | l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

const INDEX: &str = "\
mdrdr api

  GET  /state            — current state as JSON
  GET  /tree             — current file tree as JSON
  GET  /screenshot       — PNG of the current viewport
  POST /scroll?y=N       — absolute scroll
  POST /scroll?dy=N      — relative scroll
  POST /resize?w=N&h=N   — resize viewport (state only; window follows on redraw)
  POST /open?path=P      — load a file
  POST /click?x=X&y=Y    — simulate a left click at screen coords
  POST /sidebar?w=N      — set sidebar width (0 hides it)
  POST /sidebar?visible=0|1
  POST /select?x1=&y1=&x2=&y2=  — select a range (screen coords)
  POST /select/clear
  POST /copy                    — copy selection to clipboard (also GET /selection)
  POST /quit             — exit

  Test driver — used by automated checks; would not normally be hand-driven.
  POST /search/open
  POST /search/close
  GET  /search                  — query, cursor, anchor, current, match_count
  POST /quickopen/open
  POST /quickopen/close
  GET  /quickopen               — query, cursor, anchor, selected, scanning, scan_count
  POST /key?name=K[&shift=1][&ctrl=1][&alt=1]
       Names: any single character, or one of:
       Escape, Enter, Tab, Backspace, Delete, Space, Home, End,
       ArrowLeft, ArrowRight, ArrowUp, ArrowDown, PageUp, PageDown
  POST /type?text=...           — sugar for one /key per char
";

// ───── test-driver endpoints ────────────────────────────────────────────────

/// Map an endpoint name string to a winit NamedKey or a Char(s).
fn parse_synth_key(name: &str) -> Option<SynthKey> {
    use winit::keyboard::NamedKey as N;
    let named = match name {
        "Escape" => N::Escape,
        "Enter" => N::Enter,
        "Tab" => N::Tab,
        "Backspace" => N::Backspace,
        "Delete" => N::Delete,
        "Space" => N::Space,
        "Home" => N::Home,
        "End" => N::End,
        "ArrowLeft" => N::ArrowLeft,
        "ArrowRight" => N::ArrowRight,
        "ArrowUp" => N::ArrowUp,
        "ArrowDown" => N::ArrowDown,
        "PageUp" => N::PageUp,
        "PageDown" => N::PageDown,
        _ => {
            // Anything else is a literal character (or short string,
            // which winit can deliver via Key::Character on platforms
            // with composed input).
            return Some(SynthKey::Char(name.to_string()));
        }
    };
    Some(SynthKey::Named(named))
}

fn flag(q: &HashMap<String, String>, key: &str) -> bool {
    matches!(q.get(key).map(|s| s.as_str()), Some("1") | Some("true") | Some("yes"))
}

fn do_key(
    stream: &mut TcpStream,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let name = match q.get("name") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return ok_text(stream, "{\"ok\":false,\"error\":\"missing name\"}"),
    };
    let Some(kind) = parse_synth_key(&name) else {
        return ok_text(stream, "{\"ok\":false,\"error\":\"unknown key\"}");
    };
    let _ = proxy.send_event(UserEvent::SynthKey {
        kind,
        shift: flag(q, "shift"),
        ctrl: flag(q, "ctrl"),
        alt: flag(q, "alt"),
    });
    ok_text(stream, "{\"ok\":true}")
}

fn do_type(
    stream: &mut TcpStream,
    proxy: &EventLoopProxy<UserEvent>,
    q: &HashMap<String, String>,
) -> std::io::Result<()> {
    let Some(text) = q.get("text") else {
        return ok_text(stream, "{\"ok\":false,\"error\":\"missing text\"}");
    };
    for ch in text.chars() {
        // Space goes through the NamedKey arm in the input handlers,
        // so the synthesiser has to deliver it the same way.
        let kind = if ch == ' ' {
            SynthKey::Named(winit::keyboard::NamedKey::Space)
        } else {
            SynthKey::Char(ch.to_string())
        };
        let _ = proxy.send_event(UserEvent::SynthKey {
            kind,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
    ok_text(stream, "{\"ok\":true}")
}

fn do_search_open(
    stream: &mut TcpStream,
    _shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    let _ = proxy.send_event(UserEvent::OpenSearch);
    ok_text(stream, "{\"ok\":true}")
}

fn do_search_close(
    stream: &mut TcpStream,
    _shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    let _ = proxy.send_event(UserEvent::CloseSearch);
    ok_text(stream, "{\"ok\":true}")
}

fn search_json(shared: &Arc<Shared>) -> String {
    let s = shared.state.lock().unwrap();
    match &s.search {
        Some(su) => format!(
            "{{\"open\":true,\"query\":\"{}\",\"cursor\":{},\"anchor\":{},\"current\":{},\"match_count\":{}}}",
            json_escape(&su.query),
            su.cursor,
            su.anchor,
            su.current,
            su.match_count,
        ),
        None => "{\"open\":false}".to_string(),
    }
}

fn do_quickopen_open(
    stream: &mut TcpStream,
    _shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    let _ = proxy.send_event(UserEvent::OpenQuickOpen);
    ok_text(stream, "{\"ok\":true}")
}

fn do_quickopen_close(
    stream: &mut TcpStream,
    _shared: &Arc<Shared>,
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<()> {
    let _ = proxy.send_event(UserEvent::CloseQuickOpen);
    ok_text(stream, "{\"ok\":true}")
}

fn quickopen_json(shared: &Arc<Shared>) -> String {
    let s = shared.state.lock().unwrap();
    match &s.quick_open {
        Some(qo) => format!(
            "{{\"open\":true,\"query\":\"{}\",\"cursor\":{},\"anchor\":{},\"selected\":{},\"scanning\":{},\"file_count\":{}}}",
            json_escape(&qo.query),
            qo.cursor,
            qo.anchor,
            qo.selected,
            qo.scanning,
            qo.files.len(),
        ),
        None => "{\"open\":false}".to_string(),
    }
}
