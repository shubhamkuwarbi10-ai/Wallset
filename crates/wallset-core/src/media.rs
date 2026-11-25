//! Description of a selected media file.
//!
//! Populated by `wallset-media` from libmpv's property API. Kept here, free of
//! platform and FFI concerns, so the fitness analysis in [`crate::fitness`]
//! can be unit-tested against synthetic probes.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    /// A still image. Costs nothing once applied.
    Image,
    /// A video file. Needs a renderer process.
    Video,
    /// An animated GIF/WebP/APNG. Treated as video, but worth distinguishing
    /// because these are usually tiny, loop badly and decode on the CPU.
    AnimatedImage,
}

impl MediaKind {
    #[must_use]
    pub const fn needs_renderer(self) -> bool {
        matches!(self, Self::Video | Self::AnimatedImage)
    }
}

/// Transfer function of the source. Determines whether tonemapping is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorTransfer {
    /// Conventional gamma / BT.1886. The overwhelming majority of files.
    Sdr,
    /// SMPTE ST 2084, "HDR10".
    Pq,
    /// Hybrid Log-Gamma, broadcast HDR.
    Hlg,
    Unknown,
}

impl ColorTransfer {
    #[must_use]
    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Pq | Self::Hlg)
    }

    /// Parse mpv's `video-params/gamma` property value.
    #[must_use]
    pub fn from_mpv(s: &str) -> Self {
        match s {
            "pq" | "st2084" => Self::Pq,
            "hlg" => Self::Hlg,
            "" | "auto" => Self::Unknown,
            _ => Self::Sdr,
        }
    }
}

/// Colour primaries of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourcePrimaries {
    Bt709,
    Bt601,
    Bt2020,
    DciP3,
    Unknown,
}

impl SourcePrimaries {
    /// Parse mpv's `video-params/primaries` property value.
    #[must_use]
    pub fn from_mpv(s: &str) -> Self {
        match s {
            "bt.709" => Self::Bt709,
            "bt.601-525" | "bt.601-625" => Self::Bt601,
            "bt.2020" => Self::Bt2020,
            "dci-p3" | "display-p3" => Self::DciP3,
            _ => Self::Unknown,
        }
    }

    /// Roughly how wide this gamut is, for mismatch comparison.
    #[must_use]
    pub const fn is_wide(self) -> bool {
        matches!(self, Self::Bt2020 | Self::DciP3)
    }
}

/// Everything probed from a media file before it is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaProbe {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    /// Codec short name as mpv reports it: `h264`, `hevc`, `av1`, `vp9`.
    pub codec: String,
    /// Decoded pixel format, e.g. `yuv420p`, `yuv420p10le`.
    pub pixel_format: String,
    pub fps: Option<f64>,
    pub duration_secs: Option<f64>,
    pub file_size_bytes: u64,
    pub bitrate_bps: Option<u64>,
    pub transfer: ColorTransfer,
    pub primaries: SourcePrimaries,
    /// Hardware decoder actually selected, from mpv's `hwdec-current`.
    /// `None` means the file will be decoded on the CPU.
    pub hwdec: Option<String>,
    pub has_audio: bool,
}

impl MediaProbe {
    #[must_use]
    pub const fn pixel_count(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    #[must_use]
    pub fn aspect_ratio(&self) -> f64 {
        if self.height == 0 {
            return 0.0;
        }
        f64::from(self.width) / f64::from(self.height)
    }

    #[must_use]
    pub const fn is_hardware_decoded(&self) -> bool {
        self.hwdec.is_some()
    }

    /// Significant bits per sample, parsed from the FFmpeg/mpv pixel format
    /// name (e.g. `nv12` = 8, `p010` = 10, `yuv420p10le` = 10).
    #[must_use]
    pub fn bit_depth(&self) -> u8 {
        pixel_layout(&self.pixel_format).depth
    }

    /// Bytes of memory one decoded pixel occupies.
    ///
    /// High-bit-depth formats store each sample in a 16-bit container, so a
    /// 10-bit 4:2:0 frame costs 3 bytes per pixel, not 1.875.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> f32 {
        let l = pixel_layout(&self.pixel_format);
        l.samples_per_pixel * l.bytes_per_sample
    }

    /// Relative CPU cost of software-decoding this codec, with h.264 as 1.0.
    ///
    /// These are order-of-magnitude coefficients used only to produce a
    /// warning, never to make a decision. Real figures come from the preview,
    /// which measures the actual process.
    #[must_use]
    pub fn codec_cpu_factor(&self) -> f32 {
        match self.codec.to_ascii_lowercase().as_str() {
            "h264" | "avc1" => 1.0,
            "vp8" => 0.9,
            "mpeg4" | "mpeg2video" => 0.6,
            "vp9" => 1.6,
            "hevc" | "h265" => 1.8,
            "av1" => 2.5,
            _ => 1.3,
        }
    }
}

/// Memory layout of a pixel format, derived from its name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelLayout {
    pub depth: u8,
    /// Samples per pixel after subsampling: 1.5 for 4:2:0, 3 for 4:4:4.
    pub samples_per_pixel: f32,
    pub bytes_per_sample: f32,
}

/// Parse an FFmpeg pixel format name. Unknown names fall back to 8-bit 4:2:0,
/// the overwhelmingly common case, rather than failing.
///
/// Name families handled:
/// * semi-planar: `nv12` `nv21` `nv16` `nv24` (always 8-bit; the digits are
///   *not* a bit depth, which is exactly the trap a substring match falls into)
/// * hardware high-depth: `p010` `p016` `p210` `p412` (4:x:x then depth)
/// * packed 4:2:2: `y210` `y212`
/// * planar: `yuv420p`, `yuv420p10le`, `gbrp12le`, `yuva444p16be`
/// * packed RGB: `rgb24` `bgra` `rgb48le` `x2rgb10le`
/// * greyscale: `gray`, `gray10le`
#[must_use]
pub fn pixel_layout(fmt: &str) -> PixelLayout {
    let f = fmt.to_ascii_lowercase();
    let base = f.strip_suffix("le").or_else(|| f.strip_suffix("be")).unwrap_or(&f);

    let bytes = |depth: u8| if depth > 8 { 2.0 } else { 1.0 };
    let make = |depth: u8, spp: f32| PixelLayout {
        depth,
        samples_per_pixel: spp,
        bytes_per_sample: bytes(depth),
    };

    // nvXX: semi-planar 8-bit. The suffix names the subsampling, not depth.
    if let Some(rest) = base.strip_prefix("nv") {
        let spp = match rest {
            "16" | "61" => 2.0,
            "24" | "42" => 3.0,
            _ => 1.5,
        };
        return make(8, spp);
    }

    // pSDD / ySDD: S = subsampling digit (0 = 4:2:0, 2 = 4:2:2, 4 = 4:4:4),
    // DD = depth.
    if base.len() == 4 && (base.starts_with('p') || base.starts_with('y')) {
        let d = base.as_bytes();
        if d[1..].iter().all(u8::is_ascii_digit) {
            let depth = (d[2] - b'0') * 10 + (d[3] - b'0');
            let spp = match (d[0], d[1]) {
                (b'y', _) | (_, b'2') => 2.0,
                (_, b'4') => 3.0,
                _ => 1.5,
            };
            return make(depth, spp);
        }
    }

    // 10-bit packed RGB: 32 bits per pixel, a special case for memory.
    if base.starts_with("x2rgb10") || base.starts_with("x2bgr10") {
        return PixelLayout { depth: 10, samples_per_pixel: 1.0, bytes_per_sample: 4.0 };
    }

    // Packed RGB(A), whose digits are bits *per pixel*.
    let packed_rgb = ["rgb", "bgr", "argb", "abgr", "0rgb", "0bgr"];
    if packed_rgb.iter().any(|p| base.starts_with(p)) && !base.contains('p') {
        let alpha = base.contains('a') || base.contains('0');
        return match base.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '0') {
            "48" => make(16, 3.0),
            "64" => make(16, 4.0),
            _ => make(8, if alpha { 4.0 } else { 3.0 }),
        };
    }

    // Planar / greyscale: depth is the trailing number after `p` or `gray`.
    let digits_at = base.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (stem, num) = base.split_at(digits_at);
    let depth = if !num.is_empty() && (stem.ends_with('p') || stem == "gray" || stem == "y") {
        num.parse::<u8>().ok().filter(|d| (8..=16).contains(d)).unwrap_or(8)
    } else {
        8
    };
    let alpha = stem.starts_with("yuva") || stem.starts_with("gbrap");
    let spp = if stem.starts_with("gray") || stem == "y" {
        1.0
    } else if stem.contains("444") || stem.starts_with("gbr") {
        3.0
    } else if stem.contains("422") {
        2.0
    } else {
        1.5
    } + if alpha { 1.0 } else { 0.0 };
    make(depth, spp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(f: &str) -> u8 {
        pixel_layout(f).depth
    }
    fn bpp(f: &str) -> f32 {
        let l = pixel_layout(f);
        l.samples_per_pixel * l.bytes_per_sample
    }

    #[test]
    fn digits_that_are_not_bit_depths_are_not_read_as_bit_depths() {
        // The original substring parser reported nv12 as 12-bit and would
        // have reported rgb24 as 24-bit.
        assert_eq!(depth("nv12"), 8);
        assert_eq!(depth("nv21"), 8);
        assert_eq!(depth("rgb24"), 8);
        assert_eq!(depth("yuv420p"), 8);
        assert_eq!(depth("bgra"), 8);
    }

    #[test]
    fn real_high_bit_depths_are_found() {
        assert_eq!(depth("p010"), 10);
        assert_eq!(depth("p016"), 16);
        assert_eq!(depth("yuv420p10le"), 10);
        assert_eq!(depth("yuv444p12be"), 12);
        assert_eq!(depth("gbrp10le"), 10);
        assert_eq!(depth("gray10le"), 10);
        assert_eq!(depth("y210"), 10);
        assert_eq!(depth("rgb48le"), 16);
        assert_eq!(depth("x2rgb10le"), 10);
    }

    #[test]
    fn memory_per_pixel_counts_16_bit_containers() {
        assert!((bpp("nv12") - 1.5).abs() < 1e-6);
        assert!((bpp("yuv420p") - 1.5).abs() < 1e-6);
        assert!((bpp("p010") - 3.0).abs() < 1e-6, "10-bit 4:2:0 is stored in 16 bits");
        assert!((bpp("yuv420p10le") - 3.0).abs() < 1e-6);
        assert!((bpp("yuv444p10le") - 6.0).abs() < 1e-6);
        assert!((bpp("yuv422p") - 2.0).abs() < 1e-6);
        assert!((bpp("rgb24") - 3.0).abs() < 1e-6);
        assert!((bpp("bgra") - 4.0).abs() < 1e-6);
        assert!((bpp("x2rgb10le") - 4.0).abs() < 1e-6);
        assert!((bpp("gray") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_formats_fall_back_to_8_bit_420() {
        let l = pixel_layout("some_future_format");
        assert_eq!(l.depth, 8);
        assert!((l.samples_per_pixel - 1.5).abs() < 1e-6);
    }
}
