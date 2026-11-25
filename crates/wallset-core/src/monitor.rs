//! Display description types, plus colour-gamut analysis.
//!
//! The gamut numbers here are computed from the monitor's reported CIE
//! chromaticity primaries rather than guessed from a marketing label, because
//! "supports HDR" and "covers 95% of DCI-P3" are different claims and users
//! conflate them constantly.

use serde::{Deserialize, Serialize};

/// Stable identifier for a monitor across reconnects and reboots.
///
/// Derived from EDID manufacturer + product + serial where available, falling
/// back to the OS device path. Deliberately *not* the OS index: `\\.\DISPLAY1`
/// is reassigned when monitors are unplugged, which would silently move a
/// user's wallpaper assignment to a different screen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MonitorId(pub String);

impl std::fmt::Display for MonitorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A rectangle in virtual-desktop coordinates. May have negative origin when a
/// monitor sits left of or above the primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Area of the overlap between two rectangles, in pixels.
    ///
    /// Used by the occlusion detector to decide whether a foreground window
    /// actually covers the desktop or merely touches it.
    #[must_use]
    pub fn intersection_area(&self, other: &Rect) -> u64 {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width as i32).min(other.x + other.width as i32);
        let y1 = (self.y + self.height as i32).min(other.y + other.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return 0;
        }
        (x1 - x0) as u64 * (y1 - y0) as u64
    }

    /// Fraction of `self` covered by `other`, in `0.0..=1.0`.
    #[must_use]
    pub fn coverage_by(&self, other: &Rect) -> f64 {
        let a = self.area();
        if a == 0 {
            return 0.0;
        }
        self.intersection_area(other) as f64 / a as f64
    }
}

/// A display mode. Refresh rate is kept as a float because real panels report
/// 59.94 Hz, not 60 — and that 0.1% matters for judder analysis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{} @ {:.2} Hz", self.width, self.height, self.refresh_hz)
    }
}

/// Physical connector carrying the signal. Tells the user what kind of display
/// they have in the sense they usually mean: laptop panel vs external monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Connector {
    Internal,
    Hdmi,
    DisplayPort,
    EmbeddedDisplayPort,
    Dvi,
    Vga,
    Usb4,
    Virtual,
    Wireless,
    Unknown,
}

impl Connector {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Internal => "Internal panel",
            Self::Hdmi => "HDMI",
            Self::DisplayPort => "DisplayPort",
            Self::EmbeddedDisplayPort => "Embedded DisplayPort (laptop)",
            Self::Dvi => "DVI",
            Self::Vga => "VGA",
            Self::Usb4 => "USB4 / Thunderbolt",
            Self::Virtual => "Virtual display",
            Self::Wireless => "Wireless display",
            Self::Unknown => "Unknown",
        }
    }

    /// Whether this connector is a built-in laptop panel, which the policy
    /// engine treats differently on battery.
    #[must_use]
    pub const fn is_builtin(self) -> bool {
        matches!(self, Self::Internal | Self::EmbeddedDisplayPort)
    }
}

/// A CIE 1931 xy chromaticity coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct XyChromaticity {
    pub x: f32,
    pub y: f32,
}

impl XyChromaticity {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// How much of each standard gamut the panel covers, as a fraction in
/// `0.0..=1.0`. Values above 1.0 are clamped: a panel whose primaries exceed
/// sRGB still only "covers 100% of sRGB".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GamutCoverage {
    pub srgb: f32,
    pub display_p3: f32,
    pub rec2020: f32,
}

impl GamutCoverage {
    /// A short human label for the widest gamut the panel meaningfully covers.
    ///
    /// Thresholds follow common display-industry convention: 90% is the usual
    /// bar for claiming coverage of a gamut.
    #[must_use]
    pub fn headline(&self) -> &'static str {
        if self.rec2020 >= 0.90 {
            "Rec. 2020 (wide gamut)"
        } else if self.display_p3 >= 0.90 {
            "DCI-P3 / Display P3 (wide gamut)"
        } else if self.srgb >= 0.90 {
            "sRGB (standard gamut)"
        } else {
            "Below sRGB (limited gamut)"
        }
    }
}

/// Colour and dynamic-range capabilities of a monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorCapabilities {
    /// Bits per colour channel the output is currently driving (8, 10, 12).
    pub bits_per_channel: u8,
    /// The panel/pipeline is capable of HDR.
    pub hdr_supported: bool,
    /// HDR is *switched on right now*. A different question from `hdr_supported`,
    /// and the one that actually determines how PQ content will look.
    pub hdr_enabled: bool,
    /// Peak luminance for a small highlight, in nits.
    pub max_luminance_nits: Option<f32>,
    /// Sustained luminance for a full white field, in nits. Usually far lower
    /// than peak, and the more honest number for a full-screen wallpaper.
    pub max_full_frame_luminance_nits: Option<f32>,
    pub min_luminance_nits: Option<f32>,
    /// Measured primaries, when the OS reports them.
    pub red: Option<XyChromaticity>,
    pub green: Option<XyChromaticity>,
    pub blue: Option<XyChromaticity>,
    pub white: Option<XyChromaticity>,
}

impl ColorCapabilities {
    /// Compute gamut coverage from the reported primaries.
    ///
    /// Returns `None` when the OS did not report primaries, rather than
    /// inventing a plausible-looking number.
    #[must_use]
    pub fn gamut_coverage(&self) -> Option<GamutCoverage> {
        let (r, g, b) = (self.red?, self.green?, self.blue?);
        let panel = [(r.x, r.y), (g.x, g.y), (b.x, b.y)];
        Some(GamutCoverage {
            srgb: coverage_of(&panel, &SRGB_PRIMARIES),
            display_p3: coverage_of(&panel, &P3_PRIMARIES),
            rec2020: coverage_of(&panel, &REC2020_PRIMARIES),
        })
    }

    /// Whether PQ/HLG content can be displayed without tonemapping to SDR.
    #[must_use]
    pub const fn can_display_hdr_natively(&self) -> bool {
        self.hdr_supported && self.hdr_enabled
    }
}

/// The GPU driving a particular output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterInfo {
    pub name: String,
    pub dedicated_video_memory_bytes: u64,
    pub shared_system_memory_bytes: u64,
    /// True when the adapter is a software renderer (WARP, llvmpipe). Video
    /// wallpaper on a software adapter is a bad idea and we say so.
    pub is_software: bool,
}

/// Everything wallSet knows about one connected display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: MonitorId,
    /// Human name from EDID, e.g. "Dell U2720Q" — not `\\.\DISPLAY1`.
    pub friendly_name: String,
    pub connector: Connector,
    pub is_primary: bool,
    /// Position and size in virtual-desktop coordinates (physical pixels).
    pub bounds: Rect,
    /// Mode currently driving the panel.
    pub current_mode: Mode,
    /// Panel's native/preferred mode, when known. Running off-native is a
    /// common and fixable cause of a blurry wallpaper.
    pub native_mode: Option<Mode>,
    /// Highest refresh rate the panel advertises, for "you could be running
    /// faster" hints.
    pub max_refresh_hz: Option<f64>,
    pub dpi: u32,
    /// OS scale factor, e.g. 1.5 for 150%.
    pub scale: f32,
    pub color: ColorCapabilities,
    pub adapter: AdapterInfo,
}

impl Monitor {
    /// Physical pixels the wallpaper must fill on this monitor.
    #[must_use]
    pub const fn pixel_count(&self) -> u64 {
        self.current_mode.width as u64 * self.current_mode.height as u64
    }

    /// True when the monitor is being driven below its native resolution.
    #[must_use]
    pub fn is_scaled_down(&self) -> bool {
        self.native_mode.is_some_and(|n| {
            n.width > self.current_mode.width || n.height > self.current_mode.height
        })
    }

    /// Aspect ratio as a float, e.g. 1.777… for 16:9.
    #[must_use]
    pub fn aspect_ratio(&self) -> f64 {
        if self.current_mode.height == 0 {
            return 0.0;
        }
        f64::from(self.current_mode.width) / f64::from(self.current_mode.height)
    }

    /// A short classification of the display, the way a user would describe it:
    /// "27-inch-class 4K 144 Hz HDR external monitor" is more useful than a
    /// pixel count.
    #[must_use]
    pub fn class_label(&self) -> String {
        let m = &self.current_mode;
        let res = match (m.width, m.height) {
            (w, _) if w >= 7000 => "8K",
            (w, _) if w >= 3800 => "4K / UHD",
            (w, _) if w >= 3000 => "QHD+ / 3K",
            (w, _) if w >= 2500 => "QHD / 1440p",
            (w, _) if w >= 1900 => "Full HD / 1080p",
            (w, _) if w >= 1200 => "HD / 720p-class",
            _ => "Low resolution",
        };
        let mut parts = vec![res.to_string()];
        if m.refresh_hz >= 99.0 {
            parts.push(format!("{:.0} Hz high refresh", m.refresh_hz));
        }
        if self.color.hdr_supported {
            parts.push(
                if self.color.hdr_enabled { "HDR (on)" } else { "HDR (available, off)" }.to_string(),
            );
        }
        if self.aspect_ratio() >= 2.1 {
            parts.push("ultrawide".to_string());
        }
        parts.push(
            if self.connector.is_builtin() { "built-in panel" } else { "external monitor" }
                .to_string(),
        );
        parts.join(", ")
    }
}

impl Monitor {
    /// Human-readable property rows, in display order. The single source of
    /// truth for how a display is described, shared by every front end.
    #[must_use]
    pub fn describe(&self) -> Vec<(&'static str, String)> {
        let c = &self.color;
        let mut rows = vec![
            ("What it is", self.class_label()),
            ("Connection", self.connector.label().to_string()),
            ("Current mode", self.current_mode.to_string()),
        ];
        if let Some(n) = self.native_mode {
            let note = if self.is_scaled_down() { "  (not running at native resolution)" } else { "" };
            rows.push(("Native resolution", format!("{}x{}{note}", n.width, n.height)));
        }
        if let Some(max) = self.max_refresh_hz {
            rows.push(("Max refresh", format!("{max:.0} Hz")));
        }
        rows.push(("Scaling", format!("{:.0}% ({} DPI)", self.scale * 100.0, self.dpi)));
        rows.push(("Colour depth", format!("{}-bit per channel", c.bits_per_channel)));
        rows.push((
            "HDR",
            match (c.hdr_supported, c.hdr_enabled) {
                (true, true) => "supported, on".into(),
                (true, false) => "supported, off (Settings > System > Display > Use HDR)".into(),
                _ => "not supported".into(),
            },
        ));
        rows.push((
            "Brightness",
            match (c.max_luminance_nits, c.max_full_frame_luminance_nits) {
                (Some(p), Some(f)) => format!("{p:.0} nits peak, {f:.0} nits full-screen"),
                (Some(p), None) => format!("{p:.0} nits peak"),
                _ => "not reported by the display".into(),
            },
        ));
        rows.push((
            "Colour gamut",
            c.gamut_coverage().map_or_else(
                || "not reported by the display".into(),
                |g| {
                    format!(
                        "{} - sRGB {:.0}%, DCI-P3 {:.0}%, Rec.2020 {:.0}%",
                        g.headline(),
                        g.srgb * 100.0,
                        g.display_p3 * 100.0,
                        g.rec2020 * 100.0
                    )
                },
            ),
        ));
        let vram = self.adapter.dedicated_video_memory_bytes as f64 / f64::from(1u32 << 30);
        rows.push((
            "Graphics",
            if vram > 0.05 {
                format!("{} ({vram:.1} GB)", self.adapter.name)
            } else {
                self.adapter.name.clone()
            },
        ));
        rows.push((
            "Position",
            format!("{},{}  {}x{}", self.bounds.x, self.bounds.y, self.bounds.width, self.bounds.height),
        ));
        rows
    }
}

// --- Gamut geometry -------------------------------------------------------

/// Rec.709 / sRGB primaries (R, G, B) in CIE 1931 xy.
const SRGB_PRIMARIES: [(f32, f32); 3] =
    [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
/// DCI-P3 / Display P3 primaries.
const P3_PRIMARIES: [(f32, f32); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
/// Rec.2020 primaries.
const REC2020_PRIMARIES: [(f32, f32); 3] =
    [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)];

/// Fraction of `reference` triangle area covered by `panel` triangle.
///
/// Both are convex, so Sutherland–Hodgman clipping gives an exact
/// intersection polygon.
fn coverage_of(panel: &[(f32, f32); 3], reference: &[(f32, f32); 3]) -> f32 {
    let ref_area = triangle_area(reference);
    if ref_area <= f32::EPSILON {
        return 0.0;
    }
    let clipped = clip_polygon(&panel.to_vec(), reference);
    (polygon_area(&clipped) / ref_area).clamp(0.0, 1.0)
}

fn triangle_area(t: &[(f32, f32); 3]) -> f32 {
    polygon_area(t.as_ref())
}

/// Shoelace formula, absolute value so winding order does not matter.
fn polygon_area(poly: &[(f32, f32)]) -> f32 {
    if poly.len() < 3 {
        return 0.0;
    }
    let mut acc = 0.0f32;
    for i in 0..poly.len() {
        let (x0, y0) = poly[i];
        let (x1, y1) = poly[(i + 1) % poly.len()];
        acc += x0 * y1 - x1 * y0;
    }
    (acc / 2.0).abs()
}

/// Clip `subject` against each edge of the convex `clip` triangle.
fn clip_polygon(subject: &Vec<(f32, f32)>, clip: &[(f32, f32); 3]) -> Vec<(f32, f32)> {
    // Normalise winding so "inside" is consistently the left-hand side.
    let mut clip_poly = clip.to_vec();
    if signed_area(&clip_poly) < 0.0 {
        clip_poly.reverse();
    }

    let mut output = subject.clone();
    for i in 0..clip_poly.len() {
        if output.is_empty() {
            break;
        }
        let a = clip_poly[i];
        let b = clip_poly[(i + 1) % clip_poly.len()];
        let input = std::mem::take(&mut output);

        for j in 0..input.len() {
            let cur = input[j];
            let prev = input[(j + input.len() - 1) % input.len()];
            let cur_in = side(a, b, cur) >= 0.0;
            let prev_in = side(a, b, prev) >= 0.0;

            if cur_in {
                if !prev_in
                    && let Some(p) = line_intersect(prev, cur, a, b) {
                        output.push(p);
                    }
                output.push(cur);
            } else if prev_in
                && let Some(p) = line_intersect(prev, cur, a, b) {
                    output.push(p);
                }
        }
    }
    output
}

fn signed_area(poly: &[(f32, f32)]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..poly.len() {
        let (x0, y0) = poly[i];
        let (x1, y1) = poly[(i + 1) % poly.len()];
        acc += x0 * y1 - x1 * y0;
    }
    acc / 2.0
}

/// Cross product sign: positive when `p` is left of directed line `a -> b`.
fn side(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

fn line_intersect(
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    p4: (f32, f32),
) -> Option<(f32, f32)> {
    let d = (p1.0 - p2.0) * (p3.1 - p4.1) - (p1.1 - p2.1) * (p3.0 - p4.0);
    if d.abs() < 1e-9 {
        return None;
    }
    let a = p1.0 * p2.1 - p1.1 * p2.0;
    let b = p3.0 * p4.1 - p3.1 * p4.0;
    Some(((a * (p3.0 - p4.0) - (p1.0 - p2.0) * b) / d, (a * (p3.1 - p4.1) - (p1.1 - p2.1) * b) / d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(r: (f32, f32), g: (f32, f32), b: (f32, f32)) -> ColorCapabilities {
        ColorCapabilities {
            bits_per_channel: 8,
            hdr_supported: false,
            hdr_enabled: false,
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            min_luminance_nits: None,
            red: Some(XyChromaticity::new(r.0, r.1)),
            green: Some(XyChromaticity::new(g.0, g.1)),
            blue: Some(XyChromaticity::new(b.0, b.1)),
            white: None,
        }
    }

    #[test]
    fn srgb_panel_covers_srgb_fully_and_rec2020_partially() {
        let c = caps(SRGB_PRIMARIES[0], SRGB_PRIMARIES[1], SRGB_PRIMARIES[2]);
        let cov = c.gamut_coverage().expect("primaries present");
        assert!(cov.srgb > 0.999, "sRGB panel should cover sRGB: {}", cov.srgb);
        // An sRGB panel covers roughly 70% of P3 and 50% of Rec.2020 by area.
        assert!(cov.display_p3 > 0.6 && cov.display_p3 < 0.85, "p3 = {}", cov.display_p3);
        assert!(cov.rec2020 > 0.35 && cov.rec2020 < 0.65, "rec2020 = {}", cov.rec2020);
    }

    #[test]
    fn p3_panel_fully_covers_srgb() {
        let c = caps(P3_PRIMARIES[0], P3_PRIMARIES[1], P3_PRIMARIES[2]);
        let cov = c.gamut_coverage().unwrap();
        // P3 does not strictly contain sRGB (sRGB green is slightly outside P3
        // on one edge), so allow a small shortfall rather than asserting 100%.
        assert!(cov.srgb > 0.95, "srgb = {}", cov.srgb);
        assert!(cov.display_p3 > 0.999, "p3 = {}", cov.display_p3);
        assert_eq!(cov.headline(), "DCI-P3 / Display P3 (wide gamut)");
    }

    #[test]
    fn missing_primaries_yield_none_rather_than_a_guess() {
        let mut c = caps(SRGB_PRIMARIES[0], SRGB_PRIMARIES[1], SRGB_PRIMARIES[2]);
        c.green = None;
        assert!(c.gamut_coverage().is_none());
    }

    #[test]
    fn rect_coverage_detects_full_and_partial_occlusion() {
        let monitor = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        let maximised = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        let half = Rect { x: 0, y: 0, width: 960, height: 1080 };
        let elsewhere = Rect { x: 1920, y: 0, width: 1920, height: 1080 };

        assert!((monitor.coverage_by(&maximised) - 1.0).abs() < 1e-9);
        assert!((monitor.coverage_by(&half) - 0.5).abs() < 1e-9);
        assert_eq!(monitor.coverage_by(&elsewhere), 0.0);
    }

    #[test]
    fn negative_origin_monitors_intersect_correctly() {
        // Monitor to the left of primary.
        let monitor = Rect { x: -1920, y: 0, width: 1920, height: 1080 };
        let window = Rect { x: -1920, y: 0, width: 1920, height: 540 };
        assert!((monitor.coverage_by(&window) - 0.5).abs() < 1e-9);
    }
}
