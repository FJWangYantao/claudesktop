use gpui_kit::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const DECODED_BUDGET: usize = 64 * 1024 * 1024;
const VISIBLE_FOR: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImageCacheStats {
    pub entries: usize,
    pub loading: usize,
    pub decoded_bytes: usize,
    pub unpinned_bytes: usize,
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

#[derive(Clone, Copy)]
struct Policy {
    last_used: Instant,
    decoded_bytes: Option<usize>,
}

fn unpinned(policy: &Policy, now: Instant) -> bool {
    policy.decoded_bytes.is_some() && now.saturating_duration_since(policy.last_used) >= VISIBLE_FOR
}

fn unpinned_bytes<'a>(entries: impl Iterator<Item = &'a Policy>, now: Instant) -> usize {
    entries.filter(|entry| unpinned(entry, now)).filter_map(|entry| entry.decoded_bytes).sum()
}

fn oldest_unpinned<'a>(
    entries: impl Iterator<Item = (u64, &'a Policy)>,
    now: Instant,
) -> Option<u64> {
    entries
        .filter(|(_, entry)| unpinned(entry, now))
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(key, _)| key)
}

struct Entry {
    item: ImageCacheItem,
    started: Instant,
    policy: Policy,
    frames: usize,
    drawn: bool,
    loading: bool,
}

impl Entry {
    fn new(resource: &Resource, cx: &mut App) -> Self {
        let now = Instant::now();
        Self {
            item: ImageCacheItem::new(resource, cx),
            started: now,
            policy: Policy { last_used: now, decoded_bytes: None },
            frames: 0,
            drawn: false,
            loading: true,
        }
    }
}

#[derive(Default)]
pub struct DecodedImageCache {
    entries: HashMap<u64, Entry>,
    accounted_unpinned: usize,
}

impl DecodedImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self::default());
        cx.observe_release(&entity, |cache, cx| {
            for (_, entry) in std::mem::take(&mut cache.entries) {
                cache.forget(entry, None, cx);
            }
            cache.sync_unpinned(Instant::now());
        })
        .detach();
        entity
    }

    pub fn reclaim(&mut self, cx: &mut App) {
        for entry in self.entries.values_mut() {
            if !entry.loading {
                continue;
            }
            match entry.item.get() {
                Some(Ok(image)) => Self::account_ready(entry, &image),
                Some(Err(_)) => Self::account_error(entry),
                None => {}
            }
        }
        self.trim(None, cx);
    }

    fn account_ready(entry: &mut Entry, image: &Arc<RenderImage>) {
        let bytes = decoded_bytes(image);
        entry.policy.decoded_bytes = Some(bytes);
        entry.frames = image.frame_count();
        entry.loading = false;
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        stats.loading = stats.loading.saturating_sub(1);
        stats.entries += 1;
        stats.decoded_bytes += bytes;
        stats.frames += entry.frames;
    }

    fn account_error(entry: &mut Entry) {
        entry.loading = false;
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        stats.loading = stats.loading.saturating_sub(1);
    }

    fn account_first_draw(entry: &mut Entry) {
        if entry.drawn {
            return;
        }
        entry.drawn = true;
        let elapsed = entry.started.elapsed().as_secs_f64() * 1000.;
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        stats.first_draws += 1;
        stats.first_draw_ms_total += elapsed;
        stats.first_draw_ms_max = stats.first_draw_ms_max.max(elapsed);
    }

    fn sync_unpinned(&mut self, now: Instant) {
        let current = unpinned_bytes(self.entries.values().map(|entry| &entry.policy), now);
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        stats.unpinned_bytes = if current >= self.accounted_unpinned {
            stats.unpinned_bytes.saturating_add(current - self.accounted_unpinned)
        } else {
            stats.unpinned_bytes.saturating_sub(self.accounted_unpinned - current)
        };
        self.accounted_unpinned = current;
    }

    fn trim(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        let now = Instant::now();
        for entry in self.entries.values_mut() {
            if entry.policy.decoded_bytes.is_some()
                && let Some(Ok(image)) = entry.item.get()
                && externally_held(&image)
            {
                entry.policy.last_used = now;
            }
        }
        self.sync_unpinned(now);
        if std::env::var_os("CLAUDESKTOP_DISABLE_IMAGE_RECLAIM").is_some() {
            return;
        }
        while self.accounted_unpinned > DECODED_BUDGET {
            let Some(key) =
                oldest_unpinned(self.entries.iter().map(|(key, entry)| (*key, &entry.policy)), now)
            else {
                break;
            };
            let Some(entry) = self.entries.remove(&key) else { break };
            stats().lock().unwrap_or_else(|p| p.into_inner()).evictions += 1;
            self.forget(entry, window.as_deref_mut(), cx);
            self.sync_unpinned(now);
        }
    }

    fn forget(&self, entry: Entry, window: Option<&mut Window>, cx: &mut App) {
        let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
        if entry.loading {
            stats.loading = stats.loading.saturating_sub(1);
        }
        if let Some(bytes) = entry.policy.decoded_bytes {
            stats.entries = stats.entries.saturating_sub(1);
            stats.decoded_bytes = stats.decoded_bytes.saturating_sub(bytes);
            stats.frames = stats.frames.saturating_sub(entry.frames);
        }
        drop(stats);
        if let Some(Ok(image)) = entry.item.get() {
            cx.drop_image(image, window);
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
        let mut ready = false;
        let result = {
            let entry = self.entries.entry(key).or_insert_with(|| {
                let mut stats = stats().lock().unwrap_or_else(|p| p.into_inner());
                stats.loading += 1;
                stats.misses += 1;
                Entry::new(resource, cx)
            });
            entry.policy.last_used = now;
            let was_ready = entry.policy.decoded_bytes.is_some();
            let result = entry.item.use_image(window);
            match &result {
                Some(Ok(image)) => {
                    if was_ready {
                        stats().lock().unwrap_or_else(|p| p.into_inner()).hits += 1;
                    } else {
                        Self::account_ready(entry, image);
                        ready = true;
                    }
                    Self::account_first_draw(entry);
                }
                Some(Err(_)) if entry.loading => Self::account_error(entry),
                _ => {}
            }
            result
        };
        if ready {
            self.trim(Some(window), cx);
        }
        result
    }
}

fn decoded_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count()).filter_map(|frame| image.as_bytes(frame)).map(<[u8]>::len).sum()
}

fn externally_held(image: &Arc<RenderImage>) -> bool {
    // `get()` 临时 clone 与缓存条目各占一个引用；此外的引用是仍可重用的渲染状态。
    Arc::strong_count(image) > 2
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

    #[core::prelude::v1::test]
    fn rendered_references_pin_an_image_even_without_new_load_calls() {
        let cache_entry = Arc::new(RenderImage::new(Vec::<image::Frame>::new()));
        let temporary_get = cache_entry.clone();
        assert!(!externally_held(&temporary_get));
        let rendered = cache_entry.clone();
        assert!(externally_held(&temporary_get));
        drop(rendered);
        assert!(!externally_held(&temporary_get));
    }

    #[core::prelude::v1::test]
    fn lru_skips_visible_and_loading_entries() {
        let now = Instant::now();
        let old = now - VISIBLE_FOR - Duration::from_millis(1);
        let visible = Policy { last_used: now, decoded_bytes: Some(50) };
        let loading = Policy { last_used: old, decoded_bytes: None };
        let oldest = Policy { last_used: old, decoded_bytes: Some(40) };
        let newer = Policy { last_used: old + Duration::from_millis(1), decoded_bytes: Some(30) };
        let entries = [(1, visible), (2, loading), (3, oldest), (4, newer)];
        assert_eq!(unpinned_bytes(entries.iter().map(|(_, policy)| policy), now), 70);
        assert_eq!(
            oldest_unpinned(entries.iter().map(|(key, policy)| (*key, policy)), now),
            Some(3)
        );
        assert_eq!(oldest_unpinned([(1, &visible), (2, &loading)].into_iter(), now), None);
    }
}
