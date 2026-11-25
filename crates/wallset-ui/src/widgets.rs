//! The few WinUI controls egui does not have: card, settings row, toggle
//! switch, segmented control, accent button, info bar. Each is drawn from
//! the palette; none opens a popup (the preview is a native window that a
//! popup would hide behind).

use crate::theme::{self, Palette, icon};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, vec2,
};

pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.card_stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A group of settings rows in one card, separated by hairlines, the way
/// Windows Settings stacks them.
pub fn group(ui: &mut Ui, p: &Palette, rows: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.card_stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            rows(ui);
        });
}

pub fn group_header(ui: &mut Ui, text: &str) {
    ui.add_space(18.0);
    ui.label(RichText::new(text).font(theme::strong(theme::BODY)));
    ui.add_space(6.0);
}

/// One settings row: icon, title and description on the left, the control
/// on the right. `divider` draws the hairline above it.
pub fn row(ui: &mut Ui, p: &Palette, glyph: char, title: &str, desc: &str, divider: bool, control: impl FnOnce(&mut Ui)) {
    if divider {
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0_f32, p.card_stroke));
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.set_min_height(40.0);
        ui.add_space(2.0);
        ui.label(RichText::new(glyph.to_string()).font(theme::icon_font(16.0)).color(p.text));
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().interact_size.y = 20.0;
            control(ui);
            ui.add_space(16.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.add_space(if desc.is_empty() { 11.0 } else { 2.0 });
                ui.label(title);
                if !desc.is_empty() {
                    ui.label(RichText::new(desc).font(theme::font(theme::CAPTION)).color(p.text2));
                }
            });
        });
    });
    ui.add_space(10.0);
}

/// A key/value line for read-only facts.
pub fn fact(ui: &mut Ui, p: &Palette, key: &str, value: &str, divider: bool) {
    if divider {
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0_f32, p.card_stroke));
    }
    ui.add_space(9.0);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(170.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(170.0);
            ui.add(egui::Label::new(RichText::new(key).color(p.text2)).truncate());
        });
        ui.add(egui::Label::new(value).wrap());
    });
    ui.add_space(9.0);
}

/// WinUI ToggleSwitch, 40x20, with its "On"/"Off" label on the left.
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(40.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let hovered = resp.hovered();
    let painter = ui.painter();
    if *on {
        let fill = if hovered { p.accent.gamma_multiply(0.9) } else { p.accent };
        painter.rect_filled(rect, CornerRadius::same(10), fill);
    } else {
        let fill = if hovered { p.subtle_hover } else { p.subtle };
        painter.rect(rect, CornerRadius::same(10), fill, Stroke::new(1.0_f32, p.text2), StrokeKind::Inside);
    }
    let knob_r = if hovered { 7.0 } else { 6.0 };
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
    painter.circle_filled(egui::pos2(x, rect.center().y), knob_r, if *on { p.on_accent } else { p.text2 });
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(3.0), CornerRadius::same(13), Stroke::new(2.0_f32, p.text), StrokeKind::Outside);
    }
    ui.add_space(4.0);
    ui.label(RichText::new(if *on { "On" } else { "Off" }).color(p.text));
    resp
}

/// WinUI SelectorBar / segmented control: the selected item is raised and
/// carries a short accent pill underneath.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, p: &Palette, value: &mut T, items: &[(T, &str)]) -> bool {
    let mut changed = false;
    Frame::new()
        .fill(p.subtle)
        .stroke(Stroke::new(1.0_f32, p.card_stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (v, label) in items {
                    let selected = *value == *v;
                    let galley = ui.painter().layout_no_wrap(
                        (*label).to_string(),
                        if selected { theme::strong(theme::BODY) } else { theme::font(theme::BODY) },
                        Color32::PLACEHOLDER,
                    );
                    let size = vec2(galley.size().x + 28.0, 28.0);
                    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                    if resp.clicked() && !selected {
                        *value = *v;
                        changed = true;
                    }
                    let fill = if selected {
                        p.control
                    } else if resp.hovered() {
                        p.subtle_hover
                    } else {
                        Color32::TRANSPARENT
                    };
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(4), fill);
                    if selected {
                        painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0_f32, p.control_stroke), StrokeKind::Inside);
                        let pill = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.bottom() - 3.0), vec2(16.0, 3.0));
                        painter.rect_filled(pill, CornerRadius::same(2), p.accent);
                    }
                    let color = if selected { p.text } else { p.text2 };
                    painter.galley(rect.center() - galley.size() / 2.0 - vec2(0.0, 1.0), galley, color);
                }
            });
        });
    changed
}

/// Accent-filled primary button.
pub fn accent_button(ui: &mut Ui, p: &Palette, glyph: Option<char>, text: &str, enabled: bool) -> Response {
    button_impl(ui, p, glyph, text, enabled, true)
}

/// Standard (secondary) button with an optional leading icon.
pub fn button(ui: &mut Ui, p: &Palette, glyph: Option<char>, text: &str) -> Response {
    button_impl(ui, p, glyph, text, true, false)
}

fn button_impl(ui: &mut Ui, p: &Palette, glyph: Option<char>, text: &str, enabled: bool, accent: bool) -> Response {
    let text_font = if accent { theme::strong(theme::BODY) } else { theme::font(theme::BODY) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), text_font, Color32::PLACEHOLDER);
    let icon_w = if glyph.is_some() { 24.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 24.0, 32.0).max(vec2(if accent { 96.0 } else { 0.0 }, 32.0));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter();
    let (fill, fg, stroke) = match (accent, enabled) {
        (true, true) => (
            if resp.is_pointer_button_down_on() {
                p.accent.gamma_multiply(0.8)
            } else if resp.hovered() {
                p.accent.gamma_multiply(0.9)
            } else {
                p.accent
            },
            p.on_accent,
            Stroke::NONE,
        ),
        (_, false) => (p.subtle, p.text3, Stroke::NONE),
        (false, true) => (
            if resp.is_pointer_button_down_on() {
                p.subtle
            } else if resp.hovered() {
                p.control_hover
            } else {
                p.control
            },
            p.text,
            Stroke::new(1.0_f32, p.control_stroke),
        ),
    };
    painter.rect(rect, CornerRadius::same(4), fill, stroke, StrokeKind::Inside);
    // Content is centred: the accent button has a minimum width.
    let mut x = rect.center().x - (galley.size().x + icon_w) / 2.0;
    if let Some(g) = glyph {
        painter.text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, g, theme::icon_font(14.0), fg);
        x += icon_w;
    }
    painter.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0 - 1.0), galley, fg);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), CornerRadius::same(6), Stroke::new(2.0_f32, p.text), StrokeKind::Outside);
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A borderless icon-only button (close, copy).
pub fn icon_button(ui: &mut Ui, p: &Palette, glyph: char, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size + 14.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), p.subtle_hover);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, theme::icon_font(size), p.text2);
    resp
}

/// A small rounded label: "Primary", "GPU decode". Sized up front so
/// `horizontal_wrapped` can wrap whole badges.
pub fn badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), theme::font(theme::CAPTION), fg);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(16.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Critical,
}

impl Tone {
    pub fn colors(self, p: &Palette) -> (char, Color32, Color32) {
        match self {
            Tone::Success => (icon::CHECK, p.success, p.success_bg),
            Tone::Critical => (icon::ERROR, p.critical, p.critical_bg),
        }
    }
}

/// WinUI InfoBar. Returns true when its close button was pressed.
pub fn info_bar(ui: &mut Ui, p: &Palette, tone: Tone, message: &str) -> bool {
    let (glyph, fg, bg) = tone.colors(p);
    let mut closed = false;
    Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, p.card_stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(14, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                status_icon(ui, glyph, fg, p);
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    closed = icon_button(ui, p, icon::CLOSE, 10.0).clicked();
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(message).wrap());
                    });
                });
            });
        });
    closed
}

/// The filled circle WinUI uses for InfoBar and status glyphs.
pub fn status_icon(ui: &mut Ui, glyph: char, color: Color32, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 8.0, color);
    let fg = if p.dark { Color32::BLACK } else { Color32::WHITE };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, theme::icon_font(9.0), fg);
}

/// A thin labelled meter, for gamut coverage.
pub fn meter(ui: &mut Ui, p: &Palette, label: &str, fraction: f32) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(64.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(64.0);
            ui.label(RichText::new(label).color(p.text2));
        });
        let (rect, _) = ui.allocate_exact_size(vec2(160.0, 18.0), Sense::hover());
        let bar = egui::Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        ui.painter().rect_filled(bar, CornerRadius::same(2), p.subtle_hover);
        let mut fill = bar;
        fill.set_width(bar.width() * fraction.clamp(0.0, 1.0));
        ui.painter().rect_filled(fill, CornerRadius::same(2), p.accent);
        ui.label(format!("{:.0}%", fraction * 100.0));
    });
}
