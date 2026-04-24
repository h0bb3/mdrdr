//! mdrdr — a from-scratch markdown viewer.
//!
//! Subcommands:
//!   mdrdr render [FILE] [--tree DIR] [--out PATH] [--width W] [--height H] [--scroll Y]
//!   mdrdr open   [FILE_OR_DIR]
//!
//! A bare path is shorthand for `open`, so `mdrdr .` and `mdrdr foo.md`
//! both work — handy for shell use and for registering the binary as an
//! OS-level .md handler.

mod api;
mod clipboard;
mod font;
mod headless;
mod images;
mod layout;
mod math;
mod md;
mod mermaid;
mod render;
mod theme;
mod tree;
mod watch;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         mdrdr                       (same as `mdrdr open`)\n  \
         mdrdr <FILE_OR_DIR>         shorthand for `mdrdr open <FILE_OR_DIR>`\n  \
         mdrdr open   [FILE_OR_DIR] [--api] [--port N]\n  \
         mdrdr render [FILE] [--tree DIR] [--out PATH] [--width W] [--height H] [--scroll Y]\n\n\
         --api         enable the HTTP control API on a random port\n  \
         --port N      bind the API on port N (implies --api)"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);

    match cmd {
        Some("render") => cmd_render(&args[1..]),
        Some("open") => cmd_open(&args[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            usage();
            ExitCode::SUCCESS
        }
        // No args, a bare path, or a bare flag — all shorthand for `open`.
        // Lets the OS use `mdrdr` directly as a MIME handler, makes the
        // CLI feel like `less` / `bat`, and lets `mdrdr --api foo.md`
        // work without typing the subcommand.
        None => cmd_open(&[]),
        _ => cmd_open(&args),
    }
}

fn cmd_render(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut out: PathBuf = PathBuf::from("mdrdr.png");
    let mut width: u32 = 1200;
    let mut height: u32 = 900;
    let mut scroll: f32 = 0.0;
    let mut tree_root: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = PathBuf::from(args.get(i).cloned().unwrap_or_default());
            }
            "--width" => {
                i += 1;
                width = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1200);
            }
            "--height" => {
                i += 1;
                height = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(900);
            }
            "--scroll" => {
                i += 1;
                scroll = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            }
            "--tree" => {
                i += 1;
                tree_root = args.get(i).cloned().map(PathBuf::from);
            }
            other if !other.starts_with("--") && file.is_none() => {
                file = Some(PathBuf::from(other));
            }
            _ => {
                eprintln!("unknown arg: {}", args[i]);
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let source = match &file {
        Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| {
            eprintln!("could not read {}: {}", p.display(), e);
            String::new()
        }),
        None => String::new(),
    };

    match headless::render_to_png(
        &source,
        file.as_deref(),
        tree_root.as_deref(),
        width,
        height,
        scroll,
        &out,
    ) {
        Ok(()) => {
            println!("wrote {} ({}x{})", out.display(), width, height);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_open(args: &[String]) -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut api_enabled = false;
    let mut api_port: u16 = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--api" => {
                api_enabled = true;
            }
            "--port" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u16>().ok()) {
                    Some(p) => {
                        api_port = p;
                        api_enabled = true;
                    }
                    None => {
                        eprintln!("--port expects a number 0..=65535");
                        return ExitCode::from(2);
                    }
                }
            }
            other if !other.starts_with('-') && path.is_none() => {
                path = Some(PathBuf::from(other));
            }
            _ => {
                eprintln!("unknown arg: {}", args[i]);
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    window::run(window::WindowOptions {
        path,
        api_port: api_enabled.then_some(api_port),
    })
}
