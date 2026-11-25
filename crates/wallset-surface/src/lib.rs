//! Wallpaper surface acquisition.
//!
//! Two jobs:
//!
//! 1. **Static wallpaper** per monitor, through the OS's own wallpaper API.
//!    Once set, wallSet costs nothing: the OS compositor owns the image.
//! 2. **Video host window**: a child window placed *behind the desktop icons*
//!    and *above the OS wallpaper*, into which the render process draws.
//!
//! [`capabilities`] reports what the current session can do, so the UI can
//! explain "video is unavailable here, and why" instead of failing silently.
//! See `docs/ARCHITECTURE.md` section 2 for the per-platform picture.

use wallset_core::Rect;

#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    #[error("the desktop window (Progman) was not found; is Explorer running?")]
    NoProgman,
    #[error("could not locate the desktop layer behind the icons ({0})")]
    NoDesktopLayer(String),
    #[error("window system call failed: {0}")]
    Os(String),
    #[error("no monitor matches the requested bounds {0:?}")]
    NoSuchMonitor(Rect),
    #[error("this platform does not support {0} yet")]
    Unsupported(&'static str),
}

/// What wallpaper modes this session supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub static_images: bool,
    pub video: bool,
    /// Human explanation when `video` is false.
    pub video_unavailable_reason: Option<String>,
}

pub mod process;

pub use backend::{
    DesktopHost, DesktopLayout, HostWindow, describe_desktop_hierarchy, get_static_wallpaper,
    set_static_wallpaper,
};

/// Probe the current session.
#[must_use]
pub fn capabilities() -> Capabilities {
    backend::capabilities()
}

#[cfg(windows)]
#[path = "win.rs"]
mod backend;

#[cfg(not(windows))]
#[path = "stub.rs"]
mod backend;
