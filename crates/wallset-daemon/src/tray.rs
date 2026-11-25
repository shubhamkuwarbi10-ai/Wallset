//! Notification-area icon and its menu.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_CHECKED,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{PCWSTR, w};

pub const WM_APP_TRAY: u32 = WM_APP + 1;

pub const CMD_SETTINGS: u32 = 1;
pub const CMD_PAUSE: u32 = 2;
pub const CMD_QUIT: u32 = 3;

fn data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        // SAFETY: stock system icon. A branded icon is a packaging task.
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        ..Default::default()
    };
    for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    nid
}

pub fn add(hwnd: HWND, tip: &str) {
    // SAFETY: fully initialised struct.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data(hwnd, tip));
    }
}

pub fn set_tip(hwnd: HWND, tip: &str) {
    // SAFETY: as above.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data(hwnd, tip));
    }
}

pub fn remove(hwnd: HWND) {
    // SAFETY: as above.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd, ""));
    }
}

/// Show the menu and return the chosen command, if any.
///
/// `TrackPopupMenu` runs a modal message loop, so the window procedure can be
/// re-entered while it is open. Callers must not hold daemon state borrowed
/// across this call.
pub fn menu(hwnd: HWND, paused: bool) -> Option<u32> {
    // SAFETY: standard popup menu sequence on our own window.
    unsafe {
        let m = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(m, MF_STRING, CMD_SETTINGS as usize, w!("Open wallSet settings"));
        let pause_flags = if paused { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(m, pause_flags, CMD_PAUSE as usize, w!("Pause video wallpapers"));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(m, MF_STRING, CMD_QUIT as usize, w!("Quit wallSet"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required, or the menu does not close when the user clicks away.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(m);
        (cmd.0 > 0).then_some(cmd.0 as u32)
    }
}
