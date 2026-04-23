//! Window mode: winit event loop + softbuffer framebuffer push.
//! Also spawns the HTTP API so the window can be driven externally.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::api;
use crate::font::Fonts;
use crate::render::{render, Viewport};
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
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
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
