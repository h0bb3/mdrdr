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
    in_sidebar_scrollbar_strip, measure, render, scrollbar_geom, sidebar_content_height,
    sidebar_scrollbar_geom, RenderInput, SbGeom, Viewport,
};
use crate::theme::Theme;
use crate::tree::FileTree;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Redraw,
    Quit,
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
            theme: Theme::light(),
            sidebar_width: s.sidebar_width,
            sidebar_scroll: s.sidebar_scroll,
            content_zoom: s.content_zoom,
            sidebar_zoom: s.sidebar_zoom,
            selection,
            hover_pos,
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
                let (dragging_sidebar, dragging_scrollbar, dragging_sb_sidebar, selecting, scroll, grip, sb_sidebar_grip, sidebar_w, tree_visible, viewport) = {
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
                    )
                };
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
                let (pos, sidebar_w, tree_visible, scroll, viewport) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.last_mouse, s.sidebar_width, s.tree.is_some(), s.scroll, s.viewport)
                };
                let x = pos.x as f32;
                let y = pos.y as f32;

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

                // 2. Sidebar's internal scrollbar strip (intercept before
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

                // 3. Sidebar's right-edge drag strip (resize sidebar width).
                if tree_visible && sidebar_w > 0.0 && (x - sidebar_w).abs() <= 6.0 {
                    let mut s = self.shared.state.lock().unwrap();
                    s.sidebar_dragging = true;
                } else if x < sidebar_w {
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
        let fb = render(
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
