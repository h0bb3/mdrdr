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
use winit::window::{Window, WindowId};

use crate::api;
use crate::clipboard;
use crate::font::Fonts;
use crate::images::ImageCache;
use crate::layout::HitAction;
use crate::render::{compute_hit_targets, extract_selection, hit_test, measure, render, RenderInput, Viewport};
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

    /// Selection anchor and head in *document* coordinates (x from the left
    /// of the window including sidebar, y in the unscrolled document).
    /// `None` on both means no selection.
    pub sel_anchor: Option<(f32, f32)>,
    pub sel_head: Option<(f32, f32)>,
    pub is_selecting: bool,
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
        Snapshot {
            source: s.source.clone(),
            source_path: s.source_path.clone(),
            scroll: s.scroll,
            viewport: s.viewport,
            tree_flat: s.tree.as_ref().map(|t| t.flatten()),
            theme: Theme::light(),
            sidebar_width: s.sidebar_width,
            selection,
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
    pub selection: Option<((f32, f32), (f32, f32))>,
}

struct App {
    shared: Arc<Shared>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    proxy: EventLoopProxy<UserEvent>,
    modifiers: Modifiers,
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
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines * 40.0,
                    MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
                };
                {
                    let mut s = self.shared.state.lock().unwrap();
                    s.scroll = (s.scroll + dy).max(0.0);
                }
                self.clamp_scroll();
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                let (was_dragging_sidebar, was_selecting, scroll) = {
                    let mut s = self.shared.state.lock().unwrap();
                    s.last_mouse = position;
                    (s.sidebar_dragging, s.is_selecting, s.scroll)
                };
                if was_dragging_sidebar {
                    let new_w = (position.x as f32).clamp(120.0, 640.0);
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sidebar_width = new_w;
                        s.sidebar_width_restore = new_w;
                    }
                    self.request_redraw();
                } else if was_selecting {
                    let doc = (position.x as f32, position.y as f32 + scroll);
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sel_head = Some(doc);
                    }
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (pos, sidebar_w, tree_visible, scroll) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.last_mouse, s.sidebar_width, s.tree.is_some(), s.scroll)
                };
                let x = pos.x as f32;
                let y = pos.y as f32;

                // Sidebar's right-edge drag strip.
                if tree_visible && sidebar_w > 0.0 && (x - sidebar_w).abs() <= 6.0 {
                    let mut s = self.shared.state.lock().unwrap();
                    s.sidebar_dragging = true;
                } else if x < sidebar_w {
                    // Inside sidebar — treat as a tree click.
                    {
                        let mut s = self.shared.state.lock().unwrap();
                        s.sel_anchor = None;
                        s.sel_head = None;
                    }
                    click_at(&self.shared, x, y);
                } else {
                    // Content area — start a text selection.
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
                let mut s = self.shared.state.lock().unwrap();
                s.sidebar_dragging = false;
                s.is_selecting = false;
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
                        let sidebar_w = self.shared.state.lock().unwrap().sidebar_width;
                        let mut images = self.shared.images.lock().unwrap();
                        let doc_h = measure(
                            &source,
                            vw,
                            vh as u32,
                            base_dir.as_deref(),
                            sidebar_w,
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
        let (source, vw, vh, base_dir, sidebar_w) = {
            let s = self.shared.state.lock().unwrap();
            (
                s.source.clone(),
                s.viewport.width,
                s.viewport.height as f32,
                s.source_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf()),
                s.sidebar_width,
            )
        };
        let mut images = self.shared.images.lock().unwrap();
        let doc_h = measure(
            &source,
            vw,
            vh as u32,
            base_dir.as_deref(),
            sidebar_w,
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
                    selection: snap.selection,
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
                selection: snap.selection,
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
    let targets = {
        let mut images = shared.images.lock().unwrap();
        compute_hit_targets(
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
                selection: None,
            },
            &mut images,
        )
    };
    let Some(hit) = hit_test(&targets, x, y) else {
        return None;
    };
    let action = hit.action.clone();
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
    }
    Some(action)
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
            sel_anchor: None,
            sel_head: None,
            is_selecting: false,
        }),
    });

    let event_loop: EventLoop<UserEvent> = EventLoop::with_user_event().build().unwrap();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let port = api::spawn(shared.clone(), proxy.clone());
    println!("mdrdr api listening on http://127.0.0.1:{port}");

    crate::watch::spawn(shared.clone(), proxy.clone());

    let mut app = App { shared, window: None, surface: None, proxy, modifiers: Modifiers::default() };

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
