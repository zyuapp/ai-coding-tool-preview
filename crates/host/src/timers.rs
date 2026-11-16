//! Timers the reducer schedules by wall-clock time: a toast's dismissal, a snooze's end, a usage
//! limit's reset. Each follows the wall clock, so a machine that slept fires on waking.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Sleeps until the wall clock reaches `at`, re-reading it at least every `step`.
async fn until(at: i64, step: Duration) -> i64 {
    loop {
        let now = now_ms();
        if now >= at {
            return now;
        }
        let remaining = Duration::from_millis((at - now) as u64);
        tokio::time::sleep(remaining.min(step)).await;
    }
}

/// One timer, rescheduled or stopped as a whole. `elapsed` hears the time it fired at.
pub struct WallTimer {
    elapsed: Arc<dyn Fn(i64) + Send + Sync>,
    current: Mutex<Option<JoinHandle<()>>>,
}

impl WallTimer {
    pub fn new(elapsed: impl Fn(i64) + Send + Sync + 'static) -> Self {
        Self { elapsed: Arc::new(elapsed), current: Mutex::new(None) }
    }

    /// Replaces whatever was scheduled; None stops it. Must be called within a Tokio runtime.
    pub fn schedule(&self, at: Option<i64>) {
        let mut current = self.current.lock().unwrap();
        if let Some(handle) = current.take() {
            handle.abort();
        }
        let Some(at) = at else { return };
        let elapsed = self.elapsed.clone();
        *current = Some(tokio::spawn(async move {
            let now = until(at, Duration::from_secs(60)).await;
            elapsed(now);
        }));
    }
}

impl Drop for WallTimer {
    fn drop(&mut self) {
        self.schedule(None);
    }
}

/// One timer per key. Scheduling a key again restarts its time, None stops it.
pub struct KeyedTimers<K: Eq + Hash + Clone + Send + 'static> {
    elapsed: Arc<dyn Fn(K) + Send + Sync>,
    timers: Arc<Mutex<HashMap<K, JoinHandle<()>>>>,
}

impl<K: Eq + Hash + Clone + Send + Sync + 'static> KeyedTimers<K> {
    pub fn new(elapsed: impl Fn(K) + Send + Sync + 'static) -> Self {
        Self { elapsed: Arc::new(elapsed), timers: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Must be called within a Tokio runtime.
    pub fn schedule(&self, key: K, at: Option<i64>) {
        let mut timers = self.timers.lock().unwrap();
        if let Some(handle) = timers.remove(&key) {
            handle.abort();
        }
        let Some(at) = at else { return };
        let elapsed = self.elapsed.clone();
        let registry = Arc::downgrade(&self.timers);
        let fired = key.clone();
        let handle = tokio::spawn(async move {
            until(at, Duration::from_secs(60)).await;
            if let Some(registry) = registry.upgrade() {
                registry.lock().unwrap().remove(&fired);
            }
            elapsed(fired);
        });
        timers.insert(key, handle);
    }

    pub fn dispose(&self) {
        for (_, handle) in self.timers.lock().unwrap().drain() {
            handle.abort();
        }
    }
}

impl<K: Eq + Hash + Clone + Send + 'static> Drop for KeyedTimers<K> {
    fn drop(&mut self) {
        for (_, handle) in self.timers.lock().unwrap().drain() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[tokio::test]
    async fn rescheduling_a_key_restarts_it_and_none_stops_it() {
        let (sender, received) = mpsc::channel();
        let timers = KeyedTimers::new(move |id: u32| sender.send(id).unwrap());
        timers.schedule(1, Some(now_ms() + 10_000));
        timers.schedule(1, Some(now_ms() + 20));
        timers.schedule(2, Some(now_ms() + 20));
        timers.schedule(2, None);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(received.try_iter().collect::<Vec<_>>(), vec![1]);
    }
}
