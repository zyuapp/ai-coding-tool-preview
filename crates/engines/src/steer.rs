use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerMessage {
    pub message_id: String,
    pub prompt: String,
}

#[derive(Default)]
struct State {
    waiting: VecDeque<SteerMessage>,
    closed: bool,
}

/// Messages the user pushed into a run that had already started. The coordinator pushes; the engine
/// pulls them into its session. `next` resolves `None` once the run is over.
#[derive(Clone, Default)]
pub struct SteerChannel {
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
}

impl SteerChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// False once the channel is closed.
    pub fn push(&self, message: SteerMessage) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        state.waiting.push_back(message);
        drop(state);
        self.notify.notify_one();
        true
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.notify.notify_waiters();
        self.notify.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    pub async fn next(&self) -> Option<SteerMessage> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(message) = state.waiting.pop_front() {
                    return Some(message);
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_queued_messages_then_ends_on_close() {
        let channel = SteerChannel::new();
        assert!(channel.push(SteerMessage {
            message_id: "1".into(),
            prompt: "a".into()
        }));
        let reader = channel.clone();
        let pending = tokio::spawn(async move { (reader.next().await, reader.next().await) });
        tokio::task::yield_now().await;
        channel.close();
        let (first, second) = pending.await.unwrap();
        assert_eq!(first.unwrap().message_id, "1");
        assert!(second.is_none());
        assert!(!channel.push(SteerMessage {
            message_id: "2".into(),
            prompt: "b".into()
        }));
    }
}
