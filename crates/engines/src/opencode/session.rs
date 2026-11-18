//! One OpenCode session, kept across turns on a server of its own. The server belongs to the session
//! because the app's tools and the run's instructions are given to it as it starts.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::app_plugin::app_skills_root;
use crate::brief::{
    needs_side_chat_boundary, run_brief, BriefDialect, RunBrief, SIDE_CHAT_BOUNDARY,
};
use crate::markdown::{end_markdown, stream_markdown, MarkdownBuffer};
use crate::provider::{ProviderEvent, ProviderResult, RunInput};
use crate::session_pool::{PooledSession, SessionHooks};
use crate::session_turn::{
    answer_of, drain_steering, first_line, spawn_abort_watch, Compacting, TurnSlot,
};
use crate::tools::{ServedTools, ToolHost};
use crate::types::{
    AgentEngine, AgentQuestion, CompactionTrigger, ComputerUseRunConfig, Continuation,
    QuestionAnswers, QuestionOption, QuestionRequest, RetryNotice, RunOperation, ToolIntent,
};

use super::catalog::model_ref;
use super::policy::{
    agent_for, permission_rules, PermissionRule, APP_SERVER_NAME, COMPUTER_USE_SERVER_NAME,
};
use super::server::{
    location_query, OpenCodeConnect, OpenCodeError, OpenCodeEvent, OpenCodeServer, ServerOptions,
    FEED_ENDED,
};

const ENDED: &str = "The OpenCode session ended before the run could start.";
/// How long a lost feed waits for its server's exit, which says why it went.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

pub const OPENCODE_DIALECT: BriefDialect = BriefDialect {
    native_work: "OpenCode sessions or subagents",
    native_worktree: None,
    coordinator_reads: "Your own tools read and search; none edits files or runs shell commands.",
    own_checkout_commits: None,
};

/// What an OpenCode session is told beyond its prompt, and the app tools it is served.
pub fn open_code_brief(input: &RunInput) -> RunBrief {
    run_brief(input, &OPENCODE_DIALECT)
}

/// Tokens a response read and wrote, as the server counts a step.
fn tokens_of(value: &Value) -> u64 {
    let count = |part: &Value| part.as_f64().filter(|n| n.is_finite()).unwrap_or(0.0);
    (count(&value["input"])
        + count(&value["output"])
        + count(&value["reasoning"])
        + count(&value["cache"]["read"])
        + count(&value["cache"]["write"]))
    .max(0.0) as u64
}

/// The app's own tools are filed under one server; their bare name is what the app knows them by.
pub fn tool_name(name: &str) -> String {
    name.strip_prefix(&format!("{APP_SERVER_NAME}_"))
        .unwrap_or(name)
        .to_string()
}

/// What a tool call is about. A write names the file it changes.
pub fn intent_of(tool_id: &str, name: &str, input: &Value, root: &str) -> ToolIntent {
    let args = if input.is_object() {
        input.clone()
    } else {
        json!({})
    };
    let file = args["filePath"].as_str().or(args["path"].as_str());
    let writes = ["write", "edit", "apply_patch", "patch", "multiedit"].contains(&name);
    let write_path = file.filter(|_| writes).map(|file| {
        crate::codex::home::normalize(&std::path::Path::new(root).join(file))
            .to_string_lossy()
            .into_owned()
    });
    ToolIntent {
        tool_id: tool_id.to_string(),
        name: tool_name(name),
        input: args,
        write_path,
    }
}

/// A form OpenCode puts to the user, asked as the composer asks its questions: one per field.
pub fn questions_of(form: &Value) -> Option<QuestionRequest> {
    let title = form["title"].as_str().unwrap_or_default();
    let fields: Vec<&Value> = form["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|field| field["type"] != "external")
        .collect();
    if fields.is_empty() {
        return None;
    }
    Some(QuestionRequest {
        blocking: true,
        questions: fields
            .iter()
            .map(|field| AgentQuestion {
                id: field["key"].as_str().unwrap_or_default().to_string(),
                header: field["title"].as_str().unwrap_or(title).to_string(),
                question: field["description"]
                    .as_str()
                    .or(field["title"].as_str())
                    .unwrap_or(title)
                    .to_string(),
                options: if field["type"] == "boolean" {
                    vec![
                        QuestionOption {
                            label: "Yes".into(),
                            description: String::new(),
                        },
                        QuestionOption {
                            label: "No".into(),
                            description: String::new(),
                        },
                    ]
                } else {
                    field["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|option| QuestionOption {
                            label: option["label"].as_str().unwrap_or_default().to_string(),
                            description: option["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        })
                        .collect()
                },
            })
            .collect(),
    })
}

/// The answers to a form, each in the type its field takes.
pub fn form_answer(form: &Value, answers: &QuestionAnswers) -> Value {
    let mut answer = Map::new();
    for field in form["fields"].as_array().into_iter().flatten() {
        let key = field["key"].as_str().unwrap_or_default();
        let Some(given) = answers.get(key) else {
            continue;
        };
        let options = field["options"].as_array().cloned().unwrap_or_default();
        let value_of = |label: &str| {
            options
                .iter()
                .find(|option| option["label"] == label)
                .map(|option| option["value"].clone())
        };
        let value = match field["type"].as_str() {
            Some("boolean") => json!(matches!(
                given.trim().to_lowercase().as_str(),
                "yes" | "true"
            )),
            Some("number" | "integer") => given
                .trim()
                .parse::<f64>()
                .map(|n| json!(n))
                .unwrap_or(Value::Null),
            Some("multiselect") => json!(given
                .split(',')
                .map(|part| value_of(part.trim()).unwrap_or_else(|| json!(part.trim())))
                .filter(|value| value.as_str() != Some(""))
                .collect::<Vec<_>>()),
            _ => value_of(given).unwrap_or_else(|| json!(given)),
        };
        answer.insert(key.to_string(), value);
    }
    Value::Object(answer)
}

#[derive(Default)]
struct TurnData {
    compacting: Compacting,
    /// Messages this turn put in the session's inbox that it has yet to take, and those it has taken.
    waiting: HashSet<String>,
    delivered: HashSet<String>,
    streamed: HashMap<String, MarkdownBuffer>,
    /// Tool names by call id, which the call itself does not repeat, and each call as it was shown.
    tools: HashMap<String, String>,
    intents: HashMap<String, ToolIntent>,
    /// Set once the server has the turn's prompt, so an interrupt has something to stop.
    started: bool,
    interrupt_wanted: bool,
}

#[derive(Default, Clone)]
struct Settings {
    model: Option<String>,
    variant: Option<String>,
    agent: Option<String>,
    rules: Option<String>,
}

type Slashes = Shared<BoxFuture<'static, (HashSet<String>, HashSet<String>)>>;
type Opening = Shared<BoxFuture<'static, Result<(), (String, bool)>>>;

struct State {
    client: Option<OpenCodeServer>,
    served: Option<ServedTools>,
    opening: Option<Opening>,
    turns: TurnSlot<TurnData>,
    ended: bool,
    closed: Option<Shared<BoxFuture<'static, ()>>>,
    outcome: Option<ProviderResult>,
    brief_file: Option<PathBuf>,
    windows: HashMap<String, u64>,
    variants: HashMap<String, HashSet<String>>,
    slashes: Slashes,
    children: HashSet<String>,
    settings: Settings,
    last_tokens: u64,
    subscription: Option<u64>,
    session_id: Option<String>,
}

pub struct OpenCodeSession {
    key: String,
    hooks: SessionHooks,
    connect: OpenCodeConnect,
    host: Option<Arc<dyn ToolHost>>,
    state: Mutex<State>,
    me: Weak<OpenCodeSession>,
}

fn reason(error: &anyhow::Error) -> String {
    first_line(&error.to_string()).to_string()
}

impl OpenCodeSession {
    pub fn new(
        key: String,
        connect: OpenCodeConnect,
        host: Option<Arc<dyn ToolHost>>,
        hooks: SessionHooks,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| OpenCodeSession {
            key,
            hooks,
            connect,
            host,
            me: me.clone(),
            state: Mutex::new(State {
                client: None,
                served: None,
                opening: None,
                turns: TurnSlot::default(),
                ended: false,
                closed: None,
                outcome: None,
                brief_file: None,
                windows: HashMap::new(),
                variants: HashMap::new(),
                slashes: async { (HashSet::new(), HashSet::new()) }.boxed().shared(),
                children: HashSet::new(),
                settings: Settings::default(),
                last_tokens: 0,
                subscription: None,
                session_id: None,
            }),
        })
    }

    /// What OpenCode calls the session. A later run resumes it by this id.
    pub fn session_id(&self) -> Option<String> {
        self.state.lock().unwrap().session_id.clone()
    }

    fn settle_if(&self, id: u64, result: ProviderResult) {
        self.state.lock().unwrap().turns.settle_if(id, result);
    }

    fn run_turn(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        let mut state = self.state.lock().unwrap();
        if state.ended {
            let outcome = state
                .outcome
                .clone()
                .unwrap_or_else(|| ProviderResult::failed(ENDED));
            return async move { outcome }.boxed();
        }
        let (taken, _) = state.turns.take(input.clone(), TurnData::default());
        if taken.begin {
            let id = taken.id;
            let interrupt = self.me.clone();
            let give_up = self.me.clone();
            let watcher = spawn_abort_watch(
                &input,
                move || {
                    if let Some(session) = interrupt.upgrade() {
                        session.interrupt(id);
                    }
                },
                move || {
                    if let Some(session) = give_up.upgrade() {
                        let settled = session
                            .state
                            .lock()
                            .unwrap()
                            .turns
                            .settle_if(id, ProviderResult::cancelled())
                            .is_some();
                        if settled {
                            tokio::spawn(session.close_now());
                        }
                    }
                },
            );
            state.turns.watch(id, watcher);
            drop(state);
            let session = self.clone();
            tokio::spawn(async move { session.begin(id, input).await });
        }
        answer_of(taken.answer).boxed()
    }

    async fn begin(self: Arc<Self>, id: u64, input: RunInput) {
        let opening = {
            let mut state = self.state.lock().unwrap();
            match &state.opening {
                Some(opening) => opening.clone(),
                None => {
                    let opening = self.clone().open(input.clone()).boxed().shared();
                    state.opening = Some(opening.clone());
                    opening
                }
            }
        };
        if let Err((message, lost)) = opening.await {
            {
                let mut state = self.state.lock().unwrap();
                if state.turns.is_current(id) {
                    if lost {
                        input.emit(ProviderEvent::ContinuationLost);
                    }
                    state.turns.settle(ProviderResult::failed(message));
                }
            }
            tokio::spawn(self.close_now());
            return;
        }
        let (client, session_id) = {
            let mut state = self.state.lock().unwrap();
            let (Some(client), Some(session_id)) = (state.client.clone(), state.session_id.clone())
            else {
                return;
            };
            if !state.turns.is_current(id) {
                return;
            }
            state
                .turns
                .announce(Continuation::new(AgentEngine::Opencode, &session_id));
            (client, session_id)
        };
        let started = async {
            self.apply(&client, &session_id, &input).await?;
            {
                let mut state = self.state.lock().unwrap();
                let Some(turn) = state.turns.turn_mut().filter(|turn| turn.id == id) else {
                    return Ok(false);
                };
                turn.data.started = true;
            }
            if matches!(input.operation, Some(RunOperation::Compact { .. })) {
                self.set_compacting(id, true, None);
                client
                    .request(
                        "POST",
                        &format!("/api/session/{session_id}/compact"),
                        Some(json!({})),
                        None,
                    )
                    .await?;
                return Ok(false);
            }
            self.send(&client, &session_id, id, &input.prompt, "queue")
                .await?;
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        match started {
            Err(error) => self.settle_if(
                id,
                ProviderResult::failed(format!(
                    "OpenCode could not start the turn: {}",
                    reason(&error)
                )),
            ),
            Ok(false) => {}
            Ok(true) => {
                let wanted = self
                    .state
                    .lock()
                    .unwrap()
                    .turns
                    .turn()
                    .filter(|turn| turn.id == id)
                    .map(|turn| turn.data.interrupt_wanted);
                match wanted {
                    Some(true) => self.interrupt(id),
                    Some(false) => self.drain(id, &input).await,
                    None => {}
                }
            }
        }
    }

    /// Puts a message in the session's inbox: a slash command as the command it names, a skill as that skill.
    async fn send(
        &self,
        client: &OpenCodeServer,
        session_id: &str,
        id: u64,
        prompt: &str,
        delivery: &str,
    ) -> anyhow::Result<()> {
        let named = crate::codex::skills::slash_names(prompt)
            .iter()
            .any(|name| !name.is_empty());
        let slashes = named.then(|| self.state.lock().unwrap().slashes.clone());
        let (known, commands) = match slashes {
            Some(slashes) => slashes.await,
            None => (HashSet::new(), HashSet::new()),
        };
        let trimmed = prompt.trim();
        if let Some(rest) = trimmed.strip_prefix('/') {
            let name: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            if !name.is_empty() && commands.contains(&name) {
                let text = rest[name.len()..].trim_start().to_string();
                let sent = client
                    .request(
                        "POST",
                        &format!("/api/session/{session_id}/command"),
                        Some(json!({ "name": name, "text": text, "delivery": delivery })),
                        None,
                    )
                    .await?;
                self.await_delivery(id, sent["id"].as_str());
                return Ok(());
            }
        }
        let mut skills: Vec<String> = Vec::new();
        for name in whole_slash_names(prompt) {
            if known.contains(name) && !skills.iter().any(|known| known == name) {
                skills.push(name.to_string());
            }
        }
        let mut body = json!({ "text": prompt, "delivery": delivery });
        if !skills.is_empty() {
            body["skills"] = json!(skills
                .iter()
                .map(|id| json!({ "id": id }))
                .collect::<Vec<_>>());
        }
        let sent = client
            .request(
                "POST",
                &format!("/api/session/{session_id}/prompt"),
                Some(body),
                None,
            )
            .await?;
        self.await_delivery(id, sent["id"].as_str());
        Ok(())
    }

    /// The server may take a message before saying it has it, so one already taken is not waited on.
    fn await_delivery(&self, id: u64, inbox_id: Option<&str>) {
        let Some(inbox_id) = inbox_id else { return };
        let mut state = self.state.lock().unwrap();
        if let Some(turn) = state.turns.turn_mut().filter(|turn| turn.id == id) {
            if !turn.data.delivered.contains(inbox_id) {
                turn.data.waiting.insert(inbox_id.to_string());
            }
        }
    }

    /// Brings the session to the run's model, effort, mode and tools, saying only what changed.
    async fn apply(
        &self,
        client: &OpenCodeServer,
        session_id: &str,
        input: &RunInput,
    ) -> anyhow::Result<()> {
        let (reference, settings) = {
            let state = self.state.lock().unwrap();
            (
                model_ref(&input.model, Self::variant_of(&state, input).as_deref()),
                state.settings.clone(),
            )
        };
        let Some(reference) = reference else {
            anyhow::bail!("OpenCode cannot run the {} model.", input.model)
        };
        let model = format!(
            "{}{}",
            reference["providerID"].as_str().unwrap_or_default(),
            reference["id"].as_str().unwrap_or_default()
        );
        let variant = reference["variant"].as_str().map(String::from);
        if Some(&model) != settings.model.as_ref() || variant != settings.variant {
            client
                .request(
                    "POST",
                    &format!("/api/session/{session_id}/model"),
                    Some(json!({ "model": reference })),
                    None,
                )
                .await?;
            let mut state = self.state.lock().unwrap();
            state.settings.model = Some(model);
            state.settings.variant = variant;
        }
        let agent = agent_for(input.policy);
        if settings.agent.as_deref() != Some(agent) {
            client
                .request(
                    "POST",
                    &format!("/api/session/{session_id}/agent"),
                    Some(json!({ "agent": agent })),
                    None,
                )
                .await?;
            self.state.lock().unwrap().settings.agent = Some(agent.into());
        }
        let rules = Self::rules(input);
        let written = serde_json::to_string(&rules)?;
        if settings.rules.as_deref() != Some(written.as_str()) {
            client
                .request(
                    "PATCH",
                    &format!("/api/session/{session_id}"),
                    Some(json!({ "permissions": rules })),
                    None,
                )
                .await?;
            self.state.lock().unwrap().settings.rules = Some(written);
        }
        Ok(())
    }

    /// The effort is passed on only as a depth the model offers; one that offers none takes none.
    fn variant_of(state: &State, input: &RunInput) -> Option<String> {
        state
            .variants
            .get(&input.model)
            .filter(|variants| variants.contains(input.effort.as_str()))
            .map(|_| input.effort.as_str().to_string())
    }

    fn rules(input: &RunInput) -> Vec<PermissionRule> {
        let sets = open_code_brief(input).tools;
        permission_rules(
            input.scope(),
            input.coordination_role,
            &sets,
            matches!(input.computer_use, ComputerUseRunConfig::Available { .. }),
        )
    }

    /// Starts the server with the app's tools and instructions, and puts the run's session on it.
    async fn open(self: Arc<Self>, seed: RunInput) -> Result<(), (String, bool)> {
        let fail = |message: String| (message, false);
        let failed =
            |error: anyhow::Error| fail(format!("OpenCode could not start: {}", reason(&error)));
        let brief = open_code_brief(&seed);
        let tools: Vec<_> = brief
            .tools
            .iter()
            .flat_map(|set| set.tools.clone())
            .collect();
        if !tools.is_empty() {
            let Some(host) = self.host.clone() else {
                return Err(fail(
                    "OpenCode could not start: the app's tool service is unavailable.".into(),
                ));
            };
            let served = host.serve(tools).await.map_err(failed)?;
            let mut state = self.state.lock().unwrap();
            if state.ended {
                drop(state);
                served.release();
                return Err(fail(ENDED.into()));
            }
            state.served = Some(served);
        }
        let folder = std::env::temp_dir().join("aicodingtool-opencode");
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|error| failed(error.into()))?;
        let brief_file = folder.join(format!("{}.md", uuid::Uuid::new_v4()));
        write_private(&brief_file, &brief.instructions.join("\n\n"))
            .map_err(|error| failed(error.into()))?;
        let mut mcp = Map::new();
        if let Some(served) = &self.state.lock().unwrap().served {
            mcp.insert(APP_SERVER_NAME.into(), json!({ "type": "remote", "url": served.url, "headers": { "Authorization": format!("Bearer {}", served.token) }, "oauth": false, "codemode": false }));
        }
        if let ComputerUseRunConfig::Available { mcp: computer } = &seed.computer_use {
            let mut command = vec![computer.command.clone()];
            command.extend(computer.args.iter().cloned());
            mcp.insert(COMPUTER_USE_SERVER_NAME.into(), json!({ "type": "local", "command": command, "environment": computer.env, "codemode": false }));
        }
        let mut config = json!({ "mcp": mcp, "instructions": [brief_file.to_string_lossy()] });
        if let Some(skills) = app_skills_root() {
            config["skills"] = json!([skills]);
        }
        self.state.lock().unwrap().brief_file = Some(brief_file);
        let client = (self.connect)(ServerOptions {
            cwd: seed.workspace_root.clone(),
            config: Some(config),
        })
        .await
        .map_err(failed)?;
        if self.state.lock().unwrap().ended {
            client.close().await;
            return Err(fail(ENDED.into()));
        }
        let me = self.me.clone();
        let subscription = client.subscribe(move |event| {
            if let Some(session) = me.upgrade() {
                session.receive(event);
            }
        });
        {
            let mut state = self.state.lock().unwrap();
            state.client = Some(client.clone());
            state.subscription = Some(subscription);
        }
        let me = self.me.clone();
        let exit = client.exited();
        tokio::spawn(async move {
            let status = exit.await;
            if let Some(session) = me.upgrade() {
                session.lost(format!("OpenCode stopped: {}", status.describe()));
            }
        });
        let location = location_query(&seed.workspace_root);
        let reader = client.clone();
        let (skills_path, commands_path) = (
            format!("/api/skill?{location}"),
            format!("/api/command?{location}"),
        );
        let slashes: Slashes = async move {
            let (skills, commands) = tokio::join!(
                reader.read_loaded(&skills_path),
                reader.read_loaded(&commands_path)
            );
            let ids = |list: anyhow::Result<Vec<Value>>, field: &str| {
                list.unwrap_or_default()
                    .iter()
                    .filter_map(|entry| entry[field].as_str().map(String::from))
                    .collect::<HashSet<_>>()
            };
            (ids(skills, "id"), ids(commands, "name"))
        }
        .boxed()
        .shared();
        tokio::spawn(slashes.clone());
        self.state.lock().unwrap().slashes = slashes;
        let models = client
            .request("GET", &format!("/api/model?{location}"), None, None)
            .await
            .unwrap_or(json!([]));
        {
            let mut state = self.state.lock().unwrap();
            for model in models.as_array().into_iter().flatten() {
                let id = format!(
                    "{}/{}",
                    model["providerID"].as_str().unwrap_or_default(),
                    model["id"].as_str().unwrap_or_default()
                );
                if let Some(window) = model["limit"]["context"]
                    .as_u64()
                    .filter(|window| *window > 0)
                {
                    state.windows.insert(id.clone(), window);
                }
                state.variants.insert(
                    id,
                    model["variants"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|variant| variant["id"].as_str().map(String::from))
                        .collect(),
                );
            }
        }
        let continuation = seed.continuation_value().map(String::from);
        let reference = {
            let state = self.state.lock().unwrap();
            model_ref(&seed.model, Self::variant_of(&state, &seed).as_deref())
        };
        let Some(reference) = reference else {
            return Err(fail(format!(
                "OpenCode cannot run the {} model.",
                seed.model
            )));
        };
        let create = || {
            client.request(
                "POST",
                "/api/session",
                Some(json!({ "location": { "directory": seed.workspace_root }, "model": reference, "agent": agent_for(seed.policy), "permissions": Self::rules(&seed), "metadata": { "aicodingtool": seed.task_id } })),
                None,
            )
        };
        let session = match &continuation {
            None => create().await.map_err(failed)?,
            Some(held_id) => {
                let held = match client.request("GET", &format!("/api/session/{held_id}"), None, None).await {
                    Ok(held) => held,
                    Err(error) => match error.downcast_ref::<OpenCodeError>() {
                        Some(refusal) if refusal.status == 404 => return Err((format!("OpenCode could not continue this session ({}). Start a new thread to keep going.", refusal.message), true)),
                        _ => return Err(failed(error)),
                    },
                };
                let session = if seed.fork_continuation {
                    match client
                        .request(
                            "POST",
                            &format!("/api/session/{held_id}/fork"),
                            Some(json!({})),
                            None,
                        )
                        .await
                    {
                        Ok(forked) => forked,
                        Err(error) => match error.downcast_ref::<OpenCodeError>() {
                            Some(refusal)
                                if refusal.status == 400
                                    && refusal.message.to_lowercase().contains("empty session") =>
                            {
                                create().await.map_err(failed)?
                            }
                            _ => return Err(failed(error)),
                        },
                    }
                } else {
                    held
                };
                if session["location"]["directory"].as_str() != Some(seed.workspace_root.as_str()) {
                    let id = session["id"].as_str().unwrap_or_default();
                    client
                        .request(
                            "POST",
                            &format!("/api/session/{id}/move"),
                            Some(json!({ "directory": seed.workspace_root })),
                            None,
                        )
                        .await
                        .map_err(failed)?;
                }
                session
            }
        };
        let session_id = session["id"].as_str().unwrap_or_default().to_string();
        self.state.lock().unwrap().session_id = Some(session_id.clone());
        if needs_side_chat_boundary(&seed) {
            client
                .request(
                    "POST",
                    &format!("/api/session/{session_id}/synthetic"),
                    Some(json!({ "text": SIDE_CHAT_BOUNDARY, "resume": false })),
                    None,
                )
                .await
                .map_err(failed)?;
        }
        Ok(())
    }

    fn interrupt(&self, id: u64) {
        let (client, session_id) = {
            let mut state = self.state.lock().unwrap();
            let Some(turn) = state.turns.turn_mut().filter(|turn| turn.id == id) else {
                return;
            };
            if !turn.data.started {
                turn.data.interrupt_wanted = true;
                return;
            }
            (state.client.clone(), state.session_id.clone())
        };
        if let (Some(client), Some(session_id)) = (client, session_id) {
            tokio::spawn(async move {
                let _ = client
                    .request(
                        "POST",
                        &format!("/api/session/{session_id}/interrupt"),
                        None,
                        None,
                    )
                    .await;
            });
        }
    }

    /// Feeds one run's steered messages into the session as it works, each taken at its next step.
    async fn drain(self: &Arc<Self>, id: u64, input: &RunInput) {
        let target = || {
            let state = self.state.lock().unwrap();
            state.turns.turn().filter(|turn| turn.id == id)?;
            Some((state.client.clone()?, state.session_id.clone()?))
        };
        let session = self.clone();
        drain_steering(
            input,
            target,
            |(client, session_id): (OpenCodeServer, String), prompt: String| {
                let session = session.clone();
                async move {
                    session
                        .send(&client, &session_id, id, &prompt, "steer")
                        .await
                }
            },
            reason,
        )
        .await;
    }

    /// A feed usually ends because its server died, whose own exit says why, so that is waited on
    /// briefly before the bare loss is reported.
    fn feed_ended(&self) {
        let exited = self
            .state
            .lock()
            .unwrap()
            .client
            .as_ref()
            .map(|client| client.exited());
        let me = self.me.clone();
        tokio::spawn(async move {
            let message = match exited {
                Some(exited) => match tokio::time::timeout(EXIT_WAIT, exited).await {
                    Ok(status) => format!("OpenCode stopped: {}", status.describe()),
                    Err(_) => "OpenCode stopped sending events".to_string(),
                },
                None => "OpenCode stopped sending events".to_string(),
            };
            if let Some(session) = me.upgrade() {
                session.lost(message);
            }
        });
    }

    /// A server the session can no longer hear is given up on: its turn fails, and the next run opens a fresh one.
    fn lost(&self, message: String) {
        {
            let mut state = self.state.lock().unwrap();
            if state.ended {
                return;
            }
            let outcome = ProviderResult::failed(message);
            state.outcome = Some(outcome.clone());
            state.turns.settle(outcome);
        }
        tokio::spawn(self.close_now());
    }

    fn owns(state: &State, session_id: &Value) -> bool {
        session_id.as_str().is_some_and(|id| {
            state.session_id.as_deref() == Some(id) || state.children.contains(id)
        })
    }

    fn receive(&self, event: OpenCodeEvent) {
        if event.kind == FEED_ENDED {
            self.feed_ended();
            return;
        }
        let data = &event.data;
        let mut rested = false;
        {
            let mut state = self.state.lock().unwrap();
            if event.kind == "session.created"
                && data["parentID"].is_string()
                && Self::owns(&state, &data["parentID"])
            {
                if let Some(child) = data["sessionID"].as_str() {
                    state.children.insert(child.to_string());
                }
                return;
            }
            if event.kind == "permission.asked" {
                if Self::owns(&state, &data["sessionID"]) {
                    drop(state);
                    self.decide(data.clone());
                }
                return;
            }
            if event.kind == "form.created" {
                if data["form"].is_object() && Self::owns(&state, &data["form"]["sessionID"]) {
                    drop(state);
                    self.ask(data["form"].clone());
                }
                return;
            }
            if data["sessionID"].as_str().is_none()
                || data["sessionID"].as_str() != state.session_id.as_deref()
            {
                return;
            }
            let last_tokens = state.last_tokens;
            let Some(turn) = state.turns.turn_mut() else {
                return;
            };
            let text_key = || {
                format!(
                    "{}:{}",
                    value_text(&data["assistantMessageID"]),
                    value_text(&data["ordinal"])
                )
            };
            match event.kind.as_str() {
                "session.inbox.delivered" => {
                    if let Some(inbox) = data["inboxID"].as_str() {
                        turn.data.delivered.insert(inbox.to_string());
                        turn.data.waiting.remove(inbox);
                    }
                }
                "session.text.delta" => {
                    let emit = turn.input.hooks.emit.clone();
                    stream_markdown(
                        &mut turn.data.streamed,
                        &*emit,
                        &text_key(),
                        data["delta"].as_str().unwrap_or_default(),
                    );
                }
                "session.text.ended" => {
                    let emit = turn.input.hooks.emit.clone();
                    end_markdown(
                        &mut turn.data.streamed,
                        &*emit,
                        &text_key(),
                        data["text"].as_str().unwrap_or_default(),
                    );
                }
                "session.tool.input.started" => {
                    if let (Some(id), Some(name)) = (data["id"].as_str(), data["name"].as_str()) {
                        turn.data.tools.insert(id.to_string(), name.to_string());
                    }
                }
                "session.tool.called" => {
                    let Some(id) = data["id"].as_str() else {
                        return;
                    };
                    let name = turn
                        .data
                        .tools
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| "tool".into());
                    let intent = intent_of(id, &name, &data["input"], &turn.input.workspace_root);
                    turn.data.intents.insert(id.to_string(), intent.clone());
                    turn.input.emit(ProviderEvent::Tool { intent });
                }
                "session.step.ended" => {
                    let tokens = tokens_of(&data["tokens"]);
                    if tokens == 0 {
                        return;
                    }
                    turn.input.emit(ProviderEvent::Spend { tokens });
                    let input = turn.input.clone();
                    state.last_tokens = tokens;
                    if let Some(limit) = state.windows.get(&input.model) {
                        input.emit(ProviderEvent::Usage {
                            tokens,
                            limit: *limit,
                            model: input.model.clone(),
                        });
                    }
                }
                "session.retry.scheduled" => {
                    let message = data["message"]
                        .as_str()
                        .or(data["error"]["message"].as_str())
                        .unwrap_or("OpenCode is retrying the request.")
                        .to_string();
                    turn.input.emit(ProviderEvent::Retry(RetryNotice {
                        message,
                        attempt: None,
                        max_retries: None,
                    }));
                }
                "session.compaction.started" => {
                    turn.data
                        .compacting
                        .report(&turn.input, true, last_tokens, None)
                }
                "session.compaction.ended" => {
                    let manual = matches!(turn.input.operation, Some(RunOperation::Compact { .. }));
                    let pre_tokens = turn.data.compacting.pre_tokens.unwrap_or(last_tokens);
                    turn.input.emit(ProviderEvent::Compaction {
                        trigger: if manual {
                            CompactionTrigger::Manual
                        } else {
                            CompactionTrigger::Auto
                        },
                        pre_tokens,
                        post_tokens: None,
                    });
                    turn.data
                        .compacting
                        .report(&turn.input, false, last_tokens, None);
                    if manual {
                        state.turns.settle(ProviderResult::succeeded());
                    }
                }
                "session.compaction.failed" => {
                    let message = data["message"]
                        .as_str()
                        .unwrap_or("OpenCode could not compact this session.");
                    turn.data.compacting.report(
                        &turn.input,
                        false,
                        last_tokens,
                        Some(format!("Could not compact context: {message}")),
                    );
                    if matches!(turn.input.operation, Some(RunOperation::Compact { .. })) {
                        state.turns.settle(ProviderResult::failed_silently());
                    }
                }
                "session.execution.failed" => {
                    let message = data["message"]
                        .as_str()
                        .or(data["error"]["message"].as_str())
                        .unwrap_or("OpenCode could not finish the turn.")
                        .to_string();
                    rested = Self::finish(&mut state, ProviderResult::failed(message));
                }
                "session.execution.interrupted" => {
                    rested = Self::finish(&mut state, ProviderResult::cancelled())
                }
                "session.execution.succeeded" => {
                    rested = Self::finish(&mut state, ProviderResult::succeeded())
                }
                _ => {}
            }
        }
        if rested {
            (self.hooks.rested)();
        }
    }

    /// A turn ends with the execution that took its last message; one still waiting in the inbox runs next.
    fn finish(state: &mut State, result: ProviderResult) -> bool {
        let Some(turn) = state.turns.turn_mut() else {
            return false;
        };
        if !turn.data.started || matches!(turn.input.operation, Some(RunOperation::Compact { .. }))
        {
            return false;
        }
        if result.status == crate::provider::ResultStatus::Succeeded
            && !turn.data.waiting.is_empty()
        {
            return false;
        }
        for (key, buffer) in turn.data.streamed.drain() {
            if !buffer.text.is_empty() {
                turn.input.emit(ProviderEvent::Assistant {
                    message_id: key,
                    text: buffer.text,
                    append: true,
                    artifact: false,
                });
            }
        }
        state.turns.settle(result);
        true
    }

    fn set_compacting(&self, id: u64, compacting: bool, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        let last_tokens = state.last_tokens;
        if let Some(turn) = state.turns.turn_mut().filter(|turn| turn.id == id) {
            turn.data
                .compacting
                .report(&turn.input, compacting, last_tokens, error);
        }
    }

    /// Puts a tool the rules left to ask about to the run's user. Nothing is allowed with no run to ask.
    fn decide(&self, request: Value) {
        let (client, turn) = {
            let state = self.state.lock().unwrap();
            (
                state.client.clone(),
                state
                    .turns
                    .turn()
                    .map(|turn| (turn.input.clone(), turn.data.intents.clone())),
            )
        };
        let (Some(client), Some(id), Some(session)) = (
            client,
            request["id"].as_str().map(String::from),
            request["sessionID"].as_str().map(String::from),
        ) else {
            return;
        };
        let reply = move |decision: &'static str, message: Option<String>| {
            let client = client.clone();
            let (session, id) = (session.clone(), id.clone());
            async move {
                let mut body = json!({ "decision": decision });
                if let Some(message) = message {
                    body["message"] = json!(message);
                }
                let _ = client
                    .request(
                        "POST",
                        &format!("/api/session/{session}/permission/{id}/reply"),
                        Some(body),
                        None,
                    )
                    .await;
            }
        };
        let Some((input, intents)) = turn else {
            tokio::spawn(reply("reject", None));
            return;
        };
        let source_id = request["source"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let action = request["action"].as_str().unwrap_or("tool");
        let tool_input = if request["metadata"]["input"].is_object() {
            request["metadata"]["input"].clone()
        } else {
            json!({ "resources": request["resources"] })
        };
        let fallback_id = if source_id.is_empty() {
            request["id"].as_str().unwrap_or_default().to_string()
        } else {
            source_id.clone()
        };
        let intent = intents
            .get(&source_id)
            .cloned()
            .unwrap_or_else(|| intent_of(&fallback_id, action, &tool_input, &input.workspace_root));
        tokio::spawn(async move {
            let decision = (input.hooks.authorize)(intent).await;
            if decision.is_allow() {
                reply("once", None).await;
            } else {
                reply("reject", Some(decision.denial_reason())).await;
            }
        });
    }

    /// A form goes to the user as the composer's questions; one nobody answers is withdrawn.
    fn ask(&self, form: Value) {
        let (client, input) = {
            let state = self.state.lock().unwrap();
            (
                state.client.clone(),
                state.turns.turn().map(|turn| turn.input.clone()),
            )
        };
        let Some(client) = client else { return };
        let session = form["sessionID"].as_str().unwrap_or_default().to_string();
        let form_id = form["id"].as_str().unwrap_or_default().to_string();
        let cancel_path = format!("/api/session/{session}/form/{form_id}");
        let request = questions_of(&form);
        let (Some(input), Some(request)) = (input, request) else {
            tokio::spawn(async move {
                let _ = client.request("DELETE", &cancel_path, None, None).await;
            });
            return;
        };
        let token = CancellationToken::new();
        let cancel_on_abort = input.cancel.clone();
        let withdraw = token.clone();
        tokio::spawn(async move {
            cancel_on_abort.cancelled().await;
            withdraw.cancel();
        });
        tokio::spawn(async move {
            let answers = (input.hooks.ask_question)(request, token).await;
            let replied = match answers {
                Some(answers) => client
                    .request(
                        "POST",
                        &format!("{cancel_path}/reply"),
                        Some(json!({ "answer": form_answer(&form, &answers) })),
                        None,
                    )
                    .await
                    .is_ok(),
                None => false,
            };
            if !replied {
                let _ = client.request("DELETE", &cancel_path, None, None).await;
            }
        });
    }

    /// Names the session in OpenCode's own history, so it reads there as it reads here.
    pub fn label(&self, title: &str) {
        let (client, session_id) = {
            let state = self.state.lock().unwrap();
            (state.client.clone(), state.session_id.clone())
        };
        if let (Some(client), Some(session_id)) = (client, session_id) {
            let title = title.to_string();
            tokio::spawn(async move {
                let _ = client
                    .request(
                        "PATCH",
                        &format!("/api/session/{session_id}"),
                        Some(json!({ "title": title })),
                        None,
                    )
                    .await;
            });
        }
    }

    /// Files a session away in OpenCode's history, or brings it back, on this session's server.
    /// False when it has none to ask.
    pub async fn file(&self, session_id: &str, archived: bool) -> anyhow::Result<bool> {
        let client = {
            let state = self.state.lock().unwrap();
            if state.ended {
                return Ok(false);
            }
            state.client.clone()
        };
        let Some(client) = client else {
            return Ok(false);
        };
        client
            .request(
                "PATCH",
                &format!("/api/session/{session_id}"),
                Some(json!({ "archived": archived })),
                None,
            )
            .await?;
        Ok(true)
    }

    fn close_now(&self) -> BoxFuture<'static, ()> {
        let (client, served, subscription, brief_file) = {
            let mut state = self.state.lock().unwrap();
            if state.ended {
                return state
                    .closed
                    .clone()
                    .map(|closed| closed.boxed())
                    .unwrap_or_else(|| async {}.boxed());
            }
            state.ended = true;
            state.turns.settle(ProviderResult::cancelled());
            (
                state.client.take(),
                state.served.take(),
                state.subscription.take(),
                state.brief_file.take(),
            )
        };
        if let (Some(client), Some(subscription)) = (&client, subscription) {
            client.unsubscribe(subscription);
        }
        if let Some(client) = &client {
            client.begin_close();
        }
        let closed: Shared<BoxFuture<'static, ()>> = async move {
            if let Some(client) = client {
                client.close().await;
            }
            if let Some(file) = brief_file {
                let _ = tokio::fs::remove_file(file).await;
            }
        }
        .boxed()
        .shared();
        tokio::spawn(closed.clone());
        self.state.lock().unwrap().closed = Some(closed.clone());
        if let Some(served) = served {
            served.release();
        }
        (self.hooks.ended)();
        closed.boxed()
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "undefined".into(),
        other => other.to_string(),
    }
}

/// Every `/name` that stands as a word of its own (`(?:^|\s)\/([^\s/]+)(?=\s|$)`).
fn whole_slash_names(prompt: &str) -> Vec<&str> {
    crate::codex::skills::slash_names(prompt)
        .into_iter()
        .filter(|name| {
            let end = name.as_ptr() as usize - prompt.as_ptr() as usize + name.len();
            prompt[end..].chars().next().is_none_or(char::is_whitespace)
        })
        .collect()
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

impl PooledSession for OpenCodeSession {
    fn key(&self) -> String {
        self.key.clone()
    }

    fn live(&self) -> bool {
        !self.state.lock().unwrap().ended
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().turns.answering()
    }

    fn answering(&self) -> bool {
        self.state.lock().unwrap().turns.answering()
    }

    fn continues(&self, continuation: Option<&str>) -> bool {
        continuation
            .is_none_or(|value| self.state.lock().unwrap().session_id.as_deref() == Some(value))
    }

    fn run(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        self.run_turn(input)
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        self.close_now()
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_read_as_questions_and_answers_take_their_field_types() {
        let form = json!({ "id": "f", "sessionID": "s", "title": "Setup", "fields": [
            { "key": "go", "type": "boolean", "title": "Go?" },
            { "key": "n", "type": "number" },
            { "key": "pick", "type": "select", "options": [{ "value": "a1", "label": "A" }] },
            { "key": "x", "type": "external" },
        ] });
        let request = questions_of(&form).unwrap();
        assert_eq!(request.questions.len(), 3);
        assert_eq!(request.questions[1].header, "Setup");
        let answers: QuestionAnswers = [
            ("go".into(), "Yes".into()),
            ("n".into(), "4".into()),
            ("pick".into(), "A".into()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            form_answer(&form, &answers),
            json!({ "go": true, "n": 4.0, "pick": "a1" })
        );
        assert_eq!(
            intent_of("t", "write", &json!({ "filePath": "a.txt" }), "/w")
                .write_path
                .as_deref(),
            Some("/w/a.txt")
        );
        assert_eq!(tool_name("aicodingtool_list_threads"), "list_threads");
        assert_eq!(whole_slash_names("/a b /c/d /e"), ["a", "e"]);
    }
}
