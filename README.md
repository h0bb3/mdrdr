# mdrdr

A fast, lightweight, from-scratch markdown viewer.

Everything above the primitive layer (window, pixels, font raster, image decode) is written here: parser, layout, file tree, math, mermaid. No web engine. No framework.

## Status

**Milestone 1 — bones + verifiable loop.** The render core, headless PNG output, window mode, and HTTP control API are all wired up. No markdown parser yet — `render` produces a hardcoded sample page that exercises every font style.

## Build

```
cargo build --release
```

Primitives only: `winit`, `softbuffer`, `fontdue`, `image`. That's the entire dep tree we didn't write.

## Run

```
mdrdr render [FILE] [--out preview.png] [--width 1200] [--height 900]
```
Headless — renders to PNG and exits. The main iteration loop for development.

```
mdrdr open [FILE]
```
Opens a native window. Prints `mdrdr api listening on http://127.0.0.1:<port>` to stdout. The window is fully drivable via HTTP:

```
curl http://127.0.0.1:<port>/state
curl http://127.0.0.1:<port>/screenshot -o shot.png
curl -X POST 'http://127.0.0.1:<port>/scroll?dy=200'
curl -X POST 'http://127.0.0.1:<port>/resize?w=800&h=600'
curl -X POST 'http://127.0.0.1:<port>/open?path=/tmp/notes.md'
curl -X POST  http://127.0.0.1:<port>/quit
```

## Architecture

```
render(source, viewport, scroll, theme, fonts) -> Framebuffer
```

One pure function. Three thin shells wrap it:
- **headless** (`mdrdr render`) — writes PNG.
- **window** (`mdrdr open`) — winit event loop, softbuffer framebuffer push.
- **api** — localhost HTTP; `/screenshot` just calls `render` directly with the current state.

Because the core is pure, `/screenshot` never touches the window — it renders the current state independently. The window is for humans; the API is for automation and for Claude.

## Bundled assets

DejaVu Sans / DejaVu Sans Mono TTFs live in `assets/fonts/`. Their license is in `assets/fonts/LICENSE.dejavu` (redistribution allowed).

## Roadmap

1. ✅ M1 — bones + verifiable loop
2. M2 — CommonMark subset (headings, paragraphs, lists, code, emphasis, links, images, word wrap, scroll)
3. M3 — file tree sidebar + keyboard nav
4. M4 — inline images (PNG/JPEG)
5. M5 — live reload on file change
6. M6 — LaTeX math subset (inline + display, fractions, sub/super, Greek, sums, roots)
7. M7 — Mermaid subset (`graph TD`/`graph LR`, boxes + arrows, layered layout)
