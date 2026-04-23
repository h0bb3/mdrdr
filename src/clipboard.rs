//! Tiny clipboard bridge — no crate, just shells out.
//!
//! Tries `wl-copy` first (Wayland), then `xclip -selection clipboard` (X11).
//! If neither is available, the copy silently no-ops. The returned `bool`
//! tells callers whether a helper was found.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy(text: &str) -> bool {
    for (prog, args) in [("wl-copy", &[][..]), ("xclip", &["-selection", "clipboard"])] {
        if let Ok(mut child) = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return true;
        }
    }
    false
}
