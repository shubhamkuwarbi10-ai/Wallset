//! `wallset-ui`: the settings window. Spawned on demand, exits when closed,
//! so its cost (a GUI toolkit and an OpenGL context) is never resident.
//!
//! egui is immediate-mode but eframe repaints *reactively*: only on input, or
//! when we ask (once a second while a preview is measuring). An idle settings
//! window uses no CPU.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod backdrop;
mod preview;
mod service;
mod theme;
mod widgets;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui, vec2,
};
use preview::Preview;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use theme::{Palette, icon};
use wallset_core::fitness::{self, FitnessReport, Severity};
use wallset_core::media::MediaProbe;
use wallset_core::{Assignment, Config, FitMode, Monitor};
use widgets::Tone;

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "gif", "png", "jpg", "jpeg", "bmp", "webp",
    "tif", "tiff", "jxl", "avif", "heic",
];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("wallSet")
            .with_icon(Arc::new(theme::app_icon()))
            .with_inner_size([1100.0, 740.0])
            .with_min_inner_size([920.0, 620.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "wallSet",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx, &Palette::new(None));
            Ok(Box::new(App::new().with_args()))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Wallpaper,
    Display,
    Performance,
}

struct Chosen {
    probe: MediaProbe,
    report: FitnessReport,
}

/// The wallpaper picture behind the window, and its accent colour.
#[derive(Default)]
struct Backdrop {
    started: bool,
    path: Option<PathBuf>,
    pending: Option<mpsc::Receiver<Option<backdrop::Art>>>,
    poster: Option<egui::TextureHandle>,
    blurred: Option<egui::TextureHandle>,
    accent: Option<Color32>,
}

struct App {
    page: Page,
    backdrop: Backdrop,
    /// The cost and fit panel under the preview is open.
    details: bool,
    monitors: Vec<Monitor>,
    selected: usize,
    cfg: Config,
    mpv: Result<wallset_media::Library, String>,
    caps: wallset_surface::Capabilities,
    adapter: Option<String>,

    chosen: Option<Chosen>,
    probing: Option<(PathBuf, mpsc::Receiver<Result<MediaProbe, String>>)>,
    fit: FitMode,
    preview: Option<Preview>,
    /// Fit mode the running preview was started with.
    preview_fit_mode: FitMode,
    status: Option<(Tone, String)>,
    hwnd: Option<isize>,
    /// Start the preview as soon as the probe finishes (`--preview`).
    pending_preview: bool,
}

impl App {
    fn new() -> Self {
        let (monitors, monitor_error) = match wallset_display::enumerate() {
            Ok(m) => (m, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let cfg = Config::load_from(&wallset_core::config::default_path()).0;
        let adapter = cfg.render.prefer_power_saving_gpu.then(wallset_display::power_saving_adapter).flatten();
        let status = monitor_error.map(|e| (Tone::Critical, format!("Cannot read your displays: {e}")));
        Self {
            page: Page::Wallpaper,
            backdrop: Backdrop::default(),
            details: false,
            monitors,
            selected: 0,
            cfg,
            mpv: wallset_media::Library::load().map_err(|e| e.to_string()),
            caps: wallset_surface::capabilities(),
            adapter,
            chosen: None,
            probing: None,
            fit: FitMode::Fill,
            preview: None,
            preview_fit_mode: FitMode::Fill,
            status,
            hwnd: None,
            pending_preview: false,
        }
    }

    /// `wallset-ui [file] [--preview]`: open with a file already chosen, as an
    /// Explorer "Open with wallSet" entry would.
    fn with_args(mut self) -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        self.pending_preview = args.iter().any(|a| a == "--preview");
        if let Some(f) = args.iter().find(|a| !a.starts_with("--")) {
            self.inspect(PathBuf::from(f));
        }
        self
    }

    fn monitor(&self) -> Option<&Monitor> {
        self.monitors.get(self.selected)
    }

    fn choose_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose an image or video")
            .add_filter("Images and videos", MEDIA_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        self.inspect(path);
    }

    fn inspect(&mut self, path: PathBuf) {
        let lib = match &self.mpv {
            Ok(l) => l.clone(),
            Err(e) => {
                self.status = Some((Tone::Critical, format!("Cannot inspect media: {e}")));
                return;
            }
        };
        self.preview = None;
        self.chosen = None;
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(wallset_media::probe(&lib, &p).map_err(|e| e.to_string()));
        });
        self.probing = Some((path, rx));
    }

    fn poll_probe(&mut self) {
        let Some((path, rx)) = &self.probing else { return };
        match rx.try_recv() {
            Ok(Ok(probe)) => {
                if let Some(m) = self.monitor() {
                    let report = fitness::analyse(&probe, m);
                    self.chosen = Some(Chosen { probe, report });
                }
                self.probing = None;
            }
            Ok(Err(e)) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.status = Some((Tone::Critical, format!("Cannot open {name}: {e}")));
                self.probing = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.probing = None,
        }
    }

    fn select_monitor(&mut self, i: usize) {
        if i == self.selected {
            return;
        }
        self.selected = i;
        self.preview = None;
        let Some(m) = self.monitors.get(i) else { return };
        if let Some(c) = &mut self.chosen {
            c.report = fitness::analyse(&c.probe, m);
        }
    }

    /// Read the config from disk, apply one change, write it back, notify
    /// the service. Never writes this window's in-memory snapshot wholesale:
    /// that would silently undo anything changed on disk since the window
    /// opened (another settings window, a hand edit, the CLI). `announce`
    /// shows success too; settings toggles only report failures, as Windows
    /// Settings does.
    fn commit(&mut self, what: &str, announce: bool, change: impl FnOnce(&mut Config)) {
        let result = Config::update_at(&wallset_core::config::default_path(), change)
            .map_err(|e| e.to_string())
            .and_then(|fresh| {
                self.cfg = fresh;
                service::notify_or_start_daemon()
            });
        match result {
            Ok(msg) if announce => self.status = Some((Tone::Success, format!("{what} {msg}"))),
            Ok(_) => {}
            Err(e) => self.status = Some((Tone::Critical, format!("{what} failed: {e}"))),
        }
    }

    fn apply(&mut self) {
        let (Some(m), Some(c)) = (self.monitor(), &self.chosen) else { return };
        let a = Assignment { monitor: m.id.clone(), path: c.probe.path.clone(), kind: c.probe.kind, fit: self.fit };
        let name = m.friendly_name.clone();
        self.preview = None;
        self.commit(&format!("Applied to {name}."), true, |c| c.assign(a));
    }

    fn remove(&mut self) {
        let Some(m) = self.monitor() else { return };
        let (id, name) = (m.id.clone(), m.friendly_name.clone());
        self.commit(&format!("Removed from {name}."), true, |c| c.unassign(&id));
    }

    fn start_preview(&mut self, px: (i32, i32, u32, u32)) {
        self.preview = None;
        let (Some(hwnd), Some(c)) = (self.hwnd, &self.chosen) else {
            self.status = Some((Tone::Critical, "Preview needs a native window handle.".into()));
            return;
        };
        clip_children(hwnd);
        match Preview::start(hwnd, px, &c.probe.path, self.fit, &self.cfg.render, self.adapter.as_deref()) {
            Ok(p) => {
                self.preview = Some(p);
                self.preview_fit_mode = self.fit;
            }
            Err(e) => self.status = Some((Tone::Critical, format!("Preview failed: {e}"))),
        }
    }
}

impl App {
    fn palette(&self) -> Palette {
        Palette::new(self.backdrop.accent)
    }

    /// The picture behind everything: the chosen file, else what this
    /// display shows now, else the Windows wallpaper. Loaded on a worker
    /// thread that wakes the window when done, so nothing polls.
    fn sync_backdrop(&mut self, ctx: &egui::Context) {
        let want = self.chosen.as_ref().map(|c| c.probe.path.clone()).or_else(|| {
            let m = self.monitor()?;
            self.cfg.assignment_for(&m.id).map(|a| a.path.clone()).or_else(backdrop::windows_wallpaper)
        });
        if !self.backdrop.started || want != self.backdrop.path {
            self.backdrop.started = true;
            self.backdrop.path = want.clone();
            // The old picture must never stand in for a different file.
            self.backdrop.poster = None;
            self.backdrop.pending = want.map(|path| {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(backdrop::load(&path));
                    ctx.request_repaint();
                });
                rx
            });
            if self.backdrop.pending.is_none() {
                self.set_art(ctx, None);
            }
        }
        if let Some(rx) = &self.backdrop.pending {
            match rx.try_recv() {
                Ok(art) => {
                    self.backdrop.pending = None;
                    self.set_art(ctx, art);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.backdrop.pending = None;
                    self.set_art(ctx, None);
                }
            }
        }
    }

    fn set_art(&mut self, ctx: &egui::Context, art: Option<backdrop::Art>) {
        let b = &mut self.backdrop;
        b.accent = art.as_ref().and_then(|a| a.accent);
        let p = Palette::new(b.accent);
        // The title bar continues the backdrop's top edge.
        let title = art.as_ref().map_or(p.bg, |a| backdrop::top_color(&a.blurred));
        match art {
            Some(a) => {
                b.poster = Some(ctx.load_texture("poster", a.poster, egui::TextureOptions::LINEAR));
                b.blurred = Some(ctx.load_texture("backdrop", a.blurred, egui::TextureOptions::LINEAR));
            }
            None => (b.poster, b.blurred) = (None, None),
        }
        theme::set_palette(ctx, &p);
        if let Some(h) = self.hwnd {
            backdrop::tint_title_bar(h, title);
        }
    }
}

impl eframe::App for App {
    /// Opaque: eframe's default clear colour is translucent.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.hwnd.is_none() {
            self.hwnd = native_hwnd(frame);
        }
        self.poll_probe();
        if self.probing.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            self.page = Page::Wallpaper;
            self.inspect(path);
        }
        self.sync_backdrop(ctx);
        let p = self.palette();

        // The room: the wallpaper, blurred and darkened, under everything.
        let screen = ctx.content_rect();
        let bg = ctx.layer_painter(egui::LayerId::background());
        bg.rect_filled(screen, CornerRadius::ZERO, p.bg);
        if let Some(t) = &self.backdrop.blurred {
            bg.image(t.id(), screen, backdrop::cover_uv(t.size_vec2(), screen.size()), Color32::WHITE);
        }

        egui::TopBottomPanel::top("header")
            .frame(Frame::new().inner_margin(Margin { left: 24, right: 24, top: 14, bottom: 4 }))
            .show_separator_line(false)
            .show(ctx, |ui| self.header(ui, &p));

        egui::CentralPanel::default()
            .frame(Frame::new().inner_margin(Margin { left: 24, right: 24, top: 10, bottom: 18 }))
            .show(ctx, |ui| match self.page {
                Page::Wallpaper => self.wallpaper_page(ui, ctx, &p),
                Page::Display => scroll_page(ui, |ui| self.display_page(ui, &p)),
                Page::Performance => scroll_page(ui, |ui| self.performance_page(ui, &p)),
            });
    }
}

/// A centred column for the settings-style pages.
fn scroll_page(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        let w = ui.available_width().min(820.0);
        ui.horizontal_top(|ui| {
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            ui.vertical(|ui| {
                ui.set_width(w);
                add(ui);
                ui.add_space(12.0);
            });
        });
    });
}

/// Centre content whose width is only known once laid out: use last pass's
/// width, and ask egui for another pass when it changed.
fn centered<R>(ui: &mut Ui, salt: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let id = ui.id().with(salt);
    let known: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    let left = ((ui.available_width() - known) / 2.0).max(0.0);
    let r = ui.horizontal(|ui| {
        ui.add_space(left);
        ui.scope(add)
    });
    let used = r.inner.response.rect.width();
    if (used - known).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, used));
        ui.ctx().request_discard("centring");
    }
    r.inner.inner
}

// --- Header ---------------------------------------------------------------

impl App {
    fn header(&mut self, ui: &mut Ui, p: &Palette) {
        let mut page = self.page;
        centered(ui, "tabs", |ui| {
            widgets::segmented(
                ui,
                p,
                &mut page,
                &[(Page::Wallpaper, "Wallpaper"), (Page::Display, "Display"), (Page::Performance, "Performance")],
            )
        });
        if page != self.page {
            // The preview is a native window; it must not float over
            // another page.
            self.preview = None;
            self.page = page;
        }
        if let Some((tone, msg)) = &self.status {
            ui.add_space(10.0);
            let w = ui.available_width().min(760.0);
            let mut closed = false;
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(w);
                    closed = widgets::info_bar(ui, p, *tone, msg);
                });
            });
            if closed {
                self.status = None;
            }
        }
        ui.add_space(4.0);
    }
}

// --- Wallpaper page -------------------------------------------------------

const DOCK_H: f32 = 52.0;
const SUMMARY_H: f32 = 24.0;
const DETAILS_H: f32 = 236.0;

impl App {
    fn wallpaper_page(&mut self, ui: &mut Ui, ctx: &egui::Context, p: &Palette) {
        let Some(m) = self.monitor().cloned() else {
            ui.label("No displays found.");
            return;
        };
        let open = self.details && self.chosen.is_some();
        // Every row also adds the item spacing below it.
        let rows = if open { 4.0 } else { 3.0 };
        let below = 14.0 + DOCK_H + 10.0 + SUMMARY_H + if open { 12.0 + DETAILS_H } else { 0.0 }
            + rows * ui.spacing().item_spacing.y
            + 4.0;
        let avail = ui.available_size();
        // The preview keeps the display's aspect ratio, so what it crops is
        // what the wallpaper will crop.
        let aspect = m.aspect_ratio().max(0.5) as f32;
        let max_h = (avail.y - below).max(150.0);
        let w = avail.x.min(1400.0);
        let mut size = vec2(w, w / aspect);
        if size.y > max_h {
            size = vec2(max_h * aspect, max_h);
        }
        // The bars need room for their controls even when the preview is
        // short and narrow.
        let bar_w = size.x.max(avail.x.min(680.0));
        let indent = |ui: &mut Ui, width: f32| ui.add_space(((avail.x - width) / 2.0).max(0.0));

        ui.horizontal(|ui| {
            indent(ui, size.x);
            self.preview_area(ui, ctx, p, size, &m);
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            indent(ui, bar_w);
            ui.allocate_ui_with_layout(vec2(bar_w, DOCK_H), Layout::top_down(Align::Min), |ui| self.dock(ui, p));
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            indent(ui, bar_w);
            ui.allocate_ui_with_layout(vec2(bar_w, SUMMARY_H), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(bar_w);
                self.summary(ui, p, &m);
            });
        });
        if open {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                indent(ui, bar_w);
                ui.allocate_ui_with_layout(vec2(bar_w, DETAILS_H), Layout::top_down(Align::Min), |ui| {
                    self.details_panel(ui, p, &m);
                });
            });
        }
    }

    fn preview_area(&mut self, ui: &mut Ui, ctx: &egui::Context, p: &Palette, size: egui::Vec2, m: &Monitor) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovering_file = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let ppp = ctx.pixels_per_point();
        let px = (
            (rect.min.x * ppp).round() as i32,
            (rect.min.y * ppp).round() as i32,
            (rect.width() * ppp).round() as u32,
            (rect.height() * ppp).round() as u32,
        );
        ui.painter().rect_filled(rect, CornerRadius::same(10), p.preview_bg);

        if self.pending_preview && self.preview.is_none() && self.hwnd.is_some() && self.chosen.is_some() {
            self.pending_preview = false;
            self.start_preview(px);
        }
        if let Some(pv) = &mut self.preview {
            pv.place(px);
            pv.sample();
            if !pv.running() {
                self.status = Some((Tone::Critical, "The preview stopped unexpectedly.".into()));
                self.preview = None;
            }
            ctx.request_repaint_after(Duration::from_secs(1));
            return;
        }

        // The still picture, cropped exactly as the chosen fit mode will.
        if let Some(t) = &self.backdrop.poster {
            let img = t.size_vec2();
            let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            let (r, uv, radius) = match self.fit {
                FitMode::Fill => (rect, backdrop::cover_uv(img, rect.size()), 10),
                FitMode::Stretch => (rect, full, 10),
                FitMode::Fit => {
                    let s = (rect.width() / img.x).min(rect.height() / img.y);
                    (egui::Rect::from_center_size(rect.center(), img * s), full, 0)
                }
            };
            egui::Image::from_texture((t.id(), img)).uv(uv).corner_radius(radius).paint_at(ui, r);
        }

        let painter = ui.painter_at(rect);
        let mode = m.current_mode;
        chip(&painter, rect.left_top() + vec2(12.0, 12.0), Align2::LEFT_TOP, &format!(
            "{}  ·  {} × {}  ·  {:.0} Hz",
            m.friendly_name, mode.width, mode.height, mode.refresh_hz
        ));
        let assigned = self.cfg.assignment_for(&m.id).map(|a| a.path.clone());
        let state = match &self.chosen {
            Some(c) if Some(&c.probe.path) == assigned.as_ref() => Some("On your desktop now"),
            Some(_) => Some("Not applied yet"),
            None if self.backdrop.poster.is_some() => Some("On your desktop now"),
            None => None,
        };
        if let Some(s) = state {
            chip(&painter, rect.right_top() + vec2(-12.0, 12.0), Align2::RIGHT_TOP, s);
        }

        let c = rect.center();
        let (light, dim) = (Color32::from_gray(245), Color32::from_gray(200));
        if hovering_file {
            painter.rect_filled(rect, CornerRadius::same(10), Color32::from_black_alpha(140));
            painter.rect_stroke(rect.shrink(1.0), CornerRadius::same(10), Stroke::new(2.0_f32, p.accent), StrokeKind::Inside);
            painter.text(c, Align2::CENTER_CENTER, "Drop to choose this file", theme::strong(theme::SUBTITLE), light);
        } else if self.probing.is_some() {
            pill_text(&painter, c, "Inspecting the file…", theme::font(theme::BODY), light);
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.chosen.is_some() {
            let hot = resp.hovered();
            painter.circle_filled(c, if hot { 34.0 } else { 32.0 }, p.accent);
            painter.text(c + vec2(2.0, 0.0), Align2::CENTER_CENTER, icon::PLAY, theme::icon_font(24.0), p.on_accent);
            pill_text(&painter, c + vec2(0.0, 58.0), "Play a live preview", theme::font(theme::CAPTION), light);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.start_preview(px);
            }
        } else {
            if self.backdrop.poster.is_some() {
                painter.rect_filled(rect, CornerRadius::same(10), Color32::from_black_alpha(90));
            }
            let hot = resp.hovered();
            let button = egui::Rect::from_center_size(c - vec2(0.0, 10.0), vec2(250.0, 44.0));
            let fill = if hot { p.accent.gamma_multiply(0.9) } else { p.accent };
            painter.rect_filled(button, CornerRadius::same(22), fill);
            painter.text(button.left_center() + vec2(26.0, 0.0), Align2::LEFT_CENTER, icon::OPEN_FILE, theme::icon_font(16.0), p.on_accent);
            painter.text(button.center() + vec2(12.0, -1.0), Align2::CENTER_CENTER, "Choose an image or video", theme::strong(theme::BODY), p.on_accent);
            painter.text(c + vec2(0.0, 34.0), Align2::CENTER_CENTER, "or drop a file onto this window", theme::font(theme::CAPTION), dim);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.choose_file();
            }
        }
    }

    /// The control bar under the preview. Segmented controls only: a
    /// drop-down would open underneath the native preview window.
    fn dock(&mut self, ui: &mut Ui, p: &Palette) {
        widgets::glass(ui, p, Margin::symmetric(10, 10), 12, |ui| {
            ui.horizontal(|ui| {
                if self.monitors.len() > 1 {
                    self.monitor_tiles(ui, p);
                    ui.add_space(4.0);
                }
                let mut fit = self.fit;
                widgets::segmented(ui, p, &mut fit, &[(FitMode::Fill, "Fill"), (FitMode::Fit, "Fit"), (FitMode::Stretch, "Stretch")]);
                self.fit = fit;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let has_file = self.chosen.is_some();
                    if widgets::accent_button(ui, p, Some(icon::CHECK), "Apply", has_file).clicked() {
                        self.apply();
                    }
                    let fit_changed = self.preview.is_some() && self.preview_fit_mode != self.fit;
                    if self.preview.is_some() && !fit_changed {
                        if widgets::button(ui, p, Some(icon::STOP), "Stop").clicked() {
                            self.preview = None;
                        }
                    } else if has_file {
                        let label = if fit_changed { "Update preview" } else { "Preview" };
                        if widgets::button(ui, p, Some(icon::PLAY), label).clicked() {
                            // The preview area starts it on the next frame,
                            // where the pixel rectangle is known.
                            self.preview = None;
                            self.pending_preview = true;
                            ui.ctx().request_repaint();
                        }
                    }
                    let label = if has_file { "Change file" } else { "Choose file" };
                    if widgets::button(ui, p, Some(icon::OPEN_FILE), label).clicked() {
                        self.choose_file();
                    }
                });
            });
        });
    }

    /// The displays drawn to scale in their real arrangement, as Windows'
    /// Display settings shows them. Click one to choose it.
    fn monitor_tiles(&mut self, ui: &mut Ui, p: &Palette) {
        let (x0, y0, x1, y1) = self.monitors.iter().fold((i32::MAX, i32::MAX, i32::MIN, i32::MIN), |a, m| {
            let b = &m.bounds;
            (a.0.min(b.x), a.1.min(b.y), a.2.max(b.x + b.width as i32), a.3.max(b.y + b.height as i32))
        });
        let (w, h) = ((x1 - x0).max(1) as f32, (y1 - y0).max(1) as f32);
        let scale = (30.0 / h).min(140.0 / w);
        let (area, _) = ui.allocate_exact_size(vec2(w * scale, h * scale), Sense::hover());
        let mut clicked = None;
        for (i, mon) in self.monitors.iter().enumerate() {
            let b = &mon.bounds;
            let r = egui::Rect::from_min_size(
                area.min + vec2((b.x - x0) as f32 * scale, (b.y - y0) as f32 * scale),
                vec2(b.width as f32 * scale, b.height as f32 * scale),
            )
            .shrink(1.5);
            let resp = ui.interact(r, ui.id().with(("tile", i)), Sense::click());
            let sel = i == self.selected;
            let fill = if sel { p.accent.gamma_multiply(0.35) } else if resp.hovered() { p.subtle_hover } else { p.subtle };
            let stroke = if sel { Stroke::new(2.0_f32, p.accent) } else { Stroke::new(1.0_f32, p.control_stroke) };
            ui.painter().rect(r, CornerRadius::same(3), fill, stroke, StrokeKind::Inside);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, (i + 1).to_string(), theme::strong(13.0), p.text);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            self.select_monitor(i);
        }
    }

    /// One line under the dock: the file, its cost and its fit, with the
    /// details one click away.
    fn summary(&mut self, ui: &mut Ui, p: &Palette, m: &Monitor) {
        let caption = |t: String, c: Color32| RichText::new(t).font(theme::font(theme::CAPTION)).color(c);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if self.chosen.is_some() {
                let (glyph, text) = if self.details { (icon::CHEVRON_UP, "Hide details") } else { (icon::CHEVRON_DOWN, "Details") };
                if widgets::text_button(ui, p, glyph, text).clicked() {
                    self.details = !self.details;
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let Some(c) = &self.chosen else {
                    let now = self.cfg.assignment_for(&m.id).cloned();
                    match now {
                        Some(a) => {
                            ui.label(caption(format!("On {} now:", m.friendly_name), p.text2));
                            ui.add(egui::Label::new(caption(format!("{} · {}", file_name(&a.path), fit_label(a.fit)), p.text)).truncate());
                            if widgets::icon_button(ui, p, icon::DELETE, 12.0).on_hover_text("Remove from this display").clicked() {
                                self.remove();
                            }
                        }
                        None => {
                            ui.label(caption(format!("{} shows the Windows wallpaper.", m.friendly_name), p.text2));
                        }
                    }
                    return;
                };
                let pr = &c.probe;
                let glyph = if pr.kind.needs_renderer() { icon::VIDEO } else { icon::PHOTO };
                ui.label(RichText::new(glyph.to_string()).font(theme::icon_font(14.0)).color(p.text2));
                ui.allocate_ui(vec2(220.0, SUMMARY_H), |ui| {
                    ui.add(egui::Label::new(RichText::new(file_name(&pr.path)).font(theme::strong(theme::BODY))).truncate());
                });
                let (cpu, ram, measured) = self.cost();
                let cost = if !pr.kind.needs_renderer() {
                    "No background cost".to_string()
                } else if measured {
                    format!("{cpu:.1}% CPU · {ram:.0} MB measured")
                } else {
                    format!("~{cpu:.1}% CPU · ~{ram:.0} MB")
                };
                ui.label(caption("·".into(), p.text3));
                ui.label(caption(cost, p.text));
                ui.label(caption("·".into(), p.text3));
                let issues = c.report.findings.iter().filter(|f| matches!(f.severity, Severity::Problem | Severity::Warning)).count();
                if issues == 0 {
                    ui.label(RichText::new(icon::CHECK.to_string()).font(theme::icon_font(12.0)).color(p.success));
                    ui.label(caption("Fits this display".into(), p.text));
                } else {
                    ui.label(RichText::new(icon::WARNING.to_string()).font(theme::icon_font(12.0)).color(p.caution));
                    let s = if issues == 1 { "" } else { "s" };
                    ui.label(caption(format!("{issues} thing{s} to review"), p.caution));
                }
                ui.label(caption("·".into(), p.text3));
                let mut spec = format!("{} × {} · {} {}-bit", pr.width, pr.height, pr.codec.to_uppercase(), pr.bit_depth());
                if let Some(f) = pr.fps {
                    spec.push_str(&format!(" · {f:.0} fps"));
                }
                ui.add(egui::Label::new(caption(spec, p.text2)).truncate());
            });
        });
    }

    /// (CPU %, MB, measured?) for the chosen file: measured during a
    /// preview, estimated before one.
    fn cost(&self) -> (f64, f64, bool) {
        let Some(c) = &self.chosen else { return (0.0, 0.0, false) };
        if !c.probe.kind.needs_renderer() {
            return (0.0, 0.0, false);
        }
        match self.preview.as_ref().and_then(|pv| pv.measured) {
            Some(mm) => (mm.cpu_percent, mm.private_mb, true),
            None => (f64::from(c.report.estimate.cpu_percent_of_one_core), f64::from(c.report.estimate.ram_mb), false),
        }
    }

    fn details_panel(&mut self, ui: &mut Ui, p: &Palette, m: &Monitor) {
        let Some(c) = &self.chosen else { return };
        let mut remove = false;
        let inner_h = DETAILS_H - 28.0;
        widgets::glass(ui, p, Margin::symmetric(18, 14), 12, |ui| {
            ui.set_height(inner_h);
            ui.columns(2, |cols| {
                // Left: cost and what happens when hidden.
                let ui = &mut cols[0];
                let is_video = c.probe.kind.needs_renderer();
                let measured = self.preview.as_ref().and_then(|pv| pv.measured);
                let measuring = self.preview.is_some() && measured.is_none();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Cost while visible").font(theme::strong(theme::BODY)));
                    if !is_video {
                        widgets::badge(ui, "None", p.success, p.success_bg);
                    } else if measured.is_some() {
                        widgets::badge(ui, "Measured on this PC", p.success, p.success_bg);
                    } else if measuring {
                        widgets::badge(ui, "Measuring…", p.text2, p.subtle);
                    } else {
                        widgets::badge(ui, "Estimate", p.text2, p.subtle);
                    }
                    if is_video {
                        match &c.probe.hwdec {
                            Some(_) => widgets::badge(ui, "GPU decode", p.text2, p.subtle),
                            None => widgets::badge(ui, "CPU decode", p.caution, p.caution_bg),
                        }
                    }
                });
                ui.add_space(6.0);
                let (cpu, ram, _) = self.cost();
                let approx = if is_video && measured.is_none() { "~" } else { "" };
                ui.horizontal(|ui| {
                    stat(ui, p, &format!("{approx}{cpu:.1}%"), "of one CPU core");
                    ui.add_space(28.0);
                    stat(ui, p, &format!("{approx}{ram:.0} MB"), "memory");
                });
                ui.add_space(6.0);
                let policy = self.cfg.policy;
                let note = if !is_video {
                    "A still image is handed to Windows. Nothing runs in the background.".to_string()
                } else if policy.pause_when_occluded {
                    format!(
                        "When windows cover the desktop: paused, 0% CPU. After {} s covered: closed, 0 MB.",
                        policy.occluded_grace_ms / 1000
                    )
                } else {
                    "Keeps playing when windows cover the desktop. Change this under Performance.".to_string()
                };
                ui.label(RichText::new(note).font(theme::font(theme::CAPTION)).color(p.text2));
                let est = c.report.estimate;
                if let Some(mm) = measured {
                    ui.label(
                        RichText::new(format!(
                            "Measured over {:.0} s of preview. The estimate was ~{:.1}% and ~{:.0} MB.",
                            mm.over_secs, est.cpu_percent_of_one_core, est.ram_mb
                        ))
                        .font(theme::font(theme::CAPTION))
                        .color(p.text3),
                    );
                } else if is_video && !measuring {
                    ui.label(RichText::new("Play a preview to measure the real cost.").font(theme::font(theme::CAPTION)).color(p.text3));
                }
                if let Some(a) = self.cfg.assignment_for(&m.id) {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Now on this display").font(theme::font(theme::CAPTION)).color(p.text2));
                    // Right to left so a long name truncates instead of
                    // widening the panel.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        remove = widgets::icon_button(ui, p, icon::DELETE, 12.0).on_hover_text("Remove from this display").clicked();
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let now = format!("{} · {}", file_name(&a.path), fit_label(a.fit));
                            ui.add(egui::Label::new(RichText::new(now).font(theme::font(theme::CAPTION))).truncate());
                        });
                    });
                }

                // Right: the fit check.
                let ui = &mut cols[1];
                ui.label(RichText::new("Fit for this display").font(theme::strong(theme::BODY)));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("fit").max_height(inner_h - 28.0).auto_shrink([false, true]).show(ui, |ui| {
                    if c.report.findings.is_empty() {
                        ui.horizontal(|ui| {
                            widgets::status_icon(ui, icon::CHECK, p.success);
                            ui.label(format!("Looks right on {}.", m.friendly_name));
                        });
                    }
                    for (i, f) in c.report.findings.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(10.0);
                        }
                        let (glyph, color) = match f.severity {
                            Severity::Problem => (icon::ERROR, p.critical),
                            Severity::Warning => (icon::WARNING, p.caution),
                            Severity::Advice => (icon::INFO, p.accent),
                            Severity::Info => (icon::INFO, p.text3),
                        };
                        ui.horizontal_top(|ui| {
                            widgets::status_icon(ui, glyph, color);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.label(RichText::new(&f.title).font(theme::strong(theme::BODY)));
                                ui.label(RichText::new(&f.detail).font(theme::font(theme::CAPTION)).color(p.text2));
                                if let Some(hint) = &f.hint {
                                    hint_box(ui, p, hint);
                                }
                            });
                        });
                    }
                });
            });
        });
        if remove {
            self.remove();
        }
    }
}

/// A small dark label over the picture.
fn chip(painter: &egui::Painter, pos: egui::Pos2, align: Align2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), theme::font(theme::CAPTION), Color32::from_gray(240));
    let rect = align.anchor_size(pos, galley.size() + vec2(20.0, 10.0));
    painter.rect_filled(rect, CornerRadius::same(12), Color32::from_black_alpha(150));
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::from_gray(240));
}

/// Centred text on a dark pill, legible over any picture.
fn pill_text(painter: &egui::Painter, center: egui::Pos2, text: &str, font: egui::FontId, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), font, color);
    let rect = egui::Rect::from_center_size(center, galley.size() + vec2(24.0, 12.0));
    painter.rect_filled(rect, CornerRadius::same(14), Color32::from_black_alpha(150));
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
}

fn stat(ui: &mut Ui, p: &Palette, value: &str, label: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(RichText::new(value).font(theme::strong(theme::TITLE)));
        ui.label(RichText::new(label).font(theme::font(theme::CAPTION)).color(p.text2));
    });
}

/// A fix suggestion. Commands get a monospace box with a copy button.
fn hint_box(ui: &mut Ui, p: &Palette, hint: &str) {
    let Some(i) = hint.find("ffmpeg") else {
        ui.label(RichText::new(hint).font(theme::font(theme::CAPTION)).color(p.text2));
        return;
    };
    let (lead, cmd) = hint.split_at(i);
    if !lead.trim().is_empty() {
        ui.label(RichText::new(lead.trim()).font(theme::font(theme::CAPTION)).color(p.text2));
    }
    Frame::new()
        .fill(Color32::from_black_alpha(90))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if widgets::icon_button(ui, p, icon::COPY, 12.0).on_hover_text("Copy command").clicked() {
                        ui.ctx().copy_text(cmd.to_string());
                    }
                    ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                        ui.add(egui::Label::new(RichText::new(cmd).monospace().size(11.0).color(p.text)).wrap());
                    });
                });
            });
        });
}

// --- Display page ---------------------------------------------------------

impl App {
    fn display_page(&mut self, ui: &mut Ui, p: &Palette) {
        if self.monitors.len() > 1 {
            let mut sel = self.selected;
            let items: Vec<(usize, String)> =
                self.monitors.iter().enumerate().map(|(i, m)| (i, format!("{}  {}", i + 1, m.friendly_name))).collect();
            let refs: Vec<(usize, &str)> = items.iter().map(|(i, s)| (*i, s.as_str())).collect();
            widgets::segmented(ui, p, &mut sel, &refs);
            self.select_monitor(sel);
            ui.add_space(8.0);
        }
        let Some(m) = self.monitor().cloned() else {
            ui.label("No displays found.");
            return;
        };

        widgets::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::DISPLAY.to_string()).font(theme::icon_font(32.0)).color(p.accent));
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&m.friendly_name).font(theme::strong(theme::SUBTITLE)));
                        if m.is_primary {
                            widgets::badge(ui, "Primary", p.text2, p.subtle);
                        }
                        widgets::badge(ui, m.connector.label(), p.text2, p.subtle);
                    });
                    ui.label(RichText::new(m.class_label()).color(p.text2));
                });
            });
        });

        widgets::group_header(ui, "Specifications");
        widgets::group(ui, p, |ui| {
            let rows = m.describe();
            let rows = rows.iter().filter(|(k, _)| !matches!(*k, "What it is" | "Colour gamut" | "Connection"));
            for (i, (k, v)) in rows.enumerate() {
                widgets::fact(ui, p, k, v, i > 0);
            }
        });

        widgets::group_header(ui, "Colour");
        widgets::card(ui, p, |ui| match m.color.gamut_coverage() {
            Some(g) => {
                ui.label(RichText::new(g.headline()).font(theme::strong(theme::BODY)));
                ui.add_space(6.0);
                widgets::meter(ui, p, "sRGB", g.srgb);
                widgets::meter(ui, p, "DCI-P3", g.display_p3);
                widgets::meter(ui, p, "Rec.2020", g.rec2020);
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Share of each colour space this panel can show, from the primaries it reports.")
                        .font(theme::font(theme::CAPTION))
                        .color(p.text3),
                );
            }
            None => {
                ui.label(RichText::new("This display does not report its colour primaries.").color(p.text2));
            }
        });

        widgets::group_header(ui, "Best wallpaper files for this display");
        widgets::group(ui, p, |ui| {
            let n = m.native_mode.unwrap_or(m.current_mode);
            widgets::row(
                ui,
                p,
                icon::DISPLAY,
                &format!("{} × {}", n.width, n.height),
                "Larger files look no sharper but use far more memory: 4K costs about 400 MB more than 1080p.",
                false,
                |_| {},
            );
            let (title, desc) = if m.color.hdr_supported {
                ("HDR or SDR", "This display supports HDR, so HDR videos keep their range.")
            } else {
                ("SDR", "This display is SDR. HDR videos are tone-mapped and usually look flat.")
            };
            widgets::row(ui, p, icon::PHOTO, title, desc, true, |_| {});
            let hz = m.current_mode.refresh_hz;
            let smooth: Vec<String> = [24.0, 25.0, 30.0, 48.0, 50.0, 60.0]
                .iter()
                .filter(|f| {
                    let r = hz / *f;
                    (r - r.round()).abs() <= 0.02
                })
                .map(|f| format!("{f:.0}"))
                .collect();
            if !smooth.is_empty() {
                widgets::row(
                    ui,
                    p,
                    icon::VIDEO,
                    &format!("{} fps", smooth.join(", ")),
                    &format!("These frame rates divide evenly into {hz:.2} Hz, so motion stays even."),
                    true,
                    |_| {},
                );
            }
        });
    }
}

// --- Performance page -----------------------------------------------------

impl App {
    fn performance_page(&mut self, ui: &mut Ui, p: &Palette) {
        widgets::group_header(ui, "Status");
        let running = service::daemon_running();
        let engine = match &self.mpv {
            Ok(l) => format!("libmpv {}", l.api_version()),
            Err(_) => "libmpv not found".into(),
        };
        let gpu = self.adapter.clone().unwrap_or_else(|| "System default".into());
        let video = if self.caps.video { "Supported".to_string() } else { "Not available".to_string() };
        let reason = self.caps.video_unavailable_reason.clone().unwrap_or_default();
        widgets::group(ui, p, |ui| {
            widgets::row(ui, p, icon::POWER, "Background service", "Plays wallpapers and pauses them when hidden.", false, |ui| {
                let (color, text) = if running { (p.success, "Running") } else { (p.text3, "Not running") };
                ui.label(text);
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
            });
            widgets::row(ui, p, icon::VIDEO, "Video wallpapers", &reason, true, |ui| {
                ui.label(&video);
            });
            widgets::row(ui, p, icon::PLAY, "Video engine", "", true, |ui| {
                ui.label(&engine);
            });
            widgets::row(ui, p, icon::CHIP, "Render GPU", "", true, |ui| {
                ui.label(&gpu);
            });
        });

        let mut policy = self.cfg.policy;
        let mut render = self.cfg.render;
        let mut autostart = self.cfg.start_with_windows;
        let mut commit_policy = false;
        let mut commit_render = false;
        let mut commit_autostart = false;

        widgets::group_header(ui, "When the desktop is hidden");
        widgets::group(ui, p, |ui| {
            widgets::row(ui, p, icon::PAUSE, "Pause when covered", "Stops decoding while windows hide the desktop.", false, |ui| {
                commit_policy |= widgets::toggle(ui, p, &mut policy.pause_when_occluded).changed();
            });
            widgets::row(
                ui,
                p,
                icon::MEMORY,
                "Free memory after",
                "Closes the video player once the desktop has been covered this long. It resumes from the same frame.",
                true,
                |ui| {
                    let mut secs = (policy.occluded_grace_ms / 1000) as u32;
                    let r = slider_cell(ui, policy.pause_when_occluded, egui::Slider::new(&mut secs, 5..=600).suffix(" s").logarithmic(true));
                    policy.occluded_grace_ms = u64::from(secs) * 1000;
                    commit_policy |= settled(&r);
                },
            );
            widgets::row(ui, p, icon::BATTERY, "Never play on battery", "Shows a still frame instead whenever the charger is unplugged.", true, |ui| {
                commit_policy |= widgets::toggle(ui, p, &mut policy.suspend_on_battery_always).changed();
            });
            widgets::row(ui, p, icon::BATTERY_LOW, "Stop playing below", "Battery level at which video stops when unplugged.", true, |ui| {
                let r = slider_cell(ui, !policy.suspend_on_battery_always, egui::Slider::new(&mut policy.battery_threshold_pct, 0..=100).suffix("%"));
                commit_policy |= settled(&r);
            });
        });

        widgets::group_header(ui, "Playback");
        widgets::group(ui, p, |ui| {
            widgets::row(ui, p, icon::VIDEO, "GPU video decoding", "Much less CPU. Turn off only if videos show artefacts.", false, |ui| {
                commit_render |= widgets::toggle(ui, p, &mut render.hardware_decode).changed();
            });
            widgets::row(ui, p, icon::SPEED, "Low-power scaling", "Simpler scaling filter. Slightly softer, noticeably cheaper.", true, |ui| {
                commit_render |= widgets::toggle(ui, p, &mut render.low_power).changed();
            });
            widgets::row(ui, p, icon::CHIP, "Use the power-saving GPU", "On laptops, keeps the dedicated GPU asleep.", true, |ui| {
                commit_render |= widgets::toggle(ui, p, &mut render.prefer_power_saving_gpu).changed();
            });
            widgets::row(ui, p, icon::LEAF, "Efficiency mode", "Lets Windows run the player on efficient cores at low priority.", true, |ui| {
                commit_render |= widgets::toggle(ui, p, &mut render.efficiency_mode).changed();
            });
        });

        widgets::group_header(ui, "Startup");
        widgets::group(ui, p, |ui| {
            widgets::row(ui, p, icon::POWER, "Start with Windows", "Restores your wallpapers when you sign in.", false, |ui| {
                commit_autostart |= widgets::toggle(ui, p, &mut autostart).changed();
            });
        });

        // Show edits immediately; write only the group that changed.
        self.cfg.policy = policy;
        self.cfg.render = render;
        self.cfg.start_with_windows = autostart;
        if commit_policy {
            self.commit("Saving", false, |c| c.policy = policy);
        }
        if commit_render {
            self.adapter = render.prefer_power_saving_gpu.then(wallset_display::power_saving_adapter).flatten();
            self.commit("Saving", false, |c| c.render = render);
        }
        if commit_autostart {
            self.commit("Saving", false, |c| c.start_with_windows = autostart);
        }
    }
}

/// A slider inside a right-to-left settings row, laid out left to right so
/// its value box stays after the rail.
fn slider_cell(ui: &mut Ui, enabled: bool, slider: egui::Slider) -> egui::Response {
    let w = ui.spacing().slider_width + 72.0;
    ui.allocate_ui_with_layout(vec2(w, 24.0), Layout::left_to_right(Align::Center), |ui| ui.add_enabled(enabled, slider)).inner
}

/// A slider edit is final when the drag ends, or on a click or key press.
fn settled(r: &egui::Response) -> bool {
    r.drag_stopped() || (r.changed() && !r.dragged())
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned())
}

fn fit_label(fit: FitMode) -> &'static str {
    match fit {
        FitMode::Fill => "Fill",
        FitMode::Fit => "Fit",
        FitMode::Stretch => "Stretch",
    }
}

#[cfg(windows)]
fn native_hwnd(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
fn native_hwnd(_frame: &eframe::Frame) -> Option<isize> {
    None
}

/// The OpenGL surface must not paint over the native preview window.
#[cfg(windows)]
fn clip_children(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_CLIPCHILDREN};
    let h = HWND(hwnd as *mut _);
    // SAFETY: adjusting our own top-level window's style.
    unsafe {
        let style = GetWindowLongPtrW(h, GWL_STYLE);
        SetWindowLongPtrW(h, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
    }
}

#[cfg(not(windows))]
fn clip_children(_hwnd: isize) {}
