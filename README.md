# mdrdr

A fast, lightweight, from-scratch markdown viewer.

Everything above the primitive layer (window, pixels, font raster, image decode) is written here: parser, layout, file tree, math, mermaid. No web engine. No framework.

## What it does

- **CommonMark subset** — headings, paragraphs, lists, fenced code, blockquotes, horizontal rules, inline emphasis / code / links / images, word-wrap, scroll.
- **GFM tables** — `| col | col |` with `:---:` / `---:` / `:---` alignment, inline formatting and math inside cells, cells wrap when narrow.
- **File tree sidebar** — click a folder to expand, click a file to open; active file highlighted. Toggleable (`b`) and drag-resizable.
- **Scrollbar** — pinned on the right when the doc exceeds the viewport.
- **Text selection + copy** — drag to select, Ctrl+C to copy (shells out to `wl-copy` / `xclip`).
- **Inline images** — PNG / JPEG, resolved relative to the current file, mtime-cached.
- **Live reload** — edit the file in any editor, the window reflows within ~250 ms.
- **LaTeX math** — `$inline$` and `$$display$$`. Greek, operators, `\frac`, `\sqrt`, `^` and `_` scripts, big ops (`\sum`, `\int`, `\prod`). Unknown commands fall back to their literal `\name`.
- **Mermaid** — `graph TD` / `graph LR` flowcharts with rectangles, rounded, circles, and diamond decisions; arrows with optional `|labels|`. Small graphs scale up to fill the available width; wide graphs shrink to fit. Header-less content falls back to a plain code block.
- **Claude-drivable API** — every window can be controlled over HTTP on localhost, including taking a PNG screenshot, selecting ranges, copying to clipboard.

## Build

```
cargo build --release
```

The only dependencies are `winit`, `softbuffer`, `fontdue`, `image`. Everything else — parser, text layout, HTTP server, URL decoding, JSON emission, math engine, mermaid layout, file watcher — is in this repo.

## Run

```
mdrdr render FILE.md [--tree DIR] [--out preview.png] [--width W] [--height H] [--scroll Y]
```
Headless. Writes a PNG and exits. This is the main iteration loop during development.

```
mdrdr open FILE_OR_DIR
```
Opens a native window. Prints `mdrdr api listening on http://127.0.0.1:<port>` to stdout.

### Keyboard / mouse

- `Space` / `PageDown` / `↓` — scroll down
- `PageUp` / `↑` — scroll up
- `Home` / `End` — top / bottom
- `b` — toggle sidebar
- drag sidebar right edge — resize it
- drag in content — select text
- `Ctrl+C` — copy selection to clipboard
- `Esc` — quit

### HTTP control

```
curl        http://127.0.0.1:$PORT/state
curl        http://127.0.0.1:$PORT/tree
curl     -o http://127.0.0.1:$PORT/screenshot shot.png
curl -X POST 'http://127.0.0.1:$PORT/scroll?dy=200'
curl -X POST 'http://127.0.0.1:$PORT/resize?w=900&h=700'
curl -X POST 'http://127.0.0.1:$PORT/open?path=/tmp/notes.md'
curl -X POST 'http://127.0.0.1:$PORT/click?x=80&y=120'
curl -X POST 'http://127.0.0.1:$PORT/sidebar?visible=0'
curl -X POST 'http://127.0.0.1:$PORT/sidebar?w=180'
curl -X POST 'http://127.0.0.1:$PORT/select?x1=50&y1=60&x2=400&y2=80'
curl -X POST  http://127.0.0.1:$PORT/copy
curl -X POST  http://127.0.0.1:$PORT/quit
```

## Architecture

```
render(source, viewport, scroll, theme, fonts, tree, base_dir, images)
    ──▶ parse()     [md.rs]
    ──▶ layout()    [layout.rs]   uses math.rs, mermaid.rs, images.rs
    ──▶ draw()      [render.rs]
    ──▶ Framebuffer (RGBA8)
```

Pure function. Three shells funnel through it:

- **headless** (`mdrdr render`) — writes PNG, exits.
- **window** (`mdrdr open`) — winit + softbuffer event loop.
- **api** — localhost HTTP. `/screenshot` just calls `render` directly against current state — never touches the window thread.

## Layout

```
src/
├── main.rs       CLI dispatch
├── md.rs         CommonMark parser (block + inline) + GFM tables
├── math.rs       LaTeX math layout
├── mermaid.rs    Flowchart layout
├── layout.rs     Page layout, word wrap, sidebar, tables
├── render.rs     Pure render core, selection highlights, scrollbar
├── images.rs     PNG/JPEG cache (mtime-keyed)
├── tree.rs       File tree scanner
├── watch.rs      mtime poller → live reload
├── clipboard.rs  wl-copy / xclip bridge
├── window.rs     winit event loop
├── api.rs        Hand-rolled HTTP/1.1
├── headless.rs   PNG exporter
├── theme.rs      Colors, sizes, margins
└── font.rs       TTF loader, bundled via include_bytes!
```

## Bundled assets

DejaVu Sans / Bold / Italic / Bold-Italic / Sans Mono TTFs live in `assets/fonts/` and are `include_bytes!`-ed into the binary. License in `assets/fonts/LICENSE.dejavu`.

Sample markdown files in `assets/samples/` — including `overview.md` which exercises every feature.

## Status

1. ✅ M1 — bones: render core + headless PNG + window + HTTP API
2. ✅ M2 — parser + layout + scroll
3. ✅ M3 — file tree sidebar with click-to-open
4. ✅ M4 — inline images
5. ✅ M5 — live reload
6. ✅ M6 — LaTeX math subset
7. ✅ M7 — Mermaid subset
8. ✅ Scrollbar, sidebar toggle + drag-resize, mermaid width
9. ✅ GFM pipe tables (with alignment, wrapping cells, inline formatting)
10. ✅ Text selection + Ctrl+C to clipboard
