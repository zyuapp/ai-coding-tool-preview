use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::provider::ProviderEvent;
use crate::running_totals::RunningTotals;

/// What a session's Codex threads spend, the main one and each child, read off the running total
/// every usage report carries. A thread first seen already holds what it spent before, so only its
/// latest response counts; after that each report adds what the total grew by. A compaction's
/// recount carries no input or output and counts for nothing.
#[derive(Clone)]
pub struct CodexSpend {
    totals: Arc<Mutex<RunningTotals>>,
    /// Reaches the run in flight, and drops what arrives while there is none.
    emit: Arc<dyn Fn(ProviderEvent) + Send + Sync>,
}

impl CodexSpend {
    pub fn new(emit: Arc<dyn Fn(ProviderEvent) + Send + Sync>) -> Self {
        CodexSpend {
            totals: Arc::default(),
            emit,
        }
    }

    pub fn report(&self, thread_id: &str, usage: &Value) {
        let total = usage["total"]["totalTokens"].as_i64().unwrap_or(0);
        let last = &usage["last"];
        let latest = if last["inputTokens"].as_i64().unwrap_or(0)
            + last["outputTokens"].as_i64().unwrap_or(0)
            > 0
        {
            last["totalTokens"].as_i64().unwrap_or(0).max(0)
        } else {
            0
        };
        let grown = self
            .totals
            .lock()
            .unwrap()
            .grow(thread_id, total, (total - latest).max(0));
        if grown > 0 {
            (self.emit)(ProviderEvent::Spend { tokens: grown });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_resumed_thread_counts_only_its_latest_response_first() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let spend = CodexSpend::new(Arc::new(move |event| sink.lock().unwrap().push(event)));
        spend.report("t", &json!({ "total": { "totalTokens": 1000 }, "last": { "inputTokens": 80, "outputTokens": 20, "totalTokens": 100 } }));
        spend.report("t", &json!({ "total": { "totalTokens": 1150 }, "last": { "inputTokens": 100, "outputTokens": 50, "totalTokens": 150 } }));
        spend.report("t", &json!({ "total": { "totalTokens": 1150 }, "last": { "inputTokens": 0, "outputTokens": 0, "totalTokens": 900 } }));
        assert_eq!(
            *seen.lock().unwrap(),
            [
                ProviderEvent::Spend { tokens: 100 },
                ProviderEvent::Spend { tokens: 150 }
            ]
        );
    }
}
