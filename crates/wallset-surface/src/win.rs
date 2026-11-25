//! Windows desktop surface.
//!
//! ## How a window ends up behind the desktop icons
//!
//! Sending the undocumented message `0x052C` to `Progman` makes Explorer split
//! the desktop into an icon layer (`SHELLDLL_DefView`) and a separate
//! wallpaper layer (`WorkerW`). A window placed between the two is drawn over
//! the wallpaper and under the icons. Explorer has done this since Windows 8
//! for its own slideshow transitions; Wallpaper Engine and Lively rely on it.
//!
//! The resulting window tree changed in Windows 11 24H2:
//!
//! ```text
//! Legacy (<= 23H2)                     Raised (24H2+)
//! ----------------                     --------------
//! WorkerW            <- has DefView    Progman
//!   SHELLDLL_DefView                     SHELLDLL_DefView   (icons, top)
//! WorkerW            <- our parent       <our window>       (inserted here)
//! Progman                                WorkerW            (wallpaper, bottom)
//! ```
//!
//! The layout is detected **from the live window tree**, not from the OS build
//! number. Build numbers lie (Insider builds, servicing changes); the tree
//! does not.

use crate::{Capabilities, SurfaceError};
use std::path::Path;
use std::sync::OnceLock;
use wallset_core::Rect;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH, MapWindowPoints};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{DWPOS_FILL, DesktopWallpaper, IDesktopWallpaper};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows,
    FindWindowExW, FindWindowW, GetClassNameW, GetWindow, GW_CHILD, GW_HWNDNEXT, HTTRANSPARENT,
    IsWindowVisible, RegisterClassExW, SMTO_NORMAL, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageTimeoutW, SetWindowPos, ShowWindow,
    WM_NCHITTEST, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE,
    WS_EX_TRANSPARENT, WS_VISIBLE,
};
use windows::core::{BOOL, PCWSTR, w};

/// Undocumented: asks Progman to create the WorkerW wallpaper layer.
const MSG_SPAWN_WORKERW: u32 = 0x052C;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLayout {
    /// Windows 10 and Windows 11 up to 23H2: parent into the empty WorkerW.
    Legacy,
    /// Windows 11 24H2 and later: parent into Progman, below SHELLDLL_DefView.
    Raised,
}

/// The located parent for wallpaper windows.
#[derive(Debug)]
pub struct DesktopHost {
    parent: HWND,
    /// In the Raised layout we must z-order our window just below this.
    defview: Option<HWND>,
    layout: DesktopLayout,
}

impl DesktopHost {
    /// Find (or make Explorer create) the wallpaper layer.
    ///
    /// Must be called again after Explorer restarts (`TaskbarCreated`), since
    /// every handle here belongs to the old Explorer process.
    pub fn acquire() -> Result<Self, SurfaceError> {
        // SAFETY: plain window lookup.
        let progman = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }
            .map_err(|_| SurfaceError::NoProgman)?;

        // Two parameter variants are in circulation. (0xD, 1) is what current
        // Explorer responds to; (0, 0) is the original. Sending both is
        // harmless: the message is idempotent once the layer exists.
        for (wp, lp) in [(0xD, 1), (0, 0)] {
            // SAFETY: timeout-bounded so a hung Explorer cannot hang us.
            unsafe {
                SendMessageTimeoutW(
                    progman,
                    MSG_SPAWN_WORKERW,
                    WPARAM(wp),
                    LPARAM(lp),
                    SMTO_NORMAL,
                    1000,
                    None,
                );
            }
        }

        // Raised layout: Progman itself holds both DefView and a WorkerW.
        let defview_in_progman = child_of_class(progman, w!("SHELLDLL_DefView"));
        let workerw_in_progman = child_of_class(progman, w!("WorkerW"));
        if let (Some(defview), Some(_)) = (defview_in_progman, workerw_in_progman) {
            return Ok(Self { parent: progman, defview: Some(defview), layout: DesktopLayout::Raised });
        }

        // Legacy layout: find the top-level WorkerW that adopted DefView; the
        // wallpaper layer is the next WorkerW after it in z-order.
        if let Some(icons_host) = top_level_with_child(w!("SHELLDLL_DefView")) {
            // SAFETY: plain window lookup.
            if let Ok(wallpaper_layer) =
                unsafe { FindWindowExW(None, Some(icons_host), w!("WorkerW"), PCWSTR::null()) }
            {
                return Ok(Self { parent: wallpaper_layer, defview: None, layout: DesktopLayout::Legacy });
            }
        }

        Err(SurfaceError::NoDesktopLayer(
            "Explorer did not create a WorkerW wallpaper layer. This happens when Explorer is \
             replaced by a third-party shell, or on a build that changed the desktop again."
                .into(),
        ))
    }

    #[must_use]
    pub const fn layout(&self) -> DesktopLayout {
        self.layout
    }

    /// Create a hidden wallpaper window covering `bounds` (virtual-desktop
    /// coordinates, physical pixels). Show it once the renderer has a frame.
    pub fn create_window(&self, bounds: Rect) -> Result<HostWindow, SurfaceError> {
        let class = host_class()?;
        // SAFETY: null module name means "this executable".
        let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .map_err(|e| SurfaceError::Os(format!("GetModuleHandleW: {e}")))?;

        let (x, y) = self.to_parent_coords(bounds);
        // SAFETY: class is registered; parent is a live window.
        let hwnd = unsafe {
            CreateWindowExW(
                // Never take focus; let clicks fall through to the desktop.
                WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                class,
                w!("wallSet"),
                WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                x,
                y,
                bounds.width as i32,
                bounds.height as i32,
                Some(self.parent),
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .map_err(|e| SurfaceError::Os(format!("CreateWindowExW: {e}")))?;

        let win = HostWindow { hwnd, insert_below: self.defview };
        win.restack();
        Ok(win)
    }

    fn to_parent_coords(&self, b: Rect) -> (i32, i32) {
        let mut pt = [POINT { x: b.x, y: b.y }];
        // SAFETY: converts screen coordinates into the parent's client area,
        // which accounts for monitors left of / above the primary.
        unsafe { MapWindowPoints(None, Some(self.parent), &mut pt) };
        (pt[0].x, pt[0].y)
    }
}

/// One per-monitor wallpaper window. Destroyed on drop.
///
/// `HWND` is not `Send`, so neither is this: it must be destroyed on the
/// thread that created it, which Win32 requires anyway.
#[derive(Debug)]
pub struct HostWindow {
    hwnd: HWND,
    insert_below: Option<HWND>,
}

impl HostWindow {
    /// A plain child window inside one of *our own* windows, for the
    /// settings UI's preview. Same class as the desktop host (black
    /// background, never takes focus) so the preview renders under exactly
    /// the conditions the wallpaper does.
    pub fn create_child(parent: isize, x: i32, y: i32, w: u32, h: u32) -> Result<Self, SurfaceError> {
        let class = host_class()?;
        // SAFETY: null module name means this executable.
        let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .map_err(|e| SurfaceError::Os(format!("GetModuleHandleW: {e}")))?;
        // SAFETY: class registered; parent is the caller's live window.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                w!("wallSet preview"),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                x,
                y,
                w as i32,
                h as i32,
                Some(HWND(parent as *mut _)),
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .map_err(|e| SurfaceError::Os(format!("CreateWindowExW: {e}")))?;
        Ok(Self { hwnd, insert_below: None })
    }

    /// Move/resize in the parent's client coordinates (physical pixels).
    pub fn set_client_rect(&self, x: i32, y: i32, w: u32, h: u32) {
        // SAFETY: our own window.
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, w as i32, h as i32, SWP_NOACTIVATE | SWP_NOZORDER);
        }
    }

    /// Handle to pass to the renderer as mpv's `wid`.
    #[must_use]
    pub fn raw(&self) -> isize {
        self.hwnd.0 as isize
    }

    pub fn show(&self) {
        self.restack();
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        // SAFETY: our own window.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    /// In the Raised layout, keep our window directly beneath the icon layer
    /// so the WorkerW wallpaper ends up beneath *us*.
    fn restack(&self) {
        if let Some(defview) = self.insert_below {
            // SAFETY: inserting after `defview` places us immediately below it.
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(defview),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        // SAFETY: our own window, destroyed once, on its creating thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Set a still image as the wallpaper of the monitor whose bounds match.
///
/// Uses `IDesktopWallpaper`, which is per-monitor, rather than
/// `SystemParametersInfo(SPI_SETDESKWALLPAPER)`, which would put the same image
/// on every screen.
pub fn set_static_wallpaper(bounds: Rect, path: &Path) -> Result<(), SurfaceError> {
    let abs = std::path::absolute(path).map_err(|e| SurfaceError::Os(e.to_string()))?;
    let wide: Vec<u16> = abs.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();

    // SAFETY: COM init on this thread; S_FALSE / RPC_E_CHANGED_MODE are fine,
    // both mean COM is already usable here.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // SAFETY: standard in-proc COM activation.
    let dw: IDesktopWallpaper = unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) }
        .map_err(|e| SurfaceError::Os(format!("IDesktopWallpaper: {e}")))?;

    // SAFETY: COM calls on a live interface; each returned path string is
    // CoTaskMemAlloc'd and freed exactly once below.
    unsafe {
        let n = dw.GetMonitorDevicePathCount().map_err(|e| SurfaceError::Os(e.to_string()))?;
        for i in 0..n {
            let Ok(id) = dw.GetMonitorDevicePathAt(i) else { continue };
            let rect = dw.GetMonitorRECT(PCWSTR(id.0));
            let matched = rect.is_ok_and(|r| rect_eq(r, bounds));
            let result = if matched {
                Some(dw.SetWallpaper(PCWSTR(id.0), PCWSTR(wide.as_ptr())))
            } else {
                None
            };
            let result = match result {
                // Explorer, not this process, opens the file. If this process
                // runs under MSIX file-system virtualisation (installed as a
                // package, or launched from a packaged app), its AppData
                // writes are redirected to a private folder Explorer cannot
                // see: the file exists for us and "does not exist" for it.
                // Retry from %TEMP%, which is not redirected. Windows keeps
                // its own transcoded copy once set, so the temp file may be
                // cleaned up later without harm.
                Some(Err(e)) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() && abs.exists() => {
                    Some(explorer_visible_copy(&abs).and_then(|copy| {
                        let w: Vec<u16> = copy.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
                        dw.SetWallpaper(PCWSTR(id.0), PCWSTR(w.as_ptr())).map_err(|e| e.to_string())
                    }).map_err(|e| windows::core::Error::new(windows::core::HRESULT(0x8007_0002_u32 as i32), e)))
                }
                other => other,
            };
            CoTaskMemFree(Some(id.0 as *const _));
            if let Some(r) = result {
                r.map_err(|e| SurfaceError::Os(format!("SetWallpaper: {e}")))?;
                let _ = dw.SetPosition(DWPOS_FILL);
                return Ok(());
            }
        }
    }
    Err(SurfaceError::NoSuchMonitor(bounds))
}

/// Copy `src` to `%TEMP%\wallSet\` for Explorer to read. See the comment at
/// the call site for why this is ever necessary.
fn explorer_visible_copy(src: &Path) -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join("wallSet");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dst = dir.join(src.file_name().ok_or("image path has no file name")?);
    std::fs::copy(src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// The still image currently set on the monitor whose bounds match.
/// Used to restore the user's own wallpaper after wallSet replaces it.
pub fn get_static_wallpaper(bounds: Rect) -> Result<std::path::PathBuf, SurfaceError> {
    // SAFETY: as in set_static_wallpaper.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let dw: IDesktopWallpaper = unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) }
        .map_err(|e| SurfaceError::Os(format!("IDesktopWallpaper: {e}")))?;
    // SAFETY: COM calls on a live interface; every returned string is freed once.
    unsafe {
        let n = dw.GetMonitorDevicePathCount().map_err(|e| SurfaceError::Os(e.to_string()))?;
        for i in 0..n {
            let Ok(id) = dw.GetMonitorDevicePathAt(i) else { continue };
            let matched = dw.GetMonitorRECT(PCWSTR(id.0)).is_ok_and(|r| rect_eq(r, bounds));
            let found = if matched { dw.GetWallpaper(PCWSTR(id.0)).ok() } else { None };
            CoTaskMemFree(Some(id.0 as *const _));
            if let Some(w) = found {
                let path = w.to_string().map_err(|e| SurfaceError::Os(e.to_string()));
                CoTaskMemFree(Some(w.0 as *const _));
                return path.map(std::path::PathBuf::from);
            }
        }
    }
    Err(SurfaceError::NoSuchMonitor(bounds))
}

pub fn capabilities() -> Capabilities {
    match DesktopHost::acquire() {
        Ok(_) => Capabilities { static_images: true, video: true, video_unavailable_reason: None },
        Err(e) => Capabilities {
            static_images: true,
            video: false,
            video_unavailable_reason: Some(e.to_string()),
        },
    }
}

/// Human-readable dump of the desktop window tree, for diagnostics and bug
/// reports. This is how a new Windows build's layout change gets diagnosed.
#[must_use]
pub fn describe_desktop_hierarchy() -> String {
    let mut out = String::new();
    // SAFETY: plain lookups.
    let Ok(progman) = (unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }) else {
        return "Progman not found (Explorer not running?)".into();
    };
    out.push_str(&format!("Progman {:?}\n", progman.0));
    for child in children(progman) {
        out.push_str(&format!("  +- {} {:?}{}\n", class_name(child), child.0, vis(child)));
    }
    out.push_str("Top-level WorkerW windows:\n");
    for w in top_level_of_class("WorkerW") {
        let kids: Vec<String> = children(w).into_iter().map(class_name).collect();
        out.push_str(&format!(
            "  WorkerW {:?}{} children=[{}]\n",
            w.0,
            vis(w),
            kids.join(", ")
        ));
    }
    match DesktopHost::acquire() {
        Ok(h) => out.push_str(&format!("Detected layout: {:?}, parent {:?}\n", h.layout, h.parent.0)),
        Err(e) => out.push_str(&format!("Detection failed: {e}\n")),
    }
    out
}

// --- helpers ----------------------------------------------------------------

fn host_class() -> Result<PCWSTR, SurfaceError> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    let class = w!("wallSet.Host");

    REGISTERED
        .get_or_init(|| {
            extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
                if m == WM_NCHITTEST {
                    // Belt and braces: never be the target of a mouse click.
                    return LRESULT(HTTRANSPARENT as isize);
                }
                // SAFETY: default handling for our own window.
                unsafe { DefWindowProcW(h, m, wp, lp) }
            }
            // SAFETY: null name means this executable.
            let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| e.to_string())?;
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                // Black, not garbage, in the moment before the first frame.
                // SAFETY: stock objects are never freed.
                hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0),
                lpszClassName: class,
                ..Default::default()
            };
            // SAFETY: wc is fully initialised.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(())
        })
        .clone()
        .map(|()| class)
        .map_err(SurfaceError::Os)
}

fn child_of_class(parent: HWND, class: PCWSTR) -> Option<HWND> {
    // SAFETY: plain lookup.
    unsafe { FindWindowExW(Some(parent), None, class, PCWSTR::null()) }.ok()
}

fn children(parent: HWND) -> Vec<HWND> {
    let mut v = Vec::new();
    // SAFETY: walking a window list; GetWindow returns Err at the end.
    let mut cur = unsafe { GetWindow(parent, GW_CHILD) }.ok();
    while let Some(h) = cur {
        v.push(h);
        cur = unsafe { GetWindow(h, GW_HWNDNEXT) }.ok();
        if v.len() > 256 {
            break;
        }
    }
    v
}

fn all_top_level() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec below, alive for the synchronous call.
        unsafe { &mut *(data.0 as *mut Vec<HWND>) }.push(h);
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    // SAFETY: synchronous enumeration with a valid data pointer.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&raw mut v as isize)) };
    v
}

fn top_level_of_class(class: &str) -> Vec<HWND> {
    all_top_level().into_iter().filter(|h| class_name(*h) == class).collect()
}

fn top_level_with_child(child_class: PCWSTR) -> Option<HWND> {
    all_top_level().into_iter().find(|h| child_of_class(*h, child_class).is_some())
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: buffer length is passed implicitly by the slice.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn vis(h: HWND) -> &'static str {
    // SAFETY: any handle value is acceptable to IsWindowVisible.
    if unsafe { IsWindowVisible(h) }.as_bool() { " (visible)" } else { " (hidden)" }
}

fn rect_eq(r: RECT, b: Rect) -> bool {
    r.left == b.x
        && r.top == b.y
        && (r.right - r.left) as u32 == b.width
        && (r.bottom - r.top) as u32 == b.height
}
