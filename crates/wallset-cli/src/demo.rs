//! `wallset demo`: run the full T0 -> T1 -> T2 lifecycle on one monitor and
//! measure every state with the same counters `tools/bench.py` uses.
//!
//! This is the end-to-end proof that the policy tiers do what the
//! architecture claims, before the daemon automates them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use wallset_core::Monitor;
use wallset_surface::process::{self, ProcessStats};
use wallset_surface::{DesktopHost, HostWindow, get_static_wallpaper, set_static_wallpaper};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage,
};

/// Keep this thread's windows responsive for `d`.
///
/// The host window's parent is Explorer's desktop, which attaches the two
/// threads' input queues. Sleeping here instead of pumping would stall the
/// desktop itself.
fn pump_for(d: Duration) {
    let end = Instant::now() + d;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        // SAFETY: waits on no handles, only on this thread's message queue.
        unsafe {
            MsgWaitForMultipleObjectsEx(None, left.as_millis() as u32, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
        let mut msg = MSG::default();
        // SAFETY: standard message loop on this thread's queue.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// CPU% and peak memory of `pid` over `d`.
///
/// CPU is computed from the CPU-time delta across the **whole** window, not
/// as a median of per-second samples: Windows advances process CPU time in
/// 15.625 ms clock ticks, so a 1 s sample can only read multiples of ~1.56%,
/// which is the same size as the differences being measured. Over 10 s the
/// resolution is ~0.16%.
fn measure(pid: u32, d: Duration) -> Option<(f64, u64, u64)> {
    let first: ProcessStats = process::stats(pid)?;
    let started = Instant::now();
    let (mut peak_priv, mut peak_ws) = (first.private_bytes, first.working_set_bytes);
    let mut last = first;
    while started.elapsed() < d {
        pump_for(Duration::from_millis(500).min(d.saturating_sub(started.elapsed())));
        last = process::stats(pid)?;
        peak_priv = peak_priv.max(last.private_bytes);
        peak_ws = peak_ws.max(last.working_set_bytes);
    }
    Some((last.cpu_percent_since(&first, started.elapsed()), peak_priv, peak_ws))
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn render_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let p = exe.with_file_name(if cfg!(windows) { "wallset-render.exe" } else { "wallset-render" });
    p.exists().then_some(p.clone()).ok_or_else(|| format!("renderer not found at {}", p.display()))
}

fn send(child: &mut Child, line: &str) {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }
}

/// A renderer drawing into a fresh host window behind the desktop icons,
/// already past its first frame.
struct Playing {
    host: DesktopHost,
    win: HostWindow,
    child: Child,
    first_frame_ms: u128,
}

fn start(file: &Path, m: &Monitor, extra: &[String]) -> Result<Playing, String> {
    let file = std::path::absolute(file).map_err(|e| e.to_string())?;
    let host = DesktopHost::acquire().map_err(|e| e.to_string())?;
    let win = host.create_window(m.bounds).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(render_exe()?);
    cmd.args(["--wid", &win.raw().to_string(), "--file"]).arg(&file).arg("--show-on-ready");
    for o in extra {
        cmd.args(["--mpv-opt", o]);
    }
    let spawned = Instant::now();
    let mut child = cmd.stdin(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;

    // The renderer shows the window on its first frame.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !win.is_visible() && Instant::now() < deadline {
        pump_for(Duration::from_millis(20));
        if let Ok(Some(st)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            return Err(format!("renderer exited early ({st}): {}", err.trim()));
        }
    }
    if !win.is_visible() {
        let _ = child.kill();
        return Err("no first frame within 10 s".into());
    }
    Ok(Playing { host, win, child, first_frame_ms: spawned.elapsed().as_millis() })
}

pub fn run(file: &Path, m: &Monitor) -> Result<(), String> {
    let original_wallpaper = get_static_wallpaper(m.bounds).ok();
    let Playing { host, win, mut child, first_frame_ms } = start(file, m, &[])?;
    let pid = child.id();
    println!("Desktop layout: {:?}. Host window {:#x} on {} at {:?}.", host.layout(), win.raw(), m.friendly_name, m.bounds);
    println!("Minimise your windows (Win+D) to watch.\n");
    println!("[T0] first frame on screen after {first_frame_ms} ms");

    pump_for(Duration::from_secs(3)); // let startup allocations settle
    let (cpu0, priv0, ws0) = measure(pid, Duration::from_secs(6)).ok_or("lost the renderer")?;
    println!("[T0 playing ] renderer: {cpu0:5.2}% of one core, {:6.1} MB private, {:6.1} MB working set", mb(priv0), mb(ws0));

    send(&mut child, "pause");
    pump_for(Duration::from_secs(2));
    let (cpu1, priv1, ws1) = measure(pid, Duration::from_secs(6)).ok_or("lost the renderer")?;
    println!("[T1 paused  ] renderer: {cpu1:5.2}% of one core, {:6.1} MB private, {:6.1} MB working set", mb(priv1), mb(ws1));

    send(&mut child, "resume");
    pump_for(Duration::from_secs(2));

    let freeze_png = std::env::temp_dir().join("wallset-freeze-demo.png");
    let _ = std::fs::remove_file(&freeze_png);
    let t_freeze = Instant::now();
    send(&mut child, &format!("freeze {}", freeze_png.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break Some(st);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        pump_for(Duration::from_millis(20));
    };
    let pos = std::fs::read_to_string(format!("{}.pos", freeze_png.display())).ok();
    match (status, freeze_png.exists()) {
        (Some(st), true) => {
            set_static_wallpaper(m.bounds, &freeze_png).map_err(|e| e.to_string())?;
            win.hide();
            println!(
                "[T2 suspend ] renderer exited ({st}) {} ms after freeze; frame at {}s is now the static wallpaper. Renderer cost: 0 MB, 0%.",
                t_freeze.elapsed().as_millis(),
                pos.as_deref().unwrap_or("?").trim()
            );
        }
        (s, exists) => return Err(format!("freeze failed: exit={s:?}, frame written={exists}")),
    }

    if let Some(me) = process::current_stats() {
        println!("\nHost process (stands in for the daemon): {:.1} MB private, {:.1} MB working set", mb(me.private_bytes), mb(me.working_set_bytes));
    }

    pump_for(Duration::from_secs(4));
    match original_wallpaper {
        Some(orig) if !orig.as_os_str().is_empty() => {
            set_static_wallpaper(m.bounds, &orig).map_err(|e| e.to_string())?;
            println!("Restored your original wallpaper: {}", orig.display());
        }
        _ => println!("Note: could not read your original wallpaper, so the demo frame was left in place."),
    }
    Ok(())
}

/// Steady-state cost of playing `file` with optional extra mpv options.
/// Touches nothing persistent: no wallpaper change, window destroyed after.
pub fn measure_playback(file: &Path, m: &Monitor, extra: &[String], secs: u64) -> Result<(), String> {
    let Playing { mut child, first_frame_ms: first_ms, .. } = start(file, m, extra)?;
    let pid = child.id();
    pump_for(Duration::from_secs(4));
    let r = measure(pid, Duration::from_secs(secs));
    send(&mut child, "quit");
    pump_for(Duration::from_millis(500));
    let _ = child.kill();
    let (cpu, prv, ws) = r.ok_or("lost the renderer")?;
    let opts = if extra.is_empty() { "(profile defaults)".to_string() } else { extra.join(" ") };
    println!("{cpu:6.2}% cpu  {:7.1} MB private  {:7.1} MB ws  first-frame {first_ms:5} ms  {opts}", mb(prv), mb(ws));
    Ok(())
}
