//! Window mode: winit event loop + softbuffer framebuffer push.
//! Also spawns the HTTP API so the window can be driven externally.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::api;
use crate::font::Fonts;
use crate::render::{measure, render, Viewport};
use crate::theme::Theme;

/// Messages API thread pushes back into the winit event loop.
#[derive(Debug, Clone)]
pub enum UserEvent {
    Redraw,
    Quit,
}

/// State the API thread and the render thread both need to touch.
pub struct AppState {
    pub source: String,
    pub source_path: Option<PathBuf>,
    pub scroll: f32,
    pub viewport: Viewport,
}

pub struct Shared {
    pub fonts: Fonts,
    pub state: Mutex<AppState>,
}

impl Shared {
    pub fn snapshot(&self) -> (String, f32, Viewport, Theme) {
        let s = self.state.lock().unwrap();
        (s.source.clone(), s.scroll, s.viewport, Theme::light())
    }
}

struct App {
    shared: Arc<Shared>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
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

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                let (vh, source) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.viewport.height as f32, s.source.clone())
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
                        let vw = {
                            let s = self.shared.state.lock().unwrap();
                            s.viewport.width
                        };
                        let doc_h = measure(&source, vw, &theme, &self.shared.fonts);
                        let mut s = self.shared.state.lock().unwrap();
                        s.scroll = (doc_h - s.viewport.height as f32).max(0.0);
                        None
                    }
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
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
            UserEvent::Redraw => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
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
        let (source, vw, vh) = {
            let s = self.shared.state.lock().unwrap();
            (s.source.clone(), s.viewport.width, s.viewport.height as f32)
        };
        let doc_h = measure(&source, vw, &theme, &self.shared.fonts);
        let max_scroll = (doc_h - vh).max(0.0);
        let mut s = self.shared.state.lock().unwrap();
        if s.scroll > max_scroll {
            s.scroll = max_scroll;
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

        // Render through the pure core.
        let (source, scroll, _viewport, theme) = self.shared.snapshot();
        let fb = render(
            &source,
            Viewport { width: w, height: h },
            scroll,
            &theme,
            &self.shared.fonts,
        );

        // RGBA8 -> softbuffer u32 (0x00RRGGBB)
        let mut buffer = surface.buffer_mut().unwrap();
        for (i, px) in fb.pixels.chunks_exact(4).enumerate() {
            let r = px[0] as u32;
            let g = px[1] as u32;
            let b = px[2] as u32;
            buffer[i] = (r << 16) | (g << 8) | b;
        }
        buffer.present().unwrap();
    }
}

pub fn run(file: Option<PathBuf>) -> ExitCode {
    let fonts = Fonts::load();
    let viewport = Viewport { width: 1200, height: 900 };

    let (source, source_path) = match file {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => (s, Some(p)),
            Err(e) => {
                eprintln!("could not read {}: {}", p.display(), e);
                (String::new(), Some(p))
            }
        },
        None => (String::new(), None),
    };

    let shared = Arc::new(Shared {
        fonts,
        state: Mutex::new(AppState { source, source_path, scroll: 0.0, viewport }),
    });

    let event_loop: EventLoop<UserEvent> = EventLoop::with_user_event().build().unwrap();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let port = api::spawn(shared.clone(), proxy);
    println!("mdrdr api listening on http://127.0.0.1:{port}");

    let mut app = App { shared, window: None, surface: None };

    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}
