use super::thumbnails::Thumbnail;
use crate::RomFallback;
use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};
use playbyte_feed::LocalByteStore;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct CoverArtEntry {
    handle: TextureHandle,
    last_used: Instant,
}

pub struct CoverArtCache {
    entries: HashMap<String, CoverArtEntry>,
    order: VecDeque<String>,
    attempts: HashMap<String, Instant>,
    inflight: Arc<Mutex<HashSet<String>>>,
    max_entries: usize,
}

impl CoverArtCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            attempts: HashMap::new(),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            max_entries,
        }
    }

    pub fn get(
        &mut self,
        ctx: &egui::Context,
        store: &LocalByteStore,
        fallback: &RomFallback,
    ) -> Option<Thumbnail> {
        let title = fallback.official_title.as_ref()?;
        let key = &fallback.rom_sha1;
        let now = Instant::now();
        if let Some((id, size)) = self.entries.get_mut(key).map(|entry| {
            entry.last_used = now;
            (entry.handle.id(), entry.handle.size())
        }) {
            self.bump(key);
            return Some(Thumbnail {
                id,
                size: Vec2::new(size[0] as f32, size[1] as f32),
            });
        }

        if let Ok(data) = store.load_cover_art(fallback.system.clone(), title) {
            if let Some(handle) = load_texture(ctx, key, &data) {
                self.attempts.remove(key);
                let size = handle.size();
                self.entries.insert(
                    key.clone(),
                    CoverArtEntry {
                        handle,
                        last_used: now,
                    },
                );
                self.bump(key);
                self.evict();
                let entry = self.entries.get(key)?;
                return Some(Thumbnail {
                    id: entry.handle.id(),
                    size: Vec2::new(size[0] as f32, size[1] as f32),
                });
            }
        }

        if self.should_throttle(key, now) {
            return None;
        }
        if self.mark_inflight(key) {
            let store = store.clone();
            let system = fallback.system.clone();
            let title = title.clone();
            let key = key.clone();
            let inflight = self.inflight.clone();
            self.attempts.insert(key.clone(), now);
            std::thread::spawn(move || {
                let _ = store.ensure_cover_art(system, &title);
                if let Ok(mut guard) = inflight.lock() {
                    guard.remove(&key);
                }
            });
        }

        None
    }

    pub fn invalidate(&mut self, rom_sha1: &str) {
        self.entries.remove(rom_sha1);
        self.order.retain(|item| item != rom_sha1);
        self.attempts.remove(rom_sha1);
        if let Ok(mut guard) = self.inflight.lock() {
            guard.remove(rom_sha1);
        }
    }

    fn should_throttle(&self, key: &str, now: Instant) -> bool {
        self.attempts
            .get(key)
            .is_some_and(|last| now.saturating_duration_since(*last) < Duration::from_secs(6))
    }

    fn mark_inflight(&self, key: &str) -> bool {
        let Ok(mut guard) = self.inflight.lock() else {
            return false;
        };
        if guard.contains(key) {
            return false;
        }
        guard.insert(key.to_string());
        true
    }

    fn bump(&mut self, key: &str) {
        self.order.retain(|item| item != key);
        self.order.push_back(key.to_string());
    }

    fn evict(&mut self) {
        while self.order.len() > self.max_entries {
            if let Some(oldest) = self.order.pop_front() {
                let _ = self.entries.remove(&oldest);
            }
        }
    }
}

fn load_texture(ctx: &egui::Context, key: &str, data: &[u8]) -> Option<TextureHandle> {
    let image = image::load_from_memory(data).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, &image);
    Some(ctx.load_texture(
        format!("cover_{}", key),
        color_image,
        TextureOptions::LINEAR,
    ))
}
