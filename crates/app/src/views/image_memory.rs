//! The window's image cache. Images are decoded once and kept while they are drawn; once the cache
//! holds more than its budget, a full draw marks the images still on screen and the rest are freed
//! (GPUI's fallback cache keeps every image an `img` ever loaded).

use std::collections::HashMap;
use std::sync::Arc;

use futures::FutureExt;
use gpui::{hash, App, AppContext, Asset, AssetLogger, Entity, ImageAssetLoader, ImageCache, ImageCacheError, ImageCacheItem, RenderImage, Resource, WeakEntity, Window};

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
            for (_, mut held) in memory.images.drain() {
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
                    let stale: Vec<u64> = memory.images.iter().filter(|(_, held)| held.seen < generation && !matches!(held.item, ImageCacheItem::Loading(_))).map(|(key, _)| *key).collect();
                    for key in stale {
                        if let Some(mut held) = memory.images.remove(&key) {
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
            let loaded = held.item.get();
            if let Some(Ok(image)) = &loaded {
                if held.bytes == 0 {
                    held.bytes = (0..image.frame_count()).map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len)).sum();
                }
            }
            return loaded;
        }
        let loading = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let task = cx.background_executor().spawn(loading).shared();
        self.images.insert(key, Held { item: ImageCacheItem::Loading(task.clone()), bytes: 0, seen: generation });
        let view = window.current_view();
        window
            .spawn(cx, async move |cx| {
                let _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(view));
            })
            .detach();
        if !self.sweeping && self.over_budget() {
            self.sweeping = true;
            let memory = self.me.clone();
            window.defer(cx, move |window, cx| {
                let _ = memory.update(cx, |memory, _| memory.sweep(window));
            });
        }
        None
    }
}
