//! Every engine together: the registry of what each engine does outside a run, one provider per
//! engine routed by the run's engine, and which engines can take a run on this machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use act_core::domain::engine::{
    capabilities_for, engine_needs_attention, EngineStatus, AGENT_ENGINES,
};
use futures::future::{join_all, BoxFuture, FutureExt, Shared};

use crate::claude::ClaudeEngine;
use crate::codex::CodexEngine;
use crate::opencode::OpenCodeEngine;
use crate::provider::{AgentProvider, ProviderResult, RunInput};
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{
    AgentEngine, AvailableCommand, Continuation, CoordinationRole, EngineAccess, EngineReadiness,
    PlanUsage, SubagentMetadata,
};

/// Opens a URL for a browser sign-in.
pub type OpenUrl = Box<dyn FnOnce(String) -> BoxFuture<'static, anyhow::Result<()>> + Send>;

/// One engine as the app drives it. What it can do is `act_core`'s catalogue entry for it.
pub trait Engine: Sync {
    /// The engine's provider for one channel, on that channel's shared session pool.
    fn provider(
        &self,
        pool: SessionPool,
        host: Option<Arc<dyn ToolHost>>,
        generated_images: Option<String>,
    ) -> Arc<dyn AgentProvider>;

    /// Whether the engine can take a run on this machine, and what would fix it when it cannot.
    fn readiness(&self) -> BoxFuture<'static, EngineReadiness>;

    /// Upgrades the installed command the way the user installed it.
    fn update(&self) -> BoxFuture<'static, anyhow::Result<()>>;

    /// The commands the engine offers a composer in the workspace.
    fn commands<'a>(
        &'a self,
        workspace_root: &'a str,
        projectless: bool,
    ) -> BoxFuture<'a, anyhow::Result<Vec<AvailableCommand>>>;

    /// Names a thread from its first message and its screenshots; `model` is the one the thread runs on.
    fn suggest_title<'a>(
        &'a self,
        text: &'a str,
        images: &'a [String],
        model: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>>;

    /// Reads the plan behind the engine's account without starting a thread.
    fn plan_usage(&self) -> BoxFuture<'static, PlanUsage>;

    /// Signs in through the app, for an engine whose capabilities include `sign_in`.
    fn sign_in(&self, _open_url: OpenUrl) -> BoxFuture<'static, anyhow::Result<EngineAccess>> {
        async {
            Err(anyhow::anyhow!(
                "This engine signs in where it is installed."
            ))
        }
        .boxed()
    }

    /// A saved subagent's settings, for an engine whose capabilities include `subagent_metadata`.
    fn subagent_metadata<'a>(
        &'a self,
        _id: &'a str,
        _session_id: Option<&'a str>,
    ) -> BoxFuture<'a, Option<SubagentMetadata>> {
        async { None }.boxed()
    }

    /// Stops what the engine started to answer outside a run, as the app quits.
    fn close(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

/// The engine behind an id, or None for one only a newer build knows.
pub fn engine(engine: AgentEngine) -> Option<&'static dyn Engine> {
    match engine {
        AgentEngine::Claude => Some(&ClaudeEngine),
        AgentEngine::Codex => Some(&CodexEngine),
        AgentEngine::Opencode => Some(&OpenCodeEngine),
        AgentEngine::Other(_) => None,
    }
}

/// Every engine this build runs, in the order pickers list them.
fn engines() -> impl Iterator<Item = (AgentEngine, &'static dyn Engine)> {
    AGENT_ENGINES
        .into_iter()
        .filter_map(|id| engine(id).map(|runs| (id, runs)))
}

/// One provider per engine, told apart by the run's engine. A thread's session lives with the engine it runs on.
#[derive(Clone)]
pub struct EngineRouter {
    engines: Vec<(AgentEngine, Arc<dyn AgentProvider>)>,
}

impl EngineRouter {
    pub fn new(engines: Vec<(AgentEngine, Arc<dyn AgentProvider>)>) -> Self {
        EngineRouter { engines }
    }

    /// Every engine on one shared pool, as a channel of the app runs them.
    pub fn for_channel(
        host: Option<Arc<dyn ToolHost>>,
        pool: SessionPool,
        generated_images: Option<String>,
    ) -> Self {
        EngineRouter::new(
            engines()
                .map(|(id, runs)| {
                    (
                        id,
                        runs.provider(pool.clone(), host.clone(), generated_images.clone()),
                    )
                })
                .collect(),
        )
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

/// Stops what the engines started to answer outside a run, as the app quits.
pub async fn close_engine_services() {
    join_all(engines().map(|(_, runs)| runs.close())).await;
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
            let again = refresh || engine_needs_attention(state.status.as_ref());
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
    pub async fn sign_in(&self, id: AgentEngine, open_url: OpenUrl) -> EngineStatus {
        let Some(runs) = engine(id).filter(|_| capabilities_for(id).sign_in) else {
            return self.read(false).await;
        };
        let signing = {
            let mut state = self.state.lock().unwrap();
            match &state.signing_in {
                Some(signing) => signing.clone(),
                None => {
                    let host = self.clone();
                    let signing = async move {
                        let access = runs.sign_in(open_url).await.ok();
                        let mut status = host.read(false).await;
                        if let (Some(access), Some(readiness)) = (access, status.get_mut(id)) {
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
                        let updated = match crate::engines::engine(engine) {
                            Some(runs) => runs.update().await,
                            None => Err(anyhow::anyhow!("This engine is not available.")),
                        };
                        if let Err(error) = updated {
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
        let status: EngineStatus = join_all(
            engines().map(|(id, runs)| runs.readiness().map(move |readiness| (id, readiness))),
        )
        .await
        .into_iter()
        .collect();
        self.state.lock().unwrap().status = Some(status.clone());
        status
    }
}
