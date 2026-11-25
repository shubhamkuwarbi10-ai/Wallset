//! Persistent user configuration.
//!
//! One small JSON file, written by the settings UI and read by the daemon.
//! The UI tells the daemon to reload by posting a window message; there is no
//! socket, no file watcher and no polling (see ARCHITECTURE.md section 3).
//!
//! Every field has a default and unknown fields are ignored, so a config
//! written by a newer version still loads in an older one and vice versa.

use crate::media::MediaKind;
use crate::monitor::MonitorId;
use crate::policy::PolicyConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How the image is mapped onto a screen of a different aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    /// Cover the screen, cropping the overflow. What most people want.
    #[default]
    Fill,
    /// Show the whole image, letterboxed.
    Fit,
    /// Distort to the screen's aspect ratio.
    Stretch,
}

impl FitMode {
    #[must_use]
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
            Self::Stretch => "stretch",
        }
    }
}

/// What is shown on one monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub monitor: MonitorId,
    pub path: PathBuf,
    pub kind: MediaKind,
    #[serde(default)]
    pub fit: FitMode,
}

/// Rendering choices that trade quality for cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderSettings {
    pub hardware_decode: bool,
    /// Bilinear scaling, no dithering or debanding.
    pub low_power: bool,
    /// Windows 11 EcoQoS for the render process. Can drop frames on 4K
    /// software decode; harmless with hardware decode.
    pub efficiency_mode: bool,
    /// On hybrid-graphics laptops, render on the integrated GPU. Measured on
    /// the reference machine: -80 MB private memory, ~-45% CPU.
    pub prefer_power_saving_gpu: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self { hardware_decode: true, low_power: true, efficiency_mode: true, prefer_power_saving_gpu: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version, for future migrations.
    pub version: u32,
    pub assignments: Vec<Assignment>,
    pub policy: PolicyConfig,
    pub render: RenderSettings,
    /// Start the daemon when the user signs in.
    pub start_with_windows: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            assignments: Vec::new(),
            policy: PolicyConfig::default(),
            render: RenderSettings::default(),
            start_with_windows: false,
        }
    }
}

impl Config {
    #[must_use]
    pub fn assignment_for(&self, monitor: &MonitorId) -> Option<&Assignment> {
        self.assignments.iter().find(|a| &a.monitor == monitor)
    }

    /// Replace (or add) the assignment for one monitor.
    pub fn assign(&mut self, a: Assignment) {
        self.assignments.retain(|x| x.monitor != a.monitor);
        self.assignments.push(a);
    }

    pub fn unassign(&mut self, monitor: &MonitorId) {
        self.assignments.retain(|x| &x.monitor != monitor);
    }

    /// Clamp everything into a valid range after loading from disk.
    pub fn sanitise(&mut self) {
        self.policy.sanitise();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(mon: &str, path: &str) -> Assignment {
        Assignment {
            monitor: MonitorId(mon.into()),
            path: path.into(),
            kind: MediaKind::Video,
            fit: FitMode::Fill,
        }
    }

    #[test]
    fn assigning_twice_replaces_rather_than_duplicates() {
        let mut c = Config::default();
        c.assign(a("M1", "a.mp4"));
        c.assign(a("M1", "b.mp4"));
        c.assign(a("M2", "c.mp4"));
        assert_eq!(c.assignments.len(), 2);
        assert_eq!(c.assignment_for(&MonitorId("M1".into())).unwrap().path, PathBuf::from("b.mp4"));
    }

    #[test]
    fn empty_json_loads_as_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn unknown_fields_from_a_newer_version_are_ignored() {
        let c: Config = serde_json::from_str(r#"{"version": 9, "future_feature": true}"#).unwrap();
        assert_eq!(c.version, 9);
    }

    #[test]
    fn partial_render_section_keeps_other_defaults() {
        let c: Config = serde_json::from_str(r#"{"render": {"low_power": false}}"#).unwrap();
        assert!(!c.render.low_power);
        assert!(c.render.hardware_decode, "unspecified fields keep their defaults");
    }

    #[test]
    fn round_trips_through_json() {
        let mut c = Config::default();
        c.assign(a(r"\\?\DISPLAY#DEL41B8#5&1a2b#0#{e6f07b5f}", r"C:\Videos\rain.mp4"));
        c.policy.occluded_grace_ms = 30_000;
        let s = serde_json::to_string_pretty(&c).unwrap();
        assert!(s.contains(r#""fit": "fill""#), "fit mode is readable in the file:\n{s}");
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}
