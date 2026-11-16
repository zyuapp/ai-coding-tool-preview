use std::collections::HashSet;
use std::sync::Arc;

use crate::types::{BackgroundProcess, BackgroundReport};

/// The work an agent leaves running under a session, which outlives the turn that started it.
/// Engines report it as a level, so the whole set is swapped for each report.
pub struct BackgroundWork {
    ids: HashSet<String>,
    report: Arc<dyn Fn(BackgroundReport) + Send + Sync>,
}

impl Default for BackgroundWork {
    fn default() -> Self {
        BackgroundWork {
            ids: HashSet::new(),
            report: Arc::new(|_| {}),
        }
    }
}

impl BackgroundWork {
    /// Whether work that closing the session would cut short is still running.
    pub fn running(&self) -> bool {
        !self.ids.is_empty()
    }

    /// A fresh process has nothing running under it, so the thread's set starts empty.
    pub fn open_with(&mut self, report: Arc<dyn Fn(BackgroundReport) + Send + Sync>) {
        self.report = report;
        self.clear();
    }

    /// Swaps the whole set. `ids` names the work that keeps the session busy, which can be wider
    /// than what is reported. Returns whether work was running before, so the caller can tell the
    /// pool the session rested once nothing else is going.
    pub fn replace(&mut self, processes: Vec<BackgroundProcess>, ids: Vec<String>) -> bool {
        let was_running = self.running();
        self.ids = ids.into_iter().collect();
        (self.report)(BackgroundReport::Changed { processes });
        was_running
    }

    /// The session ending is the end of the work it holds.
    pub fn clear(&mut self) {
        self.ids.clear();
        (self.report)(BackgroundReport::Changed {
            processes: Vec::new(),
        });
    }
}
