//! mdrdr — a from-scratch markdown viewer.
//!
//! Subcommands:
//!   mdrdr render [FILE] [--out PATH] [--width W] [--height H]
//!   mdrdr open   [FILE]

mod api;
mod font;
mod headless;
mod render;
mod theme;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         mdrdr render [FILE] [--out PATH] [--width W] [--height H]\n  \
         mdrdr open   [FILE]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { return usage() };

    match cmd.as_str() {
        "render" => cmd_render(&args[1..]),
        "open" => cmd_open(&args[1..]),
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

fn cmd_render(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut out: PathBuf = PathBuf::from("mdrdr.png");
    let mut width: u32 = 1200;
    let mut height: u32 = 900;

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

    let source = match file {
        Some(p) => std::fs::read_to_string(&p).unwrap_or_else(|e| {
            eprintln!("could not read {}: {}", p.display(), e);
            String::new()
        }),
        None => String::new(),
    };

    match headless::render_to_png(&source, width, height, &out) {
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
    let file = args.iter().find(|a| !a.starts_with("--")).cloned().map(PathBuf::from);
    window::run(file)
}
