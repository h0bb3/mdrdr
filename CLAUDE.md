# mdrdr — notes for Claude

## The one rule

**Everything above the primitive layer is written by us.** The primitive layer is exactly four crates and no more:

- `winit` — window + input events
- `softbuffer` — CPU pixel framebuffer
- `fontdue` — TTF parsing + glyph rasterization
- `image` — PNG/JPEG decode/encode

If you're tempted to add a crate: don't. Markdown parsing, text layout, word wrap, file tree, math layout, mermaid layout, HTTP server, JSON emission, URL decoding — all of it is us. That's the point of the project.

## Architecture in one diagram

```
                ┌──────────────────────────────────────┐
                │  render(source, viewport, scroll,    │
                │         theme, fonts) -> Framebuffer │
                │  (pure function, the core)           │
                └───────────────┬──────────────────────┘
                                │
         ┌──────────────────────┼──────────────────────┐
         ▼                      ▼                      ▼
  headless::render_to_png    window::App.draw     api::screenshot
  (→ PNG file, exits)        (→ softbuffer)       (→ HTTP response)
```

The render core is pure. It does not touch winit, softbuffer, TCP, or the filesystem. Keep it that way — it's what lets the API screenshot the current state without involving the window thread, and what lets golden-image tests work.

## The AI feedback loop

This project has a first-class automation API because Claude needs to see what it builds. Use it:

- `mdrdr render foo.md --out /tmp/preview.png` — fastest way to check a change. Always do this after modifying the render core or the parser.
- `curl http://127.0.0.1:$PORT/screenshot -o shot.png` — grab the live window's current pixels.
- `curl http://127.0.0.1:$PORT/state` — JSON of current file / scroll / viewport.

When M3 adds a JSON AST dump (`mdrdr dump foo.md`), prefer that for logic bugs and reserve screenshots for visual bugs.

## Threading model

- Main thread: winit event loop (non-negotiable, winit requires it).
- `mdrdr-api` thread: accepts TCP connections, spawns a short-lived worker per request.
- Workers share `Arc<Shared>` with the main thread and wake it via `EventLoopProxy<UserEvent>` when state changes.
- `Shared` contains `Fonts` (read-only, no lock) and `Mutex<AppState>` (source / scroll / viewport / path).

Never block a winit callback on the API thread's mutex for long. Snapshot what you need, drop the lock, then render.

## Coordinates and units

- Framebuffer is RGBA8, row-major, top-left origin.
- `draw_text` takes `baseline_y` (not top-left) because that's what fontdue gives us with `ymin`.
- Softbuffer wants `0x00RRGGBB` u32 pixels; we convert from RGBA8 in `window::App::draw`.

## Things that look tempting but will hurt

- ❌ Adding a CLI parsing crate (`clap`, `argh`). Our flag set is tiny — hand-roll it.
- ❌ Adding `serde` / `serde_json`. The state JSON is three fields. Build the string.
- ❌ Adding `tokio` / async. The API is low-volume localhost; a thread-per-request is fine and ~20 lines simpler.
- ❌ Adding a GUI toolkit (`egui`, `iced`). Defeats the whole premise.
- ❌ Using a shaper like `rustybuzz`. DejaVu Latin + Greek doesn't need shaping; when we hit Arabic / Devanagari / Thai we'll reconsider.
- ❌ Reading TTF bytes from disk at runtime. We `include_bytes!` so the binary is self-contained and deterministic across machines.

## Git hygiene

Commit after each milestone lands and is verifiable (PNG produced, API responds). Don't commit `target/`. Commit messages lead with the milestone: `M1: bones + verifiable loop`.
