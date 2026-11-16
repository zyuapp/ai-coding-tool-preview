//! One OpenCode server the app asks things of outside a run, started on the first question and
//! stopped once nobody has asked anything for a minute.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};
use tokio::task::AbortHandle;

use crate::app_plugin::{app_skills_root, unqualified_skill_name};
use crate::binary::{installed_engine, upgrade_engine, version_readiness, OPENCODE_BINARY};
use crate::title::{title_of, title_question, TITLE_INSTRUCTIONS};
use crate::types::{
    AvailableCommand, EngineAccess, EngineReadiness, ModelSpec, PlanUsage, UsagePeriod, UsageWindow,
};

use super::catalog::{model_ref, open_code_catalog};
use super::server::{
    connect_open_code, location_query, OpenCodeConnect, OpenCodeServer, ServerOptions,
};

/// The release whose server API the app speaks. An older OpenCode, including any 1.x, is turned away.
pub const OPENCODE_BASELINE_VERSION: &str = "2.2.5";
const TITLE_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE: Duration = Duration::from_secs(60);

type Starting = Shared<BoxFuture<'static, Result<OpenCodeServer, String>>>;

#[derive(Default)]
struct State {
    client: Option<Starting>,
    idle: Option<AbortHandle>,
    in_flight: usize,
}

#[derive(Clone)]
pub struct OpenCodeServices {
    connect: OpenCodeConnect,
    state: Arc<Mutex<State>>,
}

impl OpenCodeServices {
    pub fn new(connect: OpenCodeConnect) -> Self {
        OpenCodeServices {
            connect,
            state: Arc::default(),
        }
    }

    async fn use_server<T>(
        &self,
        ask: impl FnOnce(OpenCodeServer) -> BoxFuture<'static, anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let starting = {
            let mut state = self.state.lock().unwrap();
            if let Some(idle) = state.idle.take() {
                idle.abort();
            }
            state.in_flight += 1;
            state
                .client
                .get_or_insert_with(|| {
                    let config = app_skills_root().map(|skills| json!({ "skills": [skills] }));
                    let cwd = crate::claude::services::home_dir()
                        .to_string_lossy()
                        .into_owned();
                    (self.connect)(ServerOptions { cwd, config })
                        .map(|result| result.map_err(|error| error.to_string()))
                        .boxed()
                        .shared()
                })
                .clone()
        };
        let result = async {
            let client = starting.clone().await.map_err(anyhow::Error::msg)?;
            let state = self.state.clone();
            let watched = starting.clone();
            let exited = client.exited();
            tokio::spawn(async move {
                exited.await;
                let mut state = state.lock().unwrap();
                if state
                    .client
                    .as_ref()
                    .is_some_and(|current| current.ptr_eq(&watched))
                {
                    state.client = None;
                }
            });
            ask(client).await
        }
        .await;
        let mut state = self.state.lock().unwrap();
        if result.is_err()
            && state
                .client
                .as_ref()
                .is_some_and(|current| current.ptr_eq(&starting))
        {
            state.client = None;
            let failed = starting.clone();
            tokio::spawn(async move {
                if let Ok(client) = failed.await {
                    client.close().await;
                }
            });
        }
        state.in_flight -= 1;
        if state.in_flight == 0 {
            let services = self.clone();
            state.idle = Some(
                tokio::spawn(async move {
                    tokio::time::sleep(IDLE).await;
                    services.close().await;
                })
                .abort_handle(),
            );
        }
        result
    }

    pub async fn close(&self) {
        let starting = {
            let mut state = self.state.lock().unwrap();
            if let Some(idle) = state.idle.take() {
                idle.abort();
            }
            state.client.take()
        };
        if let Some(starting) = starting {
            if let Ok(client) = starting.await {
                client.close().await;
            }
        }
    }

    /// The models the user's connected providers offer, the server's default first.
    pub async fn catalog(&self) -> anyhow::Result<Vec<ModelSpec>> {
        self.use_server(|client| {
            async move {
                let (models, providers, fallback) = tokio::join!(
                    client.request("GET", "/api/model", None, None),
                    client.request("GET", "/api/provider", None, None),
                    client.request("GET", "/api/model/default", None, None)
                );
                let models = models?.as_array().cloned().unwrap_or_default();
                let providers = providers
                    .ok()
                    .and_then(|providers| providers.as_array().cloned())
                    .unwrap_or_default();
                let fallback = fallback.ok();
                Ok(open_code_catalog(&models, &providers, fallback.as_ref()))
            }
            .boxed()
        })
        .await
    }

    /// OpenCode's commands and the skills it can load in one workspace, the app's own among them.
    pub async fn commands(&self, workspace_root: &str) -> anyhow::Result<Vec<AvailableCommand>> {
        let location = location_query(workspace_root);
        self.use_server(|client| {
            async move {
                let (command_path, skill_path) = (
                    format!("/api/command?{location}"),
                    format!("/api/skill?{location}"),
                );
                let (commands, skills) = tokio::join!(
                    client.read_loaded(&command_path),
                    client.read_loaded(&skill_path)
                );
                let mut listed: Vec<AvailableCommand> = Vec::new();
                let entries = commands.unwrap_or_default().into_iter().map(|command| {
                    (
                        command["name"].as_str().unwrap_or_default().to_string(),
                        command["description"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                });
                let skill_entries = skills.unwrap_or_default().into_iter().map(|skill| {
                    (
                        unqualified_skill_name(skill["id"].as_str().unwrap_or_default())
                            .to_string(),
                        skill["description"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                });
                for (name, description) in entries.chain(skill_entries) {
                    let command = AvailableCommand {
                        name: name.clone(),
                        description,
                        argument_hint: String::new(),
                        aliases: None,
                    };
                    match listed.iter_mut().find(|known| known.name == name) {
                        Some(known) => *known = command,
                        None => listed.push(command),
                    }
                }
                Ok(listed)
            }
            .boxed()
        })
        .await
    }

    /// Names a thread from its first message, on the model the thread runs on.
    pub async fn title(&self, text: &str, model: Option<&str>) -> Option<String> {
        let reference = model.and_then(|model| model_ref(model, None))?;
        if text.trim().is_empty() {
            return None;
        }
        let prompt = format!(
            "{TITLE_INSTRUCTIONS}\n\n{}\n\nAnswer with the title alone.",
            title_question(text)
        );
        self.use_server(|client| {
            async move {
                let answer = client
                    .request(
                        "POST",
                        "/api/experimental/generate/text",
                        Some(json!({ "prompt": prompt, "model": reference })),
                        Some(TITLE_TIMEOUT),
                    )
                    .await?;
                Ok(title_of(
                    &json!({ "title": answer["text"].as_str().unwrap_or_default() }),
                ))
            }
            .boxed()
        })
        .await
        .ok()
        .flatten()
    }

    /// OpenCode Go's allowance, when the account has one; any other provider bills by use.
    pub async fn plan_usage(&self) -> PlanUsage {
        let limits = self
            .use_server(|client| {
                async move {
                    client
                        .request("GET", "/api/experimental/usage/limits", None, None)
                        .await
                }
                .boxed()
            })
            .await;
        let limits = match limits {
            Ok(limits) => limits,
            Err(error) => {
                return PlanUsage::Unavailable {
                    message: error.to_string(),
                }
            }
        };
        let go = &limits["go"];
        if !go.is_object() {
            return PlanUsage::NotApplicable;
        }
        let window =
            |id: &str, label: &str, meter: &Value, period: Option<UsagePeriod>| UsageWindow {
                id: id.into(),
                label: label.into(),
                utilization: meter["percent"].as_f64(),
                resets_at: meter["resetsAt"]
                    .as_f64()
                    .and_then(|at| chrono::DateTime::from_timestamp_millis(at as i64))
                    .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                model: None,
                period,
                minutes: None,
            };
        PlanUsage::Available {
            subscription: Some("Go".into()),
            windows: vec![
                window(
                    "rolling",
                    "Rolling",
                    &go["rolling"],
                    Some(UsagePeriod::Session),
                ),
                window("weekly", "Weekly", &go["weekly"], Some(UsagePeriod::Week)),
                window("monthly", "Monthly", &go["monthly"], None),
            ],
        }
    }
}

/// The one services server the app keeps.
pub fn open_code_services() -> OpenCodeServices {
    static SHARED: OnceLock<OpenCodeServices> = OnceLock::new();
    SHARED
        .get_or_init(|| OpenCodeServices::new(connect_open_code()))
        .clone()
}

/// OpenCode serves a version of its API; the app speaks 2.x. A ready install names the models its
/// connected providers offer.
pub async fn read_open_code_readiness() -> EngineReadiness {
    let Some(installed) = installed_engine(&OPENCODE_BINARY).await else {
        return EngineReadiness {
            fix: Some(OPENCODE_BINARY.install.into()),
            ..EngineReadiness::of(EngineAccess::Missing)
        };
    };
    let version = version_readiness(
        &OPENCODE_BINARY,
        &installed,
        OPENCODE_BASELINE_VERSION,
        EngineAccess::Ready,
    )
    .await;
    if version.required.is_some() {
        return EngineReadiness {
            access: EngineAccess::Outdated,
            ..version
        };
    }
    match open_code_services().catalog().await {
        Ok(catalog) => EngineReadiness {
            fix: if catalog.is_empty() {
                Some("opencode auth login".into())
            } else {
                version.fix.clone()
            },
            catalog: Some(catalog),
            ..version
        },
        Err(_) => EngineReadiness {
            access: EngineAccess::Unavailable,
            ..version
        },
    }
}

pub async fn update_open_code() -> anyhow::Result<()> {
    upgrade_engine(&OPENCODE_BINARY).await
}
