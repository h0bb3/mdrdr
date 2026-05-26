//! `mdrdr render` — write a PNG, then exit. No window, no event loop.

use std::path::{Path, PathBuf};

use crate::font::Fonts;
use crate::images::ImageCache;
use crate::render::{render, RenderInput, Viewport};
use crate::theme::Theme;
use crate::tree::FileTree;

pub fn render_to_png(
    source: &str,
    source_path: Option<&Path>,
    root: Option<&Path>,
    width: u32,
    height: u32,
    scroll: f32,
    out: &Path,
) -> std::io::Result<()> {
    let theme = Theme::light();
    let fonts = Fonts::load();
    let mut images = ImageCache::new();

    let tree_owner = root.map(|r| FileTree::new(PathBuf::from(r)));
    let flat = tree_owner.as_ref().map(|t| t.flatten());

    let base_dir = source_path.and_then(|p| p.parent());

    let fb = render(
        &RenderInput {
            source,
            viewport: Viewport { width, height },
            scroll,
            theme: &theme,
            fonts: &fonts,
            tree: flat.as_deref(),
            active_path: source_path,
            base_dir,
            sidebar_width: if flat.is_some() { 260.0 } else { 0.0 },
            sidebar_scroll: 0.0,
            comment_col_width: 0.0,
            content_zoom: 1.0,
            sidebar_zoom: 1.0,
            selection: None,
            hover_pos: None,
            search: None,
            mermaid_overrides: None,
            text_column_width: f32::INFINITY,
            text_column_offset_x: 0.0,
        },
        &mut images,
    );
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(fb.width, fb.height, fb.pixels)
            .expect("framebuffer size mismatch");
    img.save(out).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}
