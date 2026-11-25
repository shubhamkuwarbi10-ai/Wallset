//! The window's backdrop: the wallpaper itself, blurred.
//!
//! Pictures come from the Windows thumbnail cache (`IShellItemImageFactory`),
//! the same ones Explorer shows, for images and videos alike, so no image or
//! video decoder ships with the window. A video Windows cannot thumbnail
//! (HEVC without the Store extension, for example) gets the plain backdrop.

use eframe::egui::{Color32, ColorImage, Rect, Vec2, ecolor::Hsva, pos2};
use std::path::Path;

pub struct Art {
    /// Sharp enough to stand in for the wallpaper in the preview area.
    pub poster: ColorImage,
    /// Small, heavily blurred and darkened by `theme::SCRIM`.
    pub blurred: ColorImage,
    /// A light tint of the picture's dominant hue, or `None` for a
    /// near-grey picture.
    pub accent: Option<Color32>,
}

/// Slow (shell, disk, sometimes a decoder): call on a worker thread.
pub fn load(path: &Path) -> Option<Art> {
    let poster = thumbnail(path, 1280)?;
    let mut blurred = blur(&poster, 96);
    let accent = accent_of(&blurred);
    // Darken as if under black at SCRIM alpha, but keep a black level of
    // BLACK_FLOOR: dark, smoothly varying areas came out corrupted on this
    // machine's compositor (OpenGL and Direct3D alike); a raised floor
    // avoids them.
    let (k, floor) = (u16::from(255 - crate::theme::SCRIM), u16::from(crate::theme::BLACK_FLOOR));
    for c in &mut blurred.pixels {
        let d = |v: u8| (floor + u16::from(v) * k / 255 * (255 - floor) / 255) as u8;
        *c = Color32::from_rgb(d(c.r()), d(c.g()), d(c.b()));
    }
    Some(Art { poster, blurred, accent })
}

/// The part of an image of size `img` that covers `area` without
/// distortion (centre crop), as a UV rectangle.
pub fn cover_uv(img: Vec2, area: Vec2) -> Rect {
    let s = (area.x / img.x).max(area.y / img.y);
    let vis = area / s;
    let off = (img - vis) / 2.0;
    Rect::from_min_max(pos2(off.x / img.x, off.y / img.y), pos2((off.x + vis.x) / img.x, (off.y + vis.y) / img.y))
}

/// Average colour of the top rows: the title bar is tinted to match.
pub fn top_color(img: &ColorImage) -> Color32 {
    let [w, h] = img.size;
    let rows = (h / 8).max(1);
    let mut sum = [0u32; 3];
    for c in &img.pixels[..w * rows] {
        sum[0] += u32::from(c.r());
        sum[1] += u32::from(c.g());
        sum[2] += u32::from(c.b());
    }
    let n = (w * rows) as u32;
    Color32::from_rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8)
}

/// Area-average down to `width` pixels wide, then three box-blur passes:
/// close to a Gaussian, and cheap at this size.
fn blur(src: &ColorImage, width: usize) -> ColorImage {
    let [sw, sh] = src.size;
    let w = width.clamp(1, sw);
    let h = (sh * w / sw).max(1);
    let mut px = vec![[0f32; 3]; w * h];
    let mut n = vec![0f32; w * h];
    for y in 0..sh {
        for x in 0..sw {
            let c = src.pixels[y * sw + x];
            let i = (y * h / sh) * w + x * w / sw;
            px[i][0] += f32::from(c.r());
            px[i][1] += f32::from(c.g());
            px[i][2] += f32::from(c.b());
            n[i] += 1.0;
        }
    }
    for (p, n) in px.iter_mut().zip(&n) {
        *p = p.map(|v| v / n.max(1.0));
    }
    for _ in 0..3 {
        box_pass(&mut px, w, h, true, 4);
        box_pass(&mut px, w, h, false, 4);
    }
    let rgb: Vec<u8> = px.iter().flat_map(|p| p.map(|v| v.round() as u8)).collect();
    ColorImage::from_rgb([w, h], &rgb)
}

fn box_pass(px: &mut [[f32; 3]], w: usize, h: usize, horizontal: bool, r: isize) {
    let src = px.to_vec();
    let k = (2 * r + 1) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for d in -r..=r {
                let (sx, sy) = if horizontal {
                    ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                } else {
                    (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                };
                let c = src[sy * w + sx];
                for i in 0..3 {
                    acc[i] += c[i];
                }
            }
            px[y * w + x] = acc.map(|a| a / k);
        }
    }
}

/// Saturation-weighted mean colour, then the same hue made light enough to
/// carry black text (the accent button) at well over 4.5:1.
fn accent_of(img: &ColorImage) -> Option<Color32> {
    let (mut sum, mut weight, mut sat) = ([0f32; 3], 0f32, 0f32);
    for c in &img.pixels {
        let hsv = Hsva::from(*c);
        let wgt = hsv.s * hsv.s * hsv.v;
        sum[0] += f32::from(c.r()) * wgt;
        sum[1] += f32::from(c.g()) * wgt;
        sum[2] += f32::from(c.b()) * wgt;
        weight += wgt;
        sat += hsv.s;
    }
    if weight <= 0.0 || sat / img.pixels.len() as f32 <= 0.12 {
        return None;
    }
    let mean = Color32::from_rgb((sum[0] / weight) as u8, (sum[1] / weight) as u8, (sum[2] / weight) as u8);
    Some(Hsva::new(Hsva::from(mean).h, 0.6, 1.0, 1.0).into())
}

#[cfg(windows)]
fn thumbnail(path: &Path, max: i32) -> Option<ColorImage> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
        GetDIBits, GetObjectW,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY,
    };
    use windows::core::HSTRING;

    // SAFETY: COM is initialised on this (worker) thread before use; the
    // bitmap is ours to read and delete; the DIB buffer is sized for w*h*4.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        // THUMBNAILONLY: fail rather than hand back the file-type icon.
        let flags = SIIGBF(SIIGBF_THUMBNAILONLY.0 | SIIGBF_BIGGERSIZEOK.0);
        let hbm = factory.GetImage(SIZE { cx: max, cy: max }, flags).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(hbm.into(), size_of::<BITMAP>() as i32, Some((&raw mut bm).cast()));
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w.max(0) * h.max(0) * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(hbm.into());
        if w <= 0 || h <= 0 || lines != h {
            return None;
        }
        let rgb: Vec<u8> = bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
        Some(ColorImage::from_rgb([w as usize, h as usize], &rgb))
    }
}

#[cfg(not(windows))]
fn thumbnail(_path: &Path, _max: i32) -> Option<ColorImage> {
    None
}

/// The picture Windows itself shows on the desktop, when it is a file.
#[cfg(windows)]
pub fn windows_wallpaper() -> Option<std::path::PathBuf> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut buf = [0u16; 260];
    // SAFETY: SPI_GETDESKWALLPAPER writes at most `buf.len()` UTF-16 units.
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    path.is_file().then_some(path)
}

#[cfg(not(windows))]
pub fn windows_wallpaper() -> Option<std::path::PathBuf> {
    None
}

/// Match the native title bar to the backdrop and use light caption text.
#[cfg(windows)]
pub fn tint_title_bar(hwnd: isize, color: Color32) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };
    let h = HWND(hwnd as *mut _);
    let caption = COLORREF(u32::from(color.r()) | u32::from(color.g()) << 8 | u32::from(color.b()) << 16);
    let text = COLORREF(0x00FF_FFFF);
    let dark = 1i32;
    // SAFETY: our own window; each pointer/size pair describes a local.
    // Failures (Windows 10 has no caption colour) leave the default bar.
    unsafe {
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const dark).cast(), 4);
        let _ = DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR, (&raw const caption).cast(), 4);
        let _ = DwmSetWindowAttribute(h, DWMWA_TEXT_COLOR, (&raw const text).cast(), 4);
    }
}

#[cfg(not(windows))]
pub fn tint_title_bar(_hwnd: isize, _color: Color32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_flat_colour_and_aspect() {
        let img = ColorImage::new([400, 200], vec![Color32::from_rgb(10, 120, 200); 400 * 200]);
        let b = blur(&img, 96);
        assert_eq!(b.size, [96, 48]);
        assert!(b.pixels.iter().all(|c| *c == Color32::from_rgb(10, 120, 200)));
    }

    #[test]
    fn grey_picture_has_no_accent_colourful_one_does() {
        let grey = ColorImage::new([8, 8], vec![Color32::from_gray(128); 64]);
        assert!(accent_of(&grey).is_none());
        let red = ColorImage::new([8, 8], vec![Color32::from_rgb(180, 30, 30); 64]);
        let a = accent_of(&red).expect("colourful");
        assert!(a.r() > a.g() && a.r() > a.b());
    }

    #[test]
    fn cover_uv_crops_the_long_side() {
        let uv = cover_uv(Vec2::new(200.0, 100.0), Vec2::new(100.0, 100.0));
        assert!((uv.min.x - 0.25).abs() < 1e-5 && (uv.max.x - 0.75).abs() < 1e-5);
        assert!(uv.min.y.abs() < 1e-5 && (uv.max.y - 1.0).abs() < 1e-5);
    }
}
