//! `wallset` — command-line front end and diagnostics.
//!
//! Hand-rolled argument parsing on purpose: a CLI with six subcommands does
//! not justify the compile time and binary size of a parser framework.

#[cfg(windows)]
mod demo;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wallset_core::fitness::{self, Severity};
use wallset_core::media::MediaKind;
use wallset_core::{Monitor, MediaProbe};

const USAGE: &str = "\
wallset — lightweight image and video wallpaper

USAGE:
    wallset displays [--json]            What each connected display supports
    wallset probe <file> [--json]        Inspect a media file
    wallset check <file> [--monitor N]   Will this file look right on display N? What will it cost?
    wallset set <image> [--monitor N]    Set a still image on display N (default: all)
    wallset caps                         What this desktop session supports
    wallset desktop                      Dump the desktop window tree (diagnostics)
    wallset demo <video> [--monitor N]   Play behind the icons; measure play, pause, suspend

Displays are numbered as `wallset displays` lists them, starting at 1.";

fn main() -> ExitCode {
    wallset_surface::process::init_dpi_awareness();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let opt = |name: &str| {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    let positional = || {
        let mut it = args.iter().skip(1);
        while let Some(a) = it.next() {
            if matches!(a.as_str(), "--mpv-opt" | "--seconds" | "--monitor") {
                it.next();
            } else if !a.starts_with("--") {
                return Some(PathBuf::from(a));
            }
        }
        None
    };

    let result = match args.first().map(String::as_str) {
        Some("displays") => cmd_displays(flag("--json")),
        Some("probe") => positional().ok_or_else(|| "probe needs a file".to_string())
            .and_then(|f| cmd_probe(&f, flag("--json"))),
        Some("check") => positional().ok_or_else(|| "check needs a file".to_string())
            .and_then(|f| cmd_check(&f, opt("--monitor"))),
        Some("set") => positional().ok_or_else(|| "set needs an image".to_string())
            .and_then(|f| cmd_set(&f, opt("--monitor"))),
        Some("caps") => cmd_caps(),
        #[cfg(windows)]
        Some("measure") => positional().ok_or_else(|| "measure needs a video".to_string()).and_then(|f| {
            let mons = monitors()?;
            let extra: Vec<String> = args.windows(2).filter(|w| w[0] == "--mpv-opt").map(|w| w[1].clone()).collect();
            let secs = opt("--seconds").and_then(|s| s.parse().ok()).unwrap_or(8);
            demo::measure_playback(&f, pick(&mons, opt("--monitor"))?, &extra, secs)
        }),
        #[cfg(windows)]
        Some("demo") => positional().ok_or_else(|| "demo needs a video".to_string()).and_then(|f| {
            let mons = monitors()?;
            demo::run(&f, pick(&mons, opt("--monitor"))?)
        }),
        Some("desktop") => {
            println!("{}", wallset_surface::describe_desktop_hierarchy());
            Ok(())
        }
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn monitors() -> Result<Vec<Monitor>, String> {
    wallset_display::enumerate().map_err(|e| e.to_string())
}

fn pick(monitors: &[Monitor], n: Option<String>) -> Result<&Monitor, String> {
    match n {
        None => monitors.first().ok_or_else(|| "no displays".into()),
        Some(s) => {
            let i: usize = s.parse().map_err(|_| format!("--monitor expects a number, got `{s}`"))?;
            monitors
                .get(i.checked_sub(1).ok_or("displays are numbered from 1")?)
                .ok_or_else(|| format!("there are only {} displays", monitors.len()))
        }
    }
}

fn cmd_displays(json: bool) -> Result<(), String> {
    let mons = monitors()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&mons).map_err(|e| e.to_string())?);
        return Ok(());
    }
    for (i, m) in mons.iter().enumerate() {
        print_monitor(i + 1, m);
    }
    Ok(())
}

fn print_monitor(n: usize, m: &Monitor) {
    println!("Display {n}: {}{}", m.friendly_name, if m.is_primary { "  [primary]" } else { "" });
    for (k, v) in m.describe() {
        println!("  {k:<18}{v}");
    }
    if m.adapter.is_software {
        println!("                  ^ software renderer: video wallpaper is not recommended");
    }
    println!();
}

fn load_mpv() -> Result<wallset_media::Library, String> {
    wallset_media::Library::load().map_err(|e| e.to_string())
}

fn cmd_probe(path: &Path, json: bool) -> Result<(), String> {
    let lib = load_mpv()?;
    let p = wallset_media::probe(&lib, path).map_err(|e| e.to_string())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?);
    } else {
        print_probe(&p);
    }
    Ok(())
}

fn print_probe(p: &MediaProbe) {
    let kind = match p.kind {
        MediaKind::Image => "still image",
        MediaKind::Video => "video",
        MediaKind::AnimatedImage => "animated image",
    };
    println!("{}", p.path.display());
    println!("  Type            {kind}");
    println!("  Resolution      {}x{}", p.width, p.height);
    println!("  Codec           {} ({}, {}-bit)", p.codec, p.pixel_format, p.bit_depth());
    if let Some(fps) = p.fps {
        println!("  Frame rate      {fps:.3} fps");
    }
    if let Some(d) = p.duration_secs {
        println!("  Duration        {d:.1} s");
    }
    println!("  Size            {:.1} MB", p.file_size_bytes as f64 / 1_048_576.0);
    println!("  Dynamic range   {:?}, primaries {:?}", p.transfer, p.primaries);
    if p.kind.needs_renderer() {
        println!(
            "  Decoding        {}",
            p.hwdec.as_deref().map_or("CPU (no hardware decoder for this file)".into(), |h| {
                format!("GPU hardware ({h})")
            })
        );
    }
    if p.has_audio {
        println!("  Audio           present (will be ignored, never decoded)");
    }
}

fn cmd_check(path: &Path, monitor: Option<String>) -> Result<(), String> {
    let mons = monitors()?;
    let m = pick(&mons, monitor)?;
    let lib = load_mpv()?;
    let p = wallset_media::probe(&lib, path).map_err(|e| e.to_string())?;
    let report = fitness::analyse(&p, m);

    print_probe(&p);
    println!("\nOn: {} ({})\n", m.friendly_name, m.current_mode);

    let e = &report.estimate;
    if p.kind.needs_renderer() {
        println!(
            "Estimated cost while visible: ~{:.0} MB private memory, ~{:.1}% of one CPU core",
            e.ram_mb, e.cpu_percent_of_one_core
        );
        println!(
            "  (calibrated model, not a measurement; while hidden, wallSet pauses and then frees it entirely)
"
        );
    } else {
        println!("Cost: none. A still image is handed to Windows and wallSet does no further work.\n");
    }

    if report.findings.is_empty() {
        println!("No issues: this file suits this display.");
    }
    for f in &report.findings {
        let mark = match f.severity {
            Severity::Problem => "[!!]",
            Severity::Warning => "[! ]",
            Severity::Advice => "[ >]",
            Severity::Info => "[ i]",
        };
        println!("{mark} {}: {}", f.severity.label(), f.title);
        println!("     {}", f.detail);
        if let Some(h) = &f.hint {
            println!("     Fix: {h}");
        }
        println!();
    }
    Ok(())
}

fn cmd_set(path: &Path, monitor: Option<String>) -> Result<(), String> {
    let mons = monitors()?;
    let targets: Vec<&Monitor> = match monitor {
        Some(_) => vec![pick(&mons, monitor)?],
        None => mons.iter().collect(),
    };
    for m in targets {
        wallset_surface::set_static_wallpaper(m.bounds, path).map_err(|e| e.to_string())?;
        println!("Set on {}.", m.friendly_name);
    }
    Ok(())
}

fn cmd_caps() -> Result<(), String> {
    let c = wallset_surface::capabilities();
    println!("Still images     {}", if c.static_images { "yes" } else { "no" });
    println!("Video wallpaper  {}", if c.video { "yes" } else { "no" });
    if let Some(r) = c.video_unavailable_reason {
        println!("  why not: {r}");
    }
    match wallset_display::power_saving_adapter() {
        Some(a) => println!("Render GPU       {a} (power-saving GPU on this hybrid system)"),
        None => println!("Render GPU       system default (single GPU, nothing to choose)"),
    }
    match wallset_media::Library::load() {
        Ok(l) => println!("libmpv           {} (client API {})", l.path().display(), l.api_version()),
        Err(e) => println!("libmpv           not found: {e}"),
    }
    Ok(())
}
