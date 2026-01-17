use egui::{ColorImage, TextureHandle, TextureId, TextureOptions, Vec2};
use playbyte_feed::LocalByteStore;
use playbyte_types::ByteMetadata;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub struct Thumbnail {
    pub id: TextureId,
    pub size: Vec2,
}

struct ThumbnailEntry {
    handle: TextureHandle,
    last_used: Instant,
}

pub struct ThumbnailCache {
    entries: HashMap<String, ThumbnailEntry>,
    order: VecDeque<String>,
    failures: HashMap<String, Instant>,
    max_entries: usize,
}

impl ThumbnailCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            failures: HashMap::new(),
            max_entries,
        }
    }

    pub fn get(
        &mut self,
        ctx: &egui::Context,
        store: &LocalByteStore,
        byte: &ByteMetadata,
    ) -> Option<Thumbnail> {
        let now = Instant::now();
        if let Some((id, size)) = self.entries.get_mut(&byte.byte_id).map(|entry| {
            entry.last_used = now;
            (entry.handle.id(), entry.handle.size())
        }) {
            self.bump(&byte.byte_id);
            return Some(Thumbnail {
                id,
                size: Vec2::new(size[0] as f32, size[1] as f32),
            });
        }
        if let Some(last_fail) = self.failures.get(&byte.byte_id) {
            if now.saturating_duration_since(*last_fail) < Duration::from_secs(4) {
                return None;
            }
        }
        let handle = match load_thumbnail(ctx, store, byte) {
            Some(handle) => handle,
            None => {
                self.failures.insert(byte.byte_id.clone(), now);
                return None;
            }
        };
        self.failures.remove(&byte.byte_id);
        let size = handle.size();
        self.entries.insert(
            byte.byte_id.clone(),
            ThumbnailEntry {
                handle,
                last_used: now,
            },
        );
        self.bump(&byte.byte_id);
        self.evict();
        let entry = self.entries.get(&byte.byte_id)?;
        Some(Thumbnail {
            id: entry.handle.id(),
            size: Vec2::new(size[0] as f32, size[1] as f32),
        })
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

fn load_thumbnail(
    ctx: &egui::Context,
    store: &LocalByteStore,
    byte: &ByteMetadata,
) -> Option<TextureHandle> {
    let data = store.load_thumbnail(&byte.byte_id).ok()?;
    let image = image::load_from_memory(&data).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, &image);
    Some(ctx.load_texture(
        format!("thumb_{}", byte.byte_id),
        color_image,
        TextureOptions::LINEAR,
    ))
}
