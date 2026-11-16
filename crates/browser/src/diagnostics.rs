//! A tab's console and network history, bounded so a noisy page cannot hold memory without limit.
// Only macOS has the WebKit pages these serve.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::{HashSet, VecDeque};

use act_host::tools::types::{BrowserConsoleEntry, BrowserNetworkEntry};

pub const MAX_DIAGNOSTIC_ENTRIES: usize = 200;
pub const MAX_DIAGNOSTIC_TEXT: usize = 4_000;
pub const NETWORK_IDLE_MS: i64 = 500;

/// An entry and the origin of the page that produced it, which decides who may read it.
#[derive(Clone, Debug)]
pub struct Recorded<T> {
    pub page_origin: String,
    pub entry: T,
}

#[derive(Default)]
pub struct Diagnostics {
    pub console: VecDeque<Recorded<BrowserConsoleEntry>>,
    pub network: VecDeque<Recorded<BrowserNetworkEntry>>,
    pub console_sequence: u64,
    pub network_sequence: u64,
    /// Requests started and not yet finished. A live socket is never one, since it may stay open for hours.
    pub pending: HashSet<String>,
    pub last_network_at: i64,
}

fn bounded_push<T>(entries: &mut VecDeque<T>, entry: T) {
    entries.push_back(entry);
    while entries.len() > MAX_DIAGNOSTIC_ENTRIES {
        entries.pop_front();
    }
}

pub fn clip(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((at, _)) => text[..at].to_string(),
        None => text.to_string(),
    }
}

pub fn console_rank(level: &str) -> u8 {
    match level {
        "info" => 1,
        "warning" => 2,
        "error" => 3,
        _ => 0,
    }
}

impl Diagnostics {
    pub fn new(now: i64) -> Self {
        Self { last_network_at: now, ..Default::default() }
    }

    pub fn log(&mut self, page_origin: String, at: i64, level: &str, message: &str, source: Option<&str>, line: Option<u64>) {
        self.console_sequence += 1;
        let entry = BrowserConsoleEntry {
            sequence: self.console_sequence,
            at,
            level: level.to_string(),
            message: clip(message, MAX_DIAGNOSTIC_TEXT),
            source: source.filter(|source| !source.is_empty()).map(|source| clip(source, MAX_DIAGNOSTIC_TEXT)),
            line: line.filter(|line| *line > 0),
        };
        bounded_push(&mut self.console, Recorded { page_origin, entry });
    }

    pub fn start_request(&mut self, id: String, now: i64) {
        self.pending.insert(id);
        self.last_network_at = now;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_request(
        &mut self,
        id: Option<&str>,
        page_origin: String,
        now: i64,
        started_at: i64,
        method: &str,
        url: &str,
        resource_type: &str,
        duration_ms: u64,
        status: Option<u16>,
        error: Option<&str>,
        from_cache: Option<bool>,
    ) {
        if let Some(id) = id {
            self.pending.remove(id);
        }
        self.last_network_at = now;
        self.network_sequence += 1;
        let entry = BrowserNetworkEntry {
            sequence: self.network_sequence,
            started_at,
            method: method.to_string(),
            url: url.to_string(),
            resource_type: resource_type.to_string(),
            duration_ms,
            status,
            error: error.filter(|error| !error.is_empty()).map(|error| clip(error, MAX_DIAGNOSTIC_TEXT)),
            from_cache,
        };
        bounded_push(&mut self.network, Recorded { page_origin, entry });
    }

    /// Requests a document left unfinished cannot finish once it is gone.
    pub fn drop_pending(&mut self) {
        self.pending.clear();
    }

    pub fn idle(&self, now: i64) -> bool {
        self.pending.is_empty() && now - self.last_network_at >= NETWORK_IDLE_MS
    }
}

/// The newest `limit` entries past `since` that `keep` accepts, and how many older ones matched too.
pub fn newest<T: Clone>(entries: impl Iterator<Item = (u64, T)>, since: u64, limit: usize, keep: impl Fn(&T) -> bool) -> (Vec<T>, u64) {
    let matching: Vec<T> = entries.filter(|(sequence, entry)| *sequence > since && keep(entry)).map(|(_, entry)| entry).collect();
    let skip = matching.len().saturating_sub(limit);
    let omitted = skip as u64;
    (matching.into_iter().skip(skip).collect(), omitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_keeps_the_latest_matches_and_counts_the_rest() {
        let entries = (1..=10u64).map(|sequence| (sequence, sequence));
        let (kept, omitted) = newest(entries, 2, 3, |value| value % 2 == 0);
        assert_eq!(kept, vec![6, 8, 10]);
        assert_eq!(omitted, 1);
    }

    #[test]
    fn history_is_bounded() {
        let mut diagnostics = Diagnostics::new(0);
        for index in 0..(MAX_DIAGNOSTIC_ENTRIES + 5) {
            diagnostics.log("https://a.com".into(), index as i64, "info", "x", None, None);
        }
        assert_eq!(diagnostics.console.len(), MAX_DIAGNOSTIC_ENTRIES);
        assert_eq!(diagnostics.console.front().unwrap().entry.sequence, 6);
    }
}
