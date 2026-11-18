//! The turn lifecycle every engine's session shares: one turn answered at a time, its abort watch,
//! its steering, and how it settles. An engine supplies only how it opens, starts and interrupts one.

use std::any::Any;
use std::future::Future;
use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::provider::{Emit, ProviderEvent, ProviderResult, RunInput};
use crate::session_pool::PooledSession;
use crate::steer::SteerChannel;
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
    released: Option<Box<dyn Fn() + Send + Sync>>,
}

impl<T> Default for TurnSlot<T> {
    fn default() -> Self {
        TurnSlot {
            current: None,
            announced: false,
            next_id: 1,
            released: None,
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
    /// A slot that calls `released` whenever a turn settles, under the session's lock.
    pub fn releasing(released: impl Fn() + Send + Sync + 'static) -> Self {
        TurnSlot {
            released: Some(Box::new(released)),
            ..Self::default()
        }
    }

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
        if let Some(released) = &self.released {
            released();
        }
        Some(turn)
    }

    /// Fails the turn in flight, saying first that its continuation is gone when `failure` lost it.
    pub fn fail(&mut self, failure: OpenFailure) -> Option<SlotTurn<T>> {
        if failure.lost {
            if let Some(turn) = &self.current {
                turn.input.emit(ProviderEvent::ContinuationLost);
            }
        }
        self.settle(ProviderResult::failed(failure.message))
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

/// What every session keeps of its lifecycle, whichever engine runs it.
pub struct SessionCore<T> {
    pub turns: TurnSlot<T>,
    pub ended: bool,
    /// What a run is answered with once the session has ended.
    pub outcome: Option<ProviderResult>,
}

impl<T> Default for SessionCore<T> {
    fn default() -> Self {
        SessionCore::with_turns(TurnSlot::default())
    }
}

impl<T> SessionCore<T> {
    pub fn with_turns(turns: TurnSlot<T>) -> Self {
        SessionCore {
            turns,
            ended: false,
            outcome: None,
        }
    }
}

/// One engine's live session as the shared turn lifecycle drives it. The engine says how a turn
/// starts and how it is interrupted; taking it, watching its cancellation and settling it are shared.
pub(crate) trait TurnSession: Send + Sync + Sized + 'static {
    /// What the engine keeps for one turn.
    type Turn: Send + 'static;
    type State: Send + 'static;
    /// What a run is told when the session ended before it could start.
    const ENDED: &'static str;

    /// Everything the session was built with. A run that disagrees needs a session of its own.
    fn key(&self) -> &str;
    fn state(&self) -> MutexGuard<'_, Self::State>;
    fn core(state: &mut Self::State) -> &mut SessionCore<Self::Turn>;
    /// The engine's own handle on the thread, which a later run continues.
    fn handle(state: &Self::State) -> Option<&str>;
    /// Anything closing the session would cut short.
    fn busy(state: &Self::State) -> bool;
    /// What the run's turn starts with, taken under the session's lock.
    fn open_turn(&self, state: &mut Self::State, input: &RunInput) -> Self::Turn;
    /// Starts the turn the run took: opens the session if it must, sends the prompt, feeds steering.
    fn begin(self: Arc<Self>, id: u64, input: RunInput) -> BoxFuture<'static, ()>;
    /// Asks the engine to stop the turn. Its own answer still ends it.
    fn interrupt(&self, id: u64);
    /// Idempotent. Settles what is in flight and starts closing at once; the future settles once
    /// the process has exited.
    fn close(&self) -> BoxFuture<'static, ()>;
}

/// Takes the run's turn and answers with the result the engine settles on. A cancelled run
/// interrupts the engine; an interrupt unanswered for the grace period gives the session up.
pub(crate) fn run_turn<S: TurnSession>(
    session: Arc<S>,
    input: RunInput,
) -> BoxFuture<'static, ProviderResult> {
    let mut state = session.state();
    let core = S::core(&mut state);
    if core.ended {
        let outcome = core
            .outcome
            .clone()
            .unwrap_or_else(|| ProviderResult::failed(S::ENDED));
        return async move { outcome }.boxed();
    }
    let turn = session.open_turn(&mut state, &input);
    let (taken, _) = S::core(&mut state).turns.take(input.clone(), turn);
    if taken.begin {
        let id = taken.id;
        let interrupt = Arc::downgrade(&session);
        let give_up = Arc::downgrade(&session);
        let watcher = spawn_abort_watch(
            &input,
            move || {
                if let Some(session) = interrupt.upgrade() {
                    session.interrupt(id);
                }
            },
            move || {
                if let Some(session) = give_up.upgrade() {
                    let settled = S::core(&mut session.state())
                        .turns
                        .settle_if(id, ProviderResult::cancelled())
                        .is_some();
                    if settled {
                        tokio::spawn(session.close());
                    }
                }
            },
        );
        S::core(&mut state).turns.watch(id, watcher);
        drop(state);
        tokio::spawn(session.begin(id, input));
    }
    answer_of(taken.answer).boxed()
}

/// Waits on the session's opening for turn `id`. One that cannot open fails the turn, saying so
/// when the continuation itself is gone, and closes the session.
pub(crate) async fn opened<S: TurnSession>(
    session: &Arc<S>,
    id: u64,
    opening: impl Future<Output = Result<(), OpenFailure>>,
) -> bool {
    let Err(failure) = opening.await else {
        return true;
    };
    {
        let mut state = session.state();
        let turns = &mut S::core(&mut state).turns;
        if turns.is_current(id) {
            turns.fail(failure);
        }
    }
    tokio::spawn(session.close());
    false
}

impl<S: TurnSession> PooledSession for S {
    fn key(&self) -> String {
        TurnSession::key(self).to_string()
    }

    fn live(&self) -> bool {
        !S::core(&mut self.state()).ended
    }

    fn busy(&self) -> bool {
        S::busy(&self.state())
    }

    fn answering(&self) -> bool {
        S::core(&mut self.state()).turns.answering()
    }

    fn continues(&self, continuation: Option<&str>) -> bool {
        continuation.is_none_or(|value| S::handle(&self.state()) == Some(value))
    }

    fn run(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        run_turn(self, input)
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        TurnSession::close(self)
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
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

/// Feeds a turn's steered messages into it for as long as `target` finds the turn going. One the
/// engine refuses stays queued on the thread, which sends it as the next turn once the run settles.
pub async fn drain_steering<C, F, Fut>(
    steering: &SteerChannel,
    emit: &Emit,
    target: impl Fn() -> Option<C>,
    steer: F,
    reason: impl Fn(&anyhow::Error) -> String,
) where
    F: Fn(C, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    while let Some(next) = steering.next().await {
        let Some(into) = target() else { return };
        match steer(into, next.prompt).await {
            Ok(()) => emit(ProviderEvent::Steered {
                message_id: next.message_id,
            }),
            Err(error) => {
                if target().is_none() {
                    return;
                }
                emit(ProviderEvent::SteerFailed {
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
        self.say(&input.hooks.emit, compacting, error);
    }

    /// Says whether the turn is compacting. A repeat says nothing unless it carries an error.
    pub fn say(&mut self, emit: &Emit, compacting: bool, error: Option<String>) {
        if self.compacting == Some(compacting) && error.is_none() {
            return;
        }
        self.compacting = Some(compacting);
        emit(ProviderEvent::CompactionStatus { compacting, error });
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
#[derive(Debug, Clone, thiserror::Error)]
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
