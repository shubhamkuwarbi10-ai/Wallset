//! Talking to the daemon: one window message. Config I/O lives in
//! `wallset_core::config`, shared with the daemon.

#[cfg(windows)]
fn daemon_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::{PCWSTR, w};
    // SAFETY: plain lookup by class name.
    unsafe { FindWindowW(w!("wallSet.Daemon"), PCWSTR::null()) }.ok()
}

#[cfg(windows)]
pub fn daemon_running() -> bool {
    daemon_window().is_some()
}

/// Tell a running daemon to reload, or start one.
#[cfg(windows)]
pub fn notify_or_start_daemon() -> Result<&'static str, String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
    if let Some(h) = daemon_window() {
        // SAFETY: posting WM_APP_RELOAD (WM_APP + 3) to the daemon.
        unsafe { PostMessageW(Some(h), WM_APP + 3, WPARAM(0), LPARAM(0)) }.map_err(|e| e.to_string())?;
        return Ok("Background service updated.");
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("wallset-daemon.exe");
    std::process::Command::new(&exe).spawn().map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    Ok("Background service started.")
}

#[cfg(not(windows))]
pub fn daemon_running() -> bool {
    false
}

#[cfg(not(windows))]
pub fn notify_or_start_daemon() -> Result<&'static str, String> {
    Err("the background service is Windows-only for now".into())
}
