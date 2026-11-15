use gpui_kit::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImageCacheStats {
    pub entries: usize,
    pub loading: usize,
    pub decoded_bytes: usize,
    pub frames: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub first_draws: u64,
    pub first_draw_ms_total: f64,
    pub first_draw_ms_max: f64,
}

fn stats() -> &'static Mutex<ImageCacheStats> {
    static STATS: OnceLock<Mutex<ImageCacheStats>> = OnceLock::new();
    STATS.get_or_init(|| Mutex::new(ImageCacheStats::default()))
}

pub fn image_cache_stats() -> ImageCacheStats {
    *stats().lock().unwrap_or_else(|p| p.into_inner())
}

struct Entry {
    item: ImageCacheItem,
    started: Instant,
    last_used: Instant,
    decoded_bytes: Option<usize>,
    frames: usize,
}

impl Entry {
    fn new(resource: &Resource, cx: &mut App) -> Self {
        stats().lock().unwrap_or_else(|p| p.into_inner()).misses += 1;
        let now = Instant::now();
        Self {
            item: ImageCacheItem::new(resource, cx),
            started: now,
            last_used: now,
            decoded_bytes: None,
            frames: 0,
        }
    }
}

#[derive(Default)]
pub struct DecodedImageCache {
    entries: HashMap<u64, Entry>,
}

impl DecodedImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self::default());
        cx.observe_release(&entity, |cache, cx| {
            for (_, entry) in std::mem::take(&mut cache.entries) {
                cache.forget(entry, cx);
            }
        })
        .detach();
        entity
    }

    fn forget(&self, entry: Entry, cx: &mut App) {
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        match entry.decoded_bytes {
            Some(bytes) => {
                stats.entries = stats.entries.saturating_sub(1);
                stats.decoded_bytes = stats.decoded_bytes.saturating_sub(bytes);
                stats.frames = stats.frames.saturating_sub(entry.frames);
                drop(stats);
                if let Some(Ok(image)) = entry.item.get() {
                    cx.drop_image(image, None);
                }
            }
            None => stats.loading = stats.loading.saturating_sub(1),
        }
    }
}

impl ImageCache for DecodedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        let now = Instant::now();
        let entry = self.entries.entry(key).or_insert_with(|| {
            stats().lock().unwrap_or_else(|p| p.into_inner()).loading += 1;
            Entry::new(resource, cx)
        });
        entry.last_used = now;
        let was_ready = entry.decoded_bytes.is_some();
        let result = entry.item.use_image(window);
        if let Some(Ok(image)) = &result {
            let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
            if was_ready {
                stats.hits += 1;
            } else {
                let bytes = decoded_bytes(image);
                let elapsed = entry.started.elapsed().as_secs_f64() * 1000.;
                entry.decoded_bytes = Some(bytes);
                entry.frames = image.frame_count();
                stats.loading = stats.loading.saturating_sub(1);
                stats.entries += 1;
                stats.decoded_bytes += bytes;
                stats.frames += entry.frames;
                stats.first_draws += 1;
                stats.first_draw_ms_total += elapsed;
                stats.first_draw_ms_max = stats.first_draw_ms_max.max(elapsed);
            }
        }
        result
    }
}

fn decoded_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count()).filter_map(|frame| image.as_bytes(frame)).map(<[u8]>::len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn decoded_bytes_counts_every_animation_frame() {
        let first = image::Frame::new(image::RgbaImage::from_pixel(3, 5, image::Rgba([0; 4])));
        let second = image::Frame::new(image::RgbaImage::from_pixel(7, 2, image::Rgba([0; 4])));
        let image = RenderImage::new(vec![first, second]);
        assert_eq!(decoded_bytes(&image), (3 * 5 + 7 * 2) * 4);
    }
}
