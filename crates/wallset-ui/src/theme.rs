//! The look: the wallpaper is the room. The window's background is the
//! chosen (or current) wallpaper, blurred and darkened; controls sit on it as
//! translucent panels. Always dark, because the backdrop is darkened for
//! contrast. Type and icons are Windows' own (Segoe UI, Segoe Fluent Icons),
//! loaded from the system and never bundled. The accent is taken from the
//! wallpaper, falling back to the user's Windows accent.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals,
};
use std::sync::Arc;

const SEMIBOLD: &str = "semibold";
const ICONS: &str = "icons";

/// WinUI type ramp, in effective pixels (egui points).
pub const CAPTION: f32 = 12.0;
pub const BODY: f32 = 14.0;
pub const SUBTITLE: f32 = 20.0;
pub const TITLE: f32 = 28.0;

/// How much the blurred wallpaper is darkened (0-255), as if covered by
/// black at this alpha. At 165, white text keeps about 5:1 contrast even
/// over a pure white picture.
pub const SCRIM: u8 = 165;

/// Darkest value the backdrop may reach (see `backdrop::load`).
pub const BLACK_FLOOR: u8 = 40;

/// Segoe Fluent Icons code points.
pub mod icon {
    pub const DISPLAY: char = '\u{E7F4}';
    pub const OPEN_FILE: char = '\u{E8E5}';
    pub const VIDEO: char = '\u{E714}';
    pub const PHOTO: char = '\u{EB9F}';
    pub const PLAY: char = '\u{E768}';
    pub const STOP: char = '\u{E71A}';
    pub const CHECK: char = '\u{E73E}';
    pub const WARNING: char = '\u{E7BA}';
    pub const INFO: char = '\u{E946}';
    pub const ERROR: char = '\u{E783}';
    pub const COPY: char = '\u{E8C8}';
    pub const DELETE: char = '\u{E74D}';
    pub const CLOSE: char = '\u{E711}';
    pub const POWER: char = '\u{E7E8}';
    pub const BATTERY: char = '\u{EBAA}';
    pub const BATTERY_LOW: char = '\u{EBA3}';
    pub const MEMORY: char = '\u{E964}';
    pub const SPEED: char = '\u{EC4A}';
    pub const PAUSE: char = '\u{E769}';
    pub const LEAF: char = '\u{E8BE}';
    pub const CHIP: char = '\u{E950}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const CHEVRON_UP: char = '\u{E70E}';
    pub const LINK: char = '\u{E71B}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const EDIT: char = '\u{E70F}';
    pub const FOLDER: char = '\u{E838}';
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub card: Color32,
    pub card_stroke: Color32,
    pub subtle: Color32,
    pub subtle_hover: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_stroke: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub caution: Color32,
    pub critical: Color32,
    pub success_bg: Color32,
    pub caution_bg: Color32,
    pub critical_bg: Color32,
    pub preview_bg: Color32,
}

impl Palette {
    /// Glass over the darkened wallpaper. `accent` comes from the wallpaper;
    /// `None` uses the Windows accent.
    pub fn new(accent: Option<Color32>) -> Self {
        let tint = |rgb: u32, a: u8| {
            let c = hex(rgb);
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
        };
        Self {
            bg: hex(0x16171A),
            card: tint(0x121214, 150),
            card_stroke: Color32::from_white_alpha(22),
            subtle: Color32::from_white_alpha(14),
            subtle_hover: Color32::from_white_alpha(26),
            control: Color32::from_white_alpha(22),
            control_hover: Color32::from_white_alpha(34),
            control_stroke: Color32::from_white_alpha(30),
            text: hex(0xFFFFFF),
            text2: hex(0xD4D4D4),
            text3: hex(0xA6A6A6),
            accent: accent.unwrap_or_else(windows_accent),
            on_accent: hex(0x000000),
            success: hex(0x6CCB5F),
            caution: hex(0xFCE100),
            critical: hex(0xFF99A4),
            success_bg: tint(0x6CCB5F, 38),
            caution_bg: tint(0xFCE100, 34),
            critical_bg: tint(0xFF6A7A, 44),
            preview_bg: Color32::from_black_alpha(150),
        }
    }
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// Install fonts, dark visuals and spacing once at startup.
pub fn install(ctx: &egui::Context, p: &Palette) {
    install_fonts(ctx);
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    set_palette(ctx, p);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.slider_width = 200.0;
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.text_styles.insert(egui::TextStyle::Body, font(BODY));
        s.text_styles.insert(egui::TextStyle::Button, font(BODY));
        s.text_styles.insert(egui::TextStyle::Small, font(CAPTION));
        s.text_styles.insert(egui::TextStyle::Heading, strong(SUBTITLE));
    });
}

/// Re-apply visuals, e.g. when a new wallpaper changes the accent.
pub fn set_palette(ctx: &egui::Context, p: &Palette) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals(p));
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = p.bg;
    // Tooltips and menus stay opaque: small text on glass reads badly.
    v.window_fill = hex(0x2B2B2B);
    v.window_stroke = Stroke::new(1.0_f32, hex(0x3B3B3B));
    v.extreme_bg_color = p.subtle;
    v.faint_bg_color = p.subtle;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    // Slider rail, text selection and focus take the accent.
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.5_f32, p.text);
    v.text_cursor.stroke = Stroke::new(1.5_f32, p.accent);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);
    for (w, fill) in [
        (&mut v.widgets.noninteractive, p.card),
        (&mut v.widgets.inactive, p.control),
        (&mut v.widgets.hovered, p.control_hover),
        (&mut v.widgets.active, p.subtle_hover),
        (&mut v.widgets.open, p.control),
    ] {
        w.corner_radius = CornerRadius::same(4);
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.expansion = 0.0;
        w.bg_stroke = Stroke::new(1.0_f32, p.control_stroke);
        w.fg_stroke = Stroke::new(1.0_f32, p.text);
    }
    // The slider rail is painted in the inactive fill: keep its unfilled part
    // visible on glass.
    v.widgets.inactive.bg_fill = Color32::from_white_alpha(60);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.card_stroke);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text2);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text2);
    v
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| std::path::PathBuf::from("C:\\Windows"), std::path::PathBuf::from)
        .join("Fonts");
    let mut load = |name: &str, files: &[&str]| -> bool {
        files.iter().any(|f| {
            std::fs::read(dir.join(f))
                .map(|bytes| fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes))))
                .is_ok()
        })
    };
    let regular = load("segoe", &["segoeui.ttf"]);
    let semibold = load("segoe-sb", &["seguisb.ttf", "segoeuib.ttf"]);
    // Windows 11 ships Fluent Icons; Windows 10 has the same code points in MDL2.
    let icons = load("segoe-icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);

    let mut base = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if regular {
        base.insert(0, "segoe".into());
    }
    let with = |first: Option<&str>| {
        let mut list = base.clone();
        if let Some(f) = first {
            list.insert(0, f.into());
        }
        list
    };
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), with(semibold.then_some("segoe-sb")));
    fonts.families.insert(FontFamily::Name(ICONS.into()), with(icons.then_some("segoe-icons")));
    fonts.families.insert(FontFamily::Proportional, base);
    ctx.set_fonts(fonts);
}

/// The user's Windows accent: Light 2 from the accent palette (what WinUI
/// uses on dark backgrounds), or Windows blue.
fn windows_accent() -> Color32 {
    static CACHE: std::sync::OnceLock<Color32> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| read_accent_palette().unwrap_or(hex(0x60CDFF)))
}

#[cfg(windows)]
fn read_accent_palette() -> Option<Color32> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u8; 32];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and length describe the same 32 bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    (status.is_ok() && len == 32).then(|| Color32::from_rgb(buf[4], buf[5], buf[6]))
}

#[cfg(not(windows))]
fn read_accent_palette() -> Option<Color32> {
    None
}

/// The title-bar and taskbar icon: a display showing a landscape, drawn
/// here so there is no image file to ship.
pub fn app_icon() -> egui::IconData {
    const N: usize = 64;
    const BEZEL: [u8; 3] = [0x26, 0x2A, 0x33];
    let round = |x: f32, y: f32, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32| {
        let (cx, cy) = (x.clamp(x0 + r, x1 - r), y.clamp(y0 + r, y1 - r));
        x >= x0 && x <= x1 && y >= y0 && y <= y1 && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    let mut rgba = vec![0u8; N * N * 4];
    for py in 0..N {
        for px in 0..N {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let c = if round(x, y, (9.0, 13.0, 55.0, 41.0), 3.0) {
                let hill = 35.0 - 8.0 * ((x - 22.0) / 12.0).cos().max(0.0);
                if y > hill {
                    [0x1B, 0x3A, 0x6B]
                } else {
                    let t = (y - 13.0) / 28.0;
                    [(0x50 as f32 + t * 0xA0 as f32) as u8, (0x9C as f32 + t * 0x30 as f32) as u8, 0xF0]
                }
            } else if round(x, y, (4.0, 8.0, 60.0, 46.0), 6.0)
                || ((x - 32.0).abs() <= 4.0 && (46.0..54.0).contains(&y))
                || ((x - 32.0).abs() <= 13.0 && (53.0..57.0).contains(&y))
            {
                BEZEL
            } else {
                continue;
            };
            let i = (py * N + px) * 4;
            rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    egui::IconData { rgba, width: N as u32, height: N as u32 }
}
