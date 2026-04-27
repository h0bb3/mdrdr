//! Window mode: winit event loop + softbuffer framebuffer push.
//! Also spawns the HTTP API so the window can be driven externally.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::api;
use crate::clipboard;
use crate::font::Fonts;
use crate::images::ImageCache;
use crate::layout::HitAction;
use crate::render::{
    compute_all_hit_targets, extract_selection, hit_test, in_scrollbar_strip,
    in_sidebar_scrollbar_strip, measure, measure_text_width, render, scrollbar_geom,
    sidebar_content_height, sidebar_scrollbar_geom, RenderInput, SbGeom, Viewport,
};
use crate::theme::Theme;
use crate::tree::FileTree;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Redraw,
    Quit,
    /// Synthesized keypress, posted by the HTTP test API. The main
    /// event-loop thread temporarily overrides its modifier state to
    /// match `shift / ctrl / alt`, dispatches the key through the same
    /// handlers that real keyboard input uses, then restores.
    SynthKey {
        kind: SynthKey,
        shift: bool,
        ctrl: bool,
        alt: bool,
    },
    /// Open the in-document search overlay (same as the user pressing
    /// Ctrl+F). Posted by the test API.
    OpenSearch,
    /// Close the in-document search overlay if open.
    CloseSearch,
    /// Open the Ctrl+P quick-open panel.
    OpenQuickOpen,
    /// Close the Ctrl+P quick-open panel if open.
    CloseQuickOpen,
}

/// Subset of winit keys the test API needs to forge. `Char` carries a
/// single character (or short string for typed input) to mirror what
/// `Key::Character` carries from real input.
#[derive(Debug, Clone)]
pub enum SynthKey {
    Char(String),
    Named(NamedKey),
}

/// One row of the right-click context menu. Kept tiny: a label and the
/// intent. New items extend `MenuAction`, not the struct.
#[derive(Debug, Clone)]
pub enum MenuAction {
    ToggleTheme,
    CopyPath(PathBuf),
    /// Marker row: hovering it opens the Outline submenu. Not directly
    /// executable — clicking does nothing by itself.
    Outline,
    /// Marker row: hovering it opens the mermaid "Layout ▸" submenu.
    MermaidMenu,
    /// Scroll the document to this doc-y. Used by outline submenu entries.
    ScrollTo(f32),
    /// Put this text on the clipboard. Used by "Copy text" (selection),
    /// "Copy code" (code block), and "Copy table as CSV" (table).
    CopyText(String),
    /// Open the in-document search overlay.
    Find,
    /// Open the Ctrl+P quick-open panel.
    QuickOpen,
    /// Set the view-only layout direction for a specific mermaid block
    /// (by its document-order index).
    SetMermaidLayout(usize, crate::mermaid::Direction),
    /// Clear the per-block layout override so the diagram renders with
    /// whatever direction its source header declares.
    ResetMermaidLayout(usize),
}

/// A context menu floating near the cursor. Coordinates are the top-left in
/// screen space. Items are laid out top-to-bottom in insertion order.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub x: f32,
    pub y: f32,
    pub items: Vec<(String, MenuAction)>,
    /// Entries for the "Outline ▸" submenu. Empty → outline row is hidden.
    /// Each entry is (indented_label, action) — the action scrolls to that
    /// heading's doc-y.
    pub outline_items: Vec<(String, MenuAction)>,
    /// Entries for the "Layout ▸" submenu on mermaid diagrams. Empty →
    /// the layout row is hidden.
    pub mermaid_items: Vec<(String, MenuAction)>,
    /// Which submenu is currently open. Sticky — once set by hovering a
    /// trigger row, stays open while the cursor is inside either the
    /// trigger row or the submenu panel. Prevents two submenus with
    /// overlapping panel rects (outline + mermaid layout both anchor at
    /// `m.x + main_w - 2`) from fighting each other.
    pub active_submenu: Option<SubmenuKind>,
    /// Keyboard selection in the main menu — `Some(i)` means item `i`
    /// is highlighted and Enter activates it. `None` means the menu is
    /// purely mouse-driven (right-click flow). Set to `Some(0)` when
    /// the menu opens via Ctrl-tap or when ↑/↓ is pressed.
    pub selected: Option<usize>,
    /// Keyboard selection inside an open submenu. When `Some`, the
    /// submenu is drawn regardless of mouse position and Enter activates
    /// the chosen entry. ←/Esc returns to the parent menu.
    pub submenu_selected: Option<(SubmenuKind, usize)>,
}

/// The two kinds of submenu sharing the same layout machinery, distinguished
/// only by their trigger row and item source.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SubmenuKind {
    Outline,
    Mermaid,
}

/// In-document find bar. A small draggable overlay with a text input,
/// prev/next arrows, match counter and close button.
#[derive(Debug, Clone)]
pub struct SearchUi {
    pub query: String,
    /// Char-index of the insertion cursor.
    pub cursor: usize,
    /// Char-index of the selection anchor; equal to `cursor` when no
    /// selection is active.
    pub anchor: usize,
    /// 0-based; clamped into [0, match_count) externally.
    pub current: usize,
    /// Last-known match count (updated by the render / scroll path).
    pub match_count: usize,
    /// Panel top-left in screen coords.
    pub x: f32,
    pub y: f32,
    /// When Some, mouse delta from panel top-left at the moment the drag
    /// started. Kept across CursorMoved events.
    pub drag_grip: Option<(f32, f32)>,
}

/// Ctrl+P quick-open panel. Lists every markdown file under the tree root,
/// filtered live by `query`. Arrow keys move `selected`, Enter opens.
#[derive(Debug, Clone)]
pub struct QuickOpenUi {
    pub query: String,
    /// Char-index of the insertion cursor in `query`.
    pub cursor: usize,
    /// Char-index of the selection anchor; equal to `cursor` when no
    /// selection is active.
    pub anchor: usize,
    /// Files discovered so far. Grows over time as the background walker
    /// finds more — the panel re-renders on each batch.
    pub files: Vec<PathBuf>,
    /// Common prefix stripped when displaying entries (the tree root).
    pub base: PathBuf,
    /// Index into the filtered list.
    pub selected: usize,
    /// Scroll offset (rows) into the filtered list.
    pub scroll: usize,
    /// True while the background walker is still running.
    pub scanning: bool,
    /// Tripped to `true` when the panel closes; the walker checks this
    /// at every directory boundary and exits.
    pub cancel: Arc<AtomicBool>,
    /// Monotonic open-id. Lets a slow walker detect that the panel was
    /// closed and reopened (different generation), so it doesn't write
    /// stale results into the new panel's file list.
    pub generation: u64,
}

pub struct AppState {
    pub source: String,
    pub source_path: Option<PathBuf>,
    pub scroll: f32,
    pub viewport: Viewport,
    pub tree: Option<FileTree>,
    pub last_mouse: PhysicalPosition<f64>,
    /// Current sidebar width. 0 → hidden. The last non-zero value is cached
    /// in `sidebar_width_restore` so toggling brings it back.
    pub sidebar_width: f32,
    pub sidebar_width_restore: f32,
    /// True while the user is dragging the sidebar's right edge.
    pub sidebar_dragging: bool,
    /// Scroll offset inside the file tree (pixels from top). 0 → top.
    pub sidebar_scroll: f32,

    /// Selection anchor and head in *document* coordinates (x from the left
    /// of the window including sidebar, y in the unscrolled document).
    /// `None` on both means no selection.
    pub sel_anchor: Option<(f32, f32)>,
    pub sel_head: Option<(f32, f32)>,
    pub is_selecting: bool,

    /// Scrollbar drag state. While active, mouse-move remaps mouse_y → scroll
    /// using `scrollbar_grip` (offset between mouse_y and thumb top at press).
    pub scrollbar_dragging: bool,
    pub scrollbar_grip: f32,

    /// Same as above but for the sidebar's internal scrollbar.
    pub sidebar_scrollbar_dragging: bool,
    pub sidebar_scrollbar_grip: f32,

    /// Font-size multipliers for the two panels. 1.0 = default theme size.
    /// Ctrl + wheel over a panel bumps its zoom.
    pub content_zoom: f32,
    pub sidebar_zoom: f32,

    /// Maximum width (px) of the narrow text reading column. Code,
    /// tables, images and diagrams ignore this and span the full
    /// content area. Ctrl+Left / Ctrl+Right shrink / grow it.
    pub text_column_width: f32,
    /// Horizontal offset of the text column from the content area's
    /// left edge. Ctrl+Shift+Left / Ctrl+Shift+Right shift it.
    pub text_column_offset_x: f32,

    /// (time, x, y, path) of the most recent tree-folder click. Used to
    /// promote a second click on the same folder within the double-click
    /// window to a SetRoot (enter directory) action.
    pub last_folder_click: Option<(std::time::Instant, f32, f32, PathBuf)>,

    /// When true, Theme::dark() is used for rendering instead of light.
    pub dark: bool,

    /// Active right-click context menu. `None` when closed.
    pub context_menu: Option<ContextMenu>,

    /// Active in-document search overlay. `None` when closed.
    pub search: Option<SearchUi>,

    /// Active Ctrl+P quick-open panel. `None` when closed.
    pub quick_open: Option<QuickOpenUi>,
    /// Monotonic counter that increments every time a new quick-open
    /// panel opens. Each background walker captures the generation at
    /// spawn and checks it before writing results, so a slow walk for
    /// a previously-closed panel can't poison a freshly-opened one.
    pub quick_open_seq: u64,

    /// Per-diagram mermaid layout overrides, keyed by
    /// `(file path, mermaid block index within the document)`. The
    /// override is view-only — never written back to the source file.
    /// Cleared on file-swap is unnecessary: stale entries key on an old
    /// path and simply aren't consulted.
    pub mermaid_overrides: std::collections::HashMap<(Option<PathBuf>, usize), crate::mermaid::Direction>,

    /// Reading cursor — doc-y of the current line. None until the user
    /// drives the keyboard navigation (↑/↓ activate it). Drawn as a thin
    /// caret in the left margin; ←/→ jumps between section headings;
    /// Ctrl alone opens the context menu at the cursor's position.
    pub read_cursor: Option<f32>,
}

pub struct Shared {
    pub fonts: Fonts,
    pub state: Mutex<AppState>,
    pub images: Mutex<ImageCache>,
}

impl Shared {
    pub fn snapshot(&self) -> Snapshot {
        let s = self.state.lock().unwrap();
        let selection = match (s.sel_anchor, s.sel_head) {
            (Some(a), Some(h)) if a != h => Some((a, h)),
            _ => None,
        };
        let quiet = !s.is_selecting
            && !s.sidebar_dragging
            && !s.scrollbar_dragging
            && !s.sidebar_scrollbar_dragging;
        let hover_pos = if quiet {
            Some((s.last_mouse.x as f32, s.last_mouse.y as f32))
        } else {
            None
        };
        Snapshot {
            source: s.source.clone(),
            source_path: s.source_path.clone(),
            scroll: s.scroll,
            viewport: s.viewport,
            tree_flat: s.tree.as_ref().map(|t| t.flatten()),
            theme: if s.dark { Theme::dark() } else { Theme::light() },
            sidebar_width: s.sidebar_width,
            sidebar_scroll: s.sidebar_scroll,
            content_zoom: s.content_zoom,
            sidebar_zoom: s.sidebar_zoom,
            text_column_width: s.text_column_width,
            text_column_offset_x: s.text_column_offset_x,
            selection,
            hover_pos,
            context_menu: s.context_menu.clone(),
            search: s.search.clone(),
            quick_open: s.quick_open.clone(),
            mermaid_overrides: {
                let cur = s.source_path.clone();
                s.mermaid_overrides
                    .iter()
                    .filter_map(|((path, idx), dir)| {
                        if *path == cur { Some((*idx, *dir)) } else { None }
                    })
                    .collect()
            },
            read_cursor: s.read_cursor,
        }
    }
}

pub struct Snapshot {
    pub source: String,
    pub source_path: Option<PathBuf>,
    pub scroll: f32,
    pub viewport: Viewport,
    pub tree_flat: Option<Vec<crate::tree::TreeEntry>>,
    pub theme: Theme,
    pub sidebar_width: f32,
    pub sidebar_scroll: f32,
    pub content_zoom: f32,
    pub sidebar_zoom: f32,
    pub text_column_width: f32,
    pub text_column_offset_x: f32,
    pub selection: Option<((f32, f32), (f32, f32))>,
    /// Mouse position in screen coords, but only when the window is in a
    /// "quiet" state — not dragging, not selecting. Drawn hover highlights
    /// flicker if left on during active interaction.
    pub hover_pos: Option<(f32, f32)>,
    pub context_menu: Option<ContextMenu>,
    pub search: Option<SearchUi>,
    pub quick_open: Option<QuickOpenUi>,
    /// Mermaid-block-index → direction overrides for the current file only.
    /// Pre-filtered at snapshot time so the render path doesn't need to know
    /// the current file path.
    pub mermaid_overrides: std::collections::HashMap<usize, crate::mermaid::Direction>,
    pub read_cursor: Option<f32>,
}

struct App {
    shared: Arc<Shared>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    proxy: EventLoopProxy<UserEvent>,
    modifiers: Modifiers,
    /// When `Some`, modifier-helper methods read from this tuple instead
    /// of `self.modifiers`. Set during synthesised key events so the
    /// shortcut detection sees the modifier state the API caller asked
    /// for, regardless of physical keyboard state. Tuple is
    /// `(shift, ctrl, alt, meta)`.
    synth_mods: Option<(bool, bool, bool, bool)>,
    /// Cached last-set cursor so we don't spam `set_cursor` every mouse move.
    cursor: CursorIcon,
    /// Last hit-target rect under the pointer (screen coords for pinned,
    /// doc coords for content). Used to detect the need to repaint the
    /// hover highlight when crossing between adjacent clickable rects.
    last_hover_rect: Option<(f32, f32, f32, f32)>,
    /// Tracks "Ctrl pressed alone, no chord yet" — set true when Ctrl
    /// becomes the only modifier with no other keys, cleared the moment
    /// any other key is pressed. On Ctrl release while still armed, we
    /// open the context menu at the read cursor.
    ctrl_alone_armed: bool,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let (vw, vh) = {
            let s = self.shared.state.lock().unwrap();
            (s.viewport.width, s.viewport.height)
        };
        let attrs = Window::default_attributes()
            .with_title("mdrdr")
            .with_inner_size(LogicalSize::new(vw, vh));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let context = Context::new(window.clone()).unwrap();
        let surface = Surface::new(&context, window.clone()).unwrap();
        self.window = Some(window);
        self.surface = Some(surface);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(sz) => {
                {
                    let mut s = self.shared.state.lock().unwrap();
                    s.viewport = Viewport { width: sz.width.max(1), height: sz.height.max(1) };
                }
                self.clamp_scroll();
                self.request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Note: lines-based deltas differ from pixel deltas in unit;
                // we normalize to a scroll-pixel dy and a zoom-step count.
                let (dy, zoom_step) = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => (-lines * 40.0, lines),
                    MouseScrollDelta::PixelDelta(pos) => {
                        let dy = -pos.y as f32;
                        (dy, (pos.y as f32) / 40.0)
                    }
                };
                // Quick-open owns the wheel while open — scrolls the result list.
                let qo_open = self.shared.state.lock().unwrap().quick_open.is_some();
                if qo_open {
                    let step = (dy / 24.0).round() as i32;
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(qo) = &mut s.quick_open {
                        let ms_len = Self::quick_open_matches(qo).len();
                        let max_scroll = ms_len.saturating_sub(QUICK_OPEN_ROWS);
                        let new_scroll = (qo.scroll as i32 + step).clamp(0, max_scroll as i32) as usize;
                        qo.scroll = new_scroll;
                    }
                    drop(s);
                    self.request_redraw();
                    return;
                }
                let ctrl = self.shortcut_mod();
                let over_sidebar = {
                    let s = self.shared.state.lock().unwrap();
                    s.tree.is_some()
                        && s.sidebar_width > 0.0
                        && (s.last_mouse.x as f32) < s.sidebar_width
                };
                if ctrl {
                    // Ctrl+wheel: zoom the panel under the cursor.
                    let factor = (1.0 + zoom_step * 0.1).clamp(0.5, 2.0);
                    let mut s = self.shared.state.lock().unwrap();
                    if over_sidebar {
                        s.sidebar_zoom = (s.sidebar_zoom * factor).clamp(0.5, 3.0);
                    } else {
                        s.content_zoom = (s.content_zoom * factor).clamp(0.5, 3.0);
                    }
                    drop(s);
                    self.clamp_scroll();
                    self.clamp_sidebar_scroll();
                } else if over_sidebar {
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sidebar_scroll = (s.sidebar_scroll + dy).max(0.0);
                    }
                    self.clamp_sidebar_scroll();
                } else {
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.scroll = (s.scroll + dy).max(0.0);
                    }
                    self.clamp_scroll();
                }
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                let (dragging_sidebar, dragging_scrollbar, dragging_sb_sidebar, selecting, scroll, grip, sb_sidebar_grip, sidebar_w, tree_visible, viewport, menu_open) = {
                    let mut s = self.shared.state.lock().unwrap();
                    s.last_mouse = position;
                    (
                        s.sidebar_dragging,
                        s.scrollbar_dragging,
                        s.sidebar_scrollbar_dragging,
                        s.is_selecting,
                        s.scroll,
                        s.scrollbar_grip,
                        s.sidebar_scrollbar_grip,
                        s.sidebar_width,
                        s.tree.is_some(),
                        s.viewport,
                        s.context_menu.is_some(),
                    )
                };
                if menu_open {
                    // Repaint so the hovered-row tint follows the cursor.
                    // Also update the sticky active submenu so the correct
                    // panel renders when the cursor enters a trigger or
                    // stays inside a panel.
                    let (mx, my) = (position.x as f32, position.y as f32);
                    let fonts = &self.shared.fonts;
                    let new_active = {
                        let s = self.shared.state.lock().unwrap();
                        s.context_menu.as_ref().and_then(|m| active_submenu(m, mx, my, fonts))
                    };
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(m) = s.context_menu.as_mut() {
                        m.active_submenu = new_active;
                        // The user is steering with the mouse now — hand
                        // control back to hover so a stale keyboard
                        // selection doesn't outshine the cursor.
                        m.selected = None;
                        m.submenu_selected = None;
                    }
                    drop(s);
                    self.request_redraw();
                }
                if self.shared.state.lock().unwrap().quick_open.is_some() {
                    // Hover highlight in the quick-open list tracks the cursor.
                    self.request_redraw();
                }
                // Search panel drag: move the overlay with the cursor
                // while drag_grip is set, clamping inside the viewport.
                {
                    let mut s = self.shared.state.lock().unwrap();
                    let vw = s.viewport.width as f32;
                    let vh = s.viewport.height as f32;
                    let mut moved = false;
                    if let Some(su) = s.search.as_mut() {
                        if let Some((gx, gy)) = su.drag_grip {
                            su.x = (position.x as f32 - gx).clamp(0.0, (vw - SEARCH_PANEL_W).max(0.0));
                            su.y = (position.y as f32 - gy).clamp(0.0, (vh - SEARCH_PANEL_H).max(0.0));
                            moved = true;
                        }
                    }
                    drop(s);
                    if moved { self.request_redraw(); }
                }
                // Repaint when hovering the search buttons so their tint
                // tracks the cursor.
                if self.shared.state.lock().unwrap().search.is_some() {
                    self.request_redraw();
                }
                let cursor_changed = self.update_cursor(
                    position.x as f32,
                    position.y as f32,
                    dragging_sidebar,
                    dragging_scrollbar || dragging_sb_sidebar,
                    selecting,
                    sidebar_w,
                    tree_visible,
                    viewport,
                );
                if cursor_changed
                    && !dragging_scrollbar
                    && !dragging_sb_sidebar
                    && !dragging_sidebar
                    && !selecting
                {
                    // Hover highlight follows the cursor icon; repaint when
                    // the clickable/non-clickable boundary crosses.
                    self.request_redraw();
                }
                if dragging_scrollbar {
                    if let Some(g) = self.current_scrollbar_geom() {
                        let vh = g.track_h;
                        let track_avail = (vh - g.thumb_h).max(1.0);
                        let new_thumb_top =
                            (position.y as f32 - grip).clamp(0.0, vh - g.thumb_h);
                        let frac = new_thumb_top / track_avail;
                        let new_scroll = frac * g.max_scroll;
                        {
                            let mut s = self.shared.state.lock().unwrap();
                            s.scroll = new_scroll;
                        }
                        self.clamp_scroll();
                        self.request_redraw();
                    }
                } else if dragging_sb_sidebar {
                    if let Some(g) = self.current_sidebar_scrollbar_geom() {
                        let vh = g.track_h;
                        let track_avail = (vh - g.thumb_h).max(1.0);
                        let new_thumb_top =
                            (position.y as f32 - sb_sidebar_grip).clamp(0.0, vh - g.thumb_h);
                        let frac = new_thumb_top / track_avail;
                        let new_scroll = frac * g.max_scroll;
                        {
                            let mut s = self.shared.state.lock().unwrap();
                            s.sidebar_scroll = new_scroll;
                        }
                        self.clamp_sidebar_scroll();
                        self.request_redraw();
                    }
                } else if dragging_sidebar {
                    let new_w = (position.x as f32).clamp(120.0, 640.0);
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sidebar_width = new_w;
                        s.sidebar_width_restore = new_w;
                    }
                    self.request_redraw();
                } else if selecting {
                    let doc = (position.x as f32, position.y as f32 + scroll);
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sel_head = Some(doc);
                    }
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.ctrl_alone_armed = false;
                let (pos, sidebar_w, tree_visible, scroll, viewport, menu) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.last_mouse, s.sidebar_width, s.tree.is_some(), s.scroll, s.viewport, s.context_menu.clone())
                };
                let x = pos.x as f32;
                let y = pos.y as f32;

                // 0-. Quick-open — fully modal. Click on a row opens that
                //     file; click outside the panel closes.
                let quick_open = self.shared.state.lock().unwrap().quick_open.clone();
                if let Some(qo) = quick_open {
                    let g = quick_open_geom(viewport);
                    if !point_in(g.panel, x, y) {
                        self.close_quick_open();
                        self.request_redraw();
                        return;
                    }
                    if let Some(row) = quick_open_row_hit(&g, x, y) {
                        let matches = Self::quick_open_matches(&qo);
                        let abs = qo.scroll + row;
                        if let Some(&m_idx) = matches.get(abs) {
                            if let Some(p) = qo.files.get(m_idx).cloned() {
                                self.close_quick_open();
                                self.open_path(&p, event_loop);
                                self.request_redraw();
                                return;
                            }
                        }
                    }
                    // Click elsewhere on the panel (input, status) → absorbed.
                    return;
                }

                // 0a. Search overlay — if open and hit, handle buttons / drag
                //     and short-circuit before anything else.
                let search_ui = self.shared.state.lock().unwrap().search.clone();
                if let Some(su) = search_ui {
                    let hit = search_hit_test(&su, x, y);
                    match hit {
                        SearchHit::Close => {
                            self.close_search();
                            self.request_redraw();
                            return;
                        }
                        SearchHit::Next => {
                            self.step_search(false);
                            self.request_redraw();
                            return;
                        }
                        SearchHit::Prev => {
                            self.step_search(true);
                            self.request_redraw();
                            return;
                        }
                        SearchHit::Drag => {
                            let mut s = self.shared.state.lock().unwrap();
                            if let Some(s2) = &mut s.search {
                                s2.drag_grip = Some((x - s2.x, y - s2.y));
                            }
                            return;
                        }
                        SearchHit::Input | SearchHit::Panel => {
                            // Clicks inside the panel are absorbed — the
                            // panel itself doesn't take any focus action
                            // beyond "don't fall through".
                            return;
                        }
                        SearchHit::Outside => {
                            // Let other handlers run; search stays open.
                        }
                    }
                }

                // 0. Context menu — if one is open, it captures this click.
                //    Hit inside → execute item. Hit outside → just close.
                //    Clicking the "Outline ▸" row leaves the menu open so
                //    the submenu stays reachable.
                if let Some(m) = menu.as_ref() {
                    let hit = menu_item_hit(m, x, y, &self.shared.fonts);
                    match hit {
                        Some(MenuAction::Outline) | Some(MenuAction::MermaidMenu) => {
                            // keep menu open — these rows only exist to host
                            // their submenu and don't execute on click.
                            self.request_redraw();
                        }
                        Some(action) => {
                            {
                                let mut s = self.shared.state.lock().unwrap();
                                s.context_menu = None;
                            }
                            apply_menu_action(&self.shared, &self.proxy, &action);
                            self.request_redraw();
                        }
                        None => {
                            let mut s = self.shared.state.lock().unwrap();
                            s.context_menu = None;
                            drop(s);
                            self.request_redraw();
                        }
                    }
                    return;
                }

                // 1. Scrollbar (highest priority — it sits on top of content).
                if let Some(g) = self.current_scrollbar_geom() {
                    if in_scrollbar_strip(&g, viewport, x, y) {
                        if y >= g.thumb_y && y < g.thumb_y + g.thumb_h {
                            // Grab the thumb.
                            let mut s = self.shared.state.lock().unwrap();
                            s.scrollbar_dragging = true;
                            s.scrollbar_grip = y - g.thumb_y;
                        } else {
                            // Page above / below.
                            let page = g.track_h * 0.9;
                            let mut s = self.shared.state.lock().unwrap();
                            if y < g.thumb_y {
                                s.scroll = (s.scroll - page).max(0.0);
                            } else {
                                s.scroll = (s.scroll + page).min(g.max_scroll);
                            }
                        }
                        self.clamp_scroll();
                        self.request_redraw();
                        return;
                    }
                }

                // 2. Sidebar's right-edge drag strip (resize sidebar width).
                //    Claim this before the scrollbar strip — the resize
                //    hit region is only 6px around the edge, and would
                //    otherwise be swallowed by the wider scrollbar strip.
                if tree_visible && sidebar_w > 0.0 && (x - sidebar_w).abs() <= 6.0 {
                    let mut s = self.shared.state.lock().unwrap();
                    s.sidebar_dragging = true;
                    self.request_redraw();
                    return;
                }

                // 3. Sidebar's internal scrollbar strip (intercept before
                //    anything in the sidebar so it doesn't click-through
                //    to a tree row underneath).
                if tree_visible {
                    if let Some(g) = self.current_sidebar_scrollbar_geom() {
                        if in_sidebar_scrollbar_strip(&g, sidebar_w, x, y) {
                            if y >= g.thumb_y && y < g.thumb_y + g.thumb_h {
                                let mut s = self.shared.state.lock().unwrap();
                                s.sidebar_scrollbar_dragging = true;
                                s.sidebar_scrollbar_grip = y - g.thumb_y;
                            } else {
                                let page = g.track_h * 0.9;
                                let mut s = self.shared.state.lock().unwrap();
                                if y < g.thumb_y {
                                    s.sidebar_scroll = (s.sidebar_scroll - page).max(0.0);
                                } else {
                                    s.sidebar_scroll = (s.sidebar_scroll + page).min(g.max_scroll);
                                }
                            }
                            self.clamp_sidebar_scroll();
                            self.request_redraw();
                            return;
                        }
                    }
                }

                if x < sidebar_w {
                    // 3. Inside sidebar — tree click.
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sel_anchor = None;
                        s.sel_head = None;
                    }
                    click_at(&self.shared, x, y);
                } else {
                    // 4. Content area — start a text selection. A click (no
                    // drag) will be caught on release and dispatched to
                    // click_at so links still fire.
                    let doc = (x, y + scroll);
                    let mut s = self.shared.state.lock().unwrap();
                    s.sel_anchor = Some(doc);
                    s.sel_head = Some(doc);
                    s.is_selecting = true;
                }
                self.clamp_scroll();
                self.request_redraw();
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                self.ctrl_alone_armed = false;
                // Open (or reposition) the context menu at the cursor. The
                // menu is small and position-clamped to stay on-screen.
                let (pos, viewport, dark, active_path, scroll, sidebar_w) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.last_mouse, s.viewport, s.dark, s.source_path.clone(), s.scroll, s.sidebar_width)
                };
                let px = pos.x as f32;
                let py = pos.y as f32;
                // Contextual path for "Copy path": if the cursor is over a
                // tree row, copy that path; otherwise copy the currently
                // open document's path (if any).
                let copy_path = {
                    let (pinned, _content) = self.current_hit_targets();
                    if let Some(hit) = crate::render::hit_test(&pinned, px, py) {
                        match &hit.action {
                            HitAction::Open(p)
                            | HitAction::Toggle(p)
                            | HitAction::SetRoot(p) => Some(p.clone()),
                            _ => active_path.clone(),
                        }
                    } else {
                        active_path.clone()
                    }
                };
                // Copy zones (code blocks / tables) — only in the content
                // area, not over the sidebar.
                let zones = if px >= sidebar_w { self.current_copy_zones() } else { Vec::new() };
                let doc_y = py + scroll;
                let zone_hit = zones
                    .iter()
                    .find(|z| px >= z.x && px < z.x + z.w && doc_y >= z.y && doc_y < z.y + z.h)
                    .cloned();
                let copy_selection = self.current_selection_text();
                let outline_items = outline_to_menu_items(&self.current_outline());
                let mermaid_items = zone_hit
                    .as_ref()
                    .and_then(|z| z.mermaid_block.map(mermaid_layout_items))
                    .unwrap_or_default();
                let items = build_context_menu_items(
                    dark,
                    copy_path,
                    !outline_items.is_empty(),
                    copy_selection,
                    zone_hit.as_ref(),
                );
                let menu_w = context_menu_width(&items, &self.shared.fonts);
                let menu_h = context_menu_height(&items);
                let mut mx = pos.x as f32;
                let mut my = pos.y as f32;
                mx = mx.min(viewport.width as f32 - menu_w - 4.0).max(4.0);
                my = my.min(viewport.height as f32 - menu_h - 4.0).max(4.0);
                {
                    let mut s = self.shared.state.lock().unwrap();
                    // Cancel an in-progress drag so the menu doesn't open mid-
                    // rubber-band, but keep a completed selection alive so the
                    // user can right-click → "Copy text".
                    s.is_selecting = false;
                    s.context_menu = Some(ContextMenu {
                        x: mx, y: my, items, outline_items, mermaid_items,
                        active_submenu: None,
                        selected: None,
                        submenu_selected: None,
                    });
                }
                self.request_redraw();
            }

            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Decide whether the press→release pair was a click (no drag)
                // that should dispatch to click_at (for link handling).
                let (click_candidate, last_mouse) = {
                    let mut s = self.shared.state.lock().unwrap();
                    let was_selecting = s.is_selecting;
                    let anchor = s.sel_anchor;
                    let head = s.sel_head;
                    s.sidebar_dragging = false;
                    s.is_selecting = false;
                    s.scrollbar_dragging = false;
                    s.sidebar_scrollbar_dragging = false;
                    if let Some(su) = s.search.as_mut() {
                        su.drag_grip = None;
                    }
                    let click = match (was_selecting, anchor, head) {
                        (true, Some(a), Some(h)) => {
                            let dx = (a.0 - h.0).abs();
                            let dy = (a.1 - h.1).abs();
                            if dx < 3.0 && dy < 3.0 {
                                // Degenerate selection → treat as a click.
                                s.sel_anchor = None;
                                s.sel_head = None;
                                true
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };
                    (click, s.last_mouse)
                };
                if click_candidate {
                    let cx = last_mouse.x as f32;
                    let cy = last_mouse.y as f32;
                    click_at(&self.shared, cx, cy);
                    // Move the read cursor to the clicked row when the
                    // click lands inside the content area. Snap to the
                    // nearest baseline so the marker aligns with text.
                    let (sidebar_w, scroll) = {
                        let s = self.shared.state.lock().unwrap();
                        (s.sidebar_width, s.scroll)
                    };
                    if cx >= sidebar_w {
                        let target = cy + scroll;
                        let baselines = self.current_baselines();
                        if !baselines.is_empty() {
                            let snap_y = *baselines
                                .iter()
                                .min_by(|a, b| {
                                    (**a - target)
                                        .abs()
                                        .partial_cmp(&(**b - target).abs())
                                        .unwrap()
                                })
                                .unwrap();
                            self.shared.state.lock().unwrap().read_cursor = Some(snap_y);
                        }
                    }
                    self.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                let old = self.modifiers.state();
                let new = mods.state();
                let old_ctrl = old.control_key() || old.super_key();
                let new_ctrl = new.control_key() || new.super_key();
                let other_new = new.shift_key() || new.alt_key();
                if !old_ctrl && new_ctrl && !other_new {
                    self.ctrl_alone_armed = true;
                } else if old_ctrl && !new_ctrl && self.ctrl_alone_armed {
                    self.ctrl_alone_armed = false;
                    self.modifiers = mods;
                    let menu_was_open = {
                        let mut s = self.shared.state.lock().unwrap();
                        if s.context_menu.is_some() {
                            s.context_menu = None;
                            true
                        } else {
                            false
                        }
                    };
                    if menu_was_open {
                        self.request_redraw();
                    } else if self.shared.state.lock().unwrap().read_cursor.is_some() {
                        self.open_context_menu_at_cursor();
                        self.request_redraw();
                    }
                    return;
                } else {
                    self.ctrl_alone_armed = false;
                }
                self.modifiers = mods;
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                // Any non-Control key press while Ctrl is held turns the
                // tap into a chord — disarm so Ctrl release doesn't open
                // the context menu after e.g. Ctrl+C.
                if !matches!(logical_key.as_ref(), Key::Named(NamedKey::Control)
                    | Key::Named(NamedKey::Super)) {
                    self.ctrl_alone_armed = false;
                }
                // Context menu — modal for keyboard while open. ↑/↓ moves
                // the selection (wraps); → opens a submenu trigger; ←
                // returns from a submenu; Enter activates the highlighted
                // item; Esc closes (handled below in the main key match).
                if self.shared.state.lock().unwrap().context_menu.is_some() {
                    match logical_key.as_ref() {
                        Key::Named(NamedKey::ArrowDown) => {
                            self.menu_move(1);
                            self.request_redraw();
                            return;
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            self.menu_move(-1);
                            self.request_redraw();
                            return;
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            self.menu_open_submenu();
                            self.request_redraw();
                            return;
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            self.menu_close_submenu();
                            self.request_redraw();
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.menu_activate();
                            self.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }
                // Quick-open overlay is modal for keyboard when open.
                let quick_open_active = self.shared.state.lock().unwrap().quick_open.is_some();
                if quick_open_active {
                    self.handle_quick_open_key(&logical_key, event_loop);
                    self.request_redraw();
                    return;
                }
                // Search overlay is modal for keyboard — when it's open,
                // Enter / Shift+Enter cycles matches, Esc closes, other
                // keys go into the query.
                let search_open = self.shared.state.lock().unwrap().search.is_some();
                if search_open {
                    self.handle_search_key(&logical_key);
                    self.request_redraw();
                    return;
                }
                // Ctrl+F / Cmd+F opens the search overlay.
                // Ctrl+P / Cmd+P opens the quick-open panel.
                if self.shortcut_mod() {
                    if matches!(logical_key.as_ref(), Key::Character(c) if c == "f") {
                        self.open_search();
                        self.request_redraw();
                        return;
                    }
                    if matches!(logical_key.as_ref(), Key::Character(c) if c == "p") {
                        self.open_quick_open();
                        self.request_redraw();
                        return;
                    }
                    // Ctrl + Left / Right         — shrink / grow text column.
                    // Ctrl + Shift + Left / Right — slide its left edge.
                    let arrow_dir = match logical_key.as_ref() {
                        Key::Named(NamedKey::ArrowLeft) => Some(-1i32),
                        Key::Named(NamedKey::ArrowRight) => Some(1i32),
                        _ => None,
                    };
                    if let Some(dir) = arrow_dir {
                        if self.adjust_text_column(dir, self.shift_mod()) {
                            self.request_redraw();
                            return;
                        }
                    }
                }
                let (vh, source, vw, base_dir) = {
                    let s = self.shared.state.lock().unwrap();
                    (
                        s.viewport.height as f32,
                        s.source.clone(),
                        s.viewport.width,
                        s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                    )
                };
                let page = (vh * 0.85).max(60.0);
                // Plain arrows drive the read cursor (line stepping for
                // ↑/↓, section jumping for ←/→). Returning early here
                // bypasses the scroll-by-dy fallback below.
                if !self.shortcut_mod() && !self.shift_mod() && !self.alt_mod() {
                    let cursor_dir = match logical_key.as_ref() {
                        Key::Named(NamedKey::ArrowDown) => Some((true, 1)),
                        Key::Named(NamedKey::ArrowUp) => Some((true, -1)),
                        Key::Named(NamedKey::ArrowRight) => Some((false, 1)),
                        Key::Named(NamedKey::ArrowLeft) => Some((false, -1)),
                        _ => None,
                    };
                    if let Some((line, dir)) = cursor_dir {
                        if line {
                            self.move_read_cursor(dir);
                        } else {
                            self.jump_section(dir);
                        }
                        self.request_redraw();
                        return;
                    }
                }
                let dy: Option<f32> = match logical_key.as_ref() {
                    Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => Some(page),
                    Key::Named(NamedKey::PageUp) => Some(-page),
                    Key::Named(NamedKey::ArrowDown) => Some(60.0),
                    Key::Named(NamedKey::ArrowUp) => Some(-60.0),
                    Key::Named(NamedKey::Home) => {
                        let mut s = self.shared.state.lock().unwrap();
                        s.scroll = 0.0;
                        None
                    }
                    Key::Named(NamedKey::End) => {
                        let theme = Theme::light();
                        let (sidebar_w, content_zoom, tcw, tcox) = {
                            let s = self.shared.state.lock().unwrap();
                            (s.sidebar_width, s.content_zoom, s.text_column_width, s.text_column_offset_x)
                        };
                        let mut images = self.shared.images.lock().unwrap();
                        let doc_h = measure(
                            &source,
                            vw,
                            vh as u32,
                            base_dir.as_deref(),
                            sidebar_w,
                            content_zoom,
                            tcw,
                            tcox,
                            &theme,
                            &self.shared.fonts,
                            &mut images,
                        );
                        drop(images);
                        let mut s = self.shared.state.lock().unwrap();
                        s.scroll = (doc_h - s.viewport.height as f32).max(0.0);
                        None
                    }
                    Key::Named(NamedKey::Escape) => {
                        let had_menu = {
                            let mut s = self.shared.state.lock().unwrap();
                            let had = s.context_menu.is_some();
                            s.context_menu = None;
                            had
                        };
                        if had_menu {
                            self.request_redraw();
                            return;
                        }
                        event_loop.exit();
                        return;
                    }
                    Key::Character(c) if c == "b" && !self.shortcut_mod() => {
                        let mut s = self.shared.state.lock().unwrap();
                        if s.sidebar_width > 0.0 {
                            s.sidebar_width_restore = s.sidebar_width;
                            s.sidebar_width = 0.0;
                        } else {
                            s.sidebar_width = if s.sidebar_width_restore > 0.0 {
                                s.sidebar_width_restore
                            } else {
                                260.0
                            };
                        }
                        None
                    }
                    Key::Character(c) if c == "c" && self.shortcut_mod() => {
                        self.copy_selection();
                        None
                    }
                    Key::Character(c) if c == "t" && !self.shortcut_mod() => {
                        let mut s = self.shared.state.lock().unwrap();
                        s.dark = !s.dark;
                        None
                    }
                    _ => None,
                };
                if let Some(d) = dy {
                    let mut s = self.shared.state.lock().unwrap();
                    s.scroll = (s.scroll + d).max(0.0);
                }
                self.clamp_scroll();
                self.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                self.draw();
            }

            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Redraw => self.request_redraw(),
            UserEvent::Quit => event_loop.exit(),
            UserEvent::SynthKey { kind, shift, ctrl, alt } => {
                self.synth_mods = Some((shift, ctrl, alt, false));
                let key = match kind {
                    SynthKey::Char(s) => Key::Character(winit::keyboard::SmolStr::new(s)),
                    SynthKey::Named(n) => Key::Named(n),
                };
                let qo_active = self.shared.state.lock().unwrap().quick_open.is_some();
                if qo_active {
                    self.handle_quick_open_key(&key, event_loop);
                } else {
                    let search_active = self.shared.state.lock().unwrap().search.is_some();
                    if search_active {
                        self.handle_search_key(&key);
                    } else if ctrl {
                        // Main-view Ctrl+Arrow / Ctrl+Shift+Arrow column
                        // resize. Only handled here when no modal owns
                        // the keyboard.
                        let dir = match &key {
                            Key::Named(NamedKey::ArrowLeft) => Some(-1i32),
                            Key::Named(NamedKey::ArrowRight) => Some(1i32),
                            _ => None,
                        };
                        if let Some(d) = dir {
                            self.adjust_text_column(d, shift);
                        }
                    }
                }
                self.synth_mods = None;
                self.request_redraw();
            }
            UserEvent::OpenSearch => {
                self.open_search();
                self.request_redraw();
            }
            UserEvent::CloseSearch => {
                self.close_search();
                self.request_redraw();
            }
            UserEvent::OpenQuickOpen => {
                self.open_quick_open();
                self.request_redraw();
            }
            UserEvent::CloseQuickOpen => {
                self.close_quick_open();
                self.request_redraw();
            }
        }
    }
}

impl App {
    /// True when the "app-shortcut" modifier is down. On Linux / Windows
    /// that's Ctrl; on macOS it's Cmd (winit reports Cmd as super_key).
    /// Accepting either everywhere keeps Ctrl-F / Ctrl-C muscle memory
    /// working on any platform.
    fn shortcut_mod(&self) -> bool {
        if let Some((_, ctrl, _, meta)) = self.synth_mods {
            return ctrl || meta;
        }
        let m = self.modifiers.state();
        m.control_key() || m.super_key()
    }

    fn shift_mod(&self) -> bool {
        if let Some((shift, _, _, _)) = self.synth_mods {
            return shift;
        }
        self.modifiers.state().shift_key()
    }

    /// Apply a Ctrl+Arrow / Ctrl+Shift+Arrow column-resize step. Returns
    /// `true` if state actually changed (used to gate the redraw and the
    /// `return` in the keyboard handler).
    fn adjust_text_column(&self, dir: i32, slide: bool) -> bool {
        const STEP: f32 = 48.0;
        const MIN_WIDTH: f32 = 320.0;
        let mut s = self.shared.state.lock().unwrap();
        let vw = s.viewport.width as f32;
        let margin_x = Theme::light().margin_x;
        let max_content = (vw - s.sidebar_width - margin_x * 2.0).max(MIN_WIDTH);
        if slide {
            let max_off = (max_content - s.text_column_width.min(max_content)).max(0.0);
            let new_off =
                (s.text_column_offset_x + dir as f32 * STEP).clamp(0.0, max_off);
            if (new_off - s.text_column_offset_x).abs() < f32::EPSILON {
                return false;
            }
            s.text_column_offset_x = new_off;
        } else {
            let new_w = (s.text_column_width + dir as f32 * STEP)
                .clamp(MIN_WIDTH, max_content);
            if (new_w - s.text_column_width).abs() < f32::EPSILON {
                return false;
            }
            s.text_column_width = new_w;
            // Re-clamp offset against the new width so shrinking doesn't
            // leave the column dangling beyond the right edge.
            let max_off = (max_content - s.text_column_width).max(0.0);
            if s.text_column_offset_x > max_off {
                s.text_column_offset_x = max_off;
            }
        }
        true
    }

    fn alt_mod(&self) -> bool {
        if let Some((_, _, alt, _)) = self.synth_mods {
            return alt;
        }
        self.modifiers.state().alt_key()
    }

    /// Open the search overlay at the current mouse position (clamped
    /// inside the viewport). Leaves the existing overlay alone if already
    /// open — the user may have dragged it somewhere specific.
    fn open_search(&self) {
        let mut s = self.shared.state.lock().unwrap();
        if s.search.is_none() {
            let vw = s.viewport.width as f32;
            let vh = s.viewport.height as f32;
            let mx = s.last_mouse.x as f32;
            let my = s.last_mouse.y as f32;
            // Anchor the panel just right-and-down of the cursor; nudge
            // back on-screen if we're near an edge.
            let x = (mx - SEARCH_DRAG_W * 0.5).clamp(0.0, (vw - SEARCH_PANEL_W).max(0.0));
            let y = (my - SEARCH_PANEL_H * 0.5).clamp(0.0, (vh - SEARCH_PANEL_H).max(0.0));
            s.search = Some(SearchUi {
                query: String::new(),
                cursor: 0,
                anchor: 0,
                current: 0,
                match_count: 0,
                x,
                y,
                drag_grip: None,
            });
        }
    }

    fn close_search(&self) {
        let mut s = self.shared.state.lock().unwrap();
        s.search = None;
    }

    /// Interpret one keypress while the search overlay owns the keyboard.
    fn handle_search_key(&self, key: &winit::keyboard::Key) {
        use winit::keyboard::Key;
        let select = self.shift_mod();
        let word = self.alt_mod() || self.shortcut_mod();
        // Cmd / Ctrl shortcut keys take priority over plain character input
        // so e.g. typing 'c' with Ctrl held doesn't put 'c' in the query.
        if self.shortcut_mod() {
            if let Key::Character(c) = key.as_ref() {
                match c {
                    "c" => {
                        let text = {
                            let s = self.shared.state.lock().unwrap();
                            s.search.as_ref().map(|su| {
                                if crate::text_input::has_selection(su.cursor, su.anchor) {
                                    crate::text_input::selected_text(&su.query, su.cursor, su.anchor)
                                } else {
                                    su.query.clone()
                                }
                            })
                        };
                        if let Some(t) = text {
                            if !t.is_empty() { crate::clipboard::copy(&t); }
                        }
                        return;
                    }
                    "x" => {
                        let mut changed = false;
                        let to_copy = {
                            let mut s = self.shared.state.lock().unwrap();
                            s.search.as_mut().and_then(|su| {
                                if crate::text_input::has_selection(su.cursor, su.anchor) {
                                    let t = crate::text_input::selected_text(&su.query, su.cursor, su.anchor);
                                    crate::text_input::delete_selection(&mut su.query, &mut su.cursor, &mut su.anchor);
                                    su.current = 0;
                                    changed = true;
                                    Some(t)
                                } else if !su.query.is_empty() {
                                    let t = std::mem::take(&mut su.query);
                                    su.cursor = 0;
                                    su.anchor = 0;
                                    su.current = 0;
                                    changed = true;
                                    Some(t)
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(t) = to_copy {
                            if !t.is_empty() { crate::clipboard::copy(&t); }
                        }
                        if changed { self.after_search_query_change(); }
                        return;
                    }
                    "v" => {
                        if let Some(text) = crate::clipboard::paste() {
                            let cleaned = sanitise_clipboard_for_input(&text);
                            if !cleaned.is_empty() {
                                {
                                    let mut s = self.shared.state.lock().unwrap();
                                    if let Some(su) = &mut s.search {
                                        crate::text_input::insert_str(&mut su.query, &mut su.cursor, &mut su.anchor, &cleaned);
                                        su.current = 0;
                                    }
                                }
                                self.after_search_query_change();
                            }
                        }
                        return;
                    }
                    "a" => {
                        let mut s = self.shared.state.lock().unwrap();
                        if let Some(su) = &mut s.search {
                            crate::text_input::select_all(&su.query, &mut su.cursor, &mut su.anchor);
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }
        match key.as_ref() {
            Key::Named(NamedKey::Escape) => self.close_search(),
            Key::Named(NamedKey::Tab) => {
                let backward = self.shift_mod();
                self.step_search(backward);
            }
            Key::Named(NamedKey::Enter) => {
                let backward = self.shift_mod();
                self.step_search(backward);
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(su) = &mut s.search {
                    if word {
                        crate::text_input::move_word_left(&su.query, &mut su.cursor, &mut su.anchor, select);
                    } else {
                        crate::text_input::move_left(&su.query, &mut su.cursor, &mut su.anchor, select);
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(su) = &mut s.search {
                    if word {
                        crate::text_input::move_word_right(&su.query, &mut su.cursor, &mut su.anchor, select);
                    } else {
                        crate::text_input::move_right(&su.query, &mut su.cursor, &mut su.anchor, select);
                    }
                }
            }
            Key::Named(NamedKey::Home) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(su) = &mut s.search {
                    crate::text_input::move_home(&mut su.cursor, &mut su.anchor, select);
                }
            }
            Key::Named(NamedKey::End) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(su) = &mut s.search {
                    crate::text_input::move_end(&su.query, &mut su.cursor, &mut su.anchor, select);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                {
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(su) = &mut s.search {
                        crate::text_input::backspace(&mut su.query, &mut su.cursor, &mut su.anchor);
                        su.current = 0;
                    }
                }
                self.after_search_query_change();
            }
            Key::Named(NamedKey::Delete) => {
                {
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(su) = &mut s.search {
                        crate::text_input::delete_forward(&mut su.query, &mut su.cursor, &mut su.anchor);
                        su.current = 0;
                    }
                }
                self.after_search_query_change();
            }
            Key::Named(NamedKey::Space) => {
                // winit reports Space as a Named key, not Character(" "),
                // so it has its own arm.
                {
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(su) = &mut s.search {
                        crate::text_input::insert_str(&mut su.query, &mut su.cursor, &mut su.anchor, " ");
                        su.current = 0;
                    }
                }
                self.after_search_query_change();
            }
            Key::Character(txt) => {
                // Skip control chars. Insert at cursor (replacing selection).
                let cleaned: String = txt.chars().filter(|c| !c.is_control()).collect();
                if cleaned.is_empty() { return; }
                {
                    let mut s = self.shared.state.lock().unwrap();
                    if let Some(su) = &mut s.search {
                        crate::text_input::insert_str(&mut su.query, &mut su.cursor, &mut su.anchor, &cleaned);
                        su.current = 0;
                    }
                }
                self.after_search_query_change();
            }
            _ => {}
        }
    }

    /// Recompute match count after the query changes and scroll to the
    /// first match if any.
    fn after_search_query_change(&self) {
        let matches = self.compute_current_matches();
        {
            let mut s = self.shared.state.lock().unwrap();
            if let Some(su) = &mut s.search {
                su.match_count = matches.len();
                if su.current >= su.match_count {
                    su.current = 0;
                }
            }
        }
        if !matches.is_empty() {
            self.scroll_to_match(matches[0].doc_y);
        }
    }

    /// Ctrl+G / Enter / click-next advances; Shift-Enter / Shift+click goes
    /// back. Wraps around.
    fn step_search(&self, backward: bool) {
        let matches = self.compute_current_matches();
        if matches.is_empty() {
            return;
        }
        let idx = {
            let mut s = self.shared.state.lock().unwrap();
            let Some(su) = &mut s.search else { return };
            su.match_count = matches.len();
            if backward {
                su.current = if su.current == 0 { matches.len() - 1 } else { su.current - 1 };
            } else {
                su.current = (su.current + 1) % matches.len();
            }
            su.current
        };
        self.scroll_to_match(matches[idx].doc_y);
    }

    fn compute_current_matches(&self) -> Vec<crate::render::ContentMatch> {
        let snap = self.shared.snapshot();
        let query = snap.search.as_ref().map(|s| s.query.clone()).unwrap_or_default();
        if query.is_empty() {
            return Vec::new();
        }
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        let blocks = crate::md::parse(&snap.source);
        let lay = crate::layout::layout(
            crate::layout::LayoutInput {
                blocks: &blocks,
                tree: snap.tree_flat.as_deref(),
                active_path: snap.source_path.as_deref(),
                base_dir: base_dir.as_deref(),
                viewport_w: snap.viewport.width,
                viewport_h: snap.viewport.height,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
                sidebar_width: snap.sidebar_width,
                sidebar_scroll: snap.sidebar_scroll,
                content_zoom: snap.content_zoom,
                sidebar_zoom: snap.sidebar_zoom,
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        );
        drop(images);
        crate::render::find_content_matches(&lay.content_items, &query, &self.shared.fonts)
    }

    /// Centre the current match in the viewport, clamped to doc bounds.
    fn scroll_to_match(&self, doc_y: f32) {
        let theme = Theme::light();
        let (source, vw, vh, base_dir, sidebar_w, content_zoom, tcw, tcox) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport.width,
                s.viewport.height as f32,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
                s.content_zoom,
                s.text_column_width,
                s.text_column_offset_x,
            )
        };
        let mut images = self.shared.images.lock().unwrap();
        let doc_h = measure(
            &source, vw, vh as u32,
            base_dir.as_deref(), sidebar_w, content_zoom,
            tcw, tcox,
            &theme, &self.shared.fonts, &mut images,
        );
        drop(images);
        let target = (doc_y - vh * 0.35).max(0.0).min((doc_h - vh).max(0.0));
        let mut s = self.shared.state.lock().unwrap();
        s.scroll = target;
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn open_quick_open(&self) {
        launch_quick_open(&self.shared, &self.proxy);
    }

    fn close_quick_open(&self) {
        let mut s = self.shared.state.lock().unwrap();
        if let Some(qo) = s.quick_open.take() {
            // Tell the still-running walker (if any) to exit at its next
            // directory boundary. Stale batches are also rejected by the
            // generation check, but the cancel flag stops the disk I/O
            // sooner.
            qo.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Compute filtered list (indices into `files`) given the current query.
    /// Empty query shows everything in tree order. Otherwise we run a
    /// subsequence-match ("fuzzy") scorer and return matches ordered by
    /// score (best first). Ties broken by path length so shorter paths win.
    fn quick_open_matches(qo: &QuickOpenUi) -> Vec<usize> {
        if qo.query.is_empty() {
            return (0..qo.files.len()).collect();
        }
        let q_lower: Vec<char> = qo.query.chars().flat_map(|c| c.to_lowercase()).collect();
        let mut scored: Vec<(i32, usize, usize)> = Vec::new(); // (score, path_len, idx)
        for (i, p) in qo.files.iter().enumerate() {
            let rel = p.strip_prefix(&qo.base).unwrap_or(p.as_path());
            let rel_str = rel.to_string_lossy();
            if let Some(score) = fuzzy_score(&q_lower, &rel_str) {
                scored.push((score, rel_str.len(), i));
            }
        }
        // Higher score first; shorter path breaks ties.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, i)| i).collect()
    }

    fn handle_quick_open_key(
        &self,
        key: &winit::keyboard::Key,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        use winit::keyboard::Key;
        let select = self.shift_mod();
        let word = self.alt_mod() || self.shortcut_mod();
        if self.shortcut_mod() {
            if let Key::Character(c) = key.as_ref() {
                match c {
                    "c" => {
                        let text = {
                            let s = self.shared.state.lock().unwrap();
                            s.quick_open.as_ref().map(|qo| {
                                if crate::text_input::has_selection(qo.cursor, qo.anchor) {
                                    crate::text_input::selected_text(&qo.query, qo.cursor, qo.anchor)
                                } else {
                                    qo.query.clone()
                                }
                            })
                        };
                        if let Some(t) = text {
                            if !t.is_empty() { crate::clipboard::copy(&t); }
                        }
                        return;
                    }
                    "x" => {
                        let to_copy = {
                            let mut s = self.shared.state.lock().unwrap();
                            s.quick_open.as_mut().and_then(|qo| {
                                if crate::text_input::has_selection(qo.cursor, qo.anchor) {
                                    let t = crate::text_input::selected_text(&qo.query, qo.cursor, qo.anchor);
                                    crate::text_input::delete_selection(&mut qo.query, &mut qo.cursor, &mut qo.anchor);
                                    qo.selected = 0;
                                    qo.scroll = 0;
                                    Some(t)
                                } else if !qo.query.is_empty() {
                                    let t = std::mem::take(&mut qo.query);
                                    qo.cursor = 0;
                                    qo.anchor = 0;
                                    qo.selected = 0;
                                    qo.scroll = 0;
                                    Some(t)
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(t) = to_copy {
                            if !t.is_empty() { crate::clipboard::copy(&t); }
                        }
                        return;
                    }
                    "v" => {
                        if let Some(text) = crate::clipboard::paste() {
                            let cleaned = sanitise_clipboard_for_input(&text);
                            if !cleaned.is_empty() {
                                let mut s = self.shared.state.lock().unwrap();
                                if let Some(qo) = &mut s.quick_open {
                                    crate::text_input::insert_str(&mut qo.query, &mut qo.cursor, &mut qo.anchor, &cleaned);
                                    qo.selected = 0;
                                    qo.scroll = 0;
                                }
                            }
                        }
                        return;
                    }
                    "a" => {
                        let mut s = self.shared.state.lock().unwrap();
                        if let Some(qo) = &mut s.quick_open {
                            crate::text_input::select_all(&qo.query, &mut qo.cursor, &mut qo.anchor);
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }
        match key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                self.close_quick_open();
            }
            Key::Named(NamedKey::Enter) => {
                let to_open: Option<PathBuf> = {
                    let s = self.shared.state.lock().unwrap();
                    s.quick_open.as_ref().and_then(|qo| {
                        let ms = Self::quick_open_matches(qo);
                        ms.get(qo.selected).and_then(|i| qo.files.get(*i)).cloned()
                    })
                };
                if let Some(p) = to_open {
                    self.close_quick_open();
                    self.open_path(&p, event_loop);
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    let ms = Self::quick_open_matches(qo);
                    if !ms.is_empty() {
                        qo.selected = (qo.selected + 1).min(ms.len() - 1);
                    }
                    Self::clamp_quick_open_scroll(qo);
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    if qo.selected > 0 {
                        qo.selected -= 1;
                    }
                    Self::clamp_quick_open_scroll(qo);
                }
            }
            Key::Named(NamedKey::PageDown) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    let ms = Self::quick_open_matches(qo);
                    if !ms.is_empty() {
                        qo.selected = (qo.selected + QUICK_OPEN_ROWS).min(ms.len() - 1);
                    }
                    Self::clamp_quick_open_scroll(qo);
                }
            }
            Key::Named(NamedKey::PageUp) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    qo.selected = qo.selected.saturating_sub(QUICK_OPEN_ROWS);
                    Self::clamp_quick_open_scroll(qo);
                }
            }
            // Text-cursor moves inside the query input.
            Key::Named(NamedKey::ArrowLeft) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    if word {
                        crate::text_input::move_word_left(&qo.query, &mut qo.cursor, &mut qo.anchor, select);
                    } else {
                        crate::text_input::move_left(&qo.query, &mut qo.cursor, &mut qo.anchor, select);
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    if word {
                        crate::text_input::move_word_right(&qo.query, &mut qo.cursor, &mut qo.anchor, select);
                    } else {
                        crate::text_input::move_right(&qo.query, &mut qo.cursor, &mut qo.anchor, select);
                    }
                }
            }
            Key::Named(NamedKey::Home) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::move_home(&mut qo.cursor, &mut qo.anchor, select);
                }
            }
            Key::Named(NamedKey::End) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::move_end(&qo.query, &mut qo.cursor, &mut qo.anchor, select);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::backspace(&mut qo.query, &mut qo.cursor, &mut qo.anchor);
                    qo.selected = 0;
                    qo.scroll = 0;
                }
            }
            Key::Named(NamedKey::Delete) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::delete_forward(&mut qo.query, &mut qo.cursor, &mut qo.anchor);
                    qo.selected = 0;
                    qo.scroll = 0;
                }
            }
            Key::Named(NamedKey::Space) => {
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::insert_str(&mut qo.query, &mut qo.cursor, &mut qo.anchor, " ");
                    qo.selected = 0;
                    qo.scroll = 0;
                }
            }
            Key::Character(txt) => {
                let cleaned: String = txt.chars().filter(|c| !c.is_control()).collect();
                if cleaned.is_empty() { return; }
                let mut s = self.shared.state.lock().unwrap();
                if let Some(qo) = &mut s.quick_open {
                    crate::text_input::insert_str(&mut qo.query, &mut qo.cursor, &mut qo.anchor, &cleaned);
                    qo.selected = 0;
                    qo.scroll = 0;
                }
            }
            _ => {}
        }
    }

    /// Keep the selected row visible inside the result viewport.
    fn clamp_quick_open_scroll(qo: &mut QuickOpenUi) {
        if qo.selected < qo.scroll {
            qo.scroll = qo.selected;
        } else if qo.selected >= qo.scroll + QUICK_OPEN_ROWS {
            qo.scroll = qo.selected + 1 - QUICK_OPEN_ROWS;
        }
    }

    /// Open the given file in the main view, re-rooting the tree and
    /// updating scroll / path state. Shared between quick-open and sidebar
    /// clicks.
    fn open_path(&self, p: &Path, _event_loop: &winit::event_loop::ActiveEventLoop) {
        let Ok(content) = std::fs::read_to_string(p) else { return };
        let mut s = self.shared.state.lock().unwrap();
        s.source = content;
        s.source_path = Some(p.to_path_buf());
        s.scroll = 0.0;
        s.sel_anchor = None;
        s.sel_head = None;
        // Expand every ancestor folder up to the tree root so the newly
        // active file is visible (and marked) in the sidebar.
        if let Some(tree) = &mut s.tree {
            let root = tree.root.clone();
            let mut cur = p.parent();
            while let Some(dir) = cur {
                tree.expand(dir.to_path_buf());
                if dir == root {
                    break;
                }
                cur = dir.parent();
            }
        }
        drop(s);
        self.scroll_sidebar_to_active();
    }

    /// Scroll the sidebar so the row for `source_path` is visible. No-op if
    /// there's no active path or no tree.
    fn scroll_sidebar_to_active(&self) {
        let (flat, active, sidebar_h, sidebar_scroll, sidebar_zoom, theme) = {
            let s = self.shared.state.lock().unwrap();
            let Some(tree) = &s.tree else { return };
            (
                tree.flatten(),
                s.source_path.clone(),
                s.viewport.height as f32,
                s.sidebar_scroll,
                s.sidebar_zoom,
                if s.dark { Theme::dark() } else { Theme::light() },
            )
        };
        let Some(active) = active else { return };
        let Some(row) = flat.iter().position(|e| e.path == active) else { return };
        // Matches the row height used in render::sidebar_content_height.
        let row_h = theme.body_size * 0.82 * sidebar_zoom * 1.5;
        let top_pad = theme.margin_y * 0.5;
        let row_top = top_pad + row as f32 * row_h;
        let row_bot = row_top + row_h;
        let mut new_scroll = sidebar_scroll;
        if row_top < sidebar_scroll {
            new_scroll = row_top;
        } else if row_bot > sidebar_scroll + sidebar_h {
            new_scroll = (row_bot - sidebar_h).max(0.0);
        }
        if (new_scroll - sidebar_scroll).abs() > 0.5 {
            let mut s = self.shared.state.lock().unwrap();
            s.sidebar_scroll = new_scroll;
        }
    }

    /// Step the read cursor up/down by one rendered line. Activates the
    /// cursor on first call (anchored near the top of the viewport).
    /// Auto-scrolls when the new position is close to the viewport edges.
    fn move_read_cursor(&self, dir: i32) {
        let baselines = self.current_baselines();
        if baselines.is_empty() {
            return;
        }
        let (cur, scroll, vh) = {
            let s = self.shared.state.lock().unwrap();
            (s.read_cursor, s.scroll, s.viewport.height as f32)
        };
        let new_y = match cur {
            None => {
                // First activation — pick the baseline nearest the top
                // of the visible area so the cursor lands where the eye
                // already is.
                let target = scroll + 24.0;
                *baselines
                    .iter()
                    .min_by(|a, b| {
                        (**a - target)
                            .abs()
                            .partial_cmp(&(**b - target).abs())
                            .unwrap()
                    })
                    .unwrap()
            }
            Some(y) => {
                // Find current index by nearest match (the baseline list
                // can shift slightly between renders — e.g. font swap or
                // resize — so don't require exact equality).
                let idx = baselines
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        (**a - y).abs().partial_cmp(&(**b - y).abs()).unwrap()
                    })
                    .map(|(i, _)| i)
                    .unwrap();
                let new_idx = (idx as i32 + dir).clamp(0, baselines.len() as i32 - 1) as usize;
                baselines[new_idx]
            }
        };
        {
            let mut s = self.shared.state.lock().unwrap();
            s.read_cursor = Some(new_y);
            // Keep the cursor inside a comfortable reading band; nudge
            // scroll only when the cursor would fall outside it.
            let margin = 80.0_f32.min(vh * 0.25);
            let screen_y = new_y - s.scroll;
            if screen_y < margin {
                s.scroll = (new_y - margin).max(0.0);
            } else if screen_y > vh - margin {
                s.scroll = new_y - (vh - margin);
            }
        }
        self.clamp_scroll();
    }

    /// Move the read cursor to the previous/next section heading and
    /// scroll so the heading is near the top of the viewport.
    fn jump_section(&self, dir: i32) {
        let outline = self.current_outline();
        if outline.is_empty() {
            return;
        }
        let cur = self.shared.state.lock().unwrap().read_cursor.unwrap_or(0.0);
        let target = if dir > 0 {
            outline
                .iter()
                .find(|o| o.doc_y > cur + 0.5)
                .map(|o| o.doc_y)
                .unwrap_or(outline.last().unwrap().doc_y)
        } else {
            outline
                .iter()
                .rev()
                .find(|o| o.doc_y < cur - 0.5)
                .map(|o| o.doc_y)
                .unwrap_or(outline.first().unwrap().doc_y)
        };
        // Snap to the first baseline at or after the heading's doc_y so
        // the cursor lands on the heading text itself.
        let baselines = self.current_baselines();
        let snap_y = baselines
            .iter()
            .find(|b| **b >= target)
            .copied()
            .unwrap_or(target);
        {
            let mut s = self.shared.state.lock().unwrap();
            s.read_cursor = Some(snap_y);
            // Pull the heading near the top of the viewport.
            s.scroll = (snap_y - 24.0).max(0.0);
        }
        self.clamp_scroll();
    }

    /// Open the context menu at the read cursor's screen position. Menu
    /// items mirror the right-click flow so the user gets the same
    /// affordances (copy code, copy table, link actions, dark mode,
    /// outline jump, etc.) without ever touching the mouse.
    fn open_context_menu_at_cursor(&self) {
        let (cursor_y, viewport, dark, active_path, scroll, sidebar_w) = {
            let s = self.shared.state.lock().unwrap();
            let Some(y) = s.read_cursor else { return };
            (y, s.viewport, s.dark, s.source_path.clone(), s.scroll, s.sidebar_width)
        };
        // Virtual mouse position: just inside the content area at the
        // cursor's row. Used for hit-testing copy zones.
        let theme = if dark { Theme::dark() } else { Theme::light() };
        let px = sidebar_w + theme.margin_x + 8.0;
        let py = (cursor_y - scroll).max(0.0);
        let copy_path = active_path.clone();
        let zones = self.current_copy_zones();
        let doc_y = py + scroll;
        let zone_hit = zones
            .iter()
            .find(|z| px >= z.x && px < z.x + z.w && doc_y >= z.y && doc_y < z.y + z.h)
            .cloned();
        let copy_selection = self.current_selection_text();
        let outline_items = outline_to_menu_items(&self.current_outline());
        let mermaid_items = zone_hit
            .as_ref()
            .and_then(|z| z.mermaid_block.map(mermaid_layout_items))
            .unwrap_or_default();
        let items = build_context_menu_items(
            dark,
            copy_path,
            !outline_items.is_empty(),
            copy_selection,
            zone_hit.as_ref(),
        );
        let menu_w = context_menu_width(&items, &self.shared.fonts);
        let menu_h = context_menu_height(&items);
        let mut mx = px;
        let mut my = py;
        mx = mx.min(viewport.width as f32 - menu_w - 4.0).max(4.0);
        my = my.min(viewport.height as f32 - menu_h - 4.0).max(4.0);
        let mut s = self.shared.state.lock().unwrap();
        s.context_menu = Some(ContextMenu {
            x: mx,
            y: my,
            items,
            outline_items,
            mermaid_items,
            active_submenu: None,
            // Ctrl-tap is keyboard-driven — start with the first item
            // selected so ↓/Enter just works without a mouse move.
            selected: Some(0),
            submenu_selected: None,
        });
    }

    /// Move the keyboard selection in the open context menu by `dir`
    /// rows (wraps). Operates inside the active submenu when one is
    /// open; otherwise on the main list. First press on a mouse-opened
    /// menu seeds `selected` so the user doesn't have to press twice.
    fn menu_move(&self, dir: i32) {
        let mut s = self.shared.state.lock().unwrap();
        let Some(m) = s.context_menu.as_mut() else { return };
        if let Some((kind, idx)) = m.submenu_selected {
            let items = kind.items(m).len() as i32;
            if items == 0 {
                return;
            }
            let new = (idx as i32 + dir).rem_euclid(items) as usize;
            m.submenu_selected = Some((kind, new));
        } else {
            let n = m.items.len() as i32;
            if n == 0 {
                return;
            }
            let cur = m.selected.unwrap_or(if dir > 0 { n - 1 } else { 0 } as usize);
            let new = (cur as i32 + dir).rem_euclid(n) as usize;
            m.selected = Some(new);
        }
    }

    /// → on a submenu trigger row opens that submenu and moves the
    /// keyboard selection into its first item. No-op on plain rows.
    fn menu_open_submenu(&self) {
        let mut s = self.shared.state.lock().unwrap();
        let Some(m) = s.context_menu.as_mut() else { return };
        // Already in a submenu — nothing to do.
        if m.submenu_selected.is_some() {
            return;
        }
        let Some(idx) = m.selected else { return };
        let action = m.items.get(idx).map(|(_, a)| a.clone());
        let kind = match action {
            Some(MenuAction::Outline) => SubmenuKind::Outline,
            Some(MenuAction::MermaidMenu) => SubmenuKind::Mermaid,
            _ => return,
        };
        if !kind.items(m).is_empty() {
            m.submenu_selected = Some((kind, 0));
        }
    }

    /// ← inside a submenu returns focus to the parent menu's trigger row.
    fn menu_close_submenu(&self) {
        let mut s = self.shared.state.lock().unwrap();
        if let Some(m) = s.context_menu.as_mut() {
            m.submenu_selected = None;
        }
    }

    /// Activate the keyboard-selected menu item. Submenu triggers open
    /// the submenu rather than dispatching. Returns true if anything
    /// was acted on (so the caller knows to redraw).
    fn menu_activate(&self) -> bool {
        let action = {
            let s = self.shared.state.lock().unwrap();
            let Some(m) = s.context_menu.as_ref() else { return false };
            if let Some((kind, idx)) = m.submenu_selected {
                kind.items(m).get(idx).map(|(_, a)| a.clone())
            } else if let Some(idx) = m.selected {
                m.items.get(idx).map(|(_, a)| a.clone())
            } else {
                None
            }
        };
        let Some(action) = action else { return false };
        match action {
            MenuAction::Outline | MenuAction::MermaidMenu => {
                self.menu_open_submenu();
                true
            }
            other => {
                self.shared.state.lock().unwrap().context_menu = None;
                apply_menu_action(&self.shared, &self.proxy, &other);
                true
            }
        }
    }

    fn clamp_scroll(&self) {
        let theme = Theme::light();
        let (source, vw, vh, base_dir, sidebar_w, content_zoom, tcw, tcox) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport.width,
                s.viewport.height as f32,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
                s.content_zoom,
                s.text_column_width,
                s.text_column_offset_x,
            )
        };
        let mut images = self.shared.images.lock().unwrap();
        let doc_h = measure(
            &source,
            vw,
            vh as u32,
            base_dir.as_deref(),
            sidebar_w,
            content_zoom,
            tcw,
            tcox,
            &theme,
            &self.shared.fonts,
            &mut images,
        );
        drop(images);
        let max_scroll = (doc_h - vh).max(0.0);
        let mut s = self.shared.state.lock().unwrap();
        if s.scroll > max_scroll {
            s.scroll = max_scroll;
        }
    }

    /// Decide which cursor icon to show based on what the pointer is over,
    /// and push it to the window (only when it changes).
    ///
    /// Pointer is reserved for things the user can actually click:
    /// tree rows in the sidebar and markdown links in the content. The
    /// scrollbar keeps the default cursor (no hand), matching the request.
    #[allow(clippy::too_many_arguments)]
    fn update_cursor(
        &mut self,
        x: f32,
        y: f32,
        dragging_sidebar: bool,
        dragging_scrollbar: bool,
        selecting: bool,
        sidebar_w: f32,
        tree_visible: bool,
        _viewport: Viewport,
    ) -> bool {
        // If the context menu is open and the cursor is over an item, show
        // a pointer so users know the row is clickable.
        let menu = self.shared.state.lock().unwrap().context_menu.clone();
        if let Some(m) = menu {
            let over_item = menu_item_hit(&m, x, y, &self.shared.fonts).is_some();
            let icon = if over_item { CursorIcon::Pointer } else { CursorIcon::Default };
            let changed = icon != self.cursor;
            if changed {
                if let Some(w) = &self.window {
                    w.set_cursor(icon);
                }
                self.cursor = icon;
            }
            return changed;
        }
        // Compute hover rect once so we can both pick the cursor and
        // notice when the hovered target changes between equal-cursor rects
        // (e.g., two adjacent tree rows both want CursorIcon::Pointer).
        let hover_rect = if dragging_scrollbar || dragging_sidebar || selecting {
            None
        } else {
            self.hovered_rect(x, y)
        };

        let icon = if dragging_scrollbar {
            // Keep default while scroll-dragging — no grab cursor per user ask.
            CursorIcon::Default
        } else if dragging_sidebar {
            CursorIcon::EwResize
        } else if selecting {
            CursorIcon::Text
        } else if tree_visible && sidebar_w > 0.0 && (x - sidebar_w).abs() <= 6.0 {
            CursorIcon::EwResize
        } else if hover_rect.is_some() {
            CursorIcon::Pointer
        } else if x < sidebar_w {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };

        let icon_changed = icon != self.cursor;
        if icon_changed {
            if let Some(w) = &self.window {
                w.set_cursor(icon);
            }
            self.cursor = icon;
        }
        let hover_changed = hover_rect != self.last_hover_rect;
        self.last_hover_rect = hover_rect;
        icon_changed || hover_changed
    }

    /// Return the hit-target rect under (x, y), or None. Pinned rects are in
    /// screen coords; content rects in doc coords — we return them as-is so
    /// the caller can just compare the tuple.
    fn hovered_rect(&self, x: f32, y: f32) -> Option<(f32, f32, f32, f32)> {
        let (pinned, content) = self.current_hit_targets();
        if let Some(t) = crate::render::hit_test(&pinned, x, y) {
            return Some((t.x, t.y, t.w, t.h));
        }
        let scroll = self.shared.state.lock().unwrap().scroll;
        crate::render::hit_test(&content, x, y + scroll).map(|t| (t.x, t.y, t.w, t.h))
    }

    fn current_copy_zones(&self) -> Vec<crate::layout::CopyZone> {
        let snap = self.shared.snapshot();
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        crate::render::compute_copy_zones(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )
    }

    fn current_selection_text(&self) -> Option<String> {
        let snap = self.shared.snapshot();
        snap.selection?;
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        let text = extract_selection(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )?;
        if text.is_empty() { None } else { Some(text) }
    }

    /// Sorted unique baseline y's for the current document. Same recompute
    /// pattern as `current_outline` — cheap enough on each ↑/↓ press given
    /// how the rest of the helpers are written.
    fn current_baselines(&self) -> Vec<f32> {
        let snap = self.shared.snapshot();
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        crate::render::compute_baselines(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
                text_column_width: snap.text_column_width,
                text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )
    }

    fn current_outline(&self) -> Vec<crate::layout::OutlineEntry> {
        let snap = self.shared.snapshot();
        if snap.source_path.is_none() {
            return Vec::new();
        }
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        crate::render::compute_outline(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )
    }

    fn current_hit_targets(&self) -> (Vec<crate::layout::HitTarget>, Vec<crate::layout::HitTarget>) {
        let snap = self.shared.snapshot();
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        compute_all_hit_targets(
            &RenderInput {
                source: &snap.source,
                viewport: snap.viewport,
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )
    }

    /// Measure the sidebar's total content height and clamp
    /// `sidebar_scroll` to [0, content_h - viewport_h].
    fn clamp_sidebar_scroll(&self) {
        let snap = self.shared.snapshot();
        let Some(tree) = snap.tree_flat.as_deref() else { return };
        if snap.sidebar_width <= 0.0 {
            return;
        }
        let outline_len = if snap.source_path.is_some() {
            crate::md::count_headings(&snap.source)
        } else {
            0
        };
        let content_h = sidebar_content_height(&snap.theme, tree.len(), outline_len, snap.sidebar_zoom);
        let max_scroll = (content_h - snap.viewport.height as f32).max(0.0);
        let mut s = self.shared.state.lock().unwrap();
        if s.sidebar_scroll > max_scroll {
            s.sidebar_scroll = max_scroll;
        }
        if s.sidebar_scroll < 0.0 {
            s.sidebar_scroll = 0.0;
        }
    }

    fn current_sidebar_scrollbar_geom(&self) -> Option<SbGeom> {
        let snap = self.shared.snapshot();
        let tree = snap.tree_flat.as_ref()?;
        if snap.sidebar_width <= 0.0 {
            return None;
        }
        let outline_len = if snap.source_path.is_some() {
            crate::md::count_headings(&snap.source)
        } else {
            0
        };
        let content_h = sidebar_content_height(&snap.theme, tree.len(), outline_len, snap.sidebar_zoom);
        sidebar_scrollbar_geom(
            snap.sidebar_width,
            snap.viewport.height as f32,
            snap.sidebar_scroll,
            content_h,
        )
    }

    fn current_scrollbar_geom(&self) -> Option<SbGeom> {
        let theme = Theme::light();
        let (source, viewport, base_dir, sidebar_w, scroll, content_zoom, tcw, tcox) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
                s.scroll,
                s.content_zoom,
                s.text_column_width,
                s.text_column_offset_x,
            )
        };
        let mut images = self.shared.images.lock().unwrap();
        let doc_h = measure(
            &source,
            viewport.width,
            viewport.height,
            base_dir.as_deref(),
            sidebar_w,
            content_zoom,
            tcw,
            tcox,
            &theme,
            &self.shared.fonts,
            &mut images,
        );
        scrollbar_geom(viewport, scroll, doc_h)
    }

    fn copy_selection(&self) {
        let snap = self.shared.snapshot();
        if snap.selection.is_none() {
            return;
        }
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let text = {
            let mut images = self.shared.images.lock().unwrap();
            extract_selection(
                &RenderInput {
                    source: &snap.source,
                    viewport: snap.viewport,
                    scroll: snap.scroll,
                    theme: &snap.theme,
                    fonts: &self.shared.fonts,
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
                    mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
                },
                &mut images,
            )
        };
        if let Some(t) = text {
            if !t.is_empty() {
                clipboard::copy(&t);
            }
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .unwrap();

        let snap = self.shared.snapshot();
        let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut images = self.shared.images.lock().unwrap();
        let mut fb = render(
            &RenderInput {
                source: &snap.source,
                viewport: Viewport { width: w, height: h },
                scroll: snap.scroll,
                theme: &snap.theme,
                fonts: &self.shared.fonts,
                tree: snap.tree_flat.as_deref(),
                active_path: snap.source_path.as_deref(),
                base_dir: base_dir.as_deref(),
                sidebar_width: snap.sidebar_width,
                sidebar_scroll: snap.sidebar_scroll,
                content_zoom: snap.content_zoom,
                sidebar_zoom: snap.sidebar_zoom,
                selection: snap.selection,
                hover_pos: snap.hover_pos,
                search: snap.search.as_ref().filter(|s| !s.query.is_empty()).map(|s| {
                    crate::render::SearchHighlights {
                        query: &s.query,
                        current: if s.match_count > 0 { Some(s.current) } else { None },
                    }
                }),
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        );
        drop(images);

        // Read cursor — thin vertical caret in the left margin at the
        // current line's baseline. Only drawn when the user has actually
        // started keyboard navigation.
        if let Some(cursor_y) = snap.read_cursor {
            let screen_y = cursor_y - snap.scroll;
            // Approximate the caret height from body line-height; the
            // caret is purely a visual marker so this doesn't need to
            // match any specific glyph metric.
            let lh = snap.theme.body_size * snap.theme.line_height_mult;
            let top = (screen_y - lh + 2.0).round() as i32;
            let height = (lh * 0.95) as i32;
            // Sit just left of the content's left edge — close enough to
            // the text to read as "this line", far enough not to overlap
            // letters.
            let x = (snap.sidebar_width + snap.theme.margin_x - 8.0).max(snap.sidebar_width + 2.0) as i32;
            fb.fill_rect(x, top, 3, height.max(8), snap.theme.accent);
        }

        if let Some(su) = &snap.search {
            let hover = {
                let s = self.shared.state.lock().unwrap();
                Some((s.last_mouse.x as f32, s.last_mouse.y as f32))
            };
            draw_search_ui(&mut fb, &snap.theme, &self.shared.fonts, su, hover);
        }

        if let Some(qo) = &snap.quick_open {
            let hover = {
                let s = self.shared.state.lock().unwrap();
                Some((s.last_mouse.x as f32, s.last_mouse.y as f32))
            };
            draw_quick_open_ui(&mut fb, &snap.theme, &self.shared.fonts, qo, snap.viewport, hover);
        }

        if let Some(m) = &snap.context_menu {
            let hover = {
                let s = self.shared.state.lock().unwrap();
                Some((s.last_mouse.x as f32, s.last_mouse.y as f32))
            };
            draw_context_menu(&mut fb, &snap.theme, &self.shared.fonts, m, hover);
        }

        let mut buffer = surface.buffer_mut().unwrap();
        for (i, px) in fb.pixels.chunks_exact(4).enumerate() {
            let r = px[0] as u32;
            let g = px[1] as u32;
            let b = px[2] as u32;
            buffer[i] = (r << 16) | (g << 8) | b;
        }
        buffer.present().unwrap();
        // reference proxy so lint doesn't complain
        let _ = &self.proxy;
    }
}

pub fn click_at(shared: &Arc<Shared>, x: f32, y: f32) -> Option<HitAction> {
    let snap = shared.snapshot();
    let base_dir = snap.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let (pinned, content) = {
        let mut images = shared.images.lock().unwrap();
        compute_all_hit_targets(
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
                mermaid_overrides: Some(&snap.mermaid_overrides),
            text_column_width: snap.text_column_width,
            text_column_offset_x: snap.text_column_offset_x,
            },
            &mut images,
        )
    };
    // Pinned targets (tree rows) first — they're in screen coords.
    let raw_action = if let Some(hit) = hit_test(&pinned, x, y) {
        hit.action.clone()
    } else if let Some(hit) = hit_test(&content, x, y + snap.scroll) {
        // Content targets (links) are in document coords.
        hit.action.clone()
    } else {
        return None;
    };

    // Double-click detection: a second click on the same folder within
    // 400 ms and a few pixels promotes Toggle → SetRoot (enter dir).
    let action = match raw_action {
        HitAction::Toggle(ref path) => {
            let now = std::time::Instant::now();
            let mut s = shared.state.lock().unwrap();
            let is_double = match &s.last_folder_click {
                Some((t0, px, py, last_path)) => {
                    last_path == path
                        && now.duration_since(*t0).as_millis() < 400
                        && (px - x).abs() < 5.0
                        && (py - y).abs() < 5.0
                }
                None => false,
            };
            if is_double {
                s.last_folder_click = None;
                HitAction::SetRoot(path.clone())
            } else {
                s.last_folder_click = Some((now, x, y, path.clone()));
                raw_action
            }
        }
        other => other,
    };
    match &action {
        HitAction::Open(path) => {
            let source = std::fs::read_to_string(path).unwrap_or_default();
            let mut s = shared.state.lock().unwrap();
            s.source = source;
            s.source_path = Some(path.clone());
            s.scroll = 0.0;
        }
        HitAction::Toggle(path) => {
            let mut s = shared.state.lock().unwrap();
            if let Some(t) = s.tree.as_mut() {
                t.toggle(path);
            }
        }
        HitAction::OpenUrl(url) => {
            open_url(url);
        }
        HitAction::CopyCode(text) => {
            clipboard::copy(text);
        }
        HitAction::SetRoot(path) => {
            let mut s = shared.state.lock().unwrap();
            if let Some(t) = s.tree.as_mut() {
                t.set_root(path.clone());
            }
            // Re-rooting invalidates any pending double-click anchor.
            s.last_folder_click = None;
        }
        HitAction::ToggleTask { box_byte, now_checked } => {
            // Flip the byte inside the brackets and rewrite the file.
            // Doing it on the in-memory buffer first keeps the UI snappy;
            // the filesystem write is best-effort, and the watcher will
            // reconcile on the next mtime tick either way.
            let write_target = {
                let mut s = shared.state.lock().unwrap();
                let src = &mut s.source;
                let idx = *box_byte + 1;
                let new_ch = if *now_checked { b'x' } else { b' ' };
                if idx < src.len() {
                    // Validate we're actually looking at a `[ ]` / `[x]`.
                    let b = src.as_bytes();
                    let at = b.get(idx).copied();
                    let open = b.get(*box_byte).copied();
                    let close = b.get(*box_byte + 2).copied();
                    if open == Some(b'[') && close == Some(b']') && matches!(at, Some(b' ') | Some(b'x') | Some(b'X')) {
                        // str is utf-8; these 3 bytes are ASCII, so SAFETY
                        // of in-place byte patch is preserved.
                        unsafe { src.as_bytes_mut()[idx] = new_ch; }
                    }
                }
                s.source_path.clone()
            };
            if let Some(path) = write_target {
                let body = {
                    let s = shared.state.lock().unwrap();
                    s.source.clone()
                };
                let _ = std::fs::write(&path, body);
            }
        }
        HitAction::ScrollTo(y) => {
            // Clamp against the actual doc height so clicking "Smallest
            // heading" near the end doesn't scroll past the bottom.
            let theme = Theme::light();
            let (source, vw, vh, base_dir, sidebar_w, content_zoom, tcw, tcox) = {
                let s = shared.state.lock().unwrap();
                (
                    s.source.clone(),
                    s.viewport.width,
                    s.viewport.height as f32,
                    s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                    s.sidebar_width,
                    s.content_zoom,
                    s.text_column_width,
                    s.text_column_offset_x,
                )
            };
            let mut images = shared.images.lock().unwrap();
            let doc_h = measure(
                &source,
                vw,
                vh as u32,
                base_dir.as_deref(),
                sidebar_w,
                content_zoom,
                tcw,
                tcox,
                &theme,
                &shared.fonts,
                &mut images,
            );
            drop(images);
            let max_scroll = (doc_h - vh).max(0.0);
            let mut s = shared.state.lock().unwrap();
            s.scroll = (*y - 8.0).clamp(0.0, max_scroll);
            // Pull the read cursor along to the new section so the
            // reader token marks where the user just jumped.
            s.read_cursor = Some(*y);
        }
    }
    Some(action)
}

/// Shell out to `xdg-open` (Linux). Errors silently — the worst case is a
/// click that does nothing, which matches the prior "links aren't clickable"
/// state anyway.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Runtime options for the window shell.
pub struct WindowOptions {
    /// Initial file or directory to open; `None` → open the current dir.
    pub path: Option<PathBuf>,
    /// When `Some(port)`, spawn the HTTP control API bound to that port
    /// (`0` = ephemeral). `None` → don't spawn the API at all. Most
    /// human-facing invocations want `None`.
    pub api_port: Option<u16>,
}

pub fn run(opts: WindowOptions) -> ExitCode {
    let arg = opts.path;
    let fonts = Fonts::load();
    let viewport = Viewport { width: 1200, height: 900 };

    let (source, source_path, tree) = resolve_open_arg(arg);

    let shared = Arc::new(Shared {
        fonts,
        images: Mutex::new(ImageCache::new()),
        state: Mutex::new(AppState {
            source,
            source_path,
            scroll: 0.0,
            viewport,
            tree,
            last_mouse: PhysicalPosition::new(0.0, 0.0),
            sidebar_width: 260.0,
            sidebar_width_restore: 260.0,
            sidebar_dragging: false,
            sidebar_scroll: 0.0,
            sel_anchor: None,
            sel_head: None,
            is_selecting: false,
            scrollbar_dragging: false,
            scrollbar_grip: 0.0,
            sidebar_scrollbar_dragging: false,
            sidebar_scrollbar_grip: 0.0,
            content_zoom: 1.0,
            sidebar_zoom: 1.0,
            text_column_width: 720.0,
            text_column_offset_x: 0.0,
            last_folder_click: None,
            dark: false,
            context_menu: None,
            search: None,
            quick_open: None,
            quick_open_seq: 0,
            mermaid_overrides: std::collections::HashMap::new(),
            read_cursor: None,
        }),
    });

    let event_loop: EventLoop<UserEvent> = EventLoop::with_user_event().build().unwrap();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    if let Some(requested) = opts.api_port {
        match api::spawn(shared.clone(), proxy.clone(), requested) {
            Ok(bound) => println!("mdrdr api listening on http://127.0.0.1:{bound}"),
            Err(e) => eprintln!(
                "mdrdr api disabled: could not bind 127.0.0.1:{requested} ({e})"
            ),
        }
    }

    crate::watch::spawn(shared.clone(), proxy.clone());

    let mut app = App {
        shared,
        window: None,
        surface: None,
        proxy,
        modifiers: Modifiers::default(),
        synth_mods: None,
        cursor: CursorIcon::Default,
        last_hover_rect: None,
        ctrl_alone_armed: false,
    };

    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ───── search overlay ──────────────────────────────────────────────────────

const SEARCH_PANEL_W: f32 = 360.0;
const SEARCH_PANEL_H: f32 = 40.0;
const SEARCH_DRAG_W: f32 = 18.0;      // left strip that grabs drags
const SEARCH_FONT_SIZE: f32 = 14.0;

// ───── quick-open overlay (Ctrl+P) ─────────────────────────────────────────

const QUICK_OPEN_W: f32 = 560.0;
const QUICK_OPEN_INPUT_H: f32 = 36.0;
const QUICK_OPEN_ROW_H: f32 = 24.0;
const QUICK_OPEN_ROWS: usize = 12;
const QUICK_OPEN_FONT_SIZE: f32 = 14.0;
const QUICK_OPEN_ROW_FONT_SIZE: f32 = 13.0;

/// Geometry for each interactive part of the search panel — shared by
/// drawing and hit-testing so they can't drift out of sync.
#[derive(Debug, Clone, Copy)]
struct SearchGeom {
    panel: (f32, f32, f32, f32),    // x, y, w, h
    drag_strip: (f32, f32, f32, f32),
    input: (f32, f32, f32, f32),
    prev_btn: (f32, f32, f32, f32),
    next_btn: (f32, f32, f32, f32),
    close_btn: (f32, f32, f32, f32),
}

/// Render the contents of a single-line text input: selection rect,
/// glyphs, and cursor caret. Shared by the search and quick-open
/// panels — both use the same model so they get the same look.
fn draw_text_input(
    fb: &mut crate::render::Framebuffer,
    text: &str,
    cursor: usize,
    anchor: usize,
    font_size: f32,
    fonts: &Fonts,
    text_left: f32,
    text_right: f32,
    baseline: f32,
    fg: crate::theme::Rgba,
    sel: crate::theme::Rgba,
) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Pre-compute per-char advance widths and the prefix sums so we can
    // map any cursor index to a pixel x in O(1).
    let advances: Vec<f32> = chars
        .iter()
        .map(|&ch| {
            let font = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
            font.metrics(ch, font_size).advance_width
        })
        .collect();
    let mut prefix: Vec<f32> = Vec::with_capacity(n + 1);
    prefix.push(0.0);
    for w in &advances {
        let last = *prefix.last().unwrap();
        prefix.push(last + w);
    }
    let total = *prefix.last().unwrap();
    let avail_w = (text_right - text_left).max(1.0);
    let cursor_idx = cursor.min(n);
    let cursor_x = prefix[cursor_idx];

    // Scroll the visible window so the cursor stays inside it. Standard
    // text-input behaviour: when the user types past the right edge we
    // scroll along; when they Home/Left back into the leading text we
    // scroll back.
    let mut scroll = 0.0_f32;
    if total > avail_w {
        let margin = 4.0;
        if cursor_x > avail_w - margin {
            scroll = cursor_x - (avail_w - margin);
        }
        if cursor_x - scroll < margin {
            scroll = (cursor_x - margin).max(0.0);
        }
        let max_scroll = (total - avail_w).max(0.0);
        scroll = scroll.clamp(0.0, max_scroll);
    }

    // Selection rect first so glyphs sit on top.
    if cursor != anchor {
        let (lo, hi) = if cursor < anchor { (cursor, anchor) } else { (anchor, cursor) };
        let lo = lo.min(n);
        let hi = hi.min(n);
        let sx0 = (text_left + prefix[lo] - scroll).max(text_left);
        let sx1 = (text_left + prefix[hi] - scroll).min(text_right);
        if sx1 > sx0 {
            let sy = baseline - font_size * 0.85;
            let sh = font_size * 1.15;
            fb.fill_rect(sx0 as i32, sy as i32, (sx1 - sx0) as i32, sh as i32, sel);
        }
    }

    // Glyphs — skip those clipped entirely outside the visible window.
    for (i, &ch) in chars.iter().enumerate() {
        let gx = text_left + prefix[i] - scroll;
        let gx_end = text_left + prefix[i + 1] - scroll;
        if gx_end < text_left || gx > text_right { continue; }
        let font = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
        fb.draw_glyph(font, ch, font_size, gx, baseline, fg);
    }

    // Caret. Always drawn (no blink) so the user can see where they're
    // about to type.
    let caret_x = (text_left + cursor_x - scroll).clamp(text_left, text_right);
    fb.fill_rect(
        caret_x as i32,
        (baseline - font_size * 0.85) as i32,
        1,
        font_size as i32,
        fg,
    );
}

fn search_geom(su: &SearchUi) -> SearchGeom {
    let x = su.x; let y = su.y;
    let w = SEARCH_PANEL_W; let h = SEARCH_PANEL_H;
    let drag_strip = (x, y, SEARCH_DRAG_W, h);
    let btn_sz = 26.0;
    let close_btn = (x + w - btn_sz - 6.0, y + (h - btn_sz) * 0.5, btn_sz, btn_sz);
    let next_btn = (close_btn.0 - btn_sz - 4.0, close_btn.1, btn_sz, btn_sz);
    let prev_btn = (next_btn.0 - btn_sz - 4.0, next_btn.1, btn_sz, btn_sz);
    let input = (
        x + SEARCH_DRAG_W + 6.0,
        y + 8.0,
        prev_btn.0 - (x + SEARCH_DRAG_W + 6.0) - 6.0 - 50.0, // reserve ~50px for count
        h - 16.0,
    );
    SearchGeom { panel: (x, y, w, h), drag_strip, input, prev_btn, next_btn, close_btn }
}

fn point_in(rect: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchHit { Drag, Prev, Next, Close, Input, Panel, Outside }

fn search_hit_test(su: &SearchUi, x: f32, y: f32) -> SearchHit {
    let g = search_geom(su);
    if point_in(g.close_btn, x, y) { return SearchHit::Close; }
    if point_in(g.next_btn, x, y) { return SearchHit::Next; }
    if point_in(g.prev_btn, x, y) { return SearchHit::Prev; }
    if point_in(g.drag_strip, x, y) { return SearchHit::Drag; }
    if point_in(g.input, x, y) { return SearchHit::Input; }
    if point_in(g.panel, x, y) { return SearchHit::Panel; }
    SearchHit::Outside
}

pub fn draw_search_ui(
    fb: &mut crate::render::Framebuffer,
    theme: &Theme,
    fonts: &Fonts,
    su: &SearchUi,
    hover: Option<(f32, f32)>,
) {
    let g = search_geom(su);
    let (px, py, pw, ph) = g.panel;

    // Shadow + panel + border.
    fb.fill_rect(px as i32 + 3, py as i32 + 4, pw as i32, ph as i32, [0, 0, 0, 70]);
    fb.fill_rect(px as i32, py as i32, pw as i32, ph as i32, theme.sidebar_bg);
    let border = theme.muted;
    fb.fill_rect(px as i32, py as i32, pw as i32, 1, border);
    fb.fill_rect(px as i32, py as i32 + ph as i32 - 1, pw as i32, 1, border);
    fb.fill_rect(px as i32, py as i32, 1, ph as i32, border);
    fb.fill_rect(px as i32 + pw as i32 - 1, py as i32, 1, ph as i32, border);

    // Drag grabber — six dots pattern, muted color.
    let dots_x = px + SEARCH_DRAG_W * 0.5 - 2.5;
    let dots_y_start = py + ph * 0.5 - 8.0;
    for r in 0..3 {
        for c in 0..2 {
            let dx = dots_x + c as f32 * 5.0;
            let dy = dots_y_start + r as f32 * 5.0;
            fb.fill_rect(dx as i32, dy as i32, 2, 2, theme.muted);
        }
    }

    // Input box.
    let (ix, iy, iw, ih) = g.input;
    fb.fill_rect(ix as i32, iy as i32, iw as i32, ih as i32, theme.bg);
    fb.fill_rect(ix as i32, iy as i32, iw as i32, 1, theme.muted);
    fb.fill_rect(ix as i32, iy as i32 + ih as i32 - 1, iw as i32, 1, theme.muted);
    fb.fill_rect(ix as i32, iy as i32, 1, ih as i32, theme.muted);
    fb.fill_rect(ix as i32 + iw as i32 - 1, iy as i32, 1, ih as i32, theme.muted);

    // Query text + selection + caret. Cursor-aware scroll keeps the
    // insertion point visible regardless of where in the string it is.
    let baseline = iy + ih * 0.5 + SEARCH_FONT_SIZE * 0.35;
    let text_left = ix + 6.0;
    let text_right = ix + iw - 8.0;
    let sel = [theme.accent[0], theme.accent[1], theme.accent[2], 110];
    draw_text_input(
        fb,
        &su.query,
        su.cursor,
        su.anchor,
        SEARCH_FONT_SIZE,
        fonts,
        text_left,
        text_right,
        baseline,
        theme.fg,
        sel,
    );

    // Match count "n / N" right of the input. `0 / 0` when there are none
    // rather than a wordy "no matches" — keeps the width stable while
    // typing and matches the convention used by most editors.
    let count_label = if su.query.is_empty() {
        String::new()
    } else if su.match_count == 0 {
        "0 / 0".to_string()
    } else {
        format!("{} / {}", su.current + 1, su.match_count)
    };
    let count_x = ix + iw + 6.0;
    let mut ccx = count_x;
    for ch in count_label.chars() {
        fb.draw_glyph(&fonts.body, ch, SEARCH_FONT_SIZE - 1.0, ccx, baseline, theme.muted);
        ccx += fonts.body.metrics(ch, SEARCH_FONT_SIZE - 1.0).advance_width;
    }

    // Buttons: prev (◄), next (▶), close (×). Hover tint.
    let hover_rect = hover.and_then(|(hx, hy)| {
        if point_in(g.prev_btn, hx, hy) { Some(g.prev_btn) }
        else if point_in(g.next_btn, hx, hy) { Some(g.next_btn) }
        else if point_in(g.close_btn, hx, hy) { Some(g.close_btn) }
        else { None }
    });
    for (rect, ch) in [(g.prev_btn, '‹'), (g.next_btn, '›'), (g.close_btn, '×')] {
        if Some(rect) == hover_rect {
            fb.fill_rect(rect.0 as i32, rect.1 as i32, rect.2 as i32, rect.3 as i32, theme.sidebar_active_bg);
        }
        let f = &fonts.body;
        let sz = 18.0;
        let m = f.metrics(ch, sz);
        let bx = rect.0 + (rect.2 - m.advance_width) * 0.5;
        let by = rect.1 + rect.3 * 0.5 + sz * 0.35;
        fb.draw_glyph(f, ch, sz, bx, by, theme.fg);
    }
}

/// Geometry for the quick-open panel, shared by draw and hit-test.
#[derive(Debug, Clone, Copy)]
struct QuickOpenGeom {
    panel: (f32, f32, f32, f32),
    input: (f32, f32, f32, f32),
    list: (f32, f32, f32, f32),
    row_h: f32,
}

fn quick_open_geom(viewport: crate::render::Viewport) -> QuickOpenGeom {
    let vw = viewport.width as f32;
    let vh = viewport.height as f32;
    let w = QUICK_OPEN_W.min(vw - 40.0).max(200.0);
    let rows_h = QUICK_OPEN_ROW_H * QUICK_OPEN_ROWS as f32;
    let status_h = 18.0;
    let h = QUICK_OPEN_INPUT_H + rows_h + status_h;
    let x = ((vw - w) * 0.5).max(0.0);
    let y = (vh * 0.12).min((vh - h - 10.0).max(0.0));
    let input = (x + 8.0, y + 6.0, w - 16.0, QUICK_OPEN_INPUT_H - 12.0);
    let list = (x + 4.0, y + QUICK_OPEN_INPUT_H, w - 8.0, rows_h);
    QuickOpenGeom { panel: (x, y, w, h), input, list, row_h: QUICK_OPEN_ROW_H }
}

/// `Some(row_index_in_view)` if the cursor is over a result row, else None.
fn quick_open_row_hit(geom: &QuickOpenGeom, x: f32, y: f32) -> Option<usize> {
    let (lx, ly, lw, lh) = geom.list;
    if x < lx || x >= lx + lw || y < ly || y >= ly + lh {
        return None;
    }
    let row = ((y - ly) / geom.row_h) as usize;
    if row < QUICK_OPEN_ROWS { Some(row) } else { None }
}

pub fn draw_quick_open_ui(
    fb: &mut crate::render::Framebuffer,
    theme: &Theme,
    fonts: &Fonts,
    qo: &QuickOpenUi,
    viewport: crate::render::Viewport,
    hover: Option<(f32, f32)>,
) {
    let g = quick_open_geom(viewport);
    let (px, py, pw, ph) = g.panel;

    // Backdrop dimmer across the window.
    let vw = viewport.width as i32;
    let vh = viewport.height as i32;
    fb.fill_rect(0, 0, vw, vh, [0, 0, 0, 80]);

    // Shadow + panel + border.
    fb.fill_rect(px as i32 + 4, py as i32 + 6, pw as i32, ph as i32, [0, 0, 0, 90]);
    fb.fill_rect(px as i32, py as i32, pw as i32, ph as i32, theme.sidebar_bg);
    let border = theme.muted;
    fb.fill_rect(px as i32, py as i32, pw as i32, 1, border);
    fb.fill_rect(px as i32, py as i32 + ph as i32 - 1, pw as i32, 1, border);
    fb.fill_rect(px as i32, py as i32, 1, ph as i32, border);
    fb.fill_rect(px as i32 + pw as i32 - 1, py as i32, 1, ph as i32, border);

    // Input box.
    let (ix, iy, iw, ih) = g.input;
    fb.fill_rect(ix as i32, iy as i32, iw as i32, ih as i32, theme.bg);
    fb.fill_rect(ix as i32, iy as i32, iw as i32, 1, theme.muted);
    fb.fill_rect(ix as i32, iy as i32 + ih as i32 - 1, iw as i32, 1, theme.muted);
    fb.fill_rect(ix as i32, iy as i32, 1, ih as i32, theme.muted);
    fb.fill_rect(ix as i32 + iw as i32 - 1, iy as i32, 1, ih as i32, theme.muted);

    // Query text + selection + caret.
    let baseline = iy + ih * 0.5 + QUICK_OPEN_FONT_SIZE * 0.35;
    let text_left = ix + 8.0;
    let text_right = ix + iw - 8.0;
    if qo.query.is_empty() {
        // Placeholder hint — muted. Draw the caret on top so it's visible
        // even with no real text.
        let hint = "Type to filter files…";
        let mut hx = text_left;
        for ch in hint.chars() {
            fb.draw_glyph(&fonts.body, ch, QUICK_OPEN_FONT_SIZE, hx, baseline, theme.muted);
            hx += fonts.body.metrics(ch, QUICK_OPEN_FONT_SIZE).advance_width;
        }
        fb.fill_rect(
            text_left as i32,
            (baseline - QUICK_OPEN_FONT_SIZE * 0.85) as i32,
            1,
            QUICK_OPEN_FONT_SIZE as i32,
            theme.fg,
        );
    } else {
        let sel = [theme.accent[0], theme.accent[1], theme.accent[2], 110];
        draw_text_input(
            fb,
            &qo.query,
            qo.cursor,
            qo.anchor,
            QUICK_OPEN_FONT_SIZE,
            fonts,
            text_left,
            text_right,
            baseline,
            theme.fg,
            sel,
        );
    }

    // Results list.
    let matches = App::quick_open_matches(qo);
    let (lx, ly, lw, lh) = g.list;
    fb.fill_rect(lx as i32, ly as i32, lw as i32, lh as i32, theme.sidebar_bg);

    let hover_row = hover.and_then(|(hx, hy)| quick_open_row_hit(&g, hx, hy));

    let start = qo.scroll;
    let visible = matches.len().saturating_sub(start).min(QUICK_OPEN_ROWS);
    for row in 0..visible {
        let m_idx = matches[start + row];
        let p = &qo.files[m_idx];
        let rel = p.strip_prefix(&qo.base).unwrap_or(p.as_path());
        let rel_str = rel.to_string_lossy();

        let rx = lx;
        let ry = ly + row as f32 * QUICK_OPEN_ROW_H;
        let selected_in_view = qo.selected.checked_sub(start) == Some(row);
        let hovered = hover_row == Some(row);
        if selected_in_view {
            fb.fill_rect(rx as i32, ry as i32, lw as i32, QUICK_OPEN_ROW_H as i32, theme.sidebar_active_bg);
            // Left accent bar makes the selection stand out from hover.
            fb.fill_rect(rx as i32, ry as i32, 3, QUICK_OPEN_ROW_H as i32, theme.accent);
        } else if hovered {
            // Hover tint — muted with low alpha. `sidebar_active_bg` works
            // as a subtle wash on both themes.
            let mut tint = theme.sidebar_active_bg;
            tint[3] = 100;
            fb.fill_rect(rx as i32, ry as i32, lw as i32, QUICK_OPEN_ROW_H as i32, tint);
        }

        // Split filename / directory; draw filename in fg, directory muted.
        let (dir_part, file_part) = match rel.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                let d = parent.to_string_lossy().to_string();
                let f = rel.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                (d, f)
            }
            _ => (String::new(), rel_str.to_string()),
        };

        let bl = ry + QUICK_OPEN_ROW_H * 0.5 + QUICK_OPEN_ROW_FONT_SIZE * 0.35;
        let mut tx = rx + 10.0;
        // filename — bold-ish via fg colour
        for ch in file_part.chars() {
            let f = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
            fb.draw_glyph(f, ch, QUICK_OPEN_ROW_FONT_SIZE, tx, bl, theme.fg);
            tx += f.metrics(ch, QUICK_OPEN_ROW_FONT_SIZE).advance_width;
        }
        if !dir_part.is_empty() {
            tx += 8.0;
            for ch in dir_part.chars() {
                let f = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
                // Stop drawing the directory hint once we'd run past the
                // row's right edge — keeps long paths from bleeding.
                if tx + f.metrics(ch, QUICK_OPEN_ROW_FONT_SIZE).advance_width > rx + lw - 8.0 {
                    break;
                }
                fb.draw_glyph(f, ch, QUICK_OPEN_ROW_FONT_SIZE, tx, bl, theme.muted);
                tx += f.metrics(ch, QUICK_OPEN_ROW_FONT_SIZE).advance_width;
            }
        }
    }

    // Status line: N files, or "no matches".
    let status = if matches.is_empty() {
        if qo.files.is_empty() {
            "No markdown files under root".to_string()
        } else {
            "No matches".to_string()
        }
    } else if qo.query.is_empty() {
        format!("{} files", matches.len())
    } else {
        format!("{} / {} files", matches.len(), qo.files.len())
    };
    let sbl = py + ph - 8.0;
    let mut sx = px + 10.0;
    for ch in status.chars() {
        fb.draw_glyph(&fonts.body, ch, 11.0, sx, sbl, theme.muted);
        sx += fonts.body.metrics(ch, 11.0).advance_width;
    }
}

// ───── context menu ────────────────────────────────────────────────────────

const MENU_ITEM_H: f32 = 28.0;
const MENU_FONT_SIZE: f32 = 14.0;
const MENU_PAD_X: f32 = 14.0;
const MENU_PAD_Y: f32 = 4.0;

fn build_context_menu_items(
    dark: bool,
    copy_path: Option<PathBuf>,
    has_outline: bool,
    copy_selection: Option<String>,
    copy_zone: Option<&crate::layout::CopyZone>,
) -> Vec<(String, MenuAction)> {
    let mut items: Vec<(String, MenuAction)> = Vec::new();
    // Copy actions come first — they're the most common right-click intent.
    if let Some(text) = copy_selection {
        items.push(("Copy text".to_string(), MenuAction::CopyText(text)));
    }
    if let Some(z) = copy_zone {
        // Zones can offer multiple formats (e.g. tables: CSV + Markdown).
        for (label, text) in &z.actions {
            items.push((label.clone(), MenuAction::CopyText(text.clone())));
        }
        // Mermaid zones additionally offer a view-only layout override as a
        // submenu — the inline list got long. The actual entries are
        // attached to the ContextMenu.mermaid_items field; here we only
        // push the trigger row.
        if z.mermaid_block.is_some() {
            items.push(("Layout  ▸".to_string(), MenuAction::MermaidMenu));
        }
    }
    if let Some(p) = copy_path {
        items.push(("Copy path".to_string(), MenuAction::CopyPath(p)));
    }
    if has_outline {
        items.push(("Outline  ▸".to_string(), MenuAction::Outline));
    }
    items.push(("Find…  (Ctrl+F)".to_string(), MenuAction::Find));
    items.push(("Open file…  (Ctrl+P)".to_string(), MenuAction::QuickOpen));
    // Label names the theme the click will *switch to*, not the current one.
    let label = if dark { "Light Theme" } else { "Dark Theme" };
    items.push((label.to_string(), MenuAction::ToggleTheme));
    items
}

/// Turn the heading outline into submenu rows. The leading indent visually
/// nests subsections under their parent heading.
fn outline_to_menu_items(outline: &[crate::layout::OutlineEntry]) -> Vec<(String, MenuAction)> {
    outline.iter().map(|o| {
        let indent: String = "  ".repeat(o.level.saturating_sub(1) as usize);
        (format!("{}{}", indent, o.text), MenuAction::ScrollTo(o.doc_y))
    }).collect()
}

/// Layout-direction rows for the mermaid "Layout ▸" submenu.
/// Subsequence-match score of `query` (already lower-cased) against `path`.
/// Returns `None` if the characters of `query` don't appear in order in
/// `path`. Higher score = better match.
///
/// Scoring:
///   +25 per matched char (baseline — presence is the main thing).
///   +40 when the match lands at the start of a path segment (after `/`
///       or at position 0). Rewards `s/d` → `src/demo.md` over
///       mid-word hits.
///   +15 for consecutive matches (no gap since the last hit). Rewards
///       whole-word typing like `demo` finding `demo.md`.
///   −1 per skipped char between matches (small penalty — keeps the
///       scorer sensitive to tighter matches).
fn fuzzy_score(query: &[char], path: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = path.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0usize;
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut at_seg_start = true; // path[0] is the start of the first segment
    for (ci, &ch) in chars.iter().enumerate() {
        if qi < query.len() && ch == query[qi] {
            score += 25;
            if at_seg_start {
                score += 40;
            }
            if last_match == Some(ci.wrapping_sub(1)) {
                score += 15;
            }
            if let Some(lm) = last_match {
                let gap = ci.saturating_sub(lm + 1);
                score -= gap as i32;
            }
            last_match = Some(ci);
            qi += 1;
        }
        at_seg_start = ch == '/' || ch == std::path::MAIN_SEPARATOR;
    }
    if qi == query.len() { Some(score) } else { None }
}

fn mermaid_layout_items(idx: usize) -> Vec<(String, MenuAction)> {
    use crate::mermaid::Direction::*;
    vec![
        ("Top → Bottom".to_string(), MenuAction::SetMermaidLayout(idx, TopBottom)),
        ("Bottom → Top".to_string(), MenuAction::SetMermaidLayout(idx, BottomTop)),
        ("Left → Right".to_string(), MenuAction::SetMermaidLayout(idx, LeftRight)),
        ("Right → Left".to_string(), MenuAction::SetMermaidLayout(idx, RightLeft)),
        ("Diagonal".to_string(), MenuAction::SetMermaidLayout(idx, Diagonal)),
        ("Reset to source".to_string(), MenuAction::ResetMermaidLayout(idx)),
    ]
}

fn context_menu_width(items: &[(String, MenuAction)], fonts: &Fonts) -> f32 {
    let mut w = 0.0f32;
    for (label, _) in items {
        let lw = measure_text_width(&fonts.body, label, MENU_FONT_SIZE);
        if lw > w {
            w = lw;
        }
    }
    (w + MENU_PAD_X * 2.0).max(140.0)
}

fn context_menu_height(items: &[(String, MenuAction)]) -> f32 {
    MENU_PAD_Y * 2.0 + MENU_ITEM_H * items.len() as f32
}

/// Return the MenuAction at (x, y) or None if outside the menu box.
/// Submenu hits take precedence over main-menu hits.
fn menu_item_hit(m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> Option<MenuAction> {
    if let Some(active) = active_submenu(m, x, y, fonts) {
        if let Some(a) = active.item_hit(m, x, y, fonts) {
            return Some(a);
        }
    }
    let w = context_menu_width(&m.items, fonts);
    let h = context_menu_height(&m.items);
    if x < m.x || x >= m.x + w || y < m.y || y >= m.y + h {
        return None;
    }
    let local_y = y - m.y - MENU_PAD_Y;
    let idx = (local_y / MENU_ITEM_H) as usize;
    m.items.get(idx).map(|(_, a)| a.clone())
}

impl SubmenuKind {
    fn trigger(self) -> MenuAction {
        match self {
            SubmenuKind::Outline => MenuAction::Outline,
            SubmenuKind::Mermaid => MenuAction::MermaidMenu,
        }
    }
    fn items<'a>(self, m: &'a ContextMenu) -> &'a [(String, MenuAction)] {
        match self {
            SubmenuKind::Outline => &m.outline_items,
            SubmenuKind::Mermaid => &m.mermaid_items,
        }
    }
    fn trigger_row(self, m: &ContextMenu) -> Option<usize> {
        let want = self.trigger();
        m.items.iter().position(|(_, a)| std::mem::discriminant(a) == std::mem::discriminant(&want))
    }
    fn anchor(self, m: &ContextMenu, fonts: &Fonts) -> Option<(f32, f32, f32, f32)> {
        let items = self.items(m);
        if items.is_empty() { return None; }
        let row = self.trigger_row(m)?;
        let main_w = context_menu_width(&m.items, fonts);
        let sw = submenu_width(items, fonts);
        let sh = submenu_height(items);
        let sx = m.x + main_w - 2.0;
        let sy = m.y + MENU_PAD_Y + row as f32 * MENU_ITEM_H - MENU_PAD_Y;
        Some((sx, sy, sw, sh))
    }
    fn is_hovered(self, m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> bool {
        // Active when the cursor is on this kind's trigger row OR inside
        // its submenu panel. `active_submenu` uses this to pick exactly
        // one submenu at a time.
        if let Some((sx, sy, sw, sh)) = self.anchor(m, fonts) {
            if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                return true;
            }
        }
        if let Some(row) = self.trigger_row(m) {
            let main_w = context_menu_width(&m.items, fonts);
            let ry = m.y + MENU_PAD_Y + row as f32 * MENU_ITEM_H;
            if x >= m.x && x < m.x + main_w && y >= ry && y < ry + MENU_ITEM_H {
                return true;
            }
        }
        false
    }
    fn item_hit(self, m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> Option<MenuAction> {
        let (sx, sy, sw, sh) = self.anchor(m, fonts)?;
        if x < sx || x >= sx + sw || y < sy || y >= sy + sh {
            return None;
        }
        let local_y = y - sy - MENU_PAD_Y;
        if local_y < 0.0 { return None; }
        let idx = (local_y / MENU_ITEM_H) as usize;
        self.items(m).get(idx).map(|(_, a)| a.clone())
    }
}

/// Decide which submenu (if any) should be visible given the cursor
/// position. Sticky: once a trigger row has been hovered, that submenu
/// stays open while the cursor is still inside *its* trigger row or
/// panel — even if the cursor is also inside another submenu's panel
/// rect. A trigger hover on the other kind switches.
fn active_submenu(m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> Option<SubmenuKind> {
    // Priority 1: cursor on a trigger row → that submenu wins outright.
    for k in [SubmenuKind::Outline, SubmenuKind::Mermaid] {
        if let Some(row) = k.trigger_row(m) {
            let main_w = context_menu_width(&m.items, fonts);
            let ry = m.y + MENU_PAD_Y + row as f32 * MENU_ITEM_H;
            if x >= m.x && x < m.x + main_w && y >= ry && y < ry + MENU_ITEM_H {
                return Some(k);
            }
        }
    }
    // Priority 2: stay with the previously-active submenu while the
    // cursor is still inside its panel rect.
    if let Some(current) = m.active_submenu {
        if let Some((sx, sy, sw, sh)) = current.anchor(m, fonts) {
            if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                return Some(current);
            }
        }
    }
    // Priority 3: fall back to whichever panel the cursor is in (first match).
    for k in [SubmenuKind::Outline, SubmenuKind::Mermaid] {
        if let Some((sx, sy, sw, sh)) = k.anchor(m, fonts) {
            if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                return Some(k);
            }
        }
    }
    None
}

/// Strip newlines/tabs and other control chars from clipboard text
/// before it lands in a single-line text input. Pasting a multi-line
/// snippet would otherwise put hidden newlines in a search query that
/// then never matches.
fn sanitise_clipboard_for_input(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn submenu_width(items: &[(String, MenuAction)], fonts: &Fonts) -> f32 {
    let mut w = 0.0f32;
    for (label, _) in items {
        let lw = measure_text_width(&fonts.body, label, MENU_FONT_SIZE);
        if lw > w { w = lw; }
    }
    (w + MENU_PAD_X * 2.0).max(160.0)
}

fn submenu_height(items: &[(String, MenuAction)]) -> f32 {
    MENU_PAD_Y * 2.0 + MENU_ITEM_H * items.len() as f32
}

/// Open the Ctrl+P quick-open panel and spawn a background walker that
/// streams md-file results into it. Idempotent: a no-op if the panel is
/// already open.
///
/// The walker runs entirely off the UI thread so even pointing it at a
/// huge tree (`$HOME`, `~/projects`, …) leaves the window fully
/// responsive — the user can keep scrolling, typing, or Esc out while
/// files are still being discovered.
fn launch_quick_open(shared: &Arc<Shared>, proxy: &EventLoopProxy<UserEvent>) {
    let (root, base, generation, cancel) = {
        let mut s = shared.state.lock().unwrap();
        if s.quick_open.is_some() {
            return;
        }
        let (root, base) = if let Some(t) = &s.tree {
            (t.root.clone(), t.root.clone())
        } else {
            let cwd = std::env::current_dir().unwrap_or_default();
            (cwd.clone(), cwd)
        };
        s.quick_open_seq = s.quick_open_seq.wrapping_add(1);
        let generation = s.quick_open_seq;
        let cancel = Arc::new(AtomicBool::new(false));
        s.quick_open = Some(QuickOpenUi {
            query: String::new(),
            cursor: 0,
            anchor: 0,
            files: Vec::new(),
            base,
            selected: 0,
            scroll: 0,
            scanning: true,
            cancel: cancel.clone(),
            generation,
        });
        let base = s.quick_open.as_ref().map(|q| q.base.clone()).unwrap_or_default();
        (root, base, generation, cancel)
    };
    let _ = base; // base is captured into QuickOpenUi above; binding kept for symmetry
    let shared_for_worker = shared.clone();
    let proxy_for_worker = proxy.clone();
    std::thread::Builder::new()
        .name("mdrdr-quickopen".into())
        .spawn(move || {
            crate::tree::walk_streaming(&root, &cancel, |batch| {
                let mut s = shared_for_worker.state.lock().unwrap();
                let Some(qo) = s.quick_open.as_mut() else { return false };
                if qo.generation != generation {
                    return false;
                }
                qo.files.extend(batch);
                drop(s);
                let _ = proxy_for_worker.send_event(UserEvent::Redraw);
                true
            });
            // Final tidy-up: sort what we found, mark scan complete.
            let mut s = shared_for_worker.state.lock().unwrap();
            if let Some(qo) = s.quick_open.as_mut() {
                if qo.generation == generation {
                    qo.files.sort();
                    qo.scanning = false;
                }
            }
            drop(s);
            let _ = proxy_for_worker.send_event(UserEvent::Redraw);
        })
        .ok();
}

fn apply_menu_action(shared: &Arc<Shared>, proxy: &EventLoopProxy<UserEvent>, action: &MenuAction) {
    match action {
        MenuAction::ToggleTheme => {
            let mut s = shared.state.lock().unwrap();
            s.dark = !s.dark;
        }
        MenuAction::CopyPath(path) => {
            clipboard::copy(&path.to_string_lossy());
        }
        MenuAction::CopyText(text) => {
            clipboard::copy(text);
        }
        MenuAction::Outline | MenuAction::MermaidMenu => { /* submenu trigger only — handled at hit-test */ }
        MenuAction::Find => {
            // Open the search overlay at the mouse position, mirroring
            // what Ctrl+F does.
            let mut s = shared.state.lock().unwrap();
            if s.search.is_none() {
                let vw = s.viewport.width as f32;
                let vh = s.viewport.height as f32;
                let mx = s.last_mouse.x as f32;
                let my = s.last_mouse.y as f32;
                let x = (mx - SEARCH_DRAG_W * 0.5).clamp(0.0, (vw - SEARCH_PANEL_W).max(0.0));
                let y = (my - SEARCH_PANEL_H * 0.5).clamp(0.0, (vh - SEARCH_PANEL_H).max(0.0));
                s.search = Some(SearchUi {
                    query: String::new(),
                    cursor: 0,
                    anchor: 0,
                    current: 0,
                    match_count: 0,
                    x,
                    y,
                    drag_grip: None,
                });
            }
        }
        MenuAction::QuickOpen => {
            // Mirror Ctrl+P: open the quick-open panel and let its
            // background walker stream results in.
            launch_quick_open(shared, proxy);
        }
        MenuAction::SetMermaidLayout(idx, dir) => {
            let mut s = shared.state.lock().unwrap();
            let key = (s.source_path.clone(), *idx);
            s.mermaid_overrides.insert(key, *dir);
        }
        MenuAction::ResetMermaidLayout(idx) => {
            let mut s = shared.state.lock().unwrap();
            let key = (s.source_path.clone(), *idx);
            s.mermaid_overrides.remove(&key);
        }
        MenuAction::ScrollTo(doc_y) => {
            // Mirror the HitAction::ScrollTo flow in click_at so clamping
            // against the real doc height stays consistent.
            let theme = Theme::light();
            let (source, vw, vh, base_dir, sidebar_w, content_zoom, tcw, tcox) = {
                let s = shared.state.lock().unwrap();
                (
                    s.source.clone(),
                    s.viewport.width,
                    s.viewport.height as f32,
                    s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                    s.sidebar_width,
                    s.content_zoom,
                    s.text_column_width,
                    s.text_column_offset_x,
                )
            };
            let mut images = shared.images.lock().unwrap();
            let doc_h = measure(
                &source, vw, vh as u32,
                base_dir.as_deref(), sidebar_w, content_zoom,
                tcw, tcox,
                &theme, &shared.fonts, &mut images,
            );
            drop(images);
            let max_scroll = (doc_h - vh).max(0.0);
            let mut s = shared.state.lock().unwrap();
            s.scroll = (*doc_y - 8.0).clamp(0.0, max_scroll);
            // Pull the read cursor along to the new section so the
            // reader token marks where the user just jumped.
            s.read_cursor = Some(*doc_y);
        }
    }
}

/// Paint the menu (background panel + border + items) on top of the
/// framebuffer. Highlights the hovered row so the selection is visible
/// before committing a click.
fn draw_context_menu(
    fb: &mut crate::render::Framebuffer,
    theme: &Theme,
    fonts: &Fonts,
    m: &ContextMenu,
    hover: Option<(f32, f32)>,
) {
    // Main panel — highlight either the hovered row or the keyboard
    // selection. `draw_menu_panel` falls back to `keyboard_selected`
    // only when the cursor isn't on a row, so passing `m.selected`
    // unconditionally is safe; the cursor-moved handler also clears
    // `m.selected` the moment the mouse enters the menu.
    draw_menu_panel(fb, theme, fonts, m.x, m.y, &m.items, hover, m.selected);

    // Submenu — drawn when the keyboard has opened it OR the cursor is
    // over a trigger row / submenu panel.
    let kb_kind = m.submenu_selected.map(|(k, _)| k);
    let mouse_kind = hover.and_then(|(hx, hy)| active_submenu(m, hx, hy, fonts));
    if let Some(kind) = kb_kind.or(mouse_kind) {
        if let Some((sx, sy, _sw, _sh)) = kind.anchor(m, fonts) {
            let sub_kb = m.submenu_selected.and_then(|(k, i)| if k == kind { Some(i) } else { None });
            draw_submenu_panel(fb, theme, fonts, sx, sy, kind.items(m), hover, sub_kb);
        }
    }
}

fn draw_menu_panel(
    fb: &mut crate::render::Framebuffer,
    theme: &Theme,
    fonts: &Fonts,
    mx: f32,
    my: f32,
    items: &[(String, MenuAction)],
    hover: Option<(f32, f32)>,
    keyboard_selected: Option<usize>,
) {
    let w = context_menu_width(items, fonts);
    let h = context_menu_height(items);
    let x0 = mx as i32;
    let y0 = my as i32;
    let wi = w.ceil() as i32;
    let hi = h.ceil() as i32;

    let shadow: crate::theme::Rgba = [0, 0, 0, 60];
    fb.fill_rect(x0 + 2, y0 + 3, wi, hi, shadow);
    fb.fill_rect(x0, y0, wi, hi, theme.sidebar_bg);
    let border = theme.muted;
    fb.fill_rect(x0, y0, wi, 1, border);
    fb.fill_rect(x0, y0 + hi - 1, wi, 1, border);
    fb.fill_rect(x0, y0, 1, hi, border);
    fb.fill_rect(x0 + wi - 1, y0, 1, hi, border);

    let hover_idx = hover.and_then(|(hx, hy)| {
        if hx < mx || hx >= mx + w || hy < my || hy >= my + h {
            return None;
        }
        let local_y = hy - my - MENU_PAD_Y;
        let idx = (local_y / MENU_ITEM_H) as usize;
        if idx < items.len() { Some(idx) } else { None }
    });
    let highlight = hover_idx.or(keyboard_selected);

    for (i, (label, _)) in items.iter().enumerate() {
        let iy = my + MENU_PAD_Y + MENU_ITEM_H * i as f32;
        if Some(i) == highlight {
            fb.fill_rect(
                (mx + 1.0) as i32,
                iy as i32,
                (w - 2.0) as i32,
                MENU_ITEM_H as i32,
                theme.sidebar_active_bg,
            );
        }
        let baseline = iy + MENU_ITEM_H * 0.5 + MENU_FONT_SIZE * 0.35;
        let mut cx = mx + MENU_PAD_X;
        for ch in label.chars() {
            let font = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
            fb.draw_glyph(font, ch, MENU_FONT_SIZE, cx, baseline, theme.fg);
            cx += font.metrics(ch, MENU_FONT_SIZE).advance_width;
        }
    }
}

fn draw_submenu_panel(
    fb: &mut crate::render::Framebuffer,
    theme: &Theme,
    fonts: &Fonts,
    sx: f32,
    sy: f32,
    items: &[(String, MenuAction)],
    hover: Option<(f32, f32)>,
    keyboard_selected: Option<usize>,
) {
    let w = submenu_width(items, fonts);
    let h = submenu_height(items);
    let x0 = sx as i32;
    let y0 = sy as i32;
    let wi = w.ceil() as i32;
    let hi = h.ceil() as i32;

    let shadow: crate::theme::Rgba = [0, 0, 0, 60];
    fb.fill_rect(x0 + 2, y0 + 3, wi, hi, shadow);
    fb.fill_rect(x0, y0, wi, hi, theme.sidebar_bg);
    let border = theme.muted;
    fb.fill_rect(x0, y0, wi, 1, border);
    fb.fill_rect(x0, y0 + hi - 1, wi, 1, border);
    fb.fill_rect(x0, y0, 1, hi, border);
    fb.fill_rect(x0 + wi - 1, y0, 1, hi, border);

    let hover_idx = hover.and_then(|(hx, hy)| {
        if hx < sx || hx >= sx + w || hy < sy || hy >= sy + h {
            return None;
        }
        let local_y = hy - sy - MENU_PAD_Y;
        if local_y < 0.0 { return None; }
        let idx = (local_y / MENU_ITEM_H) as usize;
        if idx < items.len() { Some(idx) } else { None }
    });
    let highlight = hover_idx.or(keyboard_selected);

    for (i, (label, _)) in items.iter().enumerate() {
        let iy = sy + MENU_PAD_Y + MENU_ITEM_H * i as f32;
        if Some(i) == highlight {
            fb.fill_rect(
                (sx + 1.0) as i32,
                iy as i32,
                (w - 2.0) as i32,
                MENU_ITEM_H as i32,
                theme.sidebar_active_bg,
            );
        }
        let baseline = iy + MENU_ITEM_H * 0.5 + MENU_FONT_SIZE * 0.35;
        let mut cx = sx + MENU_PAD_X;
        for ch in label.chars() {
            let font = if crate::font::is_emoji(ch) { &fonts.emoji } else { &fonts.body };
            fb.draw_glyph(font, ch, MENU_FONT_SIZE, cx, baseline, theme.fg);
            cx += font.metrics(ch, MENU_FONT_SIZE).advance_width;
        }
    }
}

/// Turn the CLI argument into (initial source, opened path, file tree root).
/// - No arg → tree at cwd, no file open.
/// - File   → read it, tree at its parent.
/// - Dir    → tree at dir, no file open (user clicks to select).
fn resolve_open_arg(arg: Option<PathBuf>) -> (String, Option<PathBuf>, Option<FileTree>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match arg {
        None => (String::new(), None, Some(FileTree::new(cwd))),
        Some(p) => {
            let p = p.canonicalize().unwrap_or(p);
            if p.is_dir() {
                (String::new(), None, Some(FileTree::new(p)))
            } else if p.is_file() {
                let src = std::fs::read_to_string(&p).unwrap_or_default();
                let root = p.parent().map(Path::to_path_buf).unwrap_or(cwd);
                (src, Some(p), Some(FileTree::new(root)))
            } else {
                (String::new(), None, Some(FileTree::new(cwd)))
            }
        }
    }
}
