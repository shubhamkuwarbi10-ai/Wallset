//! Per-monitor display capability probing.
//!
//! Answers the brief's "show the user what their screen supports and what kind
//! of display they have". The Windows backend is complete; macOS and Linux are
//! stubs that report their intent honestly rather than returning plausible
//! fabrications.
//!
//! ## Robustness principle
//!
//! A monitor that fails one probe must still appear in the list with the
//! fields that did succeed. Display enumeration on Windows touches three
//! loosely-coupled subsystems (DXGI, the CCD display-config API, and GDI),
//! and any of them can fail on a given driver. Losing a whole monitor because
//! its EDID name could not be read would be a far worse outcome than showing
//! it with a fallback name.

use wallset_core::Monitor;

#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("no displays are attached to the desktop")]
    NoDisplays,
    #[error("display probing is not implemented on this platform yet")]
    Unsupported,
}

/// Probe every monitor currently attached to the desktop.
///
/// Monitors are returned with the primary first, then left-to-right by
/// virtual-desktop x coordinate, which is the order a user expects to see them
/// listed.
pub fn enumerate() -> Result<Vec<Monitor>, DisplayError> {
    let mut monitors = backend::enumerate()?;
    if monitors.is_empty() {
        return Err(DisplayError::NoDisplays);
    }
    monitors.sort_by(|a, b| {
        b.is_primary.cmp(&a.is_primary).then(a.bounds.x.cmp(&b.bounds.x))
    });
    Ok(monitors)
}

/// The integrated GPU's name on a hybrid laptop, for rendering there. See
/// `docs/BENCHMARKS.md`: on the reference machine this cut renderer private
/// memory by ~80 MB and CPU by ~40%.
#[must_use]
pub fn power_saving_adapter() -> Option<String> {
    backend::power_saving_adapter()
}

#[cfg(windows)]
#[path = "win.rs"]
mod backend;

#[cfg(not(windows))]
#[path = "stub.rs"]
mod backend;
