//! The one render configuration shared by the preview and the wallpaper.
//!
//! If preview and wallpaper built their option lists separately, they would
//! drift, and the preview would stop predicting what the wallpaper does. Both
//! call [`RenderProfile::apply`]; the only difference between them is the
//! window handle passed as `wid`.

use crate::mpv::{Mpv, MpvError};
use serde::{Deserialize, Serialize};

pub use wallset_core::FitMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderProfile {
    pub fit: FitMode,
    /// Use the GPU's fixed-function decoder when it supports the codec.
    pub hardware_decode: bool,
    /// Use mpv's `fast` profile: bilinear scaling, no dithering, no
    /// debanding. Visually indistinguishable for a wallpaper viewed
    /// peripherally, and a large GPU saving at 4K.
    pub low_power: bool,
    /// D3D11 adapter description prefix to render on, e.g. the integrated
    /// GPU on a hybrid laptop. `None` lets Windows choose.
    pub adapter: Option<String>,
}

impl Default for RenderProfile {
    fn default() -> Self {
        Self { fit: FitMode::Fill, hardware_decode: true, low_power: true, adapter: None }
    }
}

impl RenderProfile {
    /// The complete option list, in application order. Exposed so it can be
    /// shown in a diagnostics view and asserted on in tests.
    #[must_use]
    pub fn options(&self) -> Vec<(&'static str, String)> {
        let mut o: Vec<(&'static str, String)> = Vec::with_capacity(32);
        let mut set = |k: &'static str, v: &str| o.push((k, v.to_string()));

        // `profile` must come first: it is a bundle of options, and anything
        // set after it overrides the bundle rather than the other way round.
        if self.low_power {
            set("profile", "fast");
        }

        // Output. `gpu`, not `gpu-next`: measured ~30% less CPU for the same
        // file on this pipeline (docs/BENCHMARKS.md); gpu-next's extra quality
        // features are invisible on a wallpaper.
        set("vo", "gpu");
        // One queued frame instead of three: -17 MB measured, CPU unchanged.
        set("swapchain-depth", "1");
        if cfg!(windows) {
            set("gpu-api", "d3d11");
            if let Some(a) = &self.adapter {
                // mpv matches adapters whose description *begins with* this.
                set("d3d11-adapter", a);
            }
        }
        set("hwdec", if self.hardware_decode { "auto-safe" } else { "no" });

        // A wallpaper has no business making noise. `aid=no` means the audio
        // stream is never decoded, not merely muted.
        set("aid", "no");
        set("ao", "null");

        // Memory. The demuxer cache defaults to 150 MiB of read-ahead, sized
        // for network streams. A local looping file needs a sliver of that.
        set("cache", "no");
        set("demuxer-max-bytes", "8MiB");
        set("demuxer-max-back-bytes", "0");
        // Each decoder thread carries its own frame contexts. Two is enough
        // for 1080p/1440p software decode; hardware decode ignores it.
        set("vd-lavc-threads", "2");

        // Behaviour.
        set("loop-file", "inf");
        set("keep-open", "yes");
        set("idle", "yes");
        set("hr-seek", "no");
        set("interpolation", "no");

        // No UI, no input. The wallpaper sits behind desktop icons and must
        // never swallow a click or show an on-screen display.
        set("osc", "no");
        set("osd-level", "0");
        set("input-default-bindings", "no");
        set("input-vo-keyboard", "no");
        set("input-cursor", "no");
        set("cursor-autohide", "always");
        set("terminal", "no");
        set("background-color", "#000000");

        match self.fit {
            FitMode::Fill => {
                set("keepaspect", "yes");
                set("panscan", "1.0");
            }
            FitMode::Fit => {
                set("keepaspect", "yes");
                set("panscan", "0.0");
            }
            FitMode::Stretch => set("keepaspect", "no"),
        }
        o
    }

    /// Apply every option to an uninitialised player.
    pub fn apply(&self, mpv: &mut Mpv) -> Result<(), MpvError> {
        for (k, v) in self.options() {
            mpv.set_option(k, &v)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a>(opts: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        opts.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn audio_is_never_decoded() {
        let o = RenderProfile::default().options();
        assert_eq!(value(&o, "aid"), Some("no"));
        assert_eq!(value(&o, "ao"), Some("null"));
    }

    #[test]
    fn profile_is_applied_before_anything_it_could_override() {
        let o = RenderProfile::default().options();
        assert_eq!(o.first().map(|(k, _)| *k), Some("profile"));
    }

    #[test]
    fn demuxer_cache_is_capped_far_below_mpv_default() {
        let o = RenderProfile::default().options();
        assert_eq!(value(&o, "demuxer-max-bytes"), Some("8MiB"));
        assert_eq!(value(&o, "cache"), Some("no"));
    }

    #[test]
    fn measured_defaults_are_applied() {
        let o = RenderProfile::default().options();
        assert_eq!(value(&o, "vo"), Some("gpu"));
        assert_eq!(value(&o, "swapchain-depth"), Some("1"));
    }

    #[test]
    fn adapter_preference_is_passed_through_only_when_set() {
        assert_eq!(value(&RenderProfile::default().options(), "d3d11-adapter"), None);
        let p = RenderProfile { adapter: Some("Intel(R) UHD Graphics".into()), ..Default::default() };
        if cfg!(windows) {
            assert_eq!(value(&p.options(), "d3d11-adapter"), Some("Intel(R) UHD Graphics"));
        }
    }

    #[test]
    fn disabling_hardware_decode_is_respected() {
        let p = RenderProfile { hardware_decode: false, ..Default::default() };
        assert_eq!(value(&p.options(), "hwdec"), Some("no"));
    }

    #[test]
    fn fit_modes_map_to_distinct_scaling() {
        let fill = RenderProfile { fit: FitMode::Fill, ..Default::default() }.options();
        let fit = RenderProfile { fit: FitMode::Fit, ..Default::default() }.options();
        let stretch = RenderProfile { fit: FitMode::Stretch, ..Default::default() }.options();
        assert_eq!(value(&fill, "panscan"), Some("1.0"));
        assert_eq!(value(&fit, "panscan"), Some("0.0"));
        assert_eq!(value(&stretch, "keepaspect"), Some("no"));
    }
}
