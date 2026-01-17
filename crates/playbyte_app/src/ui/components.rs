use super::thumbnails::Thumbnail;
use super::UiTheme;
use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Rounding, Sense, Stroke, Vec2};
use playbyte_types::System;

pub fn badge(ui: &mut egui::Ui, label: &str, fill: Color32, text: Color32) -> Response {
    egui::Frame::none()
        .fill(fill)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(text)
                    .size(12.0)
                    .strong(),
            )
        })
        .response
}

pub fn pill_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    theme: &UiTheme,
) -> Response {
    let fill = if selected {
        theme.accent
    } else {
        theme.panel_alt
    };
    let text = if selected {
        theme.text_on_accent
    } else {
        theme.text
    };
    egui::Frame::none()
        .fill(fill)
        .rounding(Rounding::same(999.0))
        .inner_margin(egui::Margin::symmetric(12.0, 5.0))
        .show(ui, |ui| ui.label(egui::RichText::new(label).color(text).size(12.0)))
        .response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn primary_button(ui: &mut egui::Ui, label: &str, theme: &UiTheme) -> Response {
    egui::Frame::none()
        .fill(theme.accent)
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(theme.text_on_accent)
                    .size(13.0),
            )
        })
        .response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn library_card(
    ui: &mut egui::Ui,
    title: &str,
    system: System,
    thumb: Option<&Thumbnail>,
    selected: bool,
    anim: f32,
    size: Vec2,
    theme: &UiTheme,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let draw_rect = Rect::from_center_size(
        rect.center(),
        Vec2::new(size.x * lerp(0.96, 1.02, anim), size.y * lerp(0.96, 1.02, anim)),
    );

    let fill = if selected { theme.card_selected } else { theme.card };
    let stroke = Stroke::new(1.0, theme.card_border);
    let rounding = Rounding::same(16.0);
    let shadow_rect = draw_rect.translate(Vec2::new(0.0, 6.0));
    ui.painter().rect_filled(
        shadow_rect,
        rounding,
        Color32::from_black_alpha(80),
    );
    ui.painter().rect_filled(draw_rect, rounding, fill);
    ui.painter().rect_stroke(draw_rect, rounding, stroke);

    if selected {
        ui.painter().rect_stroke(
            draw_rect,
            rounding,
            Stroke::new(1.2, theme.accent),
        );
    }

    let image_rect = draw_rect.shrink(8.0);
    if let Some(thumb) = thumb {
        let fitted = fit_aspect(thumb.size, image_rect);
        ui.painter().image(
            thumb.id,
            fitted,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(image_rect, rounding, theme.panel_alt);
    }

    let (system_label, system_color) = match system {
        System::Nes => ("NES", theme.system_nes),
        System::Snes => ("SNES", theme.system_snes),
    };
    let badge_rect = Rect::from_min_size(
        Pos2::new(draw_rect.left() + 12.0, draw_rect.top() + 12.0),
        Vec2::new(48.0, 18.0),
    );
    ui.painter()
        .rect_filled(badge_rect, Rounding::same(6.0), system_color);
    ui.painter().text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        system_label,
        FontId::new(10.0, egui::FontFamily::Proportional),
        theme.text_on_accent,
    );

    let title_pos = Pos2::new(draw_rect.left() + 14.0, draw_rect.bottom() - 22.0);
    ui.painter().text(
        title_pos,
        Align2::LEFT_CENTER,
        title,
        FontId::new(14.0, egui::FontFamily::Proportional),
        theme.text,
    );

    response
}

pub fn hero_preview(
    ui: &mut egui::Ui,
    thumb: Option<&Thumbnail>,
    size: Vec2,
    theme: &UiTheme,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let rounding = Rounding::same(18.0);
    ui.painter().rect_filled(rect, rounding, theme.panel_alt);
    if let Some(thumb) = thumb {
        let fitted = fit_aspect(thumb.size, rect.shrink(6.0));
        ui.painter().image(
            thumb.id,
            fitted,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    response
}

pub fn toast(ui: &mut egui::Ui, text: &str, fill: Color32, text_color: Color32) {
    egui::Frame::none()
        .fill(fill)
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| ui.label(egui::RichText::new(text).color(text_color)));
}

pub fn hint_strip(ui: &mut egui::Ui, text: &str, theme: &UiTheme) {
    egui::Frame::none()
        .fill(theme.panel_alt)
        .rounding(Rounding::same(14.0))
        .inner_margin(egui::Margin::symmetric(14.0, 8.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .color(theme.text_dim)
                    .size(12.0),
            )
        });
}

fn fit_aspect(image_size: Vec2, bounds: Rect) -> Rect {
    if image_size.x <= 0.0 || image_size.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width() / image_size.x).min(bounds.height() / image_size.y);
    let size = Vec2::new(image_size.x * scale, image_size.y * scale);
    Rect::from_center_size(bounds.center(), size)
}

fn lerp(min: f32, max: f32, t: f32) -> f32 {
    min + (max - min) * t.clamp(0.0, 1.0)
}
