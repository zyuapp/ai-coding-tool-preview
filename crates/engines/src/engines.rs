//! Every engine together: one provider per engine routed by the run's engine, what the app asks of
//! each outside a run, and which engines can take a run on this machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, FutureExt, Shared};

use crate::claude::{self, ClaudeAgentProvider};
use crate::codex::{self, CodexAgentProvider, CodexProviderOptions};
use crate::opencode::{self, OpenCodeAgentProvider};
use crate::provider::{AgentProvider, ProviderResult, RunInput};
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{
    AgentEngine, AvailableCommand, Continuation, CoordinationRole, EngineAccess, EngineReadiness,
    PlanUsage, SubagentMetadata,
};

/// One provider per engine, told apart by the run's engine. A thread's session lives with the engine it runs on.
#[derive(Clone)]
pub struct EngineRouter {
    engines: Vec<(AgentEngine, Arc<dyn AgentProvider>)>,
}

impl EngineRouter {
    pub fn new(engines: Vec<(AgentEngine, Arc<dyn AgentProvider>)>) -> Self {
        EngineRouter { engines }
    }

    /// The three engines on one shared pool, as a channel of the app runs them.
    pub fn for_channel(
        host: Option<Arc<dyn ToolHost>>,
        pool: SessionPool,
        generated_images: Option<String>,
    ) -> Self {
        EngineRouter::new(vec![
            (
                AgentEngine::Claude,
                Arc::new(ClaudeAgentProvider::new(pool.clone())) as Arc<dyn AgentProvider>,
            ),
            (
                AgentEngine::Codex,
                Arc::new(CodexAgentProvider::new(
                    pool.clone(),
                    CodexProviderOptions::new(host.clone(), generated_images),
                )),
            ),
            (
                AgentEngine::OpenCode,
                Arc::new(OpenCodeAgentProvider::new(pool, host)),
            ),
        ])
    }

    fn engine(&self, engine: AgentEngine) -> Option<&Arc<dyn AgentProvider>> {
        self.engines
            .iter()
            .find(|(known, _)| *known == engine)
            .map(|(_, provider)| provider)
    }
}

impl AgentProvider for EngineRouter {
    fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        match self.engine(input.engine) {
            Some(provider) => provider.execute(input),
            None => async { ProviderResult::failed("This engine is not available.") }.boxed(),
        }
    }

    /// A thread's live session sits in its channel's shared pool, where only the engine that opened it claims it.
    fn stop_process(&self, task_id: &str, process_id: &str) -> bool {
        self.engines
            .iter()
            .any(|(_, provider)| provider.stop_process(task_id, process_id))
    }

    fn label_thread(&self, task_id: &str, title: &str) -> bool {
        self.engines
            .iter()
            .any(|(_, provider)| provider.label_thread(task_id, title))
    }

    /// Goes to the engine that made the continuation; a thread with none yet goes to whichever holds its session.
    fn archive_thread(
        &self,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        match &continuation {
            None => self
                .engines
                .iter()
                .any(|(_, provider)| provider.archive_thread(task_id, None, archived)),
            Some(handle) => AgentEngine::parse(&handle.provider)
                .and_then(|engine| self.engine(engine))
                .is_some_and(|provider| {
                    provider.archive_thread(task_id, continuation.clone(), archived)
                }),
        }
    }

    fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>) {
        for (_, provider) in &self.engines {
            provider.retire_out_of_role(task_id, role);
        }
    }

    fn close_all(&self) {
        for (_, provider) in &self.engines {
            provider.close_all();
        }
    }
}

/// Opens a URL for a browser sign-in.
pub type OpenUrl = Box<dyn FnOnce(String) -> BoxFuture<'static, anyhow::Result<()>> + Send>;

/// The commands the engine offers a composer in the workspace.
pub async fn engine_commands(
    engine: AgentEngine,
    workspace_root: &str,
    projectless: bool,
) -> anyhow::Result<Vec<AvailableCommand>> {
    match engine {
        AgentEngine::Claude => {
            claude::services::discover_claude_commands(workspace_root, projectless).await
        }
        AgentEngine::Codex => codex::services::codex_commands(workspace_root).await,
        AgentEngine::OpenCode => {
            opencode::services::open_code_services()
                .commands(workspace_root)
                .await
        }
    }
}

/// Names a thread from its first message and its screenshots; `model` is the one the thread runs on.
pub async fn suggest_title(
    engine: AgentEngine,
    text: &str,
    images: &[String],
    model: Option<&str>,
) -> Option<String> {
    match engine {
        AgentEngine::Claude => claude::services::suggest_task_title(text, images).await,
        AgentEngine::Codex => codex::services::suggest_codex_title(text, images).await,
        AgentEngine::OpenCode => {
            opencode::services::open_code_services()
                .title(text, model)
                .await
        }
    }
}

/// Reads the plan behind the engine's account without starting a thread.
pub async fn plan_usage(engine: AgentEngine) -> PlanUsage {
    match engine {
        AgentEngine::Claude => claude::services::read_plan_usage().await,
        AgentEngine::Codex => codex::plan_usage::read_codex_plan_usage().await,
        AgentEngine::OpenCode => opencode::services::open_code_services().plan_usage().await,
    }
}

/// Whether the engine can take a run on this machine, and what would fix it when it cannot.
pub async fn readiness(engine: AgentEngine) -> EngineReadiness {
    match engine {
        AgentEngine::Claude => claude::services::read_claude_readiness().await,
        AgentEngine::Codex => codex::services::read_codex_readiness().await,
        AgentEngine::OpenCode => opencode::services::read_open_code_readiness().await,
    }
}

/// Upgrades the installed command the way the user installed it.
pub async fn update(engine: AgentEngine) -> anyhow::Result<()> {
    match engine {
        AgentEngine::Claude => claude::services::update_claude().await,
        AgentEngine::Codex => codex::services::update_codex().await,
        AgentEngine::OpenCode => opencode::services::update_open_code().await,
    }
}

/// Only Codex signs in through the app; the others are signed in where they are installed.
pub async fn sign_in(
    engine: AgentEngine,
    open_url: OpenUrl,
) -> Option<anyhow::Result<EngineAccess>> {
    match engine {
        AgentEngine::Codex => Some(codex::services::sign_in_to_codex(open_url).await),
        _ => None,
    }
}

/// A saved subagent's settings, for an engine that can recover them.
pub async fn subagent_metadata(
    engine: AgentEngine,
    id: &str,
    session_id: Option<&str>,
) -> Option<SubagentMetadata> {
    match engine {
        AgentEngine::Claude => {
            Some(claude::services::read_claude_subagent_metadata(id, session_id).await)
        }
        AgentEngine::Codex => Some(codex::services::read_codex_subagent_metadata(id).await),
        AgentEngine::OpenCode => None,
    }
}

/// Stops what the engines started to answer outside a run, as the app quits.
pub async fn close_engine_services() {
    opencode::services::open_code_services().close().await;
}

/// What is wrong with an engine's install, said once for the composer, the model menu and Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineNotice {
    /// True when this stops a run outright.
    pub blocking: bool,
    pub message: String,
    pub fix: Option<String>,
    /// The app can run `fix` itself.
    pub updatable: bool,
}

/// What is wrong with this engine on this machine, or None when it is fine. A signed-out engine is
/// left out: signing in is a button rather than a sentence.
pub fn engine_notice(engine: AgentEngine, readiness: &EngineReadiness) -> Option<EngineNotice> {
    let label = engine.label();
    let named = format!(
        "{label} {}",
        readiness.version.as_deref().unwrap_or("on this machine")
    );
    let fix = readiness.fix.clone();
    let updatable = fix.is_some();
    match readiness.access {
        EngineAccess::Missing => Some(EngineNotice {
            blocking: true,
            message: format!("{label} is not installed."),
            fix,
            updatable: false,
        }),
        EngineAccess::Outdated => Some(EngineNotice {
            blocking: true,
            message: format!(
                "{named} is too old. Update to {}.",
                readiness.required.as_deref().unwrap_or_default()
            ),
            fix,
            updatable,
        }),
        EngineAccess::Unavailable => Some(EngineNotice {
            blocking: true,
            message: format!("{label} is installed but would not start."),
            fix: None,
            updatable: false,
        }),
        EngineAccess::Ready if readiness.catalog.as_ref().is_some_and(Vec::is_empty) => {
            Some(EngineNotice {
                blocking: true,
                message: format!("{label} has no models. Connect a provider first."),
                fix,
                updatable: false,
            })
        }
        _ => readiness.required.as_ref().map(|required| EngineNotice {
            blocking: false,
            message: format!("{named} is behind {required}, so some models are hidden."),
            fix,
            updatable,
        }),
    }
}

pub type EngineStatus = HashMap<AgentEngine, EngineReadiness>;

/// True when any engine has something wrong with it.
pub fn engine_needs_attention(status: &EngineStatus) -> bool {
    AgentEngine::ALL.iter().any(|engine| {
        status
            .get(engine)
            .is_some_and(|readiness| engine_notice(*engine, readiness).is_some())
    })
}

type Reading = Shared<BoxFuture<'static, EngineStatus>>;

#[derive(Default)]
struct AccessState {
    status: Option<EngineStatus>,
    reading: Option<Reading>,
    signing_in: Option<Reading>,
    updating: HashMap<AgentEngine, Reading>,
}

/// Which engines can take a run. An answer where every engine is fine is kept, since asking runs the
/// engine commands; one with anything wrong is read again on every ask, because the user fixes it.
#[derive(Clone)]
pub struct EngineAccessHost {
    state: Arc<Mutex<AccessState>>,
    /// Reads the user's shell search path again, so a command installed after launch is found.
    search_path_again: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
}

impl EngineAccessHost {
    pub fn new(search_path_again: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>) -> Self {
        EngineAccessHost {
            state: Arc::default(),
            search_path_again,
        }
    }

    pub async fn read(&self, refresh: bool) -> EngineStatus {
        let reading = {
            let mut state = self.state.lock().unwrap();
            let again = refresh || state.status.as_ref().is_some_and(engine_needs_attention);
            if let Some(status) = state.status.clone().filter(|_| !again) {
                return status;
            }
            match &state.reading {
                Some(reading) => reading.clone(),
                None => {
                    let reading = self.clone().read_all(again).boxed().shared();
                    state.reading = Some(reading.clone());
                    reading
                }
            }
        };
        let status = reading.clone().await;
        let mut state = self.state.lock().unwrap();
        if state
            .reading
            .as_ref()
            .is_some_and(|current| current.ptr_eq(&reading))
        {
            state.reading = None;
        }
        status
    }

    /// One sign-in at a time: a second ask while the browser is out joins the first.
    pub async fn sign_in(&self, engine: AgentEngine, open_url: OpenUrl) -> EngineStatus {
        if engine != AgentEngine::Codex {
            return self.read(false).await;
        }
        let signing = {
            let mut state = self.state.lock().unwrap();
            match &state.signing_in {
                Some(signing) => signing.clone(),
                None => {
                    let host = self.clone();
                    let signing = async move {
                        let access = sign_in(engine, open_url).await.and_then(Result::ok);
                        let mut status = host.read(false).await;
                        if let (Some(access), Some(readiness)) = (access, status.get_mut(&engine)) {
                            readiness.access = access;
                        }
                        host.state.lock().unwrap().status = Some(status.clone());
                        status
                    }
                    .boxed()
                    .shared();
                    state.signing_in = Some(signing.clone());
                    signing
                }
            }
        };
        let status = signing.clone().await;
        let mut state = self.state.lock().unwrap();
        if state
            .signing_in
            .as_ref()
            .is_some_and(|current| current.ptr_eq(&signing))
        {
            state.signing_in = None;
        }
        status
    }

    /// One update per engine at a time; the status is read from scratch after it.
    pub async fn update(&self, engine: AgentEngine) -> (EngineStatus, Option<String>) {
        let failure = Arc::new(Mutex::new(None));
        let updating = {
            let mut state = self.state.lock().unwrap();
            match state.updating.get(&engine) {
                Some(updating) => updating.clone(),
                None => {
                    let host = self.clone();
                    let failed = failure.clone();
                    let updating = async move {
                        if let Err(error) = update(engine).await {
                            *failed.lock().unwrap() = Some(error.to_string());
                        }
                        host.read_all(true).await
                    }
                    .boxed()
                    .shared();
                    state.updating.insert(engine, updating.clone());
                    updating
                }
            }
        };
        let status = updating.clone().await;
        let mut state = self.state.lock().unwrap();
        if state
            .updating
            .get(&engine)
            .is_some_and(|current| current.ptr_eq(&updating))
        {
            state.updating.remove(&engine);
        }
        let failed = failure.lock().unwrap().clone();
        (status, failed)
    }

    async fn read_all(self, refresh: bool) -> EngineStatus {
        if refresh {
            (self.search_path_again)().await;
        }
        let (claude, codex, opencode) = tokio::join!(
            readiness(AgentEngine::Claude),
            readiness(AgentEngine::Codex),
            readiness(AgentEngine::OpenCode)
        );
        let status: EngineStatus = [
            (AgentEngine::Claude, claude),
            (AgentEngine::Codex, codex),
            (AgentEngine::OpenCode, opencode),
        ]
        .into_iter()
        .collect();
        self.state.lock().unwrap().status = Some(status.clone());
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_out_engine_needs_no_notice_but_a_behind_one_does() {
        let signed_out = EngineReadiness::of(EngineAccess::SignedOut);
        assert_eq!(engine_notice(AgentEngine::Codex, &signed_out), None);
        let behind = EngineReadiness {
            version: Some("2.0.0".into()),
            required: Some("2.1.0".into()),
            fix: Some("claude update".into()),
            ..EngineReadiness::of(EngineAccess::Ready)
        };
        let notice = engine_notice(AgentEngine::Claude, &behind).unwrap();
        assert!(!notice.blocking && notice.updatable);
        assert_eq!(
            notice.message,
            "Claude 2.0.0 is behind 2.1.0, so some models are hidden."
        );
        let empty = EngineReadiness {
            catalog: Some(Vec::new()),
            ..EngineReadiness::of(EngineAccess::Ready)
        };
        assert!(
            engine_notice(AgentEngine::OpenCode, &empty)
                .unwrap()
                .blocking
        );
    }
}
