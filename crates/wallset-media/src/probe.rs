//! Media probing through libmpv.
//!
//! The probe opens the file paused with `vo=null`, lets the decoder initialise
//! and produce one frame, reads mpv's properties, and tears down. Reusing mpv
//! rather than shipping ffprobe means one dependency answers both "can we play
//! this" and "what is this", and the answers cannot disagree with each other.
//!
//! ## Hardware-decode detection
//!
//! With `vo=null` there is no GPU output context, so true zero-copy hwdec
//! cannot initialise. The probe uses `auto-copy-safe` instead: the *same*
//! fixed-function decoder block on the GPU, with frames copied back. If the
//! copy variant initialises, the GPU decodes this codec/profile, which is the
//! question being asked.

use crate::mpv::{Event, Library, MpvError};
use std::path::Path;
use std::time::Duration;
use wallset_core::media::{ColorTransfer, MediaKind, MediaProbe, SourcePrimaries};

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error(transparent)]
    Mpv(#[from] MpvError),
    #[error("cannot read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the file has no video or image stream")]
    NoVideo,
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Codecs that mean "this is a picture, not a film".
const STILL_CODECS: &[&str] = &[
    "png", "mjpeg", "jpeg", "jpegls", "jpeg2000", "bmp", "tiff", "webp", "jpegxl", "qoi",
    "targa", "ppm", "pgm", "pbm", "pam", "psd", "exr", "hdr", "dds", "sgi", "pcx", "xbm",
];
/// Codecs that are pictures but may animate.
const ANIMATED_IMAGE_CODECS: &[&str] = &["gif", "apng", "webp"];

pub fn probe(lib: &Library, path: &Path) -> Result<MediaProbe, ProbeError> {
    let meta = std::fs::metadata(path)?;

    let mut mpv = lib.create()?;
    for (k, v) in [
        ("vo", "null"),
        ("ao", "null"),
        ("hwdec", "auto-copy-safe"),
        ("pause", "yes"),
        ("idle", "yes"),
        ("terminal", "no"),
        ("cache", "no"),
        ("demuxer-max-bytes", "4MiB"),
    ] {
        mpv.set_option(k, v)?;
    }
    mpv.initialize()?;

    let p = path.to_string_lossy().into_owned();
    mpv.command(&["loadfile", &p])?;
    mpv.wait_for(PROBE_TIMEOUT, |e| e == Event::FileLoaded)?;

    if mpv.get("vid").as_deref() == Some("no") {
        return Err(ProbeError::NoVideo);
    }

    // video-params and hwdec-current only exist once a frame has been decoded.
    // A file whose decoder never reconfigures is still describable from the
    // container, so a timeout here degrades rather than fails.
    let _ = mpv.wait_for(PROBE_TIMEOUT, |e| {
        matches!(e, Event::VideoReconfig | Event::PlaybackRestart)
    });

    let codec = mpv.get("video-format").unwrap_or_default().to_ascii_lowercase();
    let width = mpv.get_u32("video-params/w").or_else(|| mpv.get_u32("width")).unwrap_or(0);
    let height = mpv.get_u32("video-params/h").or_else(|| mpv.get_u32("height")).unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(ProbeError::NoVideo);
    }

    let duration = mpv.get_f64("duration").filter(|d| d.is_finite() && *d > 0.0);
    let fps = mpv
        .get_f64("container-fps")
        .or_else(|| mpv.get_f64("estimated-vf-fps"))
        .filter(|f| f.is_finite() && *f > 0.0);

    let (has_audio, image_flag) = scan_tracks(&mpv);
    let kind = classify(&codec, image_flag, duration, path);

    let hwdec = mpv
        .get("hwdec-current")
        .filter(|h| !h.is_empty() && h != "no")
        // Report the decoder, not the probe's copy-back transport.
        .map(|h| h.trim_end_matches("-copy").to_string());

    Ok(MediaProbe {
        path: path.to_path_buf(),
        kind,
        width,
        height,
        codec,
        pixel_format: mpv.get("video-params/pixelformat").unwrap_or_default(),
        fps: if kind == MediaKind::Image { None } else { fps },
        duration_secs: if kind == MediaKind::Image { None } else { duration },
        file_size_bytes: meta.len(),
        transfer: ColorTransfer::from_mpv(&mpv.get("video-params/gamma").unwrap_or_default()),
        primaries: SourcePrimaries::from_mpv(
            &mpv.get("video-params/primaries").unwrap_or_default(),
        ),
        // A still image never needs a decoder at runtime, so hwdec is moot.
        hwdec: if kind == MediaKind::Image { None } else { hwdec },
        has_audio,
    })
}

/// Returns (has an audio track, video track is flagged as an image).
fn scan_tracks(mpv: &crate::mpv::Mpv) -> (bool, bool) {
    let n = mpv.get_u32("track-list/count").unwrap_or(0);
    let mut audio = false;
    let mut image = false;
    for i in 0..n {
        match mpv.get(&format!("track-list/{i}/type")).as_deref() {
            Some("audio") => audio = true,
            Some("video")
                if mpv.get(&format!("track-list/{i}/image")).as_deref() == Some("yes") => {
                    image = true;
                }
            _ => {}
        }
    }
    (audio, image)
}

/// Decide still / animated / video. Pure, so it is unit-tested below.
fn classify(codec: &str, image_flag: bool, duration: Option<f64>, path: &Path) -> MediaKind {
    let moving = duration.is_some_and(|d| d > 0.2);
    if ANIMATED_IMAGE_CODECS.contains(&codec) {
        return if moving { MediaKind::AnimatedImage } else { MediaKind::Image };
    }
    if image_flag || STILL_CODECS.contains(&codec) {
        return MediaKind::Image;
    }
    // Containers mpv could not name a codec for: fall back to the extension.
    if codec.is_empty() {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "tif" | "tiff" | "jxl" | "heic" | "avif") {
            return MediaKind::Image;
        }
    }
    MediaKind::Video
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn still_codecs_are_images() {
        assert_eq!(classify("png", false, None, &p("a.png")), MediaKind::Image);
        assert_eq!(classify("mjpeg", false, Some(0.04), &p("a.jpg")), MediaKind::Image);
    }

    #[test]
    fn animated_gif_is_distinguished_from_a_single_frame_gif() {
        assert_eq!(classify("gif", false, Some(3.0), &p("a.gif")), MediaKind::AnimatedImage);
        assert_eq!(classify("gif", false, None, &p("a.gif")), MediaKind::Image);
    }

    #[test]
    fn real_codecs_are_video() {
        for c in ["h264", "hevc", "av1", "vp9"] {
            assert_eq!(classify(c, false, Some(20.0), &p("a.mp4")), MediaKind::Video, "{c}");
        }
    }

    #[test]
    fn mpv_image_flag_wins_over_codec_name() {
        // e.g. a single-frame AV1 inside AVIF is flagged as an image by mpv.
        assert_eq!(classify("av1", true, None, &p("a.avif")), MediaKind::Image);
    }

    #[test]
    fn extension_is_only_a_last_resort() {
        assert_eq!(classify("", false, None, &p("photo.HEIC")), MediaKind::Image);
        assert_eq!(classify("", false, Some(10.0), &p("clip.mkv")), MediaKind::Video);
    }
}
