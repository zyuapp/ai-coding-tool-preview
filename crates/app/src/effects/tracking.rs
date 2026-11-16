//! An input's completion: it waits for the input's own effects and the replies they send, but not
//! for follow-up effects (src/application/workspace-execution.ts).

use std::sync::{Arc, Mutex};

use act_core::CommandResult;
use futures::channel::oneshot;
use futures::FutureExt;

const CLOSED: &str = "The workspace runtime is closed.";

struct Inner {
    accepted: Mutex<Option<CommandResult>>,
    results: Mutex<Vec<CommandResult>>,
    done: Mutex<Option<oneshot::Sender<CommandResult>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(done) = self.done.lock().unwrap().take() {
            let result = match self.accepted.lock().unwrap().take() {
                Some(accepted) => combine(accepted, std::mem::take(&mut *self.results.lock().unwrap())),
                None => CommandResult::failed(CLOSED),
            };
            let _ = done.send(result);
        }
    }
}

/// Held by every effect still working for one input, and by every reply on its way back. The input
/// completes when the last holder lets go.
#[derive(Clone)]
pub struct Tracker(Arc<Inner>);

impl Tracker {
    /// A tracker and the input's completion. An input the window never took completes as refused.
    pub fn new() -> (Tracker, futures::future::BoxFuture<'static, CommandResult>) {
        let (done, finished) = oneshot::channel();
        let tracker = Tracker(Arc::new(Inner { accepted: Mutex::new(None), results: Mutex::new(Vec::new()), done: Mutex::new(Some(done)) }));
        let completed = finished.map(|result| result.unwrap_or_else(|_| CommandResult::failed(CLOSED))).boxed();
        (tracker, completed)
    }

    /// What the reducer answered the input itself.
    pub fn accept(&self, result: CommandResult) {
        *self.0.accepted.lock().unwrap() = Some(result);
    }

    /// A reply's own answer, counted toward the input it answers.
    pub fn record(&self, result: CommandResult) {
        self.0.results.lock().unwrap().push(result);
    }
}

tokio::task_local! {
    /// The input the running effect works for; replies sent from it are counted toward that input.
    pub static CURRENT: Option<Tracker>;
}

/// The input the code running now works for, if any.
pub fn current() -> Option<Tracker> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

/// A failure among the replies wins; otherwise the last reply naming a thread does.
fn combine(accepted: CommandResult, replies: Vec<CommandResult>) -> CommandResult {
    if !accepted.is_ok() {
        return accepted;
    }
    let mut result = accepted;
    for reply in replies {
        if !reply.is_ok() {
            return reply;
        }
        if matches!(&reply, CommandResult::Ok { task_id: Some(_), .. }) {
            result = reply;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_once_every_holder_lets_go_with_the_thread_a_reply_named() {
        let (tracker, completed) = Tracker::new();
        tracker.accept(CommandResult::ok());
        let reply = tracker.clone();
        drop(tracker);
        reply.record(CommandResult::ok_with_task("t1"));
        reply.record(CommandResult::ok());
        drop(reply);
        let result = futures::executor::block_on(completed);
        assert!(matches!(result, CommandResult::Ok { task_id: Some(id), .. } if id == "t1"));
    }

    #[test]
    fn a_failed_reply_fails_the_input() {
        let (tracker, completed) = Tracker::new();
        tracker.accept(CommandResult::ok());
        tracker.record(CommandResult::failed("no"));
        drop(tracker);
        assert!(!futures::executor::block_on(completed).is_ok());
    }

    #[test]
    fn an_input_never_taken_completes_as_refused() {
        let (tracker, completed) = Tracker::new();
        drop(tracker);
        assert!(!futures::executor::block_on(completed).is_ok());
    }
}
