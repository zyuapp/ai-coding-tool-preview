use std::collections::HashMap;

/// Running totals by key, each read off a report of everything its key has spent so far. A report
/// adds only what its total grew by since the key was last seen, so a repeat or a stale report adds
/// nothing. A key seen for the first time starts from `baseline`.
#[derive(Debug, Default, Clone)]
pub struct RunningTotals {
    totals: HashMap<String, i64>,
}

impl RunningTotals {
    /// What `key`'s total grew by, now taken as seen.
    pub fn grow(&mut self, key: &str, total: i64, baseline: i64) -> u64 {
        if total < 0 {
            return 0;
        }
        let seen = self.totals.get(key).copied().unwrap_or(baseline);
        self.totals.insert(key.to_string(), seen.max(total));
        (total - seen).max(0) as u64
    }

    pub fn set(&mut self, key: &str, total: i64) {
        self.totals.insert(key.to_string(), total);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.totals.contains_key(key)
    }

    pub fn remove(&mut self, key: &str) {
        self.totals.remove(key);
    }

    pub fn keys(&self) -> Vec<String> {
        self.totals.keys().cloned().collect()
    }

    pub fn clear(&mut self) {
        self.totals.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_growth_once() {
        let mut totals = RunningTotals::default();
        assert_eq!(totals.grow("a", 10, 0), 10);
        assert_eq!(totals.grow("a", 10, 0), 0);
        assert_eq!(totals.grow("a", 8, 0), 0);
        assert_eq!(totals.grow("a", 15, 0), 5);
        assert_eq!(totals.grow("b", 100, 90), 10);
    }
}
