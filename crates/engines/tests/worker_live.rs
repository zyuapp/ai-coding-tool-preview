//! Drives real Claude and Codex runs through the worker and its coordinators, with an app tool
//! served in-process to Claude and over the MCP HTTP host to Codex, and a cancelled run.
//! `cargo test -p act_engines --test worker_live -- --ignored --nocapture --test-threads=1`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_engines::brief::{BriefDialect, BriefRun, Offers, RunToolkit};
use act_engines::coordinator::{AgentEvent, CoordinatorOptions, RunEvent, RunEventKind, StartRun};
use act_engines::tools::{BoundTool, ServedToolSet, ServerGrants, ToolResult};
use act_engines::types::*;
use act_engines::worker::{AgentWorker, RunCommand};
use futures::FutureExt;

struct Threads {
    calls: Arc<AtomicUsize>,
}

impl RunToolkit for Threads {
    fn tool_sets(&self, _run: &act_engines::provider::RunInput) -> Vec<ServedToolSet> {
        let calls = self.calls.clone();
        vec![ServedToolSet {
            server: "aicodingtool-threads".into(),
            grants: ServerGrants::All,
            tools: vec![BoundTool {
                name: "list_threads".into(),
                description: "List the other AICodingTool threads.".into(),
                input_schema: serde_json::json!({ "type": "object", "properties": {} }),
                read_only: true,
                handler: Arc::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { ToolResult::text("Threads: \"Mango migration\" (id t-42), running.") }
                        .boxed()
                }),
            }],
        }]
    }

    fn brief(
        &self,
        offers: Offers<'_>,
        _dialect: &BriefDialect,
        _run: &BriefRun<'_>,
    ) -> Vec<String> {
        if offers("aicodingtool-threads", Some("list_threads")) {
            vec!["Use list_threads to see other threads.".into()]
        } else {
            Vec::new()
        }
    }
}

fn start(
    engine: AgentEngine,
    model: &str,
    root: &str,
    run_id: &str,
    prompt: &str,
    toolkit: Option<Arc<dyn RunToolkit>>,
) -> RunCommand {
    RunCommand::Start(Box::new(StartRun {
        channel: RunChannel::Main,
        task_id: format!("{run_id}-thread"),
        title: "Worker live".into(),
        run_id: run_id.into(),
        prompt: prompt.into(),
        workspace_root: root.into(),
        project_root: None,
        projectless: true,
        computer_use: ComputerUseRunConfig::Unavailable {
            message: "off".into(),
        },
        policy: ExecutionPolicy::Bypass,
        engine,
        model: model.into(),
        effort: AgentEffort::Low,
        fast_mode: None,
        operation: None,
        chrome_browser: false,
        continuation: None,
        fork_continuation: false,
        fresh_session: false,
        unattended: false,
        coordination_role: None,
        toolkit,
    }))
}

fn final_status(events: &[AgentEvent], run_id: &str) -> Option<(RunStatus, Option<String>)> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::Run(RunEvent {
            run_id: id,
            event: RunEventKind::RunStatus {
                status, message, ..
            },
            ..
        }) if id == run_id
            && !matches!(status, RunStatus::Running | RunStatus::AwaitingApproval) =>
        {
            Some((*status, message.clone()))
        }
        _ => None,
    })
}

fn text(events: &[AgentEvent], run_id: &str) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Run(RunEvent {
                run_id: id,
                event: RunEventKind::AssistantDelta { text, .. },
                ..
            }) if id == run_id => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn wait_for(
    events: &Arc<Mutex<Vec<AgentEvent>>>,
    run_id: &str,
    limit: Duration,
) -> (RunStatus, Option<String>) {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if let Some(done) = final_status(&events.lock().unwrap(), run_id) {
            return done;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "run {run_id} did not finish"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn worker_runs_app_tools_on_claude_and_codex_and_cancels() {
    let private = tempfile::Builder::new()
        .prefix("act-engines-worker-codex-")
        .tempdir_in("/tmp")
        .unwrap();
    // SAFETY: set before any Codex launch reads the environment.
    unsafe { std::env::set_var("AICODINGTOOL_CODEX_HOME", private.path().join("home")) };
    let workspace = tempfile::Builder::new()
        .prefix("act-engines-worker-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = std::fs::canonicalize(workspace.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let worker = AgentWorker::new(
        Arc::new(move |event| sink.lock().unwrap().push(event)),
        CoordinatorOptions::default(),
        None,
    );

    let claude_calls = Arc::new(AtomicUsize::new(0));
    let prompt =
        "Call the list_threads tool, then tell me the title of the thread it lists, in quotes.";
    worker.handle(start(
        AgentEngine::Claude,
        "haiku",
        &root,
        "claude-run",
        prompt,
        Some(Arc::new(Threads {
            calls: claude_calls.clone(),
        })),
    ));
    let (status, message) = wait_for(&events, "claude-run", Duration::from_secs(120)).await;
    println!(
        "claude: {status:?} {message:?}\n{}",
        text(&events.lock().unwrap(), "claude-run")
    );
    assert_eq!(status, RunStatus::Succeeded);
    assert!(claude_calls.load(Ordering::SeqCst) > 0);
    assert!(text(&events.lock().unwrap(), "claude-run").contains("Mango"));

    let codex_calls = Arc::new(AtomicUsize::new(0));
    worker.handle(start(
        AgentEngine::Codex,
        "gpt-6-luna",
        &root,
        "codex-run",
        prompt,
        Some(Arc::new(Threads {
            calls: codex_calls.clone(),
        })),
    ));
    let (status, message) = wait_for(&events, "codex-run", Duration::from_secs(180)).await;
    println!(
        "codex: {status:?} {message:?}\n{}",
        text(&events.lock().unwrap(), "codex-run")
    );
    assert_eq!(status, RunStatus::Succeeded);
    assert!(codex_calls.load(Ordering::SeqCst) > 0);
    assert!(text(&events.lock().unwrap(), "codex-run").contains("Mango"));

    worker.handle(start(
        AgentEngine::Claude,
        "haiku",
        &root,
        "cancelled-run",
        "Use Bash to run `sleep 30`, then say finished.",
        None,
    ));
    tokio::time::sleep(Duration::from_secs(4)).await;
    worker.handle(RunCommand::Cancel {
        task_id: "cancelled-run-thread".into(),
        run_id: "cancelled-run".into(),
    });
    let (status, _) = wait_for(&events, "cancelled-run", Duration::from_secs(5)).await;
    assert_eq!(status, RunStatus::Cancelled);
    worker.handle(
        start(
            AgentEngine::Claude,
            "haiku",
            &root,
            "after-cancel",
            "Reply with the word back.",
            None,
        )
        .with_task("cancelled-run-thread"),
    );
    let (status, message) = wait_for(&events, "after-cancel", Duration::from_secs(60)).await;
    println!(
        "after cancel: {status:?} {message:?} {}",
        text(&events.lock().unwrap(), "after-cancel")
    );
    assert_eq!(status, RunStatus::Succeeded);

    worker.close();
    tokio::time::sleep(Duration::from_millis(500)).await;
}

trait WithTask {
    fn with_task(self, task: &str) -> Self;
}

impl WithTask for RunCommand {
    fn with_task(self, task: &str) -> Self {
        match self {
            RunCommand::Start(mut start) => {
                start.task_id = task.into();
                RunCommand::Start(start)
            }
            other => other,
        }
    }
}
