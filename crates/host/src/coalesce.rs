//! How effects that arrive faster than their work are folded together.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// One run per key at a time. A request that arrives while one runs replaces the single follow-up
/// still needed, the way the environment poll runs one Git scan per checkout.
pub struct Coalesced<K, T> {
    running: Mutex<HashMap<K, Option<T>>>,
}

impl<K: Eq + Hash + Clone, T> Default for Coalesced<K, T> {
    fn default() -> Self {
        Self { running: Mutex::new(HashMap::new()) }
    }
}

impl<K: Eq + Hash + Clone, T> Coalesced<K, T> {
    /// True when the caller should run `request` now; false when it was kept as the follow-up instead.
    pub fn begin(&self, key: &K, request: T) -> bool {
        let mut running = self.running.lock().unwrap();
        match running.get_mut(key) {
            Some(follow_up) => {
                *follow_up = Some(request);
                false
            }
            None => {
                running.insert(key.clone(), None);
                true
            }
        }
    }

    /// The follow-up to run next, or None, which ends the key's run.
    pub fn next(&self, key: &K) -> Option<T> {
        let mut running = self.running.lock().unwrap();
        match running.get_mut(key).and_then(Option::take) {
            Some(request) => Some(request),
            None => {
                running.remove(key);
                None
            }
        }
    }
}

/// Only the newest of a burst goes ahead: each caller takes a ticket, waits, and checks it still holds the newest.
#[derive(Default)]
pub struct Latest {
    newest: AtomicU64,
}

impl Latest {
    pub fn ticket(&self) -> u64 {
        self.newest.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_newest(&self, ticket: u64) -> bool {
        self.newest.load(Ordering::SeqCst) == ticket
    }
}

/// How long the project picker waits for typing to pause before it lists folders.
pub const DIRECTORY_LIST_DELAY: std::time::Duration = std::time::Duration::from_millis(180);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_during_a_run_replaces_the_follow_up() {
        let scans: Coalesced<&str, u32> = Coalesced::default();
        assert!(scans.begin(&"w", 1));
        assert!(!scans.begin(&"w", 2));
        assert!(!scans.begin(&"w", 3));
        assert_eq!(scans.next(&"w"), Some(3));
        assert_eq!(scans.next(&"w"), None);
        assert!(scans.begin(&"w", 4));
    }

    #[test]
    fn only_the_newest_ticket_holds() {
        let latest = Latest::default();
        let first = latest.ticket();
        let second = latest.ticket();
        assert!(!latest.is_newest(first) && latest.is_newest(second));
    }
}
