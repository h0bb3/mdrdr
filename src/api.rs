//! Tiny HTTP/1.1 server — just enough to drive the window from `curl`.
//!
//! Endpoints:
//!   GET  /state
//!   GET  /screenshot
//!   POST /scroll?y=N | ?dy=N
//!   POST /resize?w=N&h=N
//!   POST /open?path=URL_ENCODED
//!   POST /quit
//!
//! Query-string bodies keep this file dependency-free. JSON bodies can land later.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use winit::event_loop::EventLoopProxy;

use crate::render::{render, Viewport};
use crate::window::{Shared, UserEvent};

pub fn spawn(shared: Arc<Shared>, proxy: EventLoopProxy<UserEvent>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
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

    port
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
        ("GET", "/screenshot") => screenshot(&mut stream, &shared),
        ("POST", "/scroll") => do_scroll(&mut stream, &shared, &proxy, &q),
        ("POST", "/resize") => do_resize(&mut stream, &shared, &proxy, &q),
        ("POST", "/open") => do_open(&mut stream, &shared, &proxy, &q),
        ("POST", "/quit") => do_quit(&mut stream, &proxy),
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
    format!(
        "{{\"source_path\":{path_json},\"scroll\":{:.3},\"viewport\":{{\"w\":{},\"h\":{}}},\"source_len\":{}}}",
        s.scroll,
        s.viewport.width,
        s.viewport.height,
        s.source.len()
    )
}

fn screenshot(stream: &mut TcpStream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let (source, scroll, viewport, theme) = shared.snapshot();
    let fb = render(&source, viewport, scroll, &theme, &shared.fonts);
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(fb.width, fb.height, fb.pixels)
            .expect("fb size mismatch");
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
    {
        let mut s = shared.state.lock().unwrap();
        if let Some(y) = q.get("y").and_then(|v| v.parse::<f32>().ok()) {
            s.scroll = y.max(0.0);
        } else if let Some(dy) = q.get("dy").and_then(|v| v.parse::<f32>().ok()) {
            s.scroll = (s.scroll + dy).max(0.0);
        }
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
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
        s.source_path = Some(std::path::PathBuf::from(path));
        s.scroll = 0.0;
    }
    let _ = proxy.send_event(UserEvent::Redraw);
    ok_json(stream, &state_json(shared))
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
  GET  /screenshot       — PNG of the current viewport
  POST /scroll?y=N       — absolute scroll
  POST /scroll?dy=N      — relative scroll
  POST /resize?w=N&h=N   — resize viewport (state only; window follows on redraw)
  POST /open?path=P      — load a file
  POST /quit             — exit
";
