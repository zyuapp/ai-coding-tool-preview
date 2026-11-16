//! Runs a scripted scenario through one engine's provider and prints every event as JSON lines, in
//! the app's wire shapes, for comparing against the TypeScript engines.
//! `cargo run -p act_engines --example engine_harness -- '<scenario json>'`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_engines::brief::{BriefDialect, BriefRun, Offers, RunToolkit};
use act_engines::claude::ClaudeAgentProvider;
use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::mcp_http::McpHttpHost;
use act_engines::opencode::OpenCodeAgentProvider;
use act_engines::provider::{AgentProvider, AgentTurn, RunHooks, RunInput, ToolDecision};
use act_engines::session_pool::SessionPool;
use act_engines::steer::{SteerChannel, SteerMessage};
use act_engines::tools::{BoundTool, ServedToolSet, ServerGrants, ToolHost, ToolResult};
use act_engines::types::*;
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn out(turn: usize, kind: &str, data: Value) {
    println!("{}", json!({ "turn": turn, "kind": kind, "data": data }));
}

struct Terminal;

impl RunToolkit for Terminal {
    fn tool_sets(&self, _run: &RunInput) -> Vec<ServedToolSet> {
        let empty = || -> act_engines::tools::ToolHandler {
            Arc::new(|_| {
                async { ToolResult::text("The terminal panel has no terminal open.") }.boxed()
            })
        };
        vec![ServedToolSet {
            server: "aicodingtool-terminal".into(),
            grants: ServerGrants::None,
            tools: vec![
                BoundTool {
                    name: "terminal_list".into(),
                    description: "List the terminals the AICodingTool terminal panel has open, with the folder each runs in and whether its shell is still alive.".into(),
                    input_schema: json!({ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": {} }),
                    read_only: true,
                    handler: empty(),
                },
                BoundTool {
                    name: "terminal_read".into(),
                    description: "Read what a terminal in the AICodingTool terminal panel has printed, as plain text with the escape sequences resolved. This is the user's own shell, not yours: you can read it but never type into it, so use Bash to run anything yourself. Prefer `match` over a large `lines` when hunting for an error \u{2014} it filters before the output is returned.".into(),
                    input_schema: json!({ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": {
                        "terminalId": { "description": "Which terminal to read. Defaults to the one this thread opened, else the one on screen.", "type": "string", "maxLength": 256 },
                        "lines": { "description": "How many of the newest lines to return. Defaults to 100, and never exceeds 2000.", "type": "number", "minimum": 0 },
                        "match": { "description": "Keep only lines containing this text, case-insensitively.", "type": "string", "maxLength": 1000 }
                    } }),
                    read_only: true,
                    handler: empty(),
                },
            ],
        }]
    }

    fn brief(
        &self,
        offers: Offers<'_>,
        _dialect: &BriefDialect,
        _run: &BriefRun<'_>,
    ) -> Vec<String> {
        if offers("aicodingtool-terminal", None) {
            vec!["Use terminal_list and terminal_read to see what the user's terminals in the AICodingTool terminal panel have printed. Those shells are the user's; run your own commands in your own.".into()]
        } else {
            Vec::new()
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value, fallback: &str) -> T {
    serde_json::from_value(value.clone())
        .unwrap_or_else(|_| serde_json::from_value(json!(fallback)).unwrap())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let scenario: Value =
        serde_json::from_str(&std::env::args().nth(1).expect("scenario")).unwrap();
    let engine: AgentEngine = parse(&scenario["engine"], "claude");
    if let Some(root) = scenario["pluginRoot"].as_str() {
        act_engines::app_plugin::set_app_plugin_root(Some(root.into()));
    }
    let pool = SessionPool::default();
    let host: Arc<dyn ToolHost> = Arc::new(McpHttpHost::new());
    let provider: Arc<dyn AgentProvider> = match engine {
        AgentEngine::Claude => Arc::new(ClaudeAgentProvider::new(pool)),
        AgentEngine::Codex => Arc::new(CodexAgentProvider::new(
            pool,
            CodexProviderOptions::new(Some(host), None),
        )),
        AgentEngine::OpenCode => Arc::new(OpenCodeAgentProvider::new(pool, Some(host))),
    };
    let default_model = match engine {
        AgentEngine::Claude => "haiku",
        AgentEngine::Codex => "gpt-6-luna",
        AgentEngine::OpenCode => "opencode/big-pickle",
    };
    let last = Arc::new(Mutex::new(None::<Continuation>));
    for (index, turn) in scenario["turns"].as_array().unwrap().iter().enumerate() {
        let cancel = CancellationToken::new();
        let steering = SteerChannel::new();
        let steers: Vec<Value> = turn["steer"].as_array().cloned().unwrap_or_default();
        let steered = Arc::new(AtomicBool::new(false));
        let mut hooks = RunHooks::silent();
        let abort_on = turn["abortOn"].as_str().map(String::from);
        let decision = turn["decision"].as_str().unwrap_or("allow").to_string();
        {
            let cancel = cancel.clone();
            let steering = steering.clone();
            let last = last.clone();
            hooks.emit = Arc::new(move |event| {
                let wire = event.to_wire();
                let kind = wire["type"].as_str().unwrap_or_default().to_string();
                if let act_engines::provider::ProviderEvent::Continuation(c) = &event {
                    *last.lock().unwrap() = Some(c.clone());
                }
                out(index, "event", wire);
                for steer in &steers {
                    let ready = if steer["afterTool"] == true {
                        kind == "tool"
                    } else {
                        kind == "assistant" || kind == "assistant-tail"
                    };
                    if ready && !steered.swap(true, Ordering::SeqCst) {
                        steering.push(SteerMessage {
                            message_id: steer["messageId"].as_str().unwrap_or("steer").into(),
                            prompt: steer["prompt"].as_str().unwrap_or_default().into(),
                        });
                    }
                }
                if abort_on.as_deref() == Some(kind.as_str()) && !cancel.is_cancelled() {
                    out(index, "abort", Value::Null);
                    cancel.cancel();
                }
            });
        }
        hooks.authorize = Arc::new(move |intent| {
            out(index, "authorize", serde_json::to_value(&intent).unwrap());
            let decision = if decision == "deny" {
                ToolDecision::Deny
            } else {
                ToolDecision::Allow
            };
            async move { decision }.boxed()
        });
        let answer = turn["answer"].as_str().map(String::from);
        hooks.ask_question = Arc::new(move |request, _| {
            out(index, "question", serde_json::to_value(&request).unwrap());
            let answers: Option<QuestionAnswers> = answer.clone().map(|text| {
                request
                    .questions
                    .iter()
                    .map(|q| (q.id.clone(), text.clone()))
                    .collect()
            });
            async move { answers }.boxed()
        });
        hooks.report_workflow =
            Arc::new(move |r| out(index, "workflow", serde_json::to_value(r).unwrap()));
        let stopper = (scenario["stopBackground"] == true).then(|| provider.clone());
        let task_id = scenario["taskId"]
            .as_str()
            .unwrap_or("harness-thread")
            .to_string();
        hooks.report_background = Arc::new(move |r| {
            let wire = serde_json::to_value(&r).unwrap();
            out(index, "background", wire.clone());
            if let (Some(provider), Some(id)) =
                (stopper.clone(), wire["processes"][0]["id"].as_str())
            {
                let (task_id, id) = (task_id.clone(), id.to_string());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(3000)).await;
                    out(index, "stop", json!(provider.stop_process(&task_id, &id)));
                });
            }
        });
        hooks.report_subagent =
            Arc::new(move |r| out(index, "subagent", serde_json::to_value(r).unwrap()));
        hooks.report_goal = Arc::new(move |r| out(index, "goal", serde_json::to_value(r).unwrap()));
        if scenario["agentTurns"] == true {
            hooks.begin_agent_turn = Arc::new(move || {
                Some(AgentTurn {
                    emit: Arc::new(move |event| out(index, "agent-event", event.to_wire())),
                    authorize: Arc::new(move |intent| {
                        out(
                            index,
                            "agent-authorize",
                            serde_json::to_value(&intent).unwrap(),
                        );
                        async { ToolDecision::Allow }.boxed()
                    }),
                    steering: SteerChannel::new(),
                    end: Arc::new(move |result| out(index, "agent-end", result.to_wire())),
                })
            });
        }
        let continuation = if turn["continuation"] == "previous" {
            last.lock().unwrap().clone()
        } else {
            turn["continuation"]
                .as_str()
                .map(|value| Continuation::new(engine, value))
        };
        let input = RunInput {
            channel: parse(&turn["channel"], "main"),
            task_id: scenario["taskId"]
                .as_str()
                .unwrap_or("harness-thread")
                .into(),
            title: "Harness".into(),
            prompt: turn["prompt"].as_str().unwrap_or_default().into(),
            workspace_root: scenario["workspace"].as_str().unwrap().into(),
            project_root: None,
            projectless: true,
            computer_use: ComputerUseRunConfig::Unavailable {
                message: "off".into(),
            },
            policy: parse(&turn["policy"], "confirm"),
            engine,
            model: turn["model"].as_str().unwrap_or(default_model).into(),
            effort: parse(&turn["effort"], "low"),
            fast_mode: None,
            operation: serde_json::from_value(turn["operation"].clone()).ok(),
            chrome_browser: turn["chrome"] == true,
            continuation,
            fork_continuation: turn["fork"] == true,
            fresh_session: false,
            coordination_role: serde_json::from_value(turn["role"].clone()).ok(),
            toolkit: (scenario["terminal"] == true)
                .then(|| Arc::new(Terminal) as Arc<dyn RunToolkit>),
            steering: steering.clone(),
            cancel: cancel.clone(),
            hooks,
        };
        if let Some(ms) = turn["abortAfterMs"].as_u64() {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                out(index, "abort", Value::Null);
                cancel.cancel();
            });
        }
        let result = provider.execute(input).await;
        steering.close();
        out(index, "result", result.to_wire());
    }
    if let Some(ms) = scenario["lingerMs"].as_u64() {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    provider.close_all();
    act_engines::engines::close_engine_services().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
}
