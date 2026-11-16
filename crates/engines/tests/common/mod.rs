//! What the live scenario tests share: a run builder and a recorder for everything a run reports.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_engines::brief::RunToolkit;
use act_engines::provider::{
    AgentTurn, ProviderEvent, ProviderResult, RunHooks, RunInput, ToolDecision,
};
use act_engines::steer::{SteerChannel, SteerMessage};
use act_engines::types::*;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

/// Everything runs and the agent's own turns reported, in order.
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<ProviderEvent>>,
    pub asked: Mutex<Vec<ToolIntent>>,
    pub questions: Mutex<Vec<QuestionRequest>>,
    pub subagents: Mutex<Vec<SubagentReport>>,
    pub background: Mutex<Vec<BackgroundReport>>,
    pub agent_events: Mutex<Vec<ProviderEvent>>,
    pub agent_ends: Mutex<Vec<ProviderResult>>,
}

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Recorder::default())
    }

    pub fn events(&self) -> Vec<ProviderEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn take_events(&self) -> Vec<ProviderEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    pub fn subagents(&self) -> Vec<SubagentReport> {
        self.subagents.lock().unwrap().clone()
    }
}

pub type OnEvent = Arc<dyn Fn(&ProviderEvent) + Send + Sync>;

/// One run's settings. `on_event` sees every event after it is recorded.
#[derive(Clone)]
pub struct Turn {
    pub engine: AgentEngine,
    pub model: String,
    pub effort: AgentEffort,
    pub policy: ExecutionPolicy,
    pub prompt: String,
    pub task_id: String,
    pub continuation: Option<Continuation>,
    pub operation: Option<RunOperation>,
    pub decision: ToolDecision,
    pub answer: Option<String>,
    pub toolkit: Option<Arc<dyn RunToolkit>>,
    pub agent_turns: bool,
    pub steering: SteerChannel,
    pub cancel: CancellationToken,
    pub on_event: Option<OnEvent>,
}

impl Turn {
    pub fn new(engine: AgentEngine, model: &str, prompt: &str) -> Self {
        Turn {
            engine,
            model: model.into(),
            effort: AgentEffort::Low,
            policy: ExecutionPolicy::Bypass,
            prompt: prompt.into(),
            task_id: "live-scenario".into(),
            continuation: None,
            operation: None,
            decision: ToolDecision::Allow,
            answer: None,
            toolkit: None,
            agent_turns: false,
            steering: SteerChannel::new(),
            cancel: CancellationToken::new(),
            on_event: None,
        }
    }

    pub fn policy(mut self, policy: ExecutionPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn continuing(mut self, continuation: Option<Continuation>) -> Self {
        self.continuation = continuation;
        self
    }

    pub fn input(&self, root: &str, recorder: &Arc<Recorder>) -> RunInput {
        let mut hooks = RunHooks::silent();
        let (rec, on_event) = (recorder.clone(), self.on_event.clone());
        hooks.emit = Arc::new(move |event| {
            rec.events.lock().unwrap().push(event.clone());
            if let Some(on_event) = &on_event {
                on_event(&event);
            }
        });
        let (rec, decision) = (recorder.clone(), self.decision.clone());
        hooks.authorize = Arc::new(move |intent| {
            rec.asked.lock().unwrap().push(intent);
            let decision = decision.clone();
            async move { decision }.boxed()
        });
        let (rec, answer) = (recorder.clone(), self.answer.clone());
        hooks.ask_question = Arc::new(move |request, _| {
            let answers = answer.clone().map(|text| {
                request
                    .questions
                    .iter()
                    .map(|question| (question.id.clone(), text.clone()))
                    .collect()
            });
            rec.questions.lock().unwrap().push(request);
            async move { answers }.boxed()
        });
        let rec = recorder.clone();
        hooks.report_subagent = Arc::new(move |report| rec.subagents.lock().unwrap().push(report));
        let rec = recorder.clone();
        hooks.report_background =
            Arc::new(move |report| rec.background.lock().unwrap().push(report));
        if self.agent_turns {
            let rec = recorder.clone();
            hooks.begin_agent_turn = Arc::new(move || {
                let (events, ends) = (rec.clone(), rec.clone());
                Some(AgentTurn {
                    emit: Arc::new(move |event| events.agent_events.lock().unwrap().push(event)),
                    authorize: Arc::new(|_| async { ToolDecision::Allow }.boxed()),
                    steering: SteerChannel::new(),
                    end: Arc::new(move |result| ends.agent_ends.lock().unwrap().push(result)),
                })
            });
        }
        RunInput {
            channel: RunChannel::Main,
            task_id: self.task_id.clone(),
            title: "Live scenario".into(),
            prompt: self.prompt.clone(),
            workspace_root: root.into(),
            project_root: None,
            projectless: true,
            computer_use: ComputerUseRunConfig::Unavailable {
                message: "off".into(),
            },
            policy: self.policy,
            engine: self.engine,
            model: self.model.clone(),
            effort: self.effort,
            fast_mode: None,
            operation: self.operation.clone(),
            chrome_browser: false,
            continuation: self.continuation.clone(),
            fork_continuation: false,
            fresh_session: false,
            coordination_role: None,
            toolkit: self.toolkit.clone(),
            steering: self.steering.clone(),
            cancel: self.cancel.clone(),
            hooks,
        }
    }
}

/// A scratch git repository under /tmp holding `seed.txt`, and its canonical path.
pub fn workspace(prefix: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (dir, root)
}

/// All the assistant text a run reported, whole messages and appended blocks alike.
pub fn text(events: &[ProviderEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn continuation(events: &[ProviderEvent]) -> Option<Continuation> {
    events.iter().find_map(|event| match event {
        ProviderEvent::Continuation(continuation) => Some(continuation.clone()),
        _ => None,
    })
}

pub fn tools(events: &[ProviderEvent]) -> Vec<ToolIntent> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Tool { intent } => Some(intent.clone()),
            _ => None,
        })
        .collect()
}

/// Pushes one steered message the first time `when` holds.
pub fn steer_when(
    steering: &SteerChannel,
    message: &str,
    when: impl Fn(&ProviderEvent) -> bool + Send + Sync + 'static,
) -> OnEvent {
    let steering = steering.clone();
    let message = message.to_string();
    let sent = std::sync::atomic::AtomicBool::new(false);
    Arc::new(move |event| {
        if when(event) && !sent.swap(true, std::sync::atomic::Ordering::SeqCst) {
            steering.push(SteerMessage {
                message_id: "steer-1".into(),
                prompt: message.clone(),
            });
        }
    })
}

/// Cancels the run the first time `when` holds.
pub fn cancel_when(
    cancel: &CancellationToken,
    when: impl Fn(&ProviderEvent) -> bool + Send + Sync + 'static,
) -> OnEvent {
    let cancel = cancel.clone();
    Arc::new(move |event| {
        if when(event) {
            cancel.cancel();
        }
    })
}

/// Waits up to `limit` for `done` to hold.
pub async fn eventually(limit: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    done()
}
