//! The one turn a session answers at a time, and what every engine's turn shares.

use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::provider::{ProviderEvent, ProviderResult, RunInput};
use crate::types::{Continuation, RunOperation};

/// How long an interrupted turn has to come back with a result before the session is given up on.
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(10);

pub struct SlotTurn<T> {
    pub id: u64,
    pub input: RunInput,
    pub data: T,
    answer: Option<oneshot::Sender<ProviderResult>>,
    watcher: Option<AbortHandle>,
}

/// The turn a session answers. A cancelled run interrupts the engine rather than answering for it,
/// so the engine's own result still ends the turn; an interrupt unanswered for the grace period
/// gives the session up instead (the session's abort watcher does that).
pub struct TurnSlot<T> {
    current: Option<SlotTurn<T>>,
    announced: bool,
    next_id: u64,
}

impl<T> Default for TurnSlot<T> {
    fn default() -> Self {
        TurnSlot {
            current: None,
            announced: false,
            next_id: 1,
        }
    }
}

/// What `take` hands back: the turn's id, its answer, and whether the engine should begin it.
pub struct Taken {
    pub id: u64,
    pub answer: oneshot::Receiver<ProviderResult>,
    pub begin: bool,
}

impl<T> TurnSlot<T> {
    /// A turn is in flight, so the session owes an answer before it can take another.
    pub fn answering(&self) -> bool {
        self.current.is_some()
    }

    pub fn turn(&self) -> Option<&SlotTurn<T>> {
        self.current.as_ref()
    }

    pub fn turn_mut(&mut self) -> Option<&mut SlotTurn<T>> {
        self.current.as_mut()
    }

    pub fn is_current(&self, id: u64) -> bool {
        self.current.as_ref().is_some_and(|turn| turn.id == id)
    }

    pub fn current_id(&self) -> Option<u64> {
        self.current.as_ref().map(|turn| turn.id)
    }

    /// Takes the turn a run asked for. A run already cancelled is answered at once.
    pub fn take(&mut self, input: RunInput, data: T) -> (Taken, Option<SlotTurn<T>>) {
        let id = self.next_id;
        self.next_id += 1;
        let (sender, answer) = oneshot::channel();
        let cancelled = input.is_cancelled();
        self.current = Some(SlotTurn {
            id,
            input,
            data,
            answer: Some(sender),
            watcher: None,
        });
        if cancelled {
            let released = self.settle(ProviderResult::cancelled());
            return (
                Taken {
                    id,
                    answer,
                    begin: false,
                },
                released,
            );
        }
        (
            Taken {
                id,
                answer,
                begin: true,
            },
            None,
        )
    }

    pub fn watch(&mut self, id: u64, watcher: AbortHandle) {
        match self.current.as_mut() {
            Some(turn) if turn.id == id => turn.watcher = Some(watcher),
            _ => watcher.abort(),
        }
    }

    /// Answers the turn in flight, once. A run the user cancelled is cancelled whatever the engine
    /// says. Returns the released turn so the caller can drop what it held outside any lock.
    pub fn settle(&mut self, result: ProviderResult) -> Option<SlotTurn<T>> {
        let mut turn = self.current.take()?;
        if let Some(watcher) = turn.watcher.take() {
            watcher.abort();
        }
        let result = if turn.input.is_cancelled() {
            ProviderResult::cancelled()
        } else {
            result
        };
        if let Some(answer) = turn.answer.take() {
            let _ = answer.send(result);
        }
        Some(turn)
    }

    pub fn settle_if(&mut self, id: u64, result: ProviderResult) -> Option<SlotTurn<T>> {
        if self.is_current(id) {
            self.settle(result)
        } else {
            None
        }
    }

    /// Reports the session's handle to the turn in flight. Once is enough: the handle never changes.
    pub fn announce(&mut self, continuation: Continuation) {
        if self.announced {
            return;
        }
        if let Some(turn) = &self.current {
            self.announced = true;
            turn.input.emit(ProviderEvent::Continuation(continuation));
        }
    }
}

/// Watches a turn's cancellation: interrupts the engine, then gives the session up when no answer
/// comes within the grace period. The handle is aborted when the turn settles.
pub fn spawn_abort_watch(
    input: &RunInput,
    interrupt: impl FnOnce() + Send + 'static,
    give_up: impl FnOnce() + Send + 'static,
) -> AbortHandle {
    let cancel = input.cancel.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        interrupt();
        tokio::time::sleep(INTERRUPT_GRACE).await;
        give_up();
    })
    .abort_handle()
}

/// Waits on a turn's answer; a dropped sender reads as a cancellation.
pub async fn answer_of(answer: oneshot::Receiver<ProviderResult>) -> ProviderResult {
    answer.await.unwrap_or_else(|_| ProviderResult::cancelled())
}

/// Feeds a run's steered messages into its turn for as long as `target` finds the turn going. One the
/// engine refuses stays queued on the thread, which sends it as the next turn once the run settles.
pub async fn drain_steering<C, F, Fut>(
    input: &RunInput,
    target: impl Fn() -> Option<C>,
    steer: F,
    reason: impl Fn(&anyhow::Error) -> String,
) where
    F: Fn(C, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    while let Some(next) = input.steering.next().await {
        let Some(into) = target() else { return };
        match steer(into, next.prompt).await {
            Ok(()) => input.emit(ProviderEvent::Steered {
                message_id: next.message_id,
            }),
            Err(error) => {
                if target().is_none() {
                    return;
                }
                input.emit(ProviderEvent::SteerFailed {
                    message_id: next.message_id,
                    message: format!(
                        "Could not steer: {}. Your message is still queued for the next turn.",
                        reason(&error)
                    ),
                });
            }
        }
    }
}

/// What a turn keeps of a compaction going under it.
#[derive(Debug, Default, Clone)]
pub struct Compacting {
    pub compacting: Option<bool>,
    pub pre_tokens: Option<u64>,
}

impl Compacting {
    /// Says whether the turn is compacting, measured from the context before it began. A repeat says
    /// nothing unless it carries an error.
    pub fn report(
        &mut self,
        input: &RunInput,
        compacting: bool,
        last_tokens: u64,
        error: Option<String>,
    ) {
        if compacting && self.pre_tokens.is_none() {
            self.pre_tokens = Some(match input.operation {
                Some(RunOperation::Compact { pre_tokens }) => pre_tokens,
                _ => last_tokens,
            });
        }
        if self.compacting == Some(compacting) && error.is_none() {
            return;
        }
        self.compacting = Some(compacting);
        input.emit(ProviderEvent::CompactionStatus { compacting, error });
    }
}

/// The first line of a message.
pub fn first_line(message: &str) -> &str {
    message.split('\n').next().unwrap_or(message)
}

/// What went wrong, in one line.
pub fn reason_of(error: &anyhow::Error) -> String {
    first_line(&error.to_string()).to_string()
}

/// Why a session could not open, worded for the run that asked. `lost` says the continuation itself is gone.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct OpenFailure {
    pub message: String,
    pub lost: bool,
}

impl OpenFailure {
    pub fn new(message: impl Into<String>) -> Self {
        OpenFailure {
            message: message.into(),
            lost: false,
        }
    }

    pub fn lost(message: impl Into<String>) -> Self {
        OpenFailure {
            message: message.into(),
            lost: true,
        }
    }
}
