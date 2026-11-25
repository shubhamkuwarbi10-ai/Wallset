//! Non-Windows placeholder.
//!
//! Deliberately returns [`DisplayError::Unsupported`] rather than inventing a
//! single 1920x1080 sRGB monitor. A fabricated capability report is worse than
//! no report: the whole point of this feature is to tell the user something
//! true about their hardware.
//!
//! The intended implementations, from `docs/ARCHITECTURE.md` section 5:
//!
//! * **macOS** — `NSScreen` for geometry and backing scale,
//!   `maximumPotentialExtendedDynamicRangeColorComponentValue` for EDR
//!   headroom, `CGDisplayCopyDisplayMode` for the mode list and
//!   `CGDisplayCopyColorSpace` for primaries.
//! * **Linux/X11** — XRandR `RRGetScreenResourcesCurrent` plus the `EDID`
//!   output property, parsed for primaries and the preferred timing.
//! * **Linux/Wayland** — `wl_output` and `xdg-output` for geometry; DRM
//!   connector properties (`HDR_OUTPUT_METADATA`, `max_bpc`) for colour, which
//!   requires either a DRM lease or reading from `/sys/class/drm`.

use crate::DisplayError;
use wallset_core::Monitor;

pub fn enumerate() -> Result<Vec<Monitor>, DisplayError> {
    Err(DisplayError::Unsupported)
}

pub fn power_saving_adapter() -> Option<String> {
    None
}
