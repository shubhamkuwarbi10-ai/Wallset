//! What is on top of the desktop right now.
//!
//! Called only in response to a window event (debounced), never on a timer.
//! One pass over the top-level window list costs on the order of 100 µs.

use wallset_core::Rect;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsWindowVisible, WS_CAPTION, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

/// Per-monitor result of one scan, in the same order as the input rects.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    /// Largest fraction of the monitor covered by any single window.
    pub coverage: Vec<f64>,
    /// A fullscreen application owns the monitor.
    pub fullscreen: Vec<bool>,
}

/// Windows that belong to the desktop itself and never count as covering it.
const DESKTOP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Shell_InputSwitchTopLevelWindow",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "wallSet.Daemon",
];

pub fn scan(monitors: &[Rect]) -> Scan {
    let windows = visible_top_level();

    let coverage = monitors
        .iter()
        .map(|m| windows.iter().map(|w| m.coverage_by(&w.rect)).fold(0.0, f64::max))
        .collect();

    let fullscreen = fullscreen_per_monitor(monitors);
    Scan { coverage, fullscreen }
}

struct Win {
    rect: Rect,
}

fn visible_top_level() -> Vec<Win> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        // SAFETY: data is the Vec below, alive for this synchronous call.
        let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        out.push(h);
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::with_capacity(256);
    // SAFETY: synchronous enumeration with a valid data pointer.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&raw mut all as isize)) };

    all.into_iter()
        .filter(|&h| counts_as_covering(h))
        .filter_map(|h| window_rect(h).map(|rect| Win { rect }))
        .filter(|w| w.rect.width > 0 && w.rect.height > 0)
        .collect()
}

fn counts_as_covering(h: HWND) -> bool {
    // SAFETY: all of these accept any window handle.
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return false;
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        // Tool windows (palettes, tray flyouts) and click-through overlays
        // (FPS counters, recording indicators) do not hide the desktop.
        if ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_TRANSPARENT.0 != 0 {
            return false;
        }
    }
    if is_cloaked(h) {
        // On another virtual desktop, or a suspended UWP app.
        return false;
    }
    !DESKTOP_CLASSES.contains(&class_name(h).as_str())
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: out-param sized to the attribute.
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&raw mut cloaked).cast(), size_of::<u32>() as u32)
    }
    .is_ok()
        && cloaked != 0
}

/// The visible frame. DWM's extended bounds exclude the invisible resize
/// border that `GetWindowRect` includes on Windows 10/11.
fn window_rect(h: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: out-param sized to the attribute.
    let ok = unsafe {
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut r).cast(), size_of::<RECT>() as u32)
    }
    .is_ok()
        // SAFETY: plain query.
        || unsafe { GetWindowRect(h, &mut r) }.is_ok();
    ok.then(|| Rect {
        x: r.left,
        y: r.top,
        width: (r.right - r.left).max(0) as u32,
        height: (r.bottom - r.top).max(0) as u32,
    })
}

fn fullscreen_per_monitor(monitors: &[Rect]) -> Vec<bool> {
    // The shell's own notion of "a fullscreen app is running" catches
    // exclusive-mode games, which do not always look fullscreen by geometry.
    // SAFETY: no arguments.
    let quns = unsafe { SHQueryUserNotificationState() }.ok();
    if quns == Some(QUNS_PRESENTATION_MODE) {
        return vec![true; monitors.len()];
    }
    let shell_says_fullscreen = matches!(quns, Some(QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_BUSY));

    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() || !counts_as_covering(fg) {
        return vec![false; monitors.len()];
    }
    let Some(r) = window_rect(fg) else { return vec![false; monitors.len()] };
    // SAFETY: plain query.
    let borderless = unsafe { GetWindowLongPtrW(fg, GWL_STYLE) } as u32 & WS_CAPTION.0 != WS_CAPTION.0;

    monitors
        .iter()
        .map(|m| {
            let covers_exactly = m.coverage_by(&r) >= 0.999;
            // A maximised window with a title bar is *covering*, handled by
            // the grace timer; a borderless window over the whole monitor is
            // a fullscreen game or video and reclaims immediately.
            covers_exactly && (borderless || shell_says_fullscreen)
        })
        .collect()
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: buffer length passed via the slice.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}
