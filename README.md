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
- **Internal anchor links** — `[text](#heading)` scrolls to the matching heading (GitHub-style slug), or to an explicit `<a id="…">` anywhere in the document.
- **Raw HTML** — `<img>` renders as an image, `<a id="…">` becomes an invisible link target; comments (including multi-line ones) and layout-only tags are dropped rather than echoed as source.
- **Scrollbars** — content and sidebar both get thin scrollbars when content overflows.
- **Inline images** — PNG / JPEG, resolved relative to the current file, mtime-cached.
- **Claude-drivable API** — localhost HTTP for screenshots, scroll, click, selection, theme, zoom, etc.
- **AI agent review loop** — drop comments in the right margin and have a coding agent (Claude Code, Codex) answer them in place: it reads the anchored lines, edits the file (which live-reloads), and replies in the bubble. See [Driving mdrdr from an AI agent](#driving-mdrdr-from-an-ai-agent-claude-code--codex).

## Install

### Pre-built binaries (recommended)

Every push to the `release` branch cuts a GitHub Release with one binary per platform. Grab the matching one from https://github.com/h0bb3/mdrdr/releases/latest:

**Linux** (`mdrdr-linux-x86_64`)

```bash
chmod +x mdrdr-linux-x86_64
mv mdrdr-linux-x86_64 ~/.local/bin/mdrdr
```

**macOS** (`mdrdr-macos-universal` — Apple Silicon + Intel in one binary)

```bash
chmod +x mdrdr-macos-universal
xattr -d com.apple.quarantine mdrdr-macos-universal   # release binaries are unsigned
mv mdrdr-macos-universal /usr/local/bin/mdrdr         # or ~/.local/bin/mdrdr
```

If you skip the `xattr` step, Gatekeeper blocks the first launch — open Finder, right-click the binary, **Open**, then **Open** again in the warning dialog to whitelist it once.

**Windows** (`mdrdr-windows-x86_64.exe`)

Rename to `mdrdr.exe`, drop in a folder on `PATH` (e.g. `%LOCALAPPDATA%\Programs\mdrdr\`, then add that to **System Properties → Environment Variables → Path**). First launch shows a SmartScreen warning — click **More info → Run anyway** (the binary is unsigned).

### From source

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/h0bb3/mdrdr.git
cd mdrdr
cargo build --release
```

The compiled binary lands at `target/release/mdrdr` (or `mdrdr.exe` on Windows). Install it however you like:

- **Linux / macOS:** `install -Dm755 target/release/mdrdr ~/.local/bin/mdrdr`
- **Windows (PowerShell):** `Copy-Item target\release\mdrdr.exe $Env:LOCALAPPDATA\Programs\mdrdr\mdrdr.exe`
- **Any platform:** `cargo install --path .` drops it in `~/.cargo/bin` (already on `PATH` if you installed Rust via rustup).

### Register as the default `.md` handler

**Linux** — write a desktop entry, refresh the MIME database, make it the default:

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

Verify with `xdg-mime query default text/markdown` (should print `mdrdr.desktop`). Undo: `xdg-mime default <previous>.desktop text/markdown` and `rm ~/.local/share/applications/mdrdr.desktop`.

**macOS** — `mdrdr` is a CLI tool, not a `.app` bundle, so Finder's "Open With" UI can't target it directly. Easiest path is [`duti`](https://github.com/moretension/duti) plus a tiny wrapper `.app`:

```bash
brew install duti

# 1. Make a one-line wrapper .app that just shells out to mdrdr.
osacompile -o ~/Applications/mdrdr.app -e \
  'on open theFiles
     repeat with f in theFiles
       do shell script "/usr/local/bin/mdrdr " & quoted form of POSIX path of f & " >/dev/null 2>&1 &"
     end repeat
   end open'

# 2. Tell LaunchServices to use it for net.daringfireball.markdown (the
#    UTI most editors register `.md` under).
duti -s com.yourname.mdrdr net.daringfireball.markdown all
```

Adjust the bundle id in the `osacompile` step's `Info.plist` if you want the `duti` line to match. To undo, delete `~/Applications/mdrdr.app` and re-pick a default in Finder ▸ **Get Info** ▸ **Open with**.

**Windows** — from an admin **Command Prompt** (`cmd.exe`, not PowerShell — `assoc` / `ftype` are cmd builtins):

```cmd
assoc .md=mdrdrFile
ftype mdrdrFile="C:\path\to\mdrdr.exe" "%1"
```

Or via the GUI: right-click any `.md` file ▸ **Open with** ▸ **Choose another app** ▸ **Always use this app** ▸ browse to `mdrdr.exe`. Undo via **Settings ▸ Apps ▸ Default apps ▸ Choose defaults by file type**.

## Usage

```
mdrdr                         # open the window rooted at the current directory
mdrdr .                       # same
mdrdr FILE_OR_DIR             # open the file / directory (OS MIME handler uses this form)
mdrdr open FILE_OR_DIR [--api] [--port N]
mdrdr render FILE.md [--tree DIR] [--out preview.png] [--width W] [--height H] [--scroll Y]
```

`mdrdr render` is headless — writes a PNG and exits. The main iteration loop during development and during Claude agent runs.

`mdrdr open` (or a bare path) brings up a native window. By default the HTTP control API is **off** — humans don't need it. Opt in with:

- `--api` — spawn the API on a random ephemeral port.
- `--port N` — spawn the API on port N (implies `--api`). If the port is already in use, the window still opens; only the API is skipped.

Either way, the bound port is printed to stdout as `mdrdr api listening on http://127.0.0.1:<port>`.

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

### Driving mdrdr from an AI agent (Claude Code / Codex)

mdrdr doubles as a review surface for coding agents: you read a doc in the window, drop comments in the right margin, and an agent answers them in place — editing the file (which live-reloads) and replying in the bubble.

Launch with the API on a known port so the agent can find it:

```
mdrdr open NOTES.md --port 7779
```

The comment endpoints the agent uses:

```
curl      'http://127.0.0.1:7779/comments?pending=1'        # threads awaiting a reply
curl -X POST 'http://127.0.0.1:7779/comments/reply?id=1' --data-urlencode 'text=...'
curl -X POST 'http://127.0.0.1:7779/comments/resolve?id=1'  # resolve (or &resolved=0 to reopen)
```

Each pending thread carries its anchored `line_start`/`line_end`, the `quote` that was marked, and the message history — enough for the agent to reorient in the file, make an `Edit`, and reply. Edits hot-reload in the viewer immediately. Leave threads open (don't auto-resolve) so the human can follow up.

**Claude Code** — the `/mdrdr-chat` skill wraps this loop: it pulls pending threads, edits the doc, and posts replies. Drive it as a live chat with the loop skill:

```
/loop 8s /mdrdr-chat
```

Set `MDRDR_PORT` first if you launched on a non-default port. There's also `/mdrdr-edit` for editing the region currently selected in the viewer.

**Codex** (or any other agent) — there's no bundled skill, so point it at the endpoints directly: tell it to poll `GET /comments?pending=1`, read the file around each thread's anchor, edit, and `POST /comments/reply`. The same curl contract applies.

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
