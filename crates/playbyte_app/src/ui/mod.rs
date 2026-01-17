mod components;
mod thumbnails;

use crate::{FeedController, FrameStats};
use components::{badge, hero_preview, hint_strip, library_card, pill_button, primary_button, toast};
use playbyte_emulation::EmulatorRuntime;
use playbyte_types::System;
use std::time::{Duration, Instant};

use crate::input::Action;
use thumbnails::ThumbnailCache;

pub struct UiContext<'a> {
    pub feed: Option<&'a FeedController>,
    pub runtime: Option<&'a EmulatorRuntime>,
    pub frame_stats: &'a FrameStats,
    pub feed_error: Option<&'a str>,
}

pub struct UiOutput {
    pub actions: Vec<Action>,
}

#[derive(Clone, Copy)]
pub enum ToastKind {
    Success,
    Error,
}

pub struct UiState {
    theme: UiTheme,
    overlay_visible: bool,
    search_query: String,
    filter: LibraryFilter,
    last_interaction: Instant,
    toasts: Vec<Toast>,
    thumbnails: ThumbnailCache,
    transition_start: Option<Instant>,
    search_focused: bool,
}

impl UiState {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = UiTheme::default();
        apply_theme(ctx, &theme);
        Self {
            theme,
            overlay_visible: true,
            search_query: String::new(),
            filter: LibraryFilter::All,
            last_interaction: Instant::now(),
            toasts: Vec::new(),
            thumbnails: ThumbnailCache::new(64),
            transition_start: None,
            search_focused: false,
        }
    }

    pub fn render(&mut self, ctx: &egui::Context, data: UiContext<'_>) -> UiOutput {
        let mut actions = Vec::new();
        let now = Instant::now();
        self.search_focused = false;

        let mut details_visible = false;
        if self.overlay_visible {
            self.render_top_bar(ctx, &data, &mut actions);
            details_visible = self.should_show_details(now);
            self.render_library(ctx, &data, &mut actions, details_visible);
        } else {
            self.render_minimal_overlay(ctx);
        }

        if self.should_show_hint(now) {
            let offset = if self.overlay_visible {
                if details_visible {
                    -260.0
                } else {
                    -170.0
                }
            } else {
                -24.0
            };
            egui::Area::new(egui::Id::new("controls_hint"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, offset])
                .show(ctx, |ui| {
                    hint_strip(
                        ui,
                        "Swipe trackpad or L2/R2 to browse • B to bookmark • Tab to hide",
                        &self.theme,
                    );
                });
        }

        self.render_toasts(ctx, now);
        self.render_transition(ctx, now);

        UiOutput { actions }
    }

    pub fn push_toast(&mut self, kind: ToastKind, message: String) {
        self.toasts.push(Toast {
            kind,
            message,
            created_at: Instant::now(),
        });
    }

    pub fn trigger_transition(&mut self) {
        self.transition_start = Some(Instant::now());
    }

    pub fn record_interaction(&mut self) {
        self.last_interaction = Instant::now();
    }

    pub fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
    }

    pub fn is_filtering(&self) -> bool {
        self.filter != LibraryFilter::All || !self.search_query.trim().is_empty()
    }

    fn should_show_details(&self, now: Instant) -> bool {
        if self.search_focused || self.is_filtering() {
            return true;
        }
        now.saturating_duration_since(self.last_interaction) < Duration::from_secs(6)
    }

    pub fn filtered_indices(&self, feed: &FeedController) -> Vec<usize> {
        feed.items()
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                if self.matches_filter(item) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn matches_filter(&self, item: &crate::FeedItem) -> bool {
        if self.filter == LibraryFilter::Nes && item.system() != System::Nes {
            return false;
        }
        if self.filter == LibraryFilter::Snes && item.system() != System::Snes {
            return false;
        }
        let query = self.search_query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        match item {
            crate::FeedItem::Byte(byte) => {
                let in_title = byte.title.to_lowercase().contains(&query);
                let in_id = byte.byte_id.to_lowercase().contains(&query);
                let in_tags = byte
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&query));
                let in_description = byte.description.to_lowercase().contains(&query);
                in_title || in_id || in_tags || in_description
            }
            crate::FeedItem::RomFallback(fallback) => {
                let in_title = fallback.title.to_lowercase().contains(&query);
                let in_id = fallback.rom_sha1.to_lowercase().contains(&query);
                in_title || in_id
            }
        }
    }

    fn render_top_bar(
        &mut self,
        ctx: &egui::Context,
        data: &UiContext<'_>,
        actions: &mut Vec<Action>,
    ) {
        let screen_width = ctx.screen_rect().width();
        let panel_width = (screen_width - 32.0).max(320.0).min(screen_width);
        egui::Area::new(egui::Id::new("top_bar"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 14.0])
            .show(ctx, |ui| {
                ui.set_min_width(panel_width);
                egui::Frame::none()
                    .fill(self.theme.panel)
                    .rounding(egui::Rounding::same(16.0))
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Playbyte")
                                    .color(self.theme.text)
                                    .size(20.0)
                                    .strong(),
                            );
                            ui.add_space(8.0);
                            if let Some(fps) = data.frame_stats.avg_fps() {
                                ui.label(
                                    egui::RichText::new(format!("{fps:.0} fps"))
                                        .color(self.theme.text_dim)
                                        .size(12.0),
                                );
                            }
                            if let Some(runtime) = &data.runtime {
                                ui.label(
                                    egui::RichText::new(format!("emu {:.1} Hz", runtime.fps()))
                                        .color(self.theme.text_dim)
                                        .size(12.0),
                                );
                            }
                            if let Some(feed) = data.feed {
                                ui.label(
                                    egui::RichText::new(format!("{} items", feed.item_count()))
                                        .color(self.theme.text_dim)
                                        .size(12.0),
                                );
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if data.runtime.is_some() {
                                        if primary_button(ui, "Bookmark", &self.theme).clicked() {
                                            actions.push(Action::CreateByte);
                                            self.record_interaction();
                                        }
                                    }
                                    ui.add_space(10.0);
                                    let search = ui.add(
                                        egui::TextEdit::singleline(&mut self.search_query)
                                            .hint_text("Search titles, tags…")
                                            .desired_width(200.0),
                                    );
                                    self.search_focused = search.has_focus();
                                    if search.changed() {
                                        self.record_interaction();
                                    }
                                    ui.add_space(8.0);
                                    if pill_button(
                                        ui,
                                        "All",
                                        self.filter == LibraryFilter::All,
                                        &self.theme,
                                    )
                                    .clicked()
                                    {
                                        self.filter = LibraryFilter::All;
                                        self.record_interaction();
                                    }
                                    if pill_button(
                                        ui,
                                        "NES",
                                        self.filter == LibraryFilter::Nes,
                                        &self.theme,
                                    )
                                    .clicked()
                                    {
                                        self.filter = LibraryFilter::Nes;
                                        self.record_interaction();
                                    }
                                    if pill_button(
                                        ui,
                                        "SNES",
                                        self.filter == LibraryFilter::Snes,
                                        &self.theme,
                                    )
                                    .clicked()
                                    {
                                        self.filter = LibraryFilter::Snes;
                                        self.record_interaction();
                                    }
                                },
                            );
                        });
                    });
            });
    }

    fn render_library(
        &mut self,
        ctx: &egui::Context,
        data: &UiContext<'_>,
        actions: &mut Vec<Action>,
        details_visible: bool,
    ) {
        let screen_width = ctx.screen_rect().width();
        let panel_width = (screen_width - 32.0).max(320.0).min(screen_width);
        egui::Area::new(egui::Id::new("library_panel"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -18.0])
            .show(ctx, |ui| {
                ui.set_min_width(panel_width);
                let panel = egui::Frame::none()
                    .fill(self.theme.panel)
                    .rounding(egui::Rounding::same(18.0))
                    .inner_margin(egui::Margin::symmetric(18.0, 14.0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                        if let Some(feed) = data.feed {
                            if let Some(current) = feed.current() {
                                if details_visible {
                                    let preview_width =
                                        (panel_width * 0.32).clamp(240.0, 360.0);
                                    let preview_size =
                                        egui::Vec2::new(preview_width, preview_width * 0.6);
                                    ui.horizontal(|ui| {
                                        let thumb = match current {
                                            crate::FeedItem::Byte(byte) => {
                                                self.thumbnails.get(ctx, &feed.store, byte)
                                            }
                                            crate::FeedItem::RomFallback(_) => None,
                                        };
                                        hero_preview(ui, thumb.as_ref(), preview_size, &self.theme);
                                        ui.add_space(12.0);
                                        match current {
                                            crate::FeedItem::Byte(byte) => {
                                                ui.vertical(|ui| {
                                                    let title = if byte.title.is_empty() {
                                                        &byte.byte_id
                                                    } else {
                                                        &byte.title
                                                    };
                                                    ui.label(
                                                        egui::RichText::new(title)
                                                            .size(22.0)
                                                            .strong()
                                                            .color(self.theme.text),
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.horizontal(|ui| {
                                                        let (system_label, system_color) =
                                                            match byte.system {
                                                                System::Nes => {
                                                                    ("NES", self.theme.system_nes)
                                                                }
                                                                System::Snes => (
                                                                    "SNES",
                                                                    self.theme.system_snes,
                                                                ),
                                                            };
                                                        badge(
                                                            ui,
                                                            system_label,
                                                            system_color,
                                                            self.theme.text_on_accent,
                                                        );
                                                        for tag in byte.tags.iter().take(2) {
                                                            badge(
                                                                ui,
                                                                tag,
                                                                self.theme.panel_alt,
                                                                self.theme.text_dim,
                                                            );
                                                        }
                                                    });
                                                    ui.add_space(6.0);
                                                    let description = if byte.description.is_empty()
                                                    {
                                                        "No description yet."
                                                    } else {
                                                        byte.description.as_str()
                                                    };
                                                    ui.label(
                                                        egui::RichText::new(description)
                                                            .color(self.theme.text_dim)
                                                            .size(14.0),
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Saved by {} • {}",
                                                            byte.author, byte.created_at
                                                        ))
                                                        .color(self.theme.text_dim)
                                                        .size(12.0),
                                                    );
                                                });
                                            }
                                            crate::FeedItem::RomFallback(fallback) => {
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&fallback.title)
                                                            .size(22.0)
                                                            .strong()
                                                            .color(self.theme.text),
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.horizontal(|ui| {
                                                        let (system_label, system_color) =
                                                            match fallback.system {
                                                                System::Nes => {
                                                                    ("NES", self.theme.system_nes)
                                                                }
                                                                System::Snes => (
                                                                    "SNES",
                                                                    self.theme.system_snes,
                                                                ),
                                                            };
                                                        badge(
                                                            ui,
                                                            system_label,
                                                            system_color,
                                                            self.theme.text_on_accent,
                                                        );
                                                        badge(
                                                            ui,
                                                            "Default",
                                                            self.theme.panel_alt,
                                                            self.theme.text_dim,
                                                        );
                                                    });
                                                    ui.add_space(6.0);
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "Default ROM state. Bookmark to save a Byte.",
                                                        )
                                                        .color(self.theme.text_dim)
                                                        .size(14.0),
                                                    );
                                                });
                                            }
                                        }
                                    });
                                    ui.add_space(12.0);
                                }

                                if let Some(error) = data.feed_error {
                                    ui.colored_label(self.theme.error, error);
                                    ui.add_space(8.0);
                                }

                                let indices = self.filtered_indices(feed);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Library")
                                            .color(self.theme.text_dim)
                                            .size(12.0)
                                            .strong(),
                                    );
                                    ui.add_space(8.0);
                                    let count_label = if self.is_filtering() {
                                        format!("{} of {}", indices.len(), feed.item_count())
                                    } else {
                                        format!("{} items", feed.item_count())
                                    };
                                    ui.label(
                                        egui::RichText::new(count_label)
                                            .color(self.theme.text_dim)
                                            .size(12.0),
                                    );
                                });
                                ui.add_space(6.0);
                                if indices.is_empty() {
                                    ui.label(
                                        egui::RichText::new("No items match your filters.")
                                            .color(self.theme.text_dim),
                                    );
                                } else {
                                    egui::ScrollArea::horizontal()
                                        .id_source("library_scroll")
                                        .auto_shrink([false; 2])
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                for idx in indices {
                                                    let item = &feed.items()[idx];
                                                    let selected = idx == feed.current_index;
                                                    let anim = ui.ctx().animate_bool(
                                                        egui::Id::new(("card", idx)),
                                                        selected,
                                                    );
                                                    let thumb = match item {
                                                        crate::FeedItem::Byte(byte) => {
                                                            self.thumbnails.get(
                                                                ctx,
                                                                &feed.store,
                                                                byte,
                                                            )
                                                        }
                                                        crate::FeedItem::RomFallback(_) => None,
                                                    };
                                                    let response = library_card(
                                                        ui,
                                                        item.title(),
                                                        item.system(),
                                                        thumb.as_ref(),
                                                        selected,
                                                        anim,
                                                        egui::Vec2::new(180.0, 120.0),
                                                        &self.theme,
                                                    );
                                                    if response.clicked() {
                                                        actions.push(Action::SelectIndex(idx));
                                                        self.record_interaction();
                                                    }
                                                    if selected {
                                                        ui.scroll_to_rect(
                                                            response.rect,
                                                            Some(egui::Align::Center),
                                                        );
                                                    }
                                                }
                                            });
                                        });
                                }
                            } else {
                                if let Some(error) = data.feed_error {
                                    ui.colored_label(self.theme.error, error);
                                    ui.add_space(6.0);
                                }
                                ui.label(
                                    egui::RichText::new("No feed items available yet.")
                                        .color(self.theme.text_dim),
                                );
                            }
                        } else {
                            if let Some(error) = data.feed_error {
                                ui.colored_label(self.theme.error, error);
                                ui.add_space(6.0);
                            }
                            ui.label(
                                egui::RichText::new(
                                    "No feed loaded. Pass --core and --rom, or add ROMs/Bytes in your data folder (see --data).",
                                )
                                .color(self.theme.text_dim),
                            );
                        }
                    });
                if panel.response.hovered() {
                    self.record_interaction();
                }
            });
    }

    fn render_minimal_overlay(&self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("minimal_overlay"))
            .anchor(egui::Align2::LEFT_TOP, [20.0, 20.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Playbyte")
                        .size(18.0)
                        .color(self.theme.text),
                );
                ui.label(
                    egui::RichText::new("Press Tab to show library")
                        .size(12.0)
                        .color(self.theme.text_dim),
                );
            });
    }

    fn render_toasts(&mut self, ctx: &egui::Context, now: Instant) {
        let duration = Duration::from_secs(3);
        self.toasts.retain(|toast| now.saturating_duration_since(toast.created_at) < duration);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toast_area"))
            .anchor(egui::Align2::RIGHT_TOP, [-20.0, 20.0])
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for toast_item in &self.toasts {
                        let (fill, text) = toast_colors(toast_item.kind, &self.theme);
                        toast(ui, &toast_item.message, fill, text);
                        ui.add_space(6.0);
                    }
                });
            });
    }

    fn render_transition(&mut self, ctx: &egui::Context, now: Instant) {
        let Some(start) = self.transition_start else {
            return;
        };
        let elapsed = now.saturating_duration_since(start);
        let duration = Duration::from_millis(260);
        if elapsed >= duration {
            self.transition_start = None;
            return;
        }
        let t = 1.0 - elapsed.as_secs_f32() / duration.as_secs_f32();
        let alpha = (t * 180.0) as u8;
        let rect = ctx.screen_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("transition"),
        ))
        .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(alpha));
    }

    fn should_show_hint(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_interaction) < Duration::from_secs(4)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LibraryFilter {
    All,
    Nes,
    Snes,
}

struct Toast {
    kind: ToastKind,
    message: String,
    created_at: Instant,
}

pub(super) struct UiTheme {
    accent: egui::Color32,
    system_nes: egui::Color32,
    system_snes: egui::Color32,
    panel: egui::Color32,
    panel_alt: egui::Color32,
    card: egui::Color32,
    card_selected: egui::Color32,
    card_border: egui::Color32,
    text: egui::Color32,
    text_dim: egui::Color32,
    text_on_accent: egui::Color32,
    success: egui::Color32,
    error: egui::Color32,
}

impl UiTheme {
    fn default() -> Self {
        Self {
            accent: egui::Color32::from_rgb(77, 133, 255),
            system_nes: egui::Color32::from_rgb(64, 196, 255),
            system_snes: egui::Color32::from_rgb(176, 124, 255),
            panel: egui::Color32::from_rgba_unmultiplied(14, 16, 20, 190),
            panel_alt: egui::Color32::from_rgba_unmultiplied(26, 30, 38, 200),
            card: egui::Color32::from_rgba_unmultiplied(22, 26, 34, 220),
            card_selected: egui::Color32::from_rgba_unmultiplied(30, 36, 48, 230),
            card_border: egui::Color32::from_rgba_unmultiplied(44, 52, 66, 200),
            text: egui::Color32::from_rgb(232, 234, 240),
            text_dim: egui::Color32::from_rgb(162, 170, 184),
            text_on_accent: egui::Color32::from_rgb(240, 246, 255),
            success: egui::Color32::from_rgb(96, 206, 142),
            error: egui::Color32::from_rgb(248, 113, 113),
        }
    }
}

fn apply_theme(ctx: &egui::Context, theme: &UiTheme) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = theme.panel;
    style.visuals.window_rounding = egui::Rounding::same(14.0);
    style.visuals.widgets.inactive.rounding = egui::Rounding::same(10.0);
    style.visuals.widgets.hovered.rounding = egui::Rounding::same(10.0);
    style.visuals.widgets.active.rounding = egui::Rounding::same(10.0);
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(24.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    ctx.set_style(style);

    if let Some(font_data) = load_system_font() {
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("system".to_string(), egui::FontData::from_owned(font_data));
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "system".to_string());
        }
        ctx.set_fonts(fonts);
    }
}

#[cfg(target_os = "macos")]
fn load_system_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFNSDisplay.ttf",
        "/System/Library/Fonts/SFNSText.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(data) = std::fs::read(path) {
            return Some(data);
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn load_system_font() -> Option<Vec<u8>> {
    None
}

fn toast_colors(kind: ToastKind, theme: &UiTheme) -> (egui::Color32, egui::Color32) {
    match kind {
        ToastKind::Success => (theme.success, theme.text_on_accent),
        ToastKind::Error => (theme.error, theme.text_on_accent),
    }
}
