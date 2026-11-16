//! Whether a checkout holds a commit, asked once per hash (known-commits.ts). A reply can name a
//! commit in a repository other than its thread's, and only one the checkout holds has a diff to open.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use act_markdown::MarkdownView;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{App, WeakEntity};

/// A miss is asked again after a while, since a fetch can bring the commit in.
const MISS_TTL: Duration = Duration::from_secs(60);
/// Enough for every hash a long session shows, with the oldest forgotten first.
const MAX_REMEMBERED: usize = 2_000;
const MAX_COMMIT_CHECKS: usize = 64;

/// A checkout's workspace id and a lowercase hash.
type Key = (String, String);

#[derive(Default)]
struct Cache {
    held: HashMap<Key, (bool, Instant)>,
    order: VecDeque<Key>,
    /// Hashes to read on the next turn, by checkout.
    queued: HashMap<String, Vec<String>>,
    asking: HashSet<Key>,
    waiting: HashMap<Key, Vec<WeakEntity<MarkdownView>>>,
    wake: Option<UnboundedSender<()>>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

impl Cache {
    fn remembered(&mut self, key: &Key) -> Option<bool> {
        let (answer, at) = *self.held.get(key)?;
        if !answer && at.elapsed() > MISS_TTL {
            self.held.remove(key);
            return None;
        }
        Some(answer)
    }

    fn remember(&mut self, key: Key, answer: bool) {
        if self.held.insert(key.clone(), (answer, Instant::now())).is_none() {
            self.order.push_back(key);
        }
        while self.held.len() > MAX_REMEMBERED {
            let Some(oldest) = self.order.pop_front() else { break };
            self.held.remove(&oldest);
        }
    }
}

/// Starts the reader that answers queued hashes, once per app.
pub fn install(cx: &mut App) {
    if CACHE.with(|cache| cache.borrow().wake.is_some()) {
        return;
    }
    let (wake, mut woken) = unbounded::<()>();
    CACHE.with(|cache| cache.borrow_mut().wake = Some(wake));
    cx.spawn(async move |cx| {
        while woken.next().await.is_some() {
            // Hashes asked about in the same moment are read together.
            while woken.try_recv().is_ok() {}
            let batches = CACHE.with(|cache| std::mem::take(&mut cache.borrow_mut().queued));
            for (root, hashes) in batches {
                for chunk in hashes.chunks(MAX_COMMIT_CHECKS) {
                    // The computer holding the checkout answers, this one or a paired one.
                    let Ok(read) = cx.update(|cx| crate::effects::window_desktop::commits(root.clone(), chunk.to_vec(), cx)) else { return };
                    let found = read.await;
                    let mut changed: Vec<WeakEntity<MarkdownView>> = Vec::new();
                    CACHE.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        for hash in chunk {
                            let key = (root.clone(), hash.clone());
                            cache.asking.remove(&key);
                            let views = cache.waiting.remove(&key).unwrap_or_default();
                            // A checkout that cannot answer is asked again next time rather than remembered as lacking them.
                            if let Some(found) = &found {
                                let held = found.contains(hash);
                                cache.remember(key, held);
                                if held {
                                    changed.extend(views);
                                }
                            }
                        }
                    });
                    if !changed.is_empty() {
                        let _ = cx.update(|cx| {
                            for view in changed {
                                let _ = view.update(cx, |_, cx| cx.notify());
                            }
                        });
                    }
                }
            }
        }
    })
    .detach();
}

/// True once the checkout `root` names is known to hold the commit; false while asking and when it does not.
pub fn known(root: &str, hash: &str, view: &WeakEntity<MarkdownView>) -> bool {
    let key = (root.to_string(), hash.to_lowercase());
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(answer) = cache.remembered(&key) {
            return answer;
        }
        let waiting = cache.waiting.entry(key.clone()).or_default();
        if !waiting.iter().any(|waiter| waiter == view) {
            waiting.push(view.clone());
        }
        if cache.asking.insert(key.clone()) {
            cache.queued.entry(key.0).or_default().push(key.1);
            if let Some(wake) = &cache.wake {
                let _ = wake.unbounded_send(());
            }
        }
        false
    })
}
