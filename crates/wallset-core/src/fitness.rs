//! Cross-checks a selected media file against the monitor it will be shown on.
//!
//! Probing the display is only half of the brief's "show the user what their
//! screen supports". The useful output is the *comparison*: whether this file,
//! on this monitor, will look right and what it will cost.
//!
//! ## On the resource estimates
//!
//! [`ResourceEstimate`] is a coefficient model, not a measurement. Its
//! constants are documented inline and derived from the decode pipeline's
//! structure (frame-pool size x bytes per pixel) rather than from benchmarks
//! on any particular machine. It exists to flag "this will be expensive"
//! *before* the user commits. The preview window reports the genuine measured
//! figures from the live process, and those are what the UI shows once a
//! preview has run. Never present an estimate as a measurement.

use crate::media::{MediaKind, MediaProbe, SourcePrimaries};
use crate::monitor::Monitor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// Worth knowing, nothing to do.
    Info,
    /// A better outcome is available if the user acts.
    Advice,
    /// It will work, but noticeably worse than it should.
    Warning,
    /// It will work badly, or cost far more than the budget allows.
    Problem,
}

impl Severity {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::Advice => "Suggestion",
            Self::Warning => "Warning",
            Self::Problem => "Problem",
        }
    }
}

/// One observation about the file/monitor pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// Short enough to be a list row in the UI.
    pub title: String,
    /// The explanation, in plain language.
    pub detail: String,
    /// A concrete action, where one exists. Often a ready-to-run command.
    pub hint: Option<String>,
}

impl Finding {
    fn new(severity: Severity, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { severity, title: title.into(), detail: detail.into(), hint: None }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Modelled cost of playing this file on this monitor. See the module docs:
/// this is an estimate derived from pipeline structure, not a benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResourceEstimate {
    /// Private (committed) memory of the render process, megabytes. The
    /// honest memory metric; see `wallset-surface/src/process.rs`.
    pub ram_mb: f32,
    /// Percentage of a single CPU core, while the desktop is visible.
    pub cpu_percent_of_one_core: f32,
    /// True when the model's output exceeds the project's stated budget.
    pub exceeds_budget: bool,
}

// ---- Model constants --------------------------------------------------------
//
// Calibrated by linear fit to measurements in docs/BENCHMARKS.md (RTX 3060
// Laptop, NVIDIA 32.0.16.1692, libmpv 2.6, vo=gpu, swapchain-depth=1,
// Nov 2026). The NVIDIA fit is used because it is the more expensive of the
// two GPUs measured, so the model errs towards warning. It predicts the 4K
// 10-bit HDR file within ~10%. Other GPUs and drivers will differ; the
// preview always shows the measured figure for the user's own machine.

/// Private memory at zero source pixels: D3D11 device (driver user-mode
/// allocations dominate this), libmpv, and mpv's renderer.
const PIPELINE_BASE_MB: f32 = 198.0;

/// Private memory per source megapixel at 8 bits per sample: hardware
/// decode surfaces plus mpv's intermediate render textures, all allocated
/// at *source* resolution. Doubles for >8-bit sources (16-bit containers).
const PER_MEGAPIXEL_MB: f32 = 67.4;

/// Hardware-decode CPU: fixed per-stream cost plus a per-presented-frame
/// cost. Under hardware decode CPU tracks frame rate, not resolution.
///
/// Anchored to the one *visible* full-screen measurement (BENCHMARKS.md
/// section 6: 1080p 24 fps, Intel iGPU, 4.53% and 4.61% in two runs). The
/// per-fps slope still comes from runs where the wallpaper was covered,
/// which under-measure presentation; treat high-fps estimates as a lower
/// bound until visible runs at 60 fps exist.
const HW_CPU_FIXED_PERCENT: f32 = 3.47;
const HW_CPU_PER_FPS_PERCENT: f32 = 0.047;

/// Software-decode cost of 1080p30 h.264, percent of one core. Measured 17%
/// at 24 fps with hwdec disabled; scaled to 30 fps.
const SW_DECODE_1080P30_H264_PERCENT: f32 = 21.0;

/// Revised budgets. ARCHITECTURE.md section 1 records the original a-priori
/// figures, the measurements, and why they were revised.
const RAM_BUDGET_MB: f32 = 400.0;
const CPU_BUDGET_PERCENT: f32 = 5.0;

/// The complete report for one file/monitor pairing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitnessReport {
    pub findings: Vec<Finding>,
    pub estimate: ResourceEstimate,
}

impl FitnessReport {
    /// Highest severity present, or `None` when everything is fine.
    #[must_use]
    pub fn worst(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }
}

/// Analyse a media file against the monitor it is destined for.
#[must_use]
pub fn analyse(probe: &MediaProbe, monitor: &Monitor) -> FitnessReport {
    let mut findings = Vec::new();
    let mode = monitor.current_mode;

    check_adapter(monitor, &mut findings);
    check_decode_path(probe, &mut findings);
    check_resolution(probe, monitor, &mut findings);
    check_aspect(probe, monitor, &mut findings);
    check_frame_rate(probe, monitor, &mut findings);
    check_dynamic_range(probe, monitor, &mut findings);
    check_gamut(probe, monitor, &mut findings);
    check_bit_depth(probe, monitor, &mut findings);
    check_duration(probe, &mut findings);
    check_monitor_native_mode(monitor, &mut findings);

    if probe.has_audio {
        findings.push(Finding::new(
            Severity::Info,
            "Audio track will be ignored",
            "wallSet never plays audio from a wallpaper. The audio stream is \
             not decoded at all, so it costs nothing.",
        ));
    }

    let estimate = estimate_resources(probe, monitor);
    if estimate.exceeds_budget {
        findings.push(
            Finding::new(
                Severity::Warning,
                "Above the resource budget",
                format!(
                    "Estimated {:.0} MB and {:.1}% of one core while the desktop is \
                     visible, against a budget of {RAM_BUDGET_MB:.0} MB and \
                     {CPU_BUDGET_PERCENT:.1}%. This is a model, not a measurement — \
                     run the preview to see the real figures.",
                    estimate.ram_mb, estimate.cpu_percent_of_one_core
                ),
            )
            .with_hint(format!(
                "Make a screen-sized copy: {}",
                fix_command(probe, mode.width, mode.height)
            )),
        );
    }

    // The budget warning's fix is often the same command another finding
    // already gives; print it once.
    let mut seen = std::collections::HashSet::new();
    for f in &mut findings {
        // Compare the command, not the sentence around it.
        let key = f.hint.as_deref().map(|h| h.find("ffmpeg ").map_or(h, |i| &h[i..]).to_string());
        if let Some(k) = key
            && !seen.insert(k)
        {
            f.hint = None;
        }
    }

    findings.sort_by(|a, b| b.severity.cmp(&a.severity));
    FitnessReport { findings, estimate }
}

fn check_adapter(monitor: &Monitor, out: &mut Vec<Finding>) {
    if monitor.adapter.is_software {
        out.push(Finding::new(
            Severity::Problem,
            "No hardware GPU on this output",
            format!(
                "This display is driven by '{}', a software renderer. Every frame \
                 will be composited on the CPU. A video wallpaper here will cost \
                 far more than the budget allows.",
                monitor.adapter.name
            ),
        )
        .with_hint("Use a still image on this display instead."));
    }
}

fn check_decode_path(probe: &MediaProbe, out: &mut Vec<Finding>) {
    if probe.kind == MediaKind::AnimatedImage {
        out.push(Finding::new(
            Severity::Advice,
            "Animated image, not a video",
            "Animated GIF and WebP are always decoded on the CPU and store every \
             frame uncompressed. Converting to h.264 or HEVC gives hardware \
             decoding and a much smaller memory footprint.",
        )
        .with_hint(
            "ffmpeg -i input.gif -c:v libx264 -pix_fmt yuv420p -movflags +faststart out.mp4",
        ));
        return;
    }

    if probe.kind != MediaKind::Video {
        return;
    }

    if probe.hwdec.is_none() {
        let sev = if probe.pixel_count() > 2_100_000 { Severity::Problem } else { Severity::Warning };
        out.push(
            Finding::new(
                sev,
                "No hardware decoding available",
                format!(
                    "This GPU cannot decode {} in hardware, so every frame is decoded \
                     on the CPU. This is the single largest cost in the whole pipeline.",
                    probe.codec
                ),
            )
            .with_hint(
                "Re-encode to h.264 (yuv420p, 8-bit), which has hardware decode \
                 support on essentially every GPU made since 2011: \
                 ffmpeg -i input -c:v libx264 -pix_fmt yuv420p -crf 20 out.mp4",
            ),
        );
    }
}

fn check_resolution(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    let mode = monitor.current_mode;
    if probe.width == 0 || mode.width == 0 {
        return;
    }
    let ratio = f64::from(probe.width) / f64::from(mode.width);

    if ratio < 0.7 {
        out.push(Finding::new(
            Severity::Warning,
            "Source is much smaller than the screen",
            format!(
                "The file is {}x{} but this display is {}x{}. It will be upscaled \
                 by {:.0}%, which will look soft.",
                probe.width,
                probe.height,
                mode.width,
                mode.height,
                (1.0 / ratio - 1.0) * 100.0
            ),
        ));
    } else if ratio >= 1.5 {
        // Render buffers are allocated at *source* resolution, so an oversized
        // file pays for pixels this screen can never show. Measured: a 4K file
        // costs ~420 MB more than a 1080p one; 10-bit HDR roughly doubles that.
        let wasted = source_pixel_cost_mb(probe, probe.pixel_count())
            - source_pixel_cost_mb(probe, monitor.pixel_count());
        let sev = if wasted >= 150.0 { Severity::Warning } else { Severity::Advice };
        out.push(
            Finding::new(
                sev,
                "Source is larger than the screen can show",
                format!(
                    "The file is {}x{} on a {}x{} display. Render buffers are sized to the \
                     source, so about {wasted:.0} MB goes on detail this screen cannot \
                     display. A screen-sized copy is the biggest memory saving available.",
                    probe.width, probe.height, mode.width, mode.height
                ),
            )
            .with_hint(format!(
                "Make a screen-sized copy: {}",
                fix_command(probe, mode.width, mode.height)
            )),
        );
    }
}

fn check_aspect(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    let (src, dst) = (probe.aspect_ratio(), monitor.aspect_ratio());
    if src <= 0.0 || dst <= 0.0 {
        return;
    }
    let delta = (src - dst).abs() / dst;
    if delta > 0.08 {
        let crop_pct = delta * 100.0;
        out.push(Finding::new(
            Severity::Advice,
            "Aspect ratio does not match the screen",
            format!(
                "The file is {src:.2}:1 and this display is {dst:.2}:1. Filling the \
                 screen will crop about {crop_pct:.0}% of the image; fitting it will \
                 leave bars."
            ),
        ));
    }
}

fn check_frame_rate(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    let Some(fps) = probe.fps else { return };
    let refresh = monitor.current_mode.refresh_hz;
    if fps <= 0.0 || refresh <= 0.0 {
        return;
    }

    if fps > refresh + 1.0 {
        out.push(Finding::new(
            Severity::Advice,
            "Source frame rate exceeds the refresh rate",
            format!(
                "The file is {fps:.2} fps on a {refresh:.2} Hz display. Frames above \
                 the refresh rate are decoded and then discarded. wallSet caps \
                 presentation at the refresh rate, but the decode cost remains."
            ),
        )
        .with_hint(format!(
            "Re-encode at the display rate: ffmpeg -i input -r {:.0} -c:v libx264 out.mp4",
            refresh.floor()
        )));
        return;
    }

    // Judder check: a clean multiple presents evenly, anything else does not.
    let multiple = refresh / fps;
    let nearest = multiple.round();
    if nearest >= 1.0 && (multiple - nearest).abs() > 0.02 {
        out.push(Finding::new(
            Severity::Advice,
            "Frame rate does not divide into the refresh rate",
            format!(
                "{fps:.2} fps on a {refresh:.2} Hz display means each frame is held for \
                 an uneven number of refreshes, which shows as periodic stutter in \
                 slow panning shots."
            ),
        ));
    }
}

fn check_dynamic_range(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    let c = &monitor.color;
    match (probe.transfer.is_hdr(), c.hdr_supported, c.hdr_enabled) {
        (true, false, _) => out.push(Finding::new(
            Severity::Warning,
            "HDR video on an SDR display",
            "This file is HDR and the display is not. It will be tonemapped down \
             to SDR, which usually looks washed out and flat compared with the \
             source. An SDR version of the file will look better.",
        )
        .with_hint(format!(
            "Make an SDR copy: {}",
            fix_command(probe, monitor.current_mode.width, monitor.current_mode.height)
        ))),
        (true, true, false) => out.push(Finding::new(
            Severity::Advice,
            "HDR is supported here but switched off",
            format!(
                "'{}' can display HDR but HDR is currently disabled in the system \
                 display settings, so this HDR file will be tonemapped to SDR. \
                 Turning HDR on would show it as intended.",
                monitor.friendly_name
            ),
        )
        .with_hint("Windows: Settings > System > Display > Use HDR.")),
        (true, true, true) => {
            let peak = c
                .max_full_frame_luminance_nits
                .or(c.max_luminance_nits)
                .map_or_else(|| "unknown".to_string(), |n| format!("{n:.0} nits"));
            out.push(Finding::new(
                Severity::Info,
                "HDR video on an HDR display",
                format!(
                    "This file will be shown in HDR. Sustained full-screen brightness \
                     on this panel is {peak}; a wallpaper is a full-screen image, so \
                     this is the figure that matters, not the peak highlight rating."
                ),
            ));
        }
        (false, true, true) => out.push(Finding::new(
            Severity::Info,
            "SDR video on an HDR display",
            "The file is SDR and HDR is on. It will be mapped into the HDR \
             container. On some panels this makes SDR content look dimmer or \
             less saturated than with HDR off.",
        )),
        _ => {}
    }
}

fn check_gamut(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    if !probe.primaries.is_wide() {
        return;
    }
    let Some(cov) = monitor.color.gamut_coverage() else { return };
    if cov.display_p3 < 0.80 {
        let name = match probe.primaries {
            SourcePrimaries::Bt2020 => "BT.2020",
            SourcePrimaries::DciP3 => "DCI-P3",
            _ => "wide-gamut",
        };
        out.push(Finding::new(
            Severity::Advice,
            "Wide-gamut source on a standard-gamut screen",
            format!(
                "The file uses {name} primaries, but this panel covers {:.0}% of \
                 DCI-P3 ({:.0}% of sRGB). Saturated colours will be clipped to what \
                 the panel can reproduce.",
                cov.display_p3 * 100.0,
                cov.srgb * 100.0
            ),
        ));
    }
}

fn check_bit_depth(probe: &MediaProbe, monitor: &Monitor, out: &mut Vec<Finding>) {
    let src = probe.bit_depth();
    let dst = monitor.color.bits_per_channel;
    if src > dst && dst > 0 {
        out.push(Finding::new(
            Severity::Info,
            "Source has more colour depth than the output",
            format!(
                "The file is {src}-bit per channel and the display is driving {dst}-bit. \
                 Smooth gradients such as skies may show banding. Dithering reduces \
                 this but cannot remove it."
            ),
        ));
    }
}

fn check_duration(probe: &MediaProbe, out: &mut Vec<Finding>) {
    let Some(d) = probe.duration_secs else { return };
    if d > 0.0 && d < 3.0 {
        out.push(Finding::new(
            Severity::Advice,
            "Very short loop",
            format!(
                "This clip is {d:.1} seconds. Unless the last frame matches the first, \
                 the loop point will be visible every {d:.1} seconds, which is hard \
                 to stop noticing."
            ),
        ));
    }

    let size_mb = probe.file_size_bytes as f64 / 1_048_576.0;
    if d > 600.0 || size_mb > 500.0 {
        out.push(
            Finding::new(
                Severity::Advice,
                "Long or large file",
                format!(
                    "{:.0} minutes, {size_mb:.0} MB. Wallpapers are watched \
                     peripherally, so a short seamless loop usually looks better and \
                     costs less in both memory and disk cache than a long clip.",
                    d / 60.0
                ),
            )
            .with_hint("Trim a loopable section: ffmpeg -i input -ss 0 -t 20 -c copy loop.mp4"),
        );
    }
}

fn check_monitor_native_mode(monitor: &Monitor, out: &mut Vec<Finding>) {
    if !monitor.is_scaled_down() {
        return;
    }
    if let Some(native) = monitor.native_mode {
        out.push(Finding::new(
            Severity::Advice,
            "Display is not running at its native resolution",
            format!(
                "'{}' is running at {} but its panel is {}. Everything on this screen, \
                 wallpaper included, is being rescaled by the display. Switching to \
                 the native mode will look sharper.",
                monitor.friendly_name, monitor.current_mode, native
            ),
        ));
    }

    if let Some(max) = monitor.max_refresh_hz()
        && max > monitor.current_mode.refresh_hz + 1.0
    {
        out.push(Finding::new(
            Severity::Info,
            "Display supports a higher refresh rate",
            format!(
                "Currently {:.2} Hz; the panel advertises up to {max:.2} Hz.",
                monitor.current_mode.refresh_hz
            ),
        ));
    }
}

/// Build the single corrective `ffmpeg` command for this file on this screen.
///
/// Every hint that suggests re-encoding goes through here, so they cannot
/// disagree. In particular an HDR source is always tonemapped: scaling and
/// re-encoding PQ video into 8-bit SDR *without* tonemapping produces exactly
/// the washed-out result the HDR warning exists to prevent.
fn fix_command(probe: &MediaProbe, w: u32, h: u32) -> String {
    let mut filters: Vec<String> = Vec::new();
    if probe.width > w || probe.height > h {
        filters.push(format!("scale={w}:{h}:flags=lanczos"));
    }
    if probe.transfer.is_hdr() {
        // Standard zimg HDR->SDR chain: linearise, tonemap in float, then
        // re-encode the BT.709 transfer and matrix.
        filters.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv"
                .to_string(),
        );
    }
    filters.push("format=yuv420p".to_string());
    format!(
        "ffmpeg -i \"{}\" -vf \"{}\" -c:v libx264 -crf 20 -an -movflags +faststart wallpaper-{}p.mp4",
        probe.path.display(),
        filters.join(","),
        h.min(probe.height),
    )
}

/// Model the cost of playing `probe` on `monitor`. See the constants above:
/// a calibrated estimate, not a measurement.
#[must_use]
pub fn estimate_resources(probe: &MediaProbe, monitor: &Monitor) -> ResourceEstimate {
    if !probe.kind.needs_renderer() {
        // A still image is handed to the OS compositor and we exit.
        return ResourceEstimate { ram_mb: 0.0, cpu_percent_of_one_core: 0.0, exceeds_budget: false };
    }

    let ram_mb = PIPELINE_BASE_MB + source_pixel_cost_mb(probe, probe.pixel_count());

    let fps = probe.fps.unwrap_or(30.0).max(1.0) as f32;
    // Frames beyond the refresh rate are dropped before presentation.
    let presented = fps.min(monitor.current_mode.refresh_hz as f32);
    let megapixels = probe.pixel_count() as f32 / 1_000_000.0;

    let cpu = if probe.is_hardware_decoded() {
        HW_CPU_FIXED_PERCENT + HW_CPU_PER_FPS_PERCENT * presented
    } else {
        // Software decode must process every source frame, presented or not.
        SW_DECODE_1080P30_H264_PERCENT * (megapixels / 2.07) * (fps / 30.0) * probe.codec_cpu_factor()
    };

    ResourceEstimate {
        ram_mb,
        cpu_percent_of_one_core: cpu,
        exceeds_budget: ram_mb > RAM_BUDGET_MB || cpu > CPU_BUDGET_PERCENT,
    }
}

/// The per-pixel part of the memory model, shared with the oversized-source
/// check so the two can never disagree.
fn source_pixel_cost_mb(probe: &MediaProbe, pixels: u64) -> f32 {
    let depth_factor = if probe.bit_depth() > 8 { 2.0 } else { 1.0 };
    PER_MEGAPIXEL_MB * (pixels as f32 / 1_000_000.0) * depth_factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ColorTransfer;
    use crate::monitor::{
        AdapterInfo, ColorCapabilities, Connector, Mode, Monitor, MonitorId, Rect, XyChromaticity,
    };
    use std::path::PathBuf;

    fn sdr_1080p_monitor() -> Monitor {
        Monitor {
            id: MonitorId("TEST-1".into()),
            friendly_name: "Test Display".into(),
            connector: Connector::DisplayPort,
            is_primary: true,
            bounds: Rect { x: 0, y: 0, width: 1920, height: 1080 },
            current_mode: Mode { width: 1920, height: 1080, refresh_hz: 60.0 },
            native_mode: Some(Mode { width: 1920, height: 1080, refresh_hz: 60.0 }),
            dpi: 96,
            scale: 1.0,
            color: ColorCapabilities {
                bits_per_channel: 8,
                hdr_supported: false,
                hdr_enabled: false,
                max_luminance_nits: None,
                max_full_frame_luminance_nits: None,
                red: Some(XyChromaticity::new(0.640, 0.330)),
                green: Some(XyChromaticity::new(0.300, 0.600)),
                blue: Some(XyChromaticity::new(0.150, 0.060)),
            },
            adapter: AdapterInfo {
                name: "Test GPU".into(),
                dedicated_video_memory_bytes: 4 << 30,
                is_software: false,
            },
        }
    }

    fn hdr_4k_monitor() -> Monitor {
        let mut m = sdr_1080p_monitor();
        m.current_mode = Mode { width: 3840, height: 2160, refresh_hz: 144.0 };
        m.native_mode = Some(m.current_mode);
        m.bounds = Rect { x: 0, y: 0, width: 3840, height: 2160 };
        m.color.hdr_supported = true;
        m.color.hdr_enabled = true;
        m.color.bits_per_channel = 10;
        m.color.max_luminance_nits = Some(1000.0);
        m.color.max_full_frame_luminance_nits = Some(600.0);
        m
    }

    fn h264_1080p() -> MediaProbe {
        MediaProbe {
            path: PathBuf::from("test.mp4"),
            kind: MediaKind::Video,
            width: 1920,
            height: 1080,
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            fps: Some(30.0),
            duration_secs: Some(20.0),
            file_size_bytes: 10 << 20,
            transfer: ColorTransfer::Sdr,
            primaries: SourcePrimaries::Bt709,
            hwdec: Some("d3d11va".into()),
            has_audio: false,
        }
    }

    #[test]
    fn a_well_matched_file_produces_a_clean_report() {
        let r = analyse(&h264_1080p(), &sdr_1080p_monitor());
        assert!(r.worst() <= Some(Severity::Advice), "unexpected findings: {:#?}", r.findings);
        assert!(!r.estimate.exceeds_budget);
    }

    #[test]
    fn missing_hardware_decode_is_the_headline_problem_at_4k() {
        let mut p = h264_1080p();
        p.hwdec = None;
        p.width = 3840;
        p.height = 2160;
        let r = analyse(&p, &hdr_4k_monitor());

        assert_eq!(r.worst(), Some(Severity::Problem));
        assert_eq!(
            r.findings[0].severity,
            Severity::Problem,
            "findings must be sorted worst-first for the UI"
        );
        assert!(r.findings.iter().any(|f| f.title.contains("hardware decoding")));
        assert!(
            r.estimate.cpu_percent_of_one_core > 10.0,
            "4K software decode should model as expensive, got {}",
            r.estimate.cpu_percent_of_one_core
        );
    }

    #[test]
    fn software_decode_costs_far_more_cpu_than_hardware() {
        let mut p = h264_1080p();
        p.width = 3840;
        p.height = 2160;
        let hw = estimate_resources(&p, &hdr_4k_monitor());
        p.hwdec = None;
        let sw = estimate_resources(&p, &hdr_4k_monitor());
        assert!(sw.cpu_percent_of_one_core > hw.cpu_percent_of_one_core * 5.0);
    }

    #[test]
    fn model_reproduces_the_benchmark_measurements() {
        // docs/BENCHMARKS.md, NVIDIA column. Tolerance 15%: this is a fit,
        // and the point is to catch the model drifting away from the data.
        let within = |got: f32, want: f32| (got - want).abs() / want < 0.15;
        let m = sdr_1080p_monitor();

        let p = h264_1080p();
        let got = estimate_resources(&p, &m).ram_mb;
        assert!(within(got, 338.0), "1080p modelled at {got} MB");

        let mut p4k = h264_1080p();
        p4k.width = 3840;
        p4k.height = 2160;
        let got = estimate_resources(&p4k, &m).ram_mb;
        assert!(within(got, 757.0), "4K modelled at {got} MB");

        p4k.pixel_format = "p010".into();
        let got = estimate_resources(&p4k, &m).ram_mb;
        assert!(within(got, 1192.0), "4K 10-bit modelled at {got} MB");
    }

    #[test]
    fn a_4k_file_on_a_1080p_screen_is_a_warning() {
        let mut p = h264_1080p();
        p.width = 3840;
        p.height = 2160;
        let r = analyse(&p, &sdr_1080p_monitor());
        let f = r.findings.iter().find(|f| f.title.contains("larger than the screen")).unwrap();
        assert_eq!(f.severity, Severity::Warning, "~420 MB of waste is not a mere suggestion");
    }

    #[test]
    fn every_corrective_command_for_hdr_source_tonemaps() {
        let mut p = h264_1080p();
        p.transfer = ColorTransfer::Pq;
        p.width = 3840;
        p.height = 2160;
        let r = analyse(&p, &sdr_1080p_monitor());
        let cmds: Vec<&str> = r.findings.iter().filter_map(|f| f.hint.as_deref()).filter(|h| h.contains("ffmpeg")).collect();
        assert!(!cmds.is_empty());
        for c in &cmds {
            assert!(c.contains("tonemap=hable"), "HDR re-encode without tonemapping: {c}");
            assert!(c.contains("scale=1920:1080"), "should also fit the screen: {c}");
            assert!(!c.contains("  "), "users copy this; no stray whitespace: {c}");
        }
        assert_eq!(cmds.len(), 1, "the same command must not be repeated: {cmds:#?}");
    }

    #[test]
    fn sdr_fix_command_does_not_tonemap() {
        let mut p = h264_1080p();
        p.width = 3840;
        p.height = 2160;
        let r = analyse(&p, &sdr_1080p_monitor());
        let c = r.findings.iter().find_map(|f| f.hint.as_deref().filter(|h| h.contains("ffmpeg"))).unwrap();
        assert!(!c.contains("tonemap"), "{c}");
        assert!(c.contains("wallpaper-1080p.mp4"), "output named by height, not width: {c}");
    }

    #[test]
    fn hdr_capable_but_disabled_is_advice_not_a_warning() {
        let mut m = hdr_4k_monitor();
        m.color.hdr_enabled = false;
        let mut p = h264_1080p();
        p.transfer = ColorTransfer::Hlg;

        let r = analyse(&p, &m);
        let f = r.findings.iter().find(|f| f.title.contains("switched off")).unwrap();
        assert_eq!(
            f.severity,
            Severity::Advice,
            "the user can fix this in one click, so it is not a warning"
        );
    }

    #[test]
    fn oversized_source_reports_wasted_decode_memory() {
        let mut p = h264_1080p();
        p.width = 3840;
        p.height = 2160;
        let r = analyse(&p, &sdr_1080p_monitor());
        let f = r.findings.iter().find(|f| f.title.contains("larger than the screen")).unwrap();
        assert!(f.hint.as_ref().unwrap().contains("scale=1920:1080"));
    }

    #[test]
    fn undersized_source_warns_about_upscaling() {
        let mut p = h264_1080p();
        p.width = 640;
        p.height = 360;
        let r = analyse(&p, &sdr_1080p_monitor());
        assert!(r.findings.iter().any(|f| f.severity == Severity::Warning
            && f.title.contains("smaller than the screen")));
    }

    #[test]
    fn judder_detected_for_non_integer_refresh_ratio() {
        let mut p = h264_1080p();
        p.fps = Some(25.0); // 60 / 25 = 2.4
        let r = analyse(&p, &sdr_1080p_monitor());
        assert!(r.findings.iter().any(|f| f.title.contains("does not divide")));
    }

    #[test]
    fn clean_multiple_frame_rate_raises_no_judder_finding() {
        let mut p = h264_1080p();
        p.fps = Some(30.0); // 60 / 30 = 2.0 exactly
        let r = analyse(&p, &sdr_1080p_monitor());
        assert!(!r.findings.iter().any(|f| f.title.contains("does not divide")));
    }

    #[test]
    fn software_adapter_is_a_hard_problem() {
        let mut m = sdr_1080p_monitor();
        m.adapter.is_software = true;
        m.adapter.name = "Microsoft Basic Render Driver".into();
        let r = analyse(&h264_1080p(), &m);
        assert_eq!(r.worst(), Some(Severity::Problem));
    }

    #[test]
    fn still_images_cost_nothing_and_need_no_renderer() {
        let mut p = h264_1080p();
        p.kind = MediaKind::Image;
        p.fps = None;
        p.duration_secs = None;
        let e = estimate_resources(&p, &hdr_4k_monitor());
        assert_eq!(e.ram_mb, 0.0);
        assert_eq!(e.cpu_percent_of_one_core, 0.0);
        assert!(!e.exceeds_budget);
    }

    #[test]
    fn animated_gif_is_flagged_with_a_conversion_command() {
        let mut p = h264_1080p();
        p.kind = MediaKind::AnimatedImage;
        p.codec = "gif".into();
        p.hwdec = None;
        let r = analyse(&p, &sdr_1080p_monitor());
        let f = r.findings.iter().find(|f| f.title.contains("Animated image")).unwrap();
        assert!(f.hint.as_ref().unwrap().contains("ffmpeg"));
    }

    #[test]
    fn short_clip_warns_about_a_visible_loop_seam() {
        let mut p = h264_1080p();
        p.duration_secs = Some(1.5);
        let r = analyse(&p, &sdr_1080p_monitor());
        assert!(r.findings.iter().any(|f| f.title.contains("Very short loop")));
    }

    #[test]
    fn off_native_resolution_is_reported_even_when_the_file_is_fine() {
        let mut m = sdr_1080p_monitor();
        m.current_mode = Mode { width: 1280, height: 720, refresh_hz: 60.0 };
        m.native_mode = Some(Mode { width: 1920, height: 1080, refresh_hz: 60.0 });
        let mut p = h264_1080p();
        p.width = 1280;
        p.height = 720;

        let r = analyse(&p, &m);
        assert!(r.findings.iter().any(|f| f.title.contains("not running at its native")));
    }

    #[test]
    fn ten_bit_source_on_eight_bit_output_mentions_banding() {
        let mut p = h264_1080p();
        p.pixel_format = "yuv420p10le".into();
        assert_eq!(p.bit_depth(), 10);
        let r = analyse(&p, &sdr_1080p_monitor());
        assert!(r.findings.iter().any(|f| f.detail.contains("banding")));
    }

    #[test]
    fn bytes_per_pixel_follows_subsampling_and_depth() {
        let mut p = h264_1080p();
        assert!((p.bytes_per_pixel() - 1.5).abs() < 1e-6, "8-bit 4:2:0");
        p.pixel_format = "yuv444p10le".into();
        assert!((p.bytes_per_pixel() - 6.0).abs() < 1e-6, "10-bit 4:4:4 in 16-bit containers");
    }
}
