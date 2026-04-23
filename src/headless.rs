//! `mdrdr render` — write a PNG of the current render core output, then exit.
//! No window, no event loop. This is the primary iteration loop for development.

use std::path::Path;

use crate::font::Fonts;
use crate::render::{render, Viewport};
use crate::theme::Theme;

pub fn render_to_png(source: &str, width: u32, height: u32, out: &Path) -> std::io::Result<()> {
    let theme = Theme::light();
    let fonts = Fonts::load();
    let fb = render(source, Viewport { width, height }, 0.0, &theme, &fonts);
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(fb.width, fb.height, fb.pixels)
            .expect("framebuffer size mismatch");
    img.save(out).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}
