//! Platform-agnostic core for wallSet.
//!
//! This crate deliberately contains **no** platform code and forbids `unsafe`.
//! Everything here compiles and unit-tests on any target without a display
//! server, which matters because the resource policy engine — the component
//! most likely to harbour subtle logic bugs — lives here.
//!
//! Platform specifics live in `wallset-display` (capability probing) and
//! `wallset-surface` (wallpaper surface acquisition).

pub mod config;
pub mod fitness;
pub mod media;
pub mod monitor;
pub mod policy;

pub use config::{Assignment, Config, FitMode, Recent, RenderSettings};
pub use fitness::{Finding, FitnessReport, Severity, analyse};
pub use media::{ColorTransfer, MediaKind, MediaProbe};
pub use monitor::{
    AdapterInfo, ColorCapabilities, Connector, GamutCoverage, Monitor, MonitorId, Mode, Rect,
    XyChromaticity,
};
pub use policy::{PolicyConfig, PolicyEngine, PolicyInputs, Tier, Transition};
