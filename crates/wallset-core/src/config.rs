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
use std::path::{Path, PathBuf};

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

/// A wallpaper that was applied before. Kept so it can be found and reused
/// later, even after the file has moved or been deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub path: PathBuf,
    pub kind: MediaKind,
    #[serde(default)]
    pub fit: FitMode,
    /// The web link it was downloaded from, if it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When it was last applied, in seconds since the Unix epoch.
    #[serde(default)]
    pub last_used: u64,
}

/// How many past wallpapers are remembered.
pub const RECENT_MAX: usize = 30;

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

impl RenderSettings {
    /// Command-line flags for `wallset-render`. The one place they are built,
    /// shared by the service and the settings preview, so the preview cannot
    /// drift from what the wallpaper actually runs.
    #[must_use]
    pub fn renderer_args(&self, fit: FitMode, adapter: Option<&str>) -> Vec<String> {
        let mut a = vec!["--fit".to_string(), fit.as_arg().to_string()];
        if let Some(ad) = adapter {
            a.extend(["--adapter".to_string(), ad.to_string()]);
        }
        for (off, flag) in [
            (!self.hardware_decode, "--no-hwdec"),
            (!self.low_power, "--quality"),
            (!self.efficiency_mode, "--foreground"),
        ] {
            if off {
                a.push(flag.to_string());
            }
        }
        a
    }
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self { hardware_decode: true, low_power: true, efficiency_mode: true, prefer_power_saving_gpu: true }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub assignments: Vec<Assignment>,
    pub policy: PolicyConfig,
    pub render: RenderSettings,
    /// Start the daemon when the user signs in.
    pub start_with_windows: bool,
    /// Wallpapers applied before, newest first.
    pub recent: Vec<Recent>,
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

    /// Put a wallpaper at the top of the history, replacing an older entry
    /// for the same file. A web link already known for the file is kept.
    pub fn remember(&mut self, mut r: Recent) {
        if let Some(old) = self.recent.iter().position(|x| x.path == r.path) {
            let old = self.recent.remove(old);
            r.url = r.url.or(old.url);
        }
        self.recent.insert(0, r);
        self.recent.truncate(RECENT_MAX);
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|x| x.path != path);
    }

    /// Clamp everything into a valid range after loading from disk.
    pub fn sanitise(&mut self) {
        self.policy.sanitise();
        self.recent.truncate(RECENT_MAX);
    }

    /// Load and sanitise. A missing file gives defaults. A corrupt one also
    /// gives defaults, is moved aside to `config.json.bad` so a hand edit is
    /// not lost, and the parse error is returned for the caller to report.
    #[must_use]
    pub fn load_from(path: &Path) -> (Self, Option<String>) {
        let (mut cfg, err) = match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<Self>(&s) {
                Ok(c) => (c, None),
                Err(e) => {
                    let _ = std::fs::rename(path, path.with_extension("json.bad"));
                    (Self::default(), Some(e.to_string()))
                }
            },
            Err(_) => (Self::default(), None),
        };
        cfg.sanitise();
        (cfg, err)
    }

    /// Write atomically: a crash mid-write must never leave a truncated file.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// Read-modify-write: load from disk, apply `change`, save. Never writes
    /// a caller's stale snapshot, so edits made elsewhere since the caller
    /// loaded its copy survive.
    pub fn update_at(path: &Path, change: impl FnOnce(&mut Self)) -> std::io::Result<Self> {
        let (mut fresh, _) = Self::load_from(path);
        change(&mut fresh);
        fresh.sanitise();
        fresh.save_to(path)?;
        Ok(fresh)
    }
}

/// `%APPDATA%\wallSet\config.json`, roaming with the user profile (the temp
/// directory where `APPDATA` is unset).
#[must_use]
pub fn default_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("wallSet")
        .join("config.json")
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

    fn r(path: &str, url: Option<&str>) -> Recent {
        Recent { path: path.into(), kind: MediaKind::Video, fit: FitMode::Fill, url: url.map(Into::into), last_used: 1 }
    }

    #[test]
    fn remember_puts_newest_first_without_duplicates_and_keeps_the_link() {
        let mut c = Config::default();
        c.remember(r("a.mp4", Some("https://x/a.mp4")));
        c.remember(r("b.mp4", None));
        c.remember(r("a.mp4", None));
        let paths: Vec<_> = c.recent.iter().map(|x| x.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, ["a.mp4", "b.mp4"]);
        assert_eq!(c.recent[0].url.as_deref(), Some("https://x/a.mp4"));
        c.forget(Path::new("a.mp4"));
        assert_eq!(c.recent.len(), 1);
    }

    #[test]
    fn history_is_capped() {
        let mut c = Config::default();
        for i in 0..RECENT_MAX + 5 {
            c.remember(r(&format!("{i}.mp4"), None));
        }
        assert_eq!(c.recent.len(), RECENT_MAX);
        assert_eq!(c.recent[0].path, PathBuf::from(format!("{}.mp4", RECENT_MAX + 4)));
    }

    #[test]
    fn config_without_history_still_loads() {
        let c: Config = serde_json::from_str(r#"{"assignments": []}"#).unwrap();
        assert!(c.recent.is_empty());
    }

    #[test]
    fn renderer_args_default_to_fit_only() {
        assert_eq!(RenderSettings::default().renderer_args(FitMode::Fill, None), ["--fit", "fill"]);
    }

    #[test]
    fn renderer_args_carry_every_non_default_setting() {
        let r = RenderSettings { hardware_decode: false, low_power: false, efficiency_mode: false, ..Default::default() };
        let a = r.renderer_args(FitMode::Fit, Some("Intel(R) UHD Graphics"));
        assert_eq!(
            a,
            ["--fit", "fit", "--adapter", "Intel(R) UHD Graphics", "--no-hwdec", "--quality", "--foreground"]
        );
    }

    #[test]
    fn update_keeps_assignments_written_by_someone_else() {
        let dir = std::env::temp_dir().join(format!("wallset-core-test-{}", std::process::id()));
        let path = dir.join("config.json");
        // A settings window loads its snapshot (no assignments)...
        let (stale, _) = Config::load_from(&path);
        // ...meanwhile another writer assigns a wallpaper...
        let mut other = Config::default();
        other.assign(a("M1", "elsewhere.mp4"));
        other.save_to(&path).unwrap();
        // ...then the window saves a settings change.
        let low_power = !stale.render.low_power;
        let saved = Config::update_at(&path, |c| c.render.low_power = low_power).unwrap();
        assert_eq!(saved.render.low_power, low_power, "our change applied");
        assert_eq!(saved.assignments.len(), 1, "the other writer's assignment survived");
        assert_eq!(Config::load_from(&path).0, saved, "and that is what is on disk");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_file_yields_defaults_and_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("wallset-core-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        let (cfg, err) = Config::load_from(&path);
        assert_eq!(cfg, Config::default());
        assert!(err.is_some());
        assert!(path.with_extension("json.bad").exists(), "the hand-edited file is preserved");
        let _ = std::fs::remove_dir_all(dir);
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
        assert_eq!(c, Config::default());
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
