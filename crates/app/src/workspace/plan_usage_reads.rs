//! When the composer asks how much of the plan is left (plan-usage-reads.ts): once the store has
//! loaded, as the run on screen ends, and on a slow poll, each checked as the workspace changes.

use std::time::{Duration, Instant};

use act_core::state::WorkspaceState;

const PLAN_USAGE_POLL: Duration = Duration::from_secs(15 * 60);
/// A run that just ended changed the numbers, but each read starts a session with every provider.
const RUN_END_GAP: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct PlanUsageReads {
    asked: Option<Instant>,
    was_running: bool,
}

impl PlanUsageReads {
    /// Whether to read the plan limits now, noting it if so.
    pub fn due(&mut self, state: &WorkspaceState) -> bool {
        let running = state.current_id.as_ref().is_some_and(|id| state.active_runs.contains_key(id));
        let due = match self.asked {
            None => true,
            Some(at) => at.elapsed() >= PLAN_USAGE_POLL || (self.was_running && !running && at.elapsed() >= RUN_END_GAP),
        };
        self.was_running = running;
        if due && state.restored {
            self.asked = Some(Instant::now());
            return true;
        }
        false
    }
}
