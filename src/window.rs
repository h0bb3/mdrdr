//! Window mode: winit event loop + softbuffer framebuffer push.
//! Also spawns the HTTP API so the window can be driven externally.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
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
    /// Scroll the document to this doc-y. Used by outline submenu entries.
    ScrollTo(f32),
    /// Put this text on the clipboard. Used by "Copy text" (selection),
    /// "Copy code" (code block), and "Copy table as CSV" (table).
    CopyText(String),
}

/// A context menu floating near the cursor. Coordinates are the top-left in
/// screen space. Items are laid out top-to-bottom in insertion order.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub x: f32,
    pub y: f32,
    pub items: Vec<(String, MenuAction)>,
    /// Entries for the "Outline ▸" submenu. Empty → outline row is hidden.
    /// Each entry is (indented_label, doc_y).
    pub outline_items: Vec<(String, f32)>,
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

    /// (time, x, y, path) of the most recent tree-folder click. Used to
    /// promote a second click on the same folder within the double-click
    /// window to a SetRoot (enter directory) action.
    pub last_folder_click: Option<(std::time::Instant, f32, f32, PathBuf)>,

    /// When true, Theme::dark() is used for rendering instead of light.
    pub dark: bool,

    /// Active right-click context menu. `None` when closed.
    pub context_menu: Option<ContextMenu>,
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
            selection,
            hover_pos,
            context_menu: s.context_menu.clone(),
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
    pub selection: Option<((f32, f32), (f32, f32))>,
    /// Mouse position in screen coords, but only when the window is in a
    /// "quiet" state — not dragging, not selecting. Drawn hover highlights
    /// flicker if left on during active interaction.
    pub hover_pos: Option<(f32, f32)>,
    pub context_menu: Option<ContextMenu>,
}

struct App {
    shared: Arc<Shared>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    proxy: EventLoopProxy<UserEvent>,
    modifiers: Modifiers,
    /// Cached last-set cursor so we don't spam `set_cursor` every mouse move.
    cursor: CursorIcon,
    /// Last hit-target rect under the pointer (screen coords for pinned,
    /// doc coords for content). Used to detect the need to repaint the
    /// hover highlight when crossing between adjacent clickable rects.
    last_hover_rect: Option<(f32, f32, f32, f32)>,
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
                let ctrl = self.modifiers.state().control_key();
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
                let (pos, sidebar_w, tree_visible, scroll, viewport, menu) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.last_mouse, s.sidebar_width, s.tree.is_some(), s.scroll, s.viewport, s.context_menu.clone())
                };
                let x = pos.x as f32;
                let y = pos.y as f32;

                // 0. Context menu — if one is open, it captures this click.
                //    Hit inside → execute item. Hit outside → just close.
                //    Clicking the "Outline ▸" row leaves the menu open so
                //    the submenu stays reachable.
                if let Some(m) = menu.as_ref() {
                    let hit = menu_item_hit(m, x, y, &self.shared.fonts);
                    match hit {
                        Some(MenuAction::Outline) => {
                            // keep menu open
                            self.request_redraw();
                        }
                        Some(action) => {
                            {
                                let mut s = self.shared.state.lock().unwrap();
                                s.context_menu = None;
                            }
                            apply_menu_action(&self.shared, &action);
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
                    s.context_menu = Some(ContextMenu { x: mx, y: my, items, outline_items });
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
                    click_at(&self.shared, last_mouse.x as f32, last_mouse.y as f32);
                    self.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods;
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
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
                        let (sidebar_w, content_zoom) = {
                            let s = self.shared.state.lock().unwrap();
                            (s.sidebar_width, s.content_zoom)
                        };
                        let mut images = self.shared.images.lock().unwrap();
                        let doc_h = measure(
                            &source,
                            vw,
                            vh as u32,
                            base_dir.as_deref(),
                            sidebar_w,
                            content_zoom,
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
                    Key::Character(c) if c == "b" && !self.modifiers.state().control_key() => {
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
                    Key::Character(c) if c == "c" && self.modifiers.state().control_key() => {
                        self.copy_selection();
                        None
                    }
                    Key::Character(c) if c == "t" && !self.modifiers.state().control_key() => {
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
        }
    }
}

impl App {
    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn clamp_scroll(&self) {
        let theme = Theme::light();
        let (source, vw, vh, base_dir, sidebar_w, content_zoom) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport.width,
                s.viewport.height as f32,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
                s.content_zoom,
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
            },
            &mut images,
        )?;
        if text.is_empty() { None } else { Some(text) }
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
        let (source, viewport, base_dir, sidebar_w, scroll, content_zoom) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
                s.scroll,
                s.content_zoom,
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
            },
            &mut images,
        );
        drop(images);

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
        HitAction::ScrollTo(y) => {
            // Clamp against the actual doc height so clicking "Smallest
            // heading" near the end doesn't scroll past the bottom.
            let theme = Theme::light();
            let (source, vw, vh, base_dir, sidebar_w, content_zoom) = {
                let s = shared.state.lock().unwrap();
                (
                    s.source.clone(),
                    s.viewport.width,
                    s.viewport.height as f32,
                    s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                    s.sidebar_width,
                    s.content_zoom,
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
                &theme,
                &shared.fonts,
                &mut images,
            );
            drop(images);
            let max_scroll = (doc_h - vh).max(0.0);
            let mut s = shared.state.lock().unwrap();
            s.scroll = (*y - 8.0).clamp(0.0, max_scroll);
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

pub fn run(arg: Option<PathBuf>) -> ExitCode {
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
            last_folder_click: None,
            dark: false,
            context_menu: None,
        }),
    });

    let event_loop: EventLoop<UserEvent> = EventLoop::with_user_event().build().unwrap();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let port = api::spawn(shared.clone(), proxy.clone());
    println!("mdrdr api listening on http://127.0.0.1:{port}");

    crate::watch::spawn(shared.clone(), proxy.clone());

    let mut app = App {
        shared,
        window: None,
        surface: None,
        proxy,
        modifiers: Modifiers::default(),
        cursor: CursorIcon::Default,
        last_hover_rect: None,
    };

    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
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
        let (label, text) = match z.kind {
            crate::layout::CopyKind::Code => ("Copy code", z.text.clone()),
            crate::layout::CopyKind::Csv => ("Copy table as CSV", z.text.clone()),
        };
        items.push((label.to_string(), MenuAction::CopyText(text)));
    }
    if let Some(p) = copy_path {
        items.push(("Copy path".to_string(), MenuAction::CopyPath(p)));
    }
    if has_outline {
        items.push(("Outline  ▸".to_string(), MenuAction::Outline));
    }
    // Label names the theme the click will *switch to*, not the current one.
    let label = if dark { "Light Theme" } else { "Dark Theme" };
    items.push((label.to_string(), MenuAction::ToggleTheme));
    items
}

/// Turn the heading outline into submenu rows. The leading indent visually
/// nests subsections under their parent heading.
fn outline_to_menu_items(outline: &[crate::layout::OutlineEntry]) -> Vec<(String, f32)> {
    outline.iter().map(|o| {
        let indent: String = "  ".repeat(o.level.saturating_sub(1) as usize);
        (format!("{}{}", indent, o.text), o.doc_y)
    }).collect()
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
    if submenu_open(m, x, y, fonts) {
        if let Some(a) = submenu_item_hit(m, x, y, fonts) {
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

/// Index of the "Outline ▸" row in the main menu, if present.
fn outline_row_index(m: &ContextMenu) -> Option<usize> {
    m.items.iter().position(|(_, a)| matches!(a, MenuAction::Outline))
}

fn submenu_anchor(m: &ContextMenu, fonts: &Fonts) -> Option<(f32, f32, f32, f32)> {
    let idx = outline_row_index(m)?;
    if m.outline_items.is_empty() {
        return None;
    }
    let main_w = context_menu_width(&m.items, fonts);
    let sw = submenu_width(&m.outline_items, fonts);
    let sh = submenu_height(&m.outline_items);
    // Right side of the main menu. (Assumes enough room — if off-screen
    // we'd flip to the left; keep it simple for now.)
    let sx = m.x + main_w - 2.0;
    let sy = m.y + MENU_PAD_Y + idx as f32 * MENU_ITEM_H - MENU_PAD_Y;
    Some((sx, sy, sw, sh))
}

fn submenu_width(items: &[(String, f32)], fonts: &Fonts) -> f32 {
    let mut w = 0.0f32;
    for (label, _) in items {
        let lw = measure_text_width(&fonts.body, label, MENU_FONT_SIZE);
        if lw > w { w = lw; }
    }
    (w + MENU_PAD_X * 2.0).max(160.0)
}

fn submenu_height(items: &[(String, f32)]) -> f32 {
    MENU_PAD_Y * 2.0 + MENU_ITEM_H * items.len() as f32
}

/// True when the outline row is hovered, OR the submenu box is hovered.
fn submenu_open(m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> bool {
    let Some((sx, sy, sw, sh)) = submenu_anchor(m, fonts) else { return false };
    if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
        return true;
    }
    // Or over the main-menu "Outline" row.
    if let Some(idx) = outline_row_index(m) {
        let main_w = context_menu_width(&m.items, fonts);
        let row_y = m.y + MENU_PAD_Y + idx as f32 * MENU_ITEM_H;
        if x >= m.x && x < m.x + main_w && y >= row_y && y < row_y + MENU_ITEM_H {
            return true;
        }
    }
    false
}

fn submenu_item_hit(m: &ContextMenu, x: f32, y: f32, fonts: &Fonts) -> Option<MenuAction> {
    let (sx, sy, sw, sh) = submenu_anchor(m, fonts)?;
    if x < sx || x >= sx + sw || y < sy || y >= sy + sh {
        return None;
    }
    let local_y = y - sy - MENU_PAD_Y;
    if local_y < 0.0 { return None; }
    let idx = (local_y / MENU_ITEM_H) as usize;
    m.outline_items.get(idx).map(|(_, doc_y)| MenuAction::ScrollTo(*doc_y))
}

fn apply_menu_action(shared: &Arc<Shared>, action: &MenuAction) {
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
        MenuAction::Outline => { /* submenu trigger only — handled at hit-test */ }
        MenuAction::ScrollTo(doc_y) => {
            // Mirror the HitAction::ScrollTo flow in click_at so clamping
            // against the real doc height stays consistent.
            let theme = Theme::light();
            let (source, vw, vh, base_dir, sidebar_w, content_zoom) = {
                let s = shared.state.lock().unwrap();
                (
                    s.source.clone(),
                    s.viewport.width,
                    s.viewport.height as f32,
                    s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                    s.sidebar_width,
                    s.content_zoom,
                )
            };
            let mut images = shared.images.lock().unwrap();
            let doc_h = measure(
                &source, vw, vh as u32,
                base_dir.as_deref(), sidebar_w, content_zoom,
                &theme, &shared.fonts, &mut images,
            );
            drop(images);
            let max_scroll = (doc_h - vh).max(0.0);
            let mut s = shared.state.lock().unwrap();
            s.scroll = (*doc_y - 8.0).clamp(0.0, max_scroll);
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
    draw_menu_panel(fb, theme, fonts, m.x, m.y, &m.items, hover);

    // Submenu, if the hover falls on the trigger row or inside the panel.
    if let Some((hx, hy)) = hover {
        if !m.outline_items.is_empty() && submenu_open(m, hx, hy, fonts) {
            if let Some((sx, sy, _sw, _sh)) = submenu_anchor(m, fonts) {
                draw_submenu_panel(fb, theme, fonts, sx, sy, &m.outline_items, hover);
            }
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

    for (i, (label, _)) in items.iter().enumerate() {
        let iy = my + MENU_PAD_Y + MENU_ITEM_H * i as f32;
        if Some(i) == hover_idx {
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
    items: &[(String, f32)],
    hover: Option<(f32, f32)>,
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

    for (i, (label, _)) in items.iter().enumerate() {
        let iy = sy + MENU_PAD_Y + MENU_ITEM_H * i as f32;
        if Some(i) == hover_idx {
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
