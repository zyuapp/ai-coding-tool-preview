//! Real Claude turns on Haiku covering what a thread does with Claude Code. Needs `claude` signed in:
//! `cargo test -p act_engines --test claude_scenarios -- --ignored --nocapture --test-threads=2`

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use act_engines::brief::{BriefDialect, BriefRun, Offers, RunToolkit};
use act_engines::claude::services::{read_plan_usage, suggest_task_title};
use act_engines::claude::ClaudeAgentProvider;
use act_engines::provider::{AgentProvider, ProviderEvent, ResultStatus, ToolDecision};
use act_engines::session_pool::SessionPool;
use act_engines::tools::{BoundTool, ServedToolSet, ServerGrants, ToolResult};
use act_engines::types::*;
use common::*;
use futures::FutureExt;

const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

fn provider() -> ClaudeAgentProvider {
    ClaudeAgentProvider::new(SessionPool::default())
}

fn haiku(prompt: &str) -> Turn {
    Turn::new(AgentEngine::Claude, "haiku", prompt)
}

fn write_red_png(root: &str) -> String {
    use base64::Engine as _;
    let path = format!("{root}/red.png");
    std::fs::write(
        &path,
        base64::engine::general_purpose::STANDARD
            .decode(RED_PNG)
            .unwrap(),
    )
    .unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn streams_text_and_reports_a_tool_call_with_usage_and_spend() {
    let (_dir, root) = workspace("act-claude-stream-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(haiku("Run the shell command `echo hello-stream` with the Bash tool, then write a three-sentence reply that quotes exactly what it printed.").input(&root, &rec))
        .await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(tools(&events)
        .iter()
        .any(|tool| tool.name == "Bash" && tool.input["command"] == "echo hello-stream"));
    // Text after a thinking block arrives whole, as the app's own engine reports it.
    let tails: Vec<(&String, &String)> = events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::AssistantTail { message_id, text } => Some((message_id, text)),
            _ => None,
        })
        .collect();
    if let Some((id, _)) = tails.first() {
        let blocks: String = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::Assistant {
                    message_id,
                    text,
                    append: true,
                    ..
                } if message_id == *id => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            blocks.ends_with(tails.last().unwrap().1.as_str()),
            "the last block flushes what was still streaming"
        );
    } else {
        assert!(events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Assistant { append: false, .. })));
    }
    assert!(
        text(&events).to_lowercase().contains("hello-stream"),
        "{}",
        text(&events)
    );
    assert!(events.iter().any(|event| matches!(event, ProviderEvent::Usage { tokens, limit: 200_000, model } if *tokens > 0 && model.starts_with("claude-haiku"))));
    assert!(events
        .iter()
        .any(|event| matches!(event, ProviderEvent::Spend { tokens } if *tokens > 0)));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn asks_before_writing_and_honours_a_denial() {
    let (dir, root) = workspace("act-claude-approve-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = haiku("Use the Write tool to create denied.txt in the current directory containing exactly: no. If the write is refused, reply with the single word refused.")
        .policy(ExecutionPolicy::Confirm);
    turn.decision = ToolDecision::Deny;
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let asked = rec.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|intent| intent.name == "Write"
        && intent
            .write_path
            .as_deref()
            .is_some_and(|path| path.ends_with("denied.txt"))));
    assert!(!dir.path().join("denied.txt").exists());
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn plan_mode_asks_to_leave_the_plan_and_withholds_ask_user_question() {
    let (_dir, root) = workspace("act-claude-plan-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(
            haiku("Plan how to add a file hello.txt containing hi. Keep the plan to two lines and present it with ExitPlanMode. Do not ask me any questions.")
                .policy(ExecutionPolicy::Plan)
                .input(&root, &rec),
        )
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(rec
        .asked
        .lock()
        .unwrap()
        .iter()
        .any(|intent| intent.name == "ExitPlanMode"));
    assert!(tools(&rec.events())
        .iter()
        .all(|tool| tool.name != "AskUserQuestion"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn an_interrupted_turn_is_cancelled_and_the_next_run_resumes_it() {
    let (_dir, root) = workspace("act-claude-interrupt-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = haiku("My secret word is walnut. Run the shell command `sleep 30 && echo finished` with the Bash tool, then tell me what it printed.");
    turn.on_event = Some(cancel_when(&turn.cancel, |event| {
        matches!(event, ProviderEvent::Tool { .. })
    }));
    let started = std::time::Instant::now();
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Cancelled, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(25));
    let thread = continuation(&rec.take_events()).expect("session id");

    let result = provider
        .execute(
            haiku("What is my secret word? Answer with the word only.")
                .continuing(Some(thread.clone()))
                .input(&root, &rec),
        )
        .await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(text(&events).to_lowercase().contains("walnut"));
    assert_eq!(continuation(&events), Some(thread));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_new_process_resumes_a_session_by_its_continuation() {
    let (_dir, root) = workspace("act-claude-resume-");
    let rec = Recorder::new();
    let first = provider();
    let result = first
        .execute(haiku("Remember the word quartz. Reply with ok.").input(&root, &rec))
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded);
    let thread = continuation(&rec.take_events()).unwrap();
    first.close_all();

    let second = provider();
    let result = second
        .execute(
            haiku("Which word did I ask you to remember? Answer with the word only.")
                .continuing(Some(thread.clone()))
                .input(&root, &rec),
        )
        .await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(text(&events).to_lowercase().contains("quartz"));
    assert_eq!(continuation(&events), Some(thread));
    second.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_warm_session_takes_a_new_model_effort_and_permission_mode() {
    let (dir, root) = workspace("act-claude-retune-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(
            haiku("Reply with ok.")
                .policy(ExecutionPolicy::Confirm)
                .input(&root, &rec),
        )
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded);
    let thread = continuation(&rec.take_events()).unwrap();

    let mut turn = Turn::new(
        AgentEngine::Claude,
        "sonnet",
        "Use the Write tool to create edited.txt containing exactly: yes. Then reply with done.",
    )
    .policy(ExecutionPolicy::AllowEdits)
    .continuing(Some(thread.clone()));
    turn.effort = AgentEffort::Medium;
    turn.decision = ToolDecision::Deny;
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert_eq!(
        continuation(&events),
        Some(thread),
        "the same process answers"
    );
    assert!(events.iter().any(|event| matches!(event, ProviderEvent::Usage { limit: 1_000_000, model, .. } if model.starts_with("claude-sonnet"))));
    assert!(
        rec.asked.lock().unwrap().is_empty(),
        "accept-edits writes without asking"
    );
    assert!(dir.path().join("edited.txt").exists());
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reads_an_image_named_in_the_prompt() {
    let (_dir, root) = workspace("act-claude-image-");
    let image = write_red_png(&root);
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(haiku(&format!("[Attached image: {image}]\nWhat single color fills this image? Answer with the color name only.")).input(&root, &rec))
        .await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(tools(&events)
        .iter()
        .any(|tool| tool.name == "Read" && tool.input["file_path"] == image));
    assert!(text(&events).to_lowercase().contains("red"));
    provider.close_all();
}

struct Ledger {
    calls: Arc<AtomicUsize>,
}

impl RunToolkit for Ledger {
    fn tool_sets(&self, _run: &act_engines::provider::RunInput) -> Vec<ServedToolSet> {
        let calls = self.calls.clone();
        vec![ServedToolSet {
            server: "aicodingtool-ledger".into(),
            grants: ServerGrants::None,
            tools: vec![BoundTool {
                name: "read_ledger".into(),
                description: "Read the balance of the team ledger.".into(),
                input_schema: serde_json::json!({ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": { "account": { "type": "string" } } }),
                read_only: true,
                handler: Arc::new(move |args| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let account = args["account"].as_str().unwrap_or("main").to_string();
                    async move { ToolResult::text(format!("Ledger {account}: balance 4217 credits.")) }.boxed()
                }),
            }],
        }]
    }

    fn brief(&self, offers: Offers<'_>, _: &BriefDialect, _: &BriefRun<'_>) -> Vec<String> {
        if offers("aicodingtool-ledger", Some("read_ledger")) {
            vec!["Use read_ledger to read the team ledger.".into()]
        } else {
            Vec::new()
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn serves_app_tools_in_process_and_asks_for_ungranted_ones() {
    let (_dir, root) = workspace("act-claude-mcp-");
    let provider = provider();
    let rec = Recorder::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut turn = haiku("Call the read_ledger tool for account main and tell me the balance it reports, as a number only.")
        .policy(ExecutionPolicy::Confirm);
    turn.toolkit = Some(Arc::new(Ledger {
        calls: calls.clone(),
    }));
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(rec
        .asked
        .lock()
        .unwrap()
        .iter()
        .any(|intent| intent.name == "mcp__aicodingtool-ledger__read_ledger"));
    assert!(text(&events).contains("4217"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reports_a_subagent_from_start_to_finish() {
    let (_dir, root) = workspace("act-claude-subagent-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(haiku("Use the Agent tool to launch one general-purpose subagent whose task is: read the file seed.txt and report its contents. Then immediately run the Bash command `sleep 25` and after that reply with what the subagent reported.").input(&root, &rec))
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let reports = rec.subagents();
    let started = reports.iter().find_map(|report| match report {
        SubagentReport::Started {
            id,
            session_scoped: true,
            metadata,
            ..
        } => Some((id.clone(), metadata.clone())),
        _ => None,
    });
    let (id, metadata) = started.expect("the subagent starts");
    assert!(metadata
        .prompt
        .is_some_and(|prompt| prompt.contains("seed.txt")));
    assert!(reports.iter().any(|report| matches!(report, SubagentReport::Metadata { id: of, metadata } if *of == id && metadata.model.is_some())));
    assert!(reports.iter().any(
        |report| matches!(report, SubagentReport::Activity { id: of, kind: ActivityKind::Tool, .. } if *of == id)
    ));
    assert!(reports.iter().any(|report| matches!(report, SubagentReport::Finished { id: of, status: SubagentTerminalStatus::Completed, .. } if *of == id)));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_background_subagent_reports_into_a_turn_the_agent_starts() {
    let (_dir, root) = workspace("act-claude-agent-turn-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = haiku("Use the Agent tool with run_in_background true to launch one general-purpose subagent whose task is: run `sleep 10` with Bash, then read seed.txt and report its contents. Do not wait for it; reply with the single word launched. When it finishes later, tell me what it reported.");
    turn.agent_turns = true;
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let ended = eventually(Duration::from_secs(120), || {
        !rec.agent_ends.lock().unwrap().is_empty()
    })
    .await;
    assert!(ended, "the agent's own turn ends");
    assert_eq!(
        rec.agent_ends.lock().unwrap()[0].status,
        ResultStatus::Succeeded
    );
    assert!(text(&rec.agent_events.lock().unwrap()).contains("seed"));
    assert!(rec.subagents().iter().any(|report| matches!(
        report,
        SubagentReport::Finished {
            status: SubagentTerminalStatus::Completed,
            ..
        }
    )));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn compacts_on_request_and_keeps_the_conversation() {
    let (_dir, root) = workspace("act-claude-compact-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(haiku("Remember the word lantern. Reply with ok.").input(&root, &rec))
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded);
    let thread = continuation(&rec.take_events());

    let mut turn = haiku("").continuing(thread.clone());
    turn.operation = Some(RunOperation::Compact { pre_tokens: 30_000 });
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.take_events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderEvent::CompactionStatus {
            compacting: true,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(event, ProviderEvent::Compaction { trigger: CompactionTrigger::Manual, pre_tokens, .. } if *pre_tokens > 0)));

    let result = provider
        .execute(
            haiku("Which word did I ask you to remember? Answer with the word only.")
                .continuing(thread)
                .input(&root, &rec),
        )
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded);
    assert!(text(&rec.events()).to_lowercase().contains("lantern"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_steered_message_joins_the_turn_going() {
    let (_dir, root) = workspace("act-claude-steer-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = haiku(
        "Run the shell command `sleep 8 && echo slept` with the Bash tool, then reply with what it printed.",
    );
    turn.on_event = Some(steer_when(
        &turn.steering,
        "Also, end your reply with the word banana.",
        |event| matches!(event, ProviderEvent::Tool { .. }),
    ));
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(events.iter().any(
        |event| matches!(event, ProviderEvent::Steered { message_id } if message_id == "steer-1")
    ));
    assert!(text(&events).to_lowercase().contains("banana"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn stops_a_shell_left_running_in_the_background() {
    let (_dir, root) = workspace("act-claude-background-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = haiku("Start the shell command `sleep 300` with the Bash tool using run_in_background true. Do not wait for it; reply with the single word started.");
    turn.task_id = "background-thread".into();
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let running = || {
        rec.background
            .lock()
            .unwrap()
            .last()
            .and_then(|BackgroundReport::Changed { processes }| processes.first().cloned())
    };
    let shell = running().expect("the shell is reported");
    assert_eq!(shell.kind, BackgroundProcessKind::Shell);
    assert!(provider.stop_process("background-thread", &shell.id));
    assert!(eventually(Duration::from_secs(20), || running().is_none()).await);
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reads_plan_usage_and_titles_a_thread_from_an_image() {
    match read_plan_usage().await {
        PlanUsage::Available { windows, .. } => {
            assert!(windows.iter().any(|window| window.id == "five_hour"));
        }
        PlanUsage::NotApplicable => {}
        other => panic!("{other:?}"),
    }
    let (_dir, root) = workspace("act-claude-title-");
    let image = write_red_png(&root);
    let title = suggest_task_title("What is in this picture?", &[image])
        .await
        .expect("a title");
    println!("title: {title}");
    assert!(title.to_lowercase().contains("red"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reports_a_workflow_from_start_to_finish() {
    let (_dir, root) = workspace("act-claude-workflow-");
    let provider = provider();
    let rec = Recorder::new();
    let workflows = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut turn = haiku("Use the Workflow tool to run a minimal workflow with a single phase containing exactly one agent whose task is: read seed.txt and report its contents in one word. Keep it as small as possible. Wait for it to finish, then reply with what it reported.");
    turn.agent_turns = true;
    let mut input = turn.input(&root, &rec);
    let seen = workflows.clone();
    input.hooks.report_workflow = Arc::new(move |report| seen.lock().unwrap().push(report));
    let result = provider.execute(input).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let finished = || {
        workflows
            .lock()
            .unwrap()
            .iter()
            .any(|report| matches!(report, WorkflowReport::Finished { .. }))
    };
    assert!(eventually(Duration::from_secs(120), finished).await);
    let reports = workflows.lock().unwrap().clone();
    assert!(matches!(
        reports.first(),
        Some(WorkflowReport::Started { .. })
    ));
    assert!(reports.iter().any(
        |report| matches!(report, WorkflowReport::Progress { phases, .. } if !phases.is_empty())
    ));
    assert!(reports.iter().any(|report| matches!(
        report,
        WorkflowReport::Finished {
            status: WorkflowEndStatus::Completed,
            ..
        }
    )));
    provider.close_all();
}
