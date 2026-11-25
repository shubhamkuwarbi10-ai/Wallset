//! `wallset-ui`: the settings window. Spawned on demand, exits when closed,
//! so its cost (a GUI toolkit and an OpenGL context) is never resident.
//!
//! egui is immediate-mode but eframe repaints *reactively*: only on input, or
//! when we ask (once a second while a preview is measuring). An idle settings
//! window uses no CPU.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod preview;
mod service;

use eframe::egui::{self, Color32, RichText};
use preview::Preview;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use wallset_core::fitness::{self, FitnessReport, Severity};
use wallset_core::media::{MediaKind, MediaProbe};
use wallset_core::{Assignment, Config, FitMode, Monitor};

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "gif", "png", "jpg", "jpeg", "bmp", "webp",
    "tif", "tiff", "jxl", "avif", "heic",
];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("wallSet")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("wallSet", options, Box::new(|_cc| Ok(Box::new(App::new().with_args()))))
}

struct Chosen {
    probe: MediaProbe,
    report: FitnessReport,
}

struct App {
    monitors: Vec<Monitor>,
    monitor_error: Option<String>,
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
    preview_error: Option<String>,
    status: Option<(bool, String)>,
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
        let cfg = service::load_config();
        let adapter = cfg.render.prefer_power_saving_gpu.then(wallset_display::power_saving_adapter).flatten();
        Self {
            monitors,
            monitor_error,
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
            preview_error: None,
            status: None,
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
                self.status = Some((false, format!("Cannot inspect media: {e}")));
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
                self.status = Some((false, format!("Cannot open {}: {e}", path.display())));
                self.probing = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.probing = None,
        }
    }

    fn reanalyse(&mut self) {
        let Some(m) = self.monitors.get(self.selected) else { return };
        if let Some(c) = &mut self.chosen {
            c.report = fitness::analyse(&c.probe, m);
        }
    }

    /// Read the config from disk, apply one change, write it back, notify
    /// the service. Never writes this window's in-memory snapshot wholesale:
    /// that would silently undo anything changed on disk since the window
    /// opened (another settings window, a hand edit, the CLI).
    fn commit(&mut self, what: &str, change: impl FnOnce(&mut Config)) {
        let result = service::update_config(change).map_err(|e| e.to_string()).and_then(|fresh| {
            self.cfg = fresh;
            service::notify_or_start_daemon()
        });
        self.status = Some(match result {
            Ok(msg) => (true, format!("{what} {msg}")),
            Err(e) => (false, format!("{what} failed: {e}")),
        });
    }

    fn apply(&mut self) {
        let (Some(m), Some(c)) = (self.monitor(), &self.chosen) else { return };
        let a = Assignment {
            monitor: m.id.clone(),
            path: c.probe.path.clone(),
            kind: c.probe.kind,
            fit: self.fit,
        };
        let name = m.friendly_name.clone();
        self.preview = None;
        self.commit(&format!("Applied to {name}."), |c| c.assign(a));
    }

    fn remove(&mut self) {
        let Some(m) = self.monitor() else { return };
        let (id, name) = (m.id.clone(), m.friendly_name.clone());
        self.commit(&format!("Removed from {name}."), |c| c.unassign(&id));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.hwnd.is_none() {
            self.hwnd = native_hwnd(frame);
        }
        self.poll_probe();
        if self.probing.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // Two columns. Windows clamps the window to the work area (about
        // 1000 points wide on a 125%-scaled 1080p laptop), which is too
        // narrow for three.
        egui::SidePanel::left("displays").resizable(true).default_width(340.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                self.displays_panel(ui);
                ui.separator();
                self.details_panel(ui);
                ui.separator();
                self.pc_panel(ui);
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| self.wallpaper_panel(ui, ctx));
    }
}

impl App {
    fn displays_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("Displays");
        ui.add_space(4.0);
        if let Some(e) = &self.monitor_error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        let mut clicked = None;
        for (i, m) in self.monitors.iter().enumerate() {
            let assigned = self.cfg.assignment_for(&m.id).map(|a| {
                a.path.file_name().map_or_else(String::new, |f| f.to_string_lossy().into_owned())
            });
            let label = format!(
                "{}{}\n{}\n{}",
                m.friendly_name,
                if m.is_primary { "  (primary)" } else { "" },
                m.current_mode,
                assigned.map_or_else(|| "no wallSet wallpaper".to_string(), |f| format!("showing {f}")),
            );
            if ui.selectable_label(i == self.selected, label).clicked() {
                clicked = Some(i);
            }
            ui.add_space(4.0);
        }
        if let Some(i) = clicked {
            self.selected = i;
            self.preview = None;
            self.reanalyse();
        }

    }

    fn pc_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("This PC");
        let c = &self.caps;
        ui.label(format!("Still images: {}", if c.static_images { "yes" } else { "no" }));
        ui.label(format!("Video wallpaper: {}", if c.video { "yes" } else { "no" }));
        if let Some(r) = &c.video_unavailable_reason {
            ui.small(r);
        }
        ui.label(format!(
            "Video engine: {}",
            match &self.mpv {
                Ok(l) => format!("libmpv {}", l.api_version()),
                Err(_) => "libmpv not found".into(),
            }
        ));
        ui.label(format!("Render GPU: {}", self.adapter.as_deref().unwrap_or("system default")));
        let running = service::daemon_running();
        ui.label(format!("Background service: {}", if running { "running" } else { "not running" }));

        ui.separator();
        self.settings(ui);
    }

    fn details_panel(&mut self, ui: &mut egui::Ui) {
        let Some(m) = self.monitor() else {
            ui.label("No displays found.");
            return;
        };
        ui.heading("Selected display");
        ui.label(RichText::new(m.class_label()).italics());
        ui.add_space(8.0);
        for (k, v) in m.describe() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{k}:")).strong());
                ui.label(v);
            });
        }
        ui.add_space(10.0);
        ui.label(RichText::new("What this means for wallpapers").strong());
        let n = m.native_mode.unwrap_or(m.current_mode);
        ui.label(format!(
            "Ideal files for this screen are {}x{}. Larger files look no sharper but cost much more memory: \
             a 4K video uses roughly 400 MB more than a 1080p one.",
            n.width, n.height
        ));
        if !m.color.hdr_supported {
            ui.label("This display is SDR. HDR videos will be tonemapped and usually look flat; prefer SDR files.");
        }
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
            ui.label(format!("Frame rates that play perfectly evenly at {hz:.2} Hz: {} fps.", smooth.join(", ")));
        }
    }

    fn wallpaper_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(6.0);
        let Some(m) = self.monitor().cloned() else { return };
        ui.heading(format!("Wallpaper for {}", m.friendly_name));
        if let Some(a) = self.cfg.assignment_for(&m.id) {
            ui.label(format!("Current: {} ({:?}, {})", a.path.display(), a.kind, a.fit.as_arg()));
        }
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if ui.button("Choose image or video...").clicked() {
                self.choose_file();
            }
            if self.probing.is_some() {
                ui.spinner();
                ui.label("Inspecting...");
            }
        });

        // Clone out of self so the button handlers below can borrow self mutably.
        let Some((probe, report)) = self.chosen.as_ref().map(|c| (c.probe.clone(), c.report.clone())) else {
            self.preview = None;
            if let Some((ok, s)) = &self.status {
                ui.colored_label(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, s);
            }
            return;
        };
        let p = &probe;
        let kind = match p.kind {
            MediaKind::Image => "still image",
            MediaKind::Video => "video",
            MediaKind::AnimatedImage => "animated image",
        };
        ui.label(RichText::new(p.path.file_name().unwrap_or_default().to_string_lossy()).strong());
        ui.label(format!(
            "{kind}, {}x{}, {} {}-bit{}{}",
            p.width,
            p.height,
            p.codec,
            p.bit_depth(),
            p.fps.map_or_else(String::new, |f| format!(", {f:.2} fps")),
            if p.kind.needs_renderer() {
                p.hwdec.as_ref().map_or_else(|| ", CPU decode".into(), |h| format!(", GPU decode ({h})"))
            } else {
                String::new()
            }
        ));

        ui.horizontal(|ui| {
            ui.label("Fit:");
            // Radio buttons, not a drop-down: a drop-down list would open
            // underneath the native preview window and be hidden by it.
            ui.radio_value(&mut self.fit, FitMode::Fill, "Fill (crop)");
            ui.radio_value(&mut self.fit, FitMode::Fit, "Fit (bars)");
            ui.radio_value(&mut self.fit, FitMode::Stretch, "Stretch");
        });

        // Preview area at the monitor's aspect ratio, so cropping is truthful.
        let w = ui.available_width().min(680.0);
        let h = (w / m.aspect_ratio().max(0.5) as f32).min(380.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(18));
        let ppp = ctx.pixels_per_point();
        let px = (
            (rect.min.x * ppp).round() as i32,
            (rect.min.y * ppp).round() as i32,
            (rect.width() * ppp).round() as u32,
            (rect.height() * ppp).round() as u32,
        );

        if self.pending_preview && self.preview.is_none() && self.hwnd.is_some() {
            self.pending_preview = false;
            self.start_preview(px);
        }
        if let Some(pv) = &mut self.preview {
            pv.place(px);
            pv.sample();
            if !pv.running() {
                self.preview_error = Some("The preview stopped unexpectedly.".into());
                self.preview = None;
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Preview",
                egui::FontId::proportional(16.0),
                Color32::GRAY,
            );
        }

        ui.horizontal(|ui| {
            let fit_changed = self.preview.is_some() && self.preview_fit() != Some(self.fit);
            if self.preview.is_none() || fit_changed {
                if ui.button(if fit_changed { "Restart preview" } else { "Preview" }).clicked() {
                    self.start_preview(px);
                }
            } else if ui.button("Stop preview").clicked() {
                self.preview = None;
            }
            if ui.button(RichText::new("Apply to this display").strong()).clicked() {
                self.apply();
            }
            if self.cfg.assignment_for(&m.id).is_some() && ui.button("Remove").clicked() {
                self.remove();
            }
        });

        // Cost: the model before preview, the measurement during it.
        let est = report.estimate;
        if p.kind.needs_renderer() {
            match self.preview.as_ref().and_then(|pv| pv.measured) {
                Some(mm) => {
                    ui.label(
                        RichText::new(format!(
                            "Measured on this PC: {:.1}% of one CPU core, {:.0} MB private memory ({:.0} MB working set), over {:.0} s.",
                            mm.cpu_percent, mm.private_mb, mm.working_set_mb, mm.over_secs
                        ))
                        .strong(),
                    );
                    ui.small(format!(
                        "Model estimate was ~{:.0} MB and ~{:.1}%. While hidden, wallSet pauses (0% CPU) and then frees this memory entirely.",
                        est.ram_mb, est.cpu_percent_of_one_core
                    ));
                }
                None if self.preview.is_some() => {
                    ui.label("Measuring (starts 3 s after the first frame)...");
                }
                None => {
                    ui.label(format!(
                        "Estimated while visible: ~{:.0} MB private memory, ~{:.1}% of one CPU core. Preview to measure it.",
                        est.ram_mb, est.cpu_percent_of_one_core
                    ));
                }
            }
        } else {
            ui.label("Cost once applied: none. A still image is handed to Windows.");
        }
        if let Some(e) = &self.preview_error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        if let Some((ok, s)) = &self.status {
            ui.colored_label(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, s);
        }

        ui.separator();
        ui.label(RichText::new("Will it look right on this display?").strong());
        let findings = report.findings;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            if findings.is_empty() {
                ui.colored_label(Color32::LIGHT_GREEN, "Yes. No issues for this display.");
            }
            for f in &findings {
                let color = match f.severity {
                    Severity::Problem => Color32::from_rgb(240, 90, 90),
                    Severity::Warning => Color32::from_rgb(235, 170, 60),
                    Severity::Advice => Color32::from_rgb(120, 170, 240),
                    Severity::Info => Color32::GRAY,
                };
                ui.label(RichText::new(format!("{}: {}", f.severity.label(), f.title)).color(color).strong());
                ui.label(&f.detail);
                if let Some(hint) = &f.hint {
                    ui.horizontal_wrapped(|ui| {
                        ui.small(hint);
                        if hint.contains("ffmpeg") && ui.small_button("Copy").clicked() {
                            let cmd = hint.find("ffmpeg").map_or(hint.as_str(), |i| &hint[i..]);
                            ui.ctx().copy_text(cmd.to_string());
                        }
                    });
                }
                ui.add_space(6.0);
            }
        });
    }

    fn preview_fit(&self) -> Option<FitMode> {
        self.preview.as_ref().map(|_| self.preview_fit_mode)
    }

    fn start_preview(&mut self, px: (i32, i32, u32, u32)) {
        self.preview = None;
        self.preview_error = None;
        let (Some(hwnd), Some(c)) = (self.hwnd, &self.chosen) else {
            self.preview_error = Some("Preview needs a native window handle.".into());
            return;
        };
        clip_children(hwnd);
        match Preview::start(hwnd, px, &c.probe.path, self.fit, &self.cfg.render, self.adapter.as_deref()) {
            Ok(p) => {
                self.preview = Some(p);
                self.preview_fit_mode = self.fit;
            }
            Err(e) => self.preview_error = Some(e),
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Resource settings", |ui| {
            let r = &mut self.cfg.render;
            ui.checkbox(&mut r.hardware_decode, "Use GPU video decoding");
            ui.checkbox(&mut r.low_power, "Low-power scaling");
            ui.checkbox(&mut r.prefer_power_saving_gpu, "Render on the power-saving GPU (laptops)");
            ui.checkbox(&mut r.efficiency_mode, "Efficiency mode (EcoQoS)");
            ui.separator();
            let p = &mut self.cfg.policy;
            ui.checkbox(&mut p.pause_when_occluded, "Pause when covered by windows");
            let mut grace = (p.occluded_grace_ms / 1000) as u32;
            ui.add(egui::Slider::new(&mut grace, 5..=600).text("s covered, then free memory"));
            p.occluded_grace_ms = u64::from(grace) * 1000;
            ui.checkbox(&mut p.suspend_on_battery_always, "Never play on battery");
            ui.add_enabled(
                !p.suspend_on_battery_always,
                egui::Slider::new(&mut p.battery_threshold_pct, 0..=100).text("% battery: stop below"),
            );
            ui.separator();
            ui.checkbox(&mut self.cfg.start_with_windows, "Start with Windows");
            if ui.button("Save settings").clicked() {
                // Only the settings edited here; assignments come from disk.
                let (render, policy, autostart) =
                    (self.cfg.render, self.cfg.policy, self.cfg.start_with_windows);
                self.commit("Settings saved.", |c| {
                    c.render = render;
                    c.policy = policy;
                    c.start_with_windows = autostart;
                });
            }
        });
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
