//! The window's image cache. Images are decoded once and kept while they are drawn; once the cache
//! holds more than its budget, a full draw marks the images still on screen and the rest are freed
//! (GPUI's fallback cache keeps every image an `img` ever loaded).

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{hash, App, AppContext, Entity, ImageCache, ImageCacheError, ImageCacheItem, RenderImage, Resource, WeakEntity, Window};

/// How many decoded images, and how many bytes of them, are kept before unused ones are freed.
const MAX_IMAGES: usize = 48;
const MAX_BYTES: usize = 256 * 1024 * 1024;

struct Held {
    item: ImageCacheItem,
    bytes: usize,
    /// The sweep generation that last drew it.
    seen: u64,
}

pub struct ImageMemory {
    me: WeakEntity<ImageMemory>,
    images: HashMap<u64, Held>,
    generation: u64,
    sweeping: bool,
}

impl ImageMemory {
    pub fn new(cx: &mut App) -> Entity<ImageMemory> {
        let memory = cx.new(|cx| ImageMemory { me: cx.entity().downgrade(), images: HashMap::new(), generation: 0, sweeping: false });
        cx.observe_release(&memory, |memory, cx| {
            for (_, held) in memory.images.drain() {
                if let Some(Ok(image)) = held.item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        memory
    }

    fn over_budget(&self) -> bool {
        self.images.len() > MAX_IMAGES || self.images.values().map(|held| held.bytes).sum::<usize>() > MAX_BYTES
    }

    /// Draws the whole window once so every image on screen is marked, then frees the rest.
    fn sweep(&mut self, window: &mut Window) {
        self.generation += 1;
        let generation = self.generation;
        window.refresh();
        let memory = self.me.clone();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let _ = memory.update(cx, |memory, cx| {
                    memory.sweeping = false;
                    let stale: Vec<u64> = memory.images.iter().filter(|(_, held)| held.seen < generation && held.item.get().is_some()).map(|(key, _)| *key).collect();
                    for key in stale {
                        if let Some(held) = memory.images.remove(&key) {
                            if let Some(Ok(image)) = held.item.get() {
                                cx.drop_image(image, Some(window));
                            }
                        }
                    }
                });
            });
        });
    }
}

impl ImageCache for ImageMemory {
    fn load(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        let generation = self.generation;
        if let Some(held) = self.images.get_mut(&key) {
            held.seen = generation;
            let loaded = held.item.use_image(window);
            if let Some(Ok(image)) = &loaded {
                if held.bytes == 0 {
                    held.bytes = (0..image.frame_count()).map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len)).sum();
                }
            }
            return loaded;
        }
        let item = ImageCacheItem::new(resource, cx);
        let loaded = item.use_image(window);
        self.images.insert(key, Held { item, bytes: 0, seen: generation });
        if !self.sweeping && self.over_budget() {
            self.sweeping = true;
            let memory = self.me.clone();
            window.defer(cx, move |window, cx| {
                let _ = memory.update(cx, |memory, _| memory.sweep(window));
            });
        }
        loaded
    }
}
