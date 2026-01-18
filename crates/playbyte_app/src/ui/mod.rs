mod components;
mod cover_art;
mod thumbnails;

use crate::{FeedController, FrameStats};
use components::{badge, hero_preview, hint_strip, library_card, primary_button, toast};
use cover_art::CoverArtCache;
use playbyte_emulation::EmulatorRuntime;
use playbyte_types::System;
use std::time::{Duration, Instant};

use crate::input::Action;
use thumbnails::ThumbnailCache;

const OFFICIAL_PICKER_LIMIT: usize = 24;

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
    last_interaction: Instant,
    toasts: Vec<Toast>,
    thumbnails: ThumbnailCache,
    covers: CoverArtCache,
    transition_start: Option<Instant>,
    rename_target: Option<usize>,
    rename_draft: String,
    last_centered_index: Option<usize>,
    official_picker: Option<OfficialPickerState>,
}

impl UiState {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = UiTheme::default();
        apply_theme(ctx, &theme);
        Self {
            theme,
            overlay_visible: true,
            last_interaction: Instant::now(),
            toasts: Vec::new(),
            thumbnails: ThumbnailCache::new(64),
            covers: CoverArtCache::new(64),
            transition_start: None,
            rename_target: None,
            rename_draft: String::new(),
            last_centered_index: None,
            official_picker: None,
        }
    }

    pub fn render(&mut self, ctx: &egui::Context, data: UiContext<'_>) -> UiOutput {
        let mut actions = Vec::new();
        let now = Instant::now();

        if self.overlay_visible {
            self.render_top_bar(ctx, &data, &mut actions);
            self.render_library(ctx, &data, &mut actions);
        } else {
            self.render_minimal_overlay(ctx);
        }

        if self.should_show_hint(now) {
            egui::Area::new(egui::Id::new("controls_hint"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -24.0])
                .show(ctx, |ui| {
                    hint_strip(
                        ui,
                        "Swipe trackpad or L2/R2 to browse • Square/B to bookmark • Options/Tab to hide",
                        &self.theme,
                    );
                });
        }

        self.render_official_picker(ctx, &mut actions);
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

    pub fn is_overlay_visible(&self) -> bool {
        self.overlay_visible
    }

    pub fn is_editing_text(&self) -> bool {
        self.rename_target.is_some()
    }

    pub fn is_official_picker_open(&self) -> bool {
        self.official_picker.is_some()
    }

    pub fn start_rename(&mut self, index: usize, title: String) {
        self.rename_target = Some(index);
        self.rename_draft = title;
        self.record_interaction();
    }

    pub fn cancel_active_ui(&mut self) -> bool {
        let mut changed = false;
        if self.rename_target.is_some() {
            self.rename_target = None;
            changed = true;
        }
        if self.official_picker.is_some() {
            self.official_picker = None;
            changed = true;
        }
        if changed {
            self.record_interaction();
        }
        changed
    }

    pub fn move_official_picker_selection(&mut self, delta: i32) {
        let Some(state) = self.official_picker.as_mut() else {
            return;
        };
        let results_len = state.search_results(OFFICIAL_PICKER_LIMIT).len();
        if results_len == 0 {
            return;
        }
        let mut next = state.selected_result as i32 + delta;
        next = next.clamp(0, results_len.saturating_sub(1) as i32);
        let next = next as usize;
        if next != state.selected_result {
            state.selected_result = next;
            self.record_interaction();
        }
    }

    pub fn confirm_official_picker_selection(&mut self) -> Option<(usize, String)> {
        let (index, title) = {
            let state = self.official_picker.as_ref()?;
            let results = state.search_results(OFFICIAL_PICKER_LIMIT);
            if results.is_empty() {
                return None;
            }
            let selected = state.selected_result.min(results.len() - 1);
            let title = state.titles[results[selected]].clone();
            (state.index, title)
        };
        self.official_picker = None;
        self.record_interaction();
        Some((index, title))
    }

    pub fn invalidate_cover_art(&mut self, rom_sha1: &str) {
        self.covers.invalidate(rom_sha1);
    }

    pub fn open_official_picker(
        &mut self,
        index: usize,
        fallback: &crate::RomFallback,
        store: &playbyte_feed::LocalByteStore,
    ) {
        let titles = store
            .list_romdb_titles(fallback.system.clone())
            .unwrap_or_default();
        let normalized_titles = titles
            .iter()
            .map(|title| normalize_picker_query(title))
            .collect();
        self.official_picker = Some(OfficialPickerState {
            index,
            system: fallback.system.clone(),
            titles,
            normalized_titles,
            query: String::new(),
            selected_result: 0,
        });
    }

    fn render_top_bar(
        &mut self,
        ctx: &egui::Context,
        data: &UiContext<'_>,
        actions: &mut Vec<Action>,
    ) {
        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::none()
                    .fill(self.theme.panel)
                    .inner_margin(egui::Margin::symmetric(20.0, 14.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Playbyte")
                            .color(self.theme.text)
                            .size(24.0)
                            .strong(),
                    );
                    ui.add_space(12.0);
                    if let Some(fps) = data.frame_stats.avg_fps() {
                        ui.label(
                            egui::RichText::new(format!("{fps:.0} fps"))
                                .color(self.theme.text_dim),
                        );
                    }
                    if let Some(runtime) = &data.runtime {
                        ui.label(
                            egui::RichText::new(format!("emu {:.1} Hz", runtime.fps()))
                                .color(self.theme.text_dim),
                        );
                    }
                    if let Some(feed) = data.feed {
                        ui.label(
                            egui::RichText::new(format!("{} items", feed.items.len()))
                                .color(self.theme.text_dim),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if data.runtime.is_some() {
                            if primary_button(ui, "Save to Play Later", &self.theme).clicked() {
                                actions.push(Action::CreateByte);
                                self.record_interaction();
                            }
                        }
                    });
                });
            });
    }

    fn render_library(
        &mut self,
        ctx: &egui::Context,
        data: &UiContext<'_>,
        actions: &mut Vec<Action>,
    ) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(self.theme.panel)
                    .inner_margin(egui::Margin::symmetric(24.0, 20.0)),
            )
            .show(ctx, |ui| {
                if let Some(feed) = data.feed {
                    if self
                        .rename_target
                        .map(|target| target >= feed.items.len())
                        .unwrap_or(false)
                    {
                        self.rename_target = None;
                    }
                    if let Some(current) = feed.current() {
                        ui.horizontal(|ui| {
                            let thumb = match current {
                                crate::FeedItem::Byte(byte) => {
                                    self.thumbnails.get(ctx, &feed.store, byte)
                                }
                                crate::FeedItem::RomFallback(fallback) => {
                                    self.covers.get(ctx, &feed.store, fallback)
                                }
                            };
                            hero_preview(
                                ui,
                                thumb.as_ref(),
                                egui::Vec2::new(360.0, 220.0),
                                &self.theme,
                            );
                            ui.add_space(18.0);
                            ui.vertical(|ui| {
                                let title = current.title();
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(title)
                                            .size(26.0)
                                            .strong()
                                            .color(self.theme.text),
                                    );
                                    if ui.button("Rename").clicked() {
                                        self.rename_target = Some(feed.current_index);
                                        self.rename_draft = title.to_string();
                                        self.record_interaction();
                                    }
                                });
                                if self.rename_target == Some(feed.current_index) {
                                    ui.add_space(6.0);
                                    let edit = ui.text_edit_singleline(&mut self.rename_draft);
                                    if edit.changed() {
                                        self.record_interaction();
                                    }
                                    ui.horizontal(|ui| {
                                        if ui.button("Save").clicked() {
                                            actions.push(Action::RenameTitle {
                                                index: feed.current_index,
                                                title: self.rename_draft.clone(),
                                            });
                                            self.rename_target = None;
                                            self.record_interaction();
                                        }
                                        if ui.button("Cancel").clicked() {
                                            self.rename_target = None;
                                            self.record_interaction();
                                        }
                                    });
                                }
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    let system_label = match current.system() {
                                        System::Nes => "NES",
                                        System::Snes => "SNES",
                                        System::Gbc => "GBC",
                                        System::Gba => "GBA",
                                    };
                                    badge(ui, system_label, self.theme.accent_soft, self.theme.text);
                                    if let crate::FeedItem::Byte(byte) = current {
                                        for tag in byte.tags.iter().take(3) {
                                            badge(ui, tag, self.theme.panel_alt, self.theme.text_dim);
                                        }
                                    }
                                });
                                ui.add_space(8.0);
                                let description = match current {
                                    crate::FeedItem::Byte(byte) => {
                                        if byte.description.is_empty() {
                                            "No description yet."
                                        } else {
                                            byte.description.as_str()
                                        }
                                    }
                                    crate::FeedItem::RomFallback(_) => {
                                        "ROM not bookmarked yet. Press B to create a Byte."
                                    }
                                };
                                ui.label(
                                    egui::RichText::new(description)
                                        .color(self.theme.text_dim)
                                        .size(15.0),
                                );
                                ui.add_space(8.0);
                                match current {
                                    crate::FeedItem::Byte(byte) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Saved by {} • {}",
                                                byte.author, byte.created_at
                                            ))
                                            .color(self.theme.text_dim)
                                            .size(13.0),
                                        );
                                    }
                                    crate::FeedItem::RomFallback(fallback) => {
                                        let filename = fallback
                                            .rom_path
                                            .file_name()
                                            .and_then(|name| name.to_str())
                                            .unwrap_or("ROM");
                                        ui.label(
                                            egui::RichText::new(format!("Local ROM • {filename}"))
                                                .color(self.theme.text_dim)
                                                .size(13.0),
                                        );
                                    }
                                }
                            });
                        });

                        if let Some(error) = data.feed_error {
                            ui.add_space(12.0);
                            ui.colored_label(self.theme.error, error);
                        }

                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new("Library")
                                .color(self.theme.text_dim)
                                .size(14.0)
                                .strong(),
                        );
                        ui.add_space(8.0);
                        egui::ScrollArea::horizontal()
                            .id_source("library_scroll")
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    for (idx, item) in feed.items.iter().enumerate() {
                                        let selected = idx == feed.current_index;
                                        let anim = ui
                                            .ctx()
                                            .animate_bool(egui::Id::new(("card", idx)), selected);
                                        let thumb = match item {
                                            crate::FeedItem::Byte(byte) => {
                                                self.thumbnails.get(ctx, &feed.store, byte)
                                            }
                                            crate::FeedItem::RomFallback(fallback) => {
                                                self.covers.get(ctx, &feed.store, fallback)
                                            }
                                        };
                                        let response = library_card(
                                            ui,
                                            item,
                                            thumb.as_ref(),
                                            selected,
                                            anim,
                                            egui::Vec2::new(200.0, 140.0),
                                            &self.theme,
                                        );
                                        response.context_menu(|ui| {
                                            if ui.button("Rename").clicked() {
                                                self.rename_target = Some(idx);
                                                self.rename_draft = item.title().to_string();
                                                actions.push(Action::SelectIndex(idx));
                                                self.record_interaction();
                                                ui.close_menu();
                                            }
                                            if let crate::FeedItem::RomFallback(fallback) = item {
                                                if ui.button("Choose official game...").clicked() {
                                                    self.open_official_picker(
                                                        idx,
                                                        fallback,
                                                        &feed.store,
                                                    );
                                                    actions.push(Action::SelectIndex(idx));
                                                    self.record_interaction();
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        if response.clicked() {
                                            if self.rename_target != Some(idx) {
                                                self.rename_target = None;
                                            }
                                            actions.push(Action::SelectIndex(idx));
                                            self.record_interaction();
                                        }
                                        if selected {
                                            if self.last_centered_index != Some(idx) {
                                                ui.scroll_to_rect(
                                                    response.rect,
                                                    Some(egui::Align::Center),
                                                );
                                                self.last_centered_index = Some(idx);
                                            }
                                        }
                                    }
                                });
                            });
                    } else {
                        self.last_centered_index = None;
                        ui.label(
                            egui::RichText::new("No Bytes found in data/bytes.")
                                .color(self.theme.text_dim),
                        );
                    }
                } else {
                    self.last_centered_index = None;
                    ui.label(
                        egui::RichText::new(
                            "No feed loaded. Pass --core and --rom or add Bytes in ./data/bytes.",
                        )
                        .color(self.theme.text_dim),
                    );
                }
            });
    }

    fn render_official_picker(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(state) = self.official_picker.as_mut() else {
            return;
        };
        let mut close = false;
        egui::Window::new("Choose official game")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let system_label = match state.system {
                    System::Nes => "NES",
                    System::Snes => "SNES",
                    System::Gbc => "GBC",
                    System::Gba => "GBA",
                };
                ui.label(format!("System: {system_label}"));
                ui.add_space(6.0);
                ui.label("Search official titles");
                let response = ui.text_edit_singleline(&mut state.query);
                if response.changed() {
                    state.selected_result = 0;
                }
                ui.add_space(8.0);
                if state.titles.is_empty() {
                    ui.label("No official database available.");
                } else {
                    let results = state.search_results(OFFICIAL_PICKER_LIMIT);
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            if !results.is_empty() {
                                state.selected_result =
                                    state.selected_result.min(results.len().saturating_sub(1));
                            }
                            for (pos, idx) in results.iter().copied().enumerate() {
                                let title = &state.titles[idx];
                                let selected = pos == state.selected_result;
                                if ui.selectable_label(selected, title).clicked() {
                                    actions.push(Action::SetOfficialTitle {
                                        index: state.index,
                                        title: title.clone(),
                                    });
                                    close = true;
                                }
                            }
                        });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear override").clicked() {
                        actions.push(Action::ClearOfficialTitle { index: state.index });
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.official_picker = None;
        }
    }

    fn render_minimal_overlay(&self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("minimal_overlay"))
            .anchor(egui::Align2::LEFT_TOP, [20.0, 20.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Playbyte")
                        .size(20.0)
                        .color(self.theme.text),
                );
                ui.label(
                    egui::RichText::new("Press Tab to show library")
                        .size(13.0)
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

struct Toast {
    kind: ToastKind,
    message: String,
    created_at: Instant,
}

struct OfficialPickerState {
    index: usize,
    system: System,
    titles: Vec<String>,
    normalized_titles: Vec<String>,
    query: String,
    selected_result: usize,
}

impl OfficialPickerState {
    fn search_results(&self, limit: usize) -> Vec<usize> {
        let query = normalize_picker_query(&self.query);
        if query.is_empty() {
            return (0..self.titles.len()).take(limit).collect();
        }
        let mut matches: Vec<(i32, usize)> = Vec::new();
        for (idx, normalized) in self.normalized_titles.iter().enumerate() {
            if normalized.contains(&query) {
                let mut score = 400;
                if normalized == &query {
                    score = 1000;
                } else if normalized.starts_with(&query) {
                    score = 800;
                }
                score -= (normalized.len() as i32 - query.len() as i32).abs().min(200);
                matches.push((score, idx));
            }
        }
        matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| self.titles[a.1].cmp(&self.titles[b.1])));
        matches
            .into_iter()
            .take(limit)
            .map(|(_, idx)| idx)
            .collect()
    }
}

fn normalize_picker_query(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            ' '
        };
        if normalized == ' ' {
            if !last_space {
                output.push(' ');
                last_space = true;
            }
        } else {
            output.push(normalized);
            last_space = false;
        }
    }
    output.trim().to_string()
}

pub(super) struct UiTheme {
    accent: egui::Color32,
    accent_soft: egui::Color32,
    system_nes: egui::Color32,
    system_snes: egui::Color32,
    system_gbc: egui::Color32,
    system_gba: egui::Color32,
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
            accent_soft: egui::Color32::from_rgb(43, 61, 102),
            system_nes: egui::Color32::from_rgb(64, 196, 255),
            system_snes: egui::Color32::from_rgb(176, 124, 255),
            system_gbc: egui::Color32::from_rgb(96, 206, 142),
            system_gba: egui::Color32::from_rgb(248, 156, 90),
            panel: egui::Color32::from_rgba_unmultiplied(14, 16, 20, 210),
            panel_alt: egui::Color32::from_rgba_unmultiplied(26, 30, 38, 220),
            card: egui::Color32::from_rgba_unmultiplied(22, 26, 34, 230),
            card_selected: egui::Color32::from_rgba_unmultiplied(30, 36, 48, 235),
            card_border: egui::Color32::from_rgba_unmultiplied(44, 52, 66, 220),
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
    style.visuals.window_rounding = egui::Rounding::same(16.0);
    style.visuals.widgets.inactive.rounding = egui::Rounding::same(12.0);
    style.visuals.widgets.hovered.rounding = egui::Rounding::same(12.0);
    style.visuals.widgets.active.rounding = egui::Rounding::same(12.0);
    style.spacing.item_spacing = egui::vec2(12.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(26.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(16.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
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
