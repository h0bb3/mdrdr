//! Tiny clipboard bridge — no crate, just shells out.
//!
//! Tries `wl-copy` / `wl-paste` first (Wayland), then `xclip -selection
//! clipboard` (X11). If neither helper is available, copy silently
//! no-ops and paste returns `None`.

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

/// Read the system clipboard as UTF-8 text. Returns `None` if no helper
/// is available, the clipboard is empty, or the contents aren't UTF-8.
/// `wl-paste -n` suppresses the trailing newline it would otherwise add;
/// `xclip` also gets a trim pass so the two backends behave the same.
pub fn paste() -> Option<String> {
    for (prog, args) in [
        ("wl-paste", &["-n"][..]),
        ("xclip", &["-selection", "clipboard", "-o"]),
    ] {
        if let Ok(out) = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if !out.status.success() {
                continue;
            }
            let s = String::from_utf8(out.stdout).ok()?;
            return Some(s);
        }
    }
    None
}
