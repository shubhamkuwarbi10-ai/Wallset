//! `wallset-render`: draws one video into one window, then gets out of the way.
//!
//! The daemon spawns one of these per monitor while a video wallpaper is
//! visible, and terminates it when the wallpaper is hidden for long enough
//! (policy tier T2). It is also what the settings window's preview runs, so
//! the preview is the real pipeline rather than an approximation of it.
//!
//! ## Protocol
//!
//! Arguments configure the player. Afterwards, one command per line on stdin:
//!
//! | Line | Effect |
//! |---|---|
//! | `pause` | stop decoding; the process stays warm (tier T1) |
//! | `resume` | continue |
//! | `freeze <png>` | save the current frame to `<png>`, its playback position to `<png>.pos`, then exit. The daemon uses the frame as a static wallpaper so the desktop never goes black, and the position to resume seamlessly |
//! | `quit` | exit |
//!
//! End-of-file on stdin also means exit: if the daemon dies, its renderers
//! follow it rather than lingering as orphans.
//!
//! Exit codes: 0 normal, 2 bad arguments, 3 libmpv unavailable, 4 playback
//! failed.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use wallset_media::{Controller, Event, FitMode, Library, RenderProfile};

struct Args {
    wid: isize,
    file: PathBuf,
    profile: RenderProfile,
    start_secs: Option<f64>,
    /// Show the (initially hidden) host window once the first frame is ready,
    /// so the user never sees an empty black rectangle while mpv starts.
    show_on_ready: bool,
    /// Extra `key=value` mpv options applied after the profile. For
    /// measurement experiments; not used by the daemon.
    extra: Vec<(String, String)>,
}

fn parse() -> Result<Args, String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let has = |k: &str| a.iter().any(|x| x == k);

    let wid = val("--wid").ok_or("--wid is required")?.parse().map_err(|_| "--wid must be an integer")?;
    let file = PathBuf::from(val("--file").ok_or("--file is required")?);
    let fit = match val("--fit").as_deref() {
        None | Some("fill") => FitMode::Fill,
        Some("fit") => FitMode::Fit,
        Some("stretch") => FitMode::Stretch,
        Some(o) => return Err(format!("unknown --fit `{o}`")),
    };
    let start_secs = val("--start").map(|s| s.parse::<f64>()).transpose().map_err(|_| "--start must be seconds")?;

    Ok(Args {
        wid,
        file,
        profile: RenderProfile {
            fit,
            hardware_decode: !has("--no-hwdec"),
            low_power: !has("--quality"),
            adapter: val("--adapter"),
        },
        start_secs,
        show_on_ready: has("--show-on-ready"),
        extra: a
            .windows(2)
            .filter(|w| w[0] == "--mpv-opt")
            .map(|w| {
                w[1].split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .ok_or_else(|| format!("--mpv-opt expects key=value, got `{}`", w[1]))
            })
            .collect::<Result<_, _>>()?,
    })
}

fn main() -> ExitCode {
    wallset_surface::process::init_dpi_awareness();
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("wallset-render: {e}");
            return ExitCode::from(2);
        }
    };

    if !std::env::args().any(|a| a == "--foreground") {
        wallset_surface::process::enter_background_mode();
    }

    let lib = match Library::load() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wallset-render: {e}");
            return ExitCode::from(3);
        }
    };

    match run(&lib, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wallset-render: {e}");
            ExitCode::from(4)
        }
    }
}

fn run(lib: &Library, args: &Args) -> Result<(), wallset_media::MpvError> {
    let mut mpv = lib.create()?;
    args.profile.apply(&mut mpv)?;
    for (k, v) in &args.extra {
        mpv.set_option(k, v)?;
    }
    mpv.set_option("wid", &args.wid.to_string())?;
    if let Some(s) = args.start_secs {
        mpv.set_option("start", &format!("{s:.3}"))?;
    }
    mpv.initialize()?;

    let ctl = mpv.controller();
    ctl.command(&["loadfile", &args.file.to_string_lossy()])?;

    // Commands arrive on a second thread. It spends its life blocked in
    // ReadFile on the pipe: zero CPU until the daemon writes a line.
    let stdin_ctl = ctl.clone();
    std::thread::Builder::new()
        .name("stdin".into())
        .stack_size(64 * 1024)
        .spawn(move || command_loop(&stdin_ctl))
        .expect("spawning a thread with a 64 KiB stack");

    let mut shown = false;
    loop {
        // Blocks in the kernel until mpv has something to say.
        match mpv.wait_event(None) {
            Event::PlaybackRestart if args.show_on_ready && !shown => {
                show_window(args.wid);
                shown = true;
            }
            Event::EndFile { error: true, code } => {
                return Err(wallset_media::MpvError::LoadFailed(format!(
                    "playback ended with mpv error {code}"
                )));
            }
            Event::Shutdown => return Ok(()),
            _ => {}
        }
    }
}

fn command_loop(ctl: &Controller) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let result = match cmd {
            "pause" => ctl.set_property("pause", "yes"),
            "resume" => ctl.set_property("pause", "no"),
            "freeze" if !rest.is_empty() => freeze(ctl, rest),
            "quit" => break,
            "" => Ok(()),
            other => {
                eprintln!("wallset-render: ignoring unknown command `{other}`");
                Ok(())
            }
        };
        if let Err(e) = result {
            eprintln!("wallset-render: {cmd}: {e}");
        }
        if cmd == "freeze" {
            break;
        }
    }
    // Stdin closed, `quit`, or a completed freeze. Asking mpv to quit makes
    // the main thread receive Shutdown and return normally.
    let _ = ctl.command(&["quit"]);
    // If mpv is wedged, do not hang around: the daemon is waiting for us.
    std::thread::sleep(Duration::from_secs(2));
    std::process::exit(0);
}

fn freeze(ctl: &Controller, png: &str) -> Result<(), wallset_media::MpvError> {
    // `video` = the decoded frame at source resolution, without any OSD.
    // Synchronous: returns once the file is fully written.
    ctl.command(&["screenshot-to-file", png, "video"])?;
    if let Some(pos) = ctl.get("time-pos") {
        let _ = std::fs::write(format!("{png}.pos"), pos);
    }
    Ok(())
}

#[cfg(windows)]
fn show_window(wid: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindowAsync};
    // Async: the window belongs to the daemon's thread, and we must not block
    // on it (cross-process ShowWindow is a sent message).
    // SAFETY: a stale handle makes this a harmless no-op.
    unsafe {
        let _ = ShowWindowAsync(HWND(wid as *mut _), SW_SHOWNOACTIVATE);
    }
}

#[cfg(not(windows))]
fn show_window(_wid: isize) {}
