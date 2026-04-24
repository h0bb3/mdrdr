# mdrdr

A fast, lightweight, from-scratch markdown viewer.

Everything above the primitive layer (window, pixels, font raster, image decode) is written here: parser, layout, text rendering, file tree, math, mermaid, emoji routing, HTTP server. No web engine. No framework.

## What it does

- **CommonMark subset** — headings, paragraphs, lists, fenced code, blockquotes, horizontal rules, inline emphasis / code / links / images, word-wrap, scroll.
- **GFM tables** — `| col | col |` with `:---:` / `---:` / `:---` alignment, inline formatting inside cells, cells wrap when narrow, links inside cells clickable.
- **LaTeX math** — `$inline$` and `$$display$$`. Greek, operators, `\frac`, `\sqrt`, `^` and `_` scripts, `\sum` / `\int` / `\prod`, accents (`\hat`, `\bar`, `\tilde`, `\vec`, `\dot`), named operators (`\max`, `\min`, `\log`, …), `\text{…}` and `\mathrm{…}`, `\mathbb{…}` blackboard bold, `\left…\right` auto-sized delimiters, `\big` / `\Big` / `\bigg`, `\quad` / `\qquad` spacing. Italic correction around superscripts.
- **Mermaid** — `graph TD` / `graph LR` flowcharts with rectangles, rounded, circles, and diamond decisions; arrows with optional `|labels|`. Small graphs scale up to fill the width; wide graphs shrink to fit. LR edges widen to host their labels.
- **Emoji** — monochrome OpenMoji routing for both content and sidebar.
- **File tree sidebar** — click a folder to expand, click a file to open; active file highlighted. `..` row goes up one directory; double-click a directory to set it as root. Toggleable with `b`, drag-resizable.
- **Live reload** — edit the file in any editor, the window reflows within ~250 ms.
- **Selection + copy** — drag to select, Ctrl+C to copy (shells out to `wl-copy` / `xclip`).
- **Context menu (right-click)** — contextual Copy actions surface automatically:
  - Over a selection → **Copy text**
  - Over a code block → **Copy code**
  - Over a table → **Copy table as CSV** + **Copy table as Markdown**
  - Over a tree row → **Copy path** (otherwise the active document's path)
  - **Outline ▸** — side panel with every heading; click to scroll
  - **Dark / Light Theme** toggle
- **Dark mode** — `t` to toggle (or via the context menu). Tables, code blocks and Mermaid diagrams track the theme.
- **Zoom** — `Ctrl+wheel` over a panel changes its font size independently (content vs sidebar).
- **Internal anchor links** — `[text](#heading)` scrolls to the matching heading (GitHub-style slug).
- **Scrollbars** — content and sidebar both get thin scrollbars when content overflows.
- **Inline images** — PNG / JPEG, resolved relative to the current file, mtime-cached.
- **Claude-drivable API** — localhost HTTP for screenshots, scroll, click, selection, theme, zoom, etc.

## Install

### Pre-built binaries (recommended)

Every push to the `release` branch cuts a GitHub Release with:

- `mdrdr-linux-x86_64`
- `mdrdr-macos-universal` (Apple Silicon + Intel)
- `mdrdr-windows-x86_64.exe`

Grab from https://github.com/h0bb3/mdrdr/releases/latest, `chmod +x`, drop on your PATH.

### From source

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/h0bb3/mdrdr.git
cd mdrdr
cargo build --release
install -Dm755 target/release/mdrdr ~/.local/bin/mdrdr
```

`~/.local/bin` is typically already on `$PATH`. Alternatively `cargo install --path .` installs to `~/.cargo/bin`.

### Register as the default `.md` handler (Linux)

Write a desktop entry, refresh the MIME database, make it the default:

```bash
cat > ~/.local/share/applications/mdrdr.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=mdrdr
GenericName=Markdown Viewer
Comment=From-scratch markdown viewer
Exec=mdrdr %f
Terminal=false
Categories=Office;Utility;TextTools;
MimeType=text/markdown;
StartupNotify=false
EOF

update-desktop-database ~/.local/share/applications
xdg-mime default mdrdr.desktop text/markdown
```

Verify with `xdg-mime query default text/markdown` (should print `mdrdr.desktop`).

To undo: `xdg-mime default <previous>.desktop text/markdown` and `rm ~/.local/share/applications/mdrdr.desktop`.

## Usage

```
mdrdr                         # open the window rooted at the current directory
mdrdr .                       # same
mdrdr FILE_OR_DIR             # open the file / directory (OS MIME handler uses this form)
mdrdr open FILE_OR_DIR        # explicit form
mdrdr render FILE.md [--tree DIR] [--out preview.png] [--width W] [--height H] [--scroll Y]
```

`mdrdr render` is headless — writes a PNG and exits. This is the main iteration loop during development and during Claude agent runs.

`mdrdr open` (or a bare path) brings up a native window and prints `mdrdr api listening on http://127.0.0.1:<port>` to stdout.

### Keyboard / mouse

| Input | Action |
|---|---|
| `Space` / `PageDown` / `↓` | scroll down |
| `PageUp` / `↑` | scroll up |
| `Home` / `End` | top / bottom |
| `b` | toggle sidebar |
| `t` | toggle dark/light theme |
| `Ctrl+C` | copy selection |
| `Esc` | close context menu, else quit |
| drag content | select text |
| drag sidebar right edge | resize sidebar |
| wheel over sidebar | scroll sidebar |
| `Ctrl`+wheel | zoom the panel under the cursor |
| double-click a folder row | set that folder as the tree root |
| right-click | open contextual menu |

### HTTP control

```
curl        http://127.0.0.1:$PORT/state
curl        http://127.0.0.1:$PORT/tree
curl     -o shot.png http://127.0.0.1:$PORT/screenshot
curl -X POST 'http://127.0.0.1:$PORT/scroll?dy=200'
curl -X POST 'http://127.0.0.1:$PORT/resize?w=900&h=700'
curl -X POST 'http://127.0.0.1:$PORT/open?path=/tmp/notes.md'
curl -X POST 'http://127.0.0.1:$PORT/click?x=80&y=120'
curl -X POST 'http://127.0.0.1:$PORT/sidebar?visible=0'
curl -X POST 'http://127.0.0.1:$PORT/sidebar?w=180'
curl -X POST 'http://127.0.0.1:$PORT/zoom?content=1.2&sidebar=1.0'
curl -X POST 'http://127.0.0.1:$PORT/theme?dark=1'
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
- **window** (`mdrdr open` / bare path) — winit + softbuffer event loop.
- **api** — localhost HTTP. `/screenshot` calls `render` directly against current state — never touches the window thread.

## Layout

```
src/
├── main.rs       CLI dispatch
├── md.rs         CommonMark parser (block + inline) + GFM tables
├── math.rs       LaTeX math layout
├── mermaid.rs    Flowchart layout
├── layout.rs     Page layout, word wrap, sidebar, tables, copy zones
├── render.rs     Pure render core, selection highlights, scrollbar
├── images.rs     PNG/JPEG cache (mtime-keyed)
├── tree.rs       File tree scanner
├── watch.rs      mtime poller → live reload
├── clipboard.rs  wl-copy / xclip bridge
├── window.rs     winit event loop + context menu
├── api.rs        Hand-rolled HTTP/1.1
├── headless.rs   PNG exporter
├── theme.rs      Colors, sizes, margins (light + dark variants)
└── font.rs       TTF loader, bundled via include_bytes!
```

## Bundled assets

DejaVu Sans / Bold / Italic / Bold-Italic / Sans Mono TTFs live in `assets/fonts/` and are `include_bytes!`-ed into the binary (license: `assets/fonts/LICENSE.dejavu`). Emoji via OpenMoji-black (CC BY-SA 4.0, `LICENSE.openmoji`).

Sample markdown files in `assets/samples/` — including `overview.md` which exercises every feature.

## The one rule

Only four crate dependencies:

- `winit` — window + input events
- `softbuffer` — CPU pixel framebuffer
- `fontdue` — TTF parsing + glyph rasterization
- `image` — PNG/JPEG decode

Markdown parsing, text layout, word wrap, file tree, math layout, mermaid layout, HTTP server, JSON emission, URL decoding — all hand-written.

## Releases

Push or merge to the `release` branch and CI will build Linux / macOS / Windows binaries and publish them as a tagged GitHub Release. Typical flow:

```bash
git checkout release
git merge main
git push
```
