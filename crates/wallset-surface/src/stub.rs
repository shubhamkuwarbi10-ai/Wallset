//! Non-Windows placeholder. Reports honestly that nothing is implemented,
//! with the intended mechanism for each platform (see ARCHITECTURE.md §2).

use crate::{Capabilities, SurfaceError};
use std::path::Path;
use wallset_core::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLayout {
    Unsupported,
}

#[derive(Debug)]
pub struct DesktopHost;

#[derive(Debug)]
pub struct HostWindow;

impl DesktopHost {
    pub fn acquire() -> Result<Self, SurfaceError> {
        Err(SurfaceError::Unsupported("video wallpaper"))
    }
    #[must_use]
    pub const fn layout(&self) -> DesktopLayout {
        DesktopLayout::Unsupported
    }
    pub fn create_window(&self, _bounds: Rect) -> Result<HostWindow, SurfaceError> {
        Err(SurfaceError::Unsupported("video wallpaper"))
    }
}

impl HostWindow {
    pub fn create_child(_parent: isize, _x: i32, _y: i32, _w: u32, _h: u32) -> Result<Self, SurfaceError> {
        Err(SurfaceError::Unsupported("preview window"))
    }
    pub fn set_client_rect(&self, _x: i32, _y: i32, _w: u32, _h: u32) {}
    #[must_use]
    pub const fn raw(&self) -> isize {
        0
    }
    pub fn show(&self) {}
    pub fn hide(&self) {}
}

pub fn set_static_wallpaper(_bounds: Rect, _path: &Path) -> Result<(), SurfaceError> {
    // macOS: NSWorkspace.setDesktopImageURL(_:for:options:) per NSScreen.
    // GNOME: org.gnome.desktop.background picture-uri (+ picture-uri-dark).
    // KDE: org.kde.PlasmaShell evaluateScript over D-Bus.
    // wlroots: spawn swaybg, or a layer-shell surface of our own.
    Err(SurfaceError::Unsupported("static wallpaper"))
}

pub fn get_static_wallpaper(_bounds: Rect) -> Result<std::path::PathBuf, SurfaceError> {
    Err(SurfaceError::Unsupported("static wallpaper"))
}

#[must_use]
pub fn describe_desktop_hierarchy() -> String {
    "not applicable on this platform".into()
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        static_images: false,
        video: false,
        video_unavailable_reason: Some(
            "This platform backend is not implemented yet. See docs/ARCHITECTURE.md section 2 \
             for the planned mechanism, and the GNOME-Wayland limitation."
                .into(),
        ),
    }
}
