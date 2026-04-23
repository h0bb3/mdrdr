//! Image cache — lazy-loads PNG/JPEG via the `image` crate, keyed by
//! canonical path, invalidates on mtime change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug)]
pub struct CachedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub mtime: SystemTime,
}

#[derive(Default)]
pub struct ImageCache {
    map: HashMap<PathBuf, Arc<CachedImage>>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    /// Resolve `src` (as written in the markdown) against the directory
    /// containing the source file; returns the canonicalized path on disk.
    pub fn resolve(src: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
        let raw = Path::new(src);
        let absolute = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            base_dir?.join(raw)
        };
        absolute.canonicalize().ok()
    }

    /// Returns a cached image or loads it from disk. `None` if the path
    /// can't be opened or decoded.
    pub fn get_or_load(&mut self, path: &Path) -> Option<Arc<CachedImage>> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        if let Some(existing) = self.map.get(path) {
            if existing.mtime == mtime {
                return Some(existing.clone());
            }
        }
        let img = image::ImageReader::open(path).ok()?.decode().ok()?;
        let rgba = img.to_rgba8();
        let (width, height) = (rgba.width(), rgba.height());
        let cached = Arc::new(CachedImage {
            width,
            height,
            rgba: rgba.into_raw(),
            mtime,
        });
        self.map.insert(path.to_path_buf(), cached.clone());
        Some(cached)
    }
}
