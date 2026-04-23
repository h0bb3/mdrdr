//! Live reload. A polling thread watches the currently-opened file's mtime
//! and, when it changes, reloads the source and asks the window to redraw.
//! No external crate — std::fs metadata is enough and works everywhere.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use winit::event_loop::EventLoopProxy;

use crate::window::{Shared, UserEvent};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn spawn(shared: Arc<Shared>, proxy: EventLoopProxy<UserEvent>) {
    std::thread::Builder::new()
        .name("mdrdr-watch".into())
        .spawn(move || {
            // Tracks the mtime of the currently-opened file. Switching files
            // is recorded but does NOT trigger a reload — the open handler
            // already loaded the fresh source.
            let mut last: Option<(PathBuf, SystemTime)> = None;
            loop {
                std::thread::sleep(POLL_INTERVAL);
                let cur = {
                    let s = shared.state.lock().unwrap();
                    s.source_path.clone()
                };
                let Some(path) = cur else {
                    last = None;
                    continue;
                };
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                let Ok(mtime) = meta.modified() else { continue };

                let reload = match &last {
                    None => false,
                    Some((p, _)) if p != &path => false,
                    Some((_, t)) => *t != mtime,
                };
                last = Some((path.clone(), mtime));
                if !reload {
                    continue;
                }

                let Ok(new_source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                {
                    let mut s = shared.state.lock().unwrap();
                    if s.source_path.as_deref() == Some(path.as_path()) {
                        s.source = new_source;
                    }
                }
                let _ = proxy.send_event(UserEvent::Redraw);
            }
        })
        .expect("spawn watcher thread");
}
