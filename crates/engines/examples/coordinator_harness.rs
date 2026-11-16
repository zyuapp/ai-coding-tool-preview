//! Runs one run through the RunCoordinator and prints every AgentEvent as JSON lines, allowing
//! approvals and answering questions, for comparing against the TypeScript coordinator.
//! `cargo run -p act_engines --example coordinator_harness -- '<scenario json>'`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_engines::claude::ClaudeAgentProvider;
use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::coordinator::{CoordinatorOptions, RunCoordinator, StartRun};
use act_engines::opencode::OpenCodeAgentProvider;
use act_engines::provider::AgentProvider;
use act_engines::session_pool::SessionPool;
use act_engines::types::*;
use serde_json::Value;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let s: Value = serde_json::from_str(&std::env::args().nth(1).expect("scenario")).unwrap();
    let engine: AgentEngine =
        serde_json::from_value(s["engine"].clone()).unwrap_or(AgentEngine::Claude);
    let pool = SessionPool::default();
    let provider: Arc<dyn AgentProvider> = match engine {
        AgentEngine::Claude => Arc::new(ClaudeAgentProvider::new(pool)),
        AgentEngine::Codex => Arc::new(CodexAgentProvider::new(
            pool,
            CodexProviderOptions::new(None, None),
        )),
        AgentEngine::OpenCode => Arc::new(OpenCodeAgentProvider::new(pool, None)),
    };
    let slot: Arc<Mutex<Option<RunCoordinator>>> = Arc::default();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let answer = s["answer"].as_str().unwrap_or("Blue").to_string();
    let reach = slot.clone();
    let coordinator = RunCoordinator::new(
        provider.clone(),
        Arc::new(move |event| {
            let wire = serde_json::to_value(&event).unwrap();
            println!("{wire}");
            let (task, run) = (
                wire["taskId"].as_str().unwrap_or_default().to_string(),
                wire["runId"].as_str().unwrap_or_default().to_string(),
            );
            match wire["type"].as_str() {
                Some("approval.requested") => {
                    let (reach, id) = (
                        reach.clone(),
                        wire["approvalId"].as_str().unwrap().to_string(),
                    );
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        let coordinator = reach.lock().unwrap().clone().unwrap();
                        coordinator.decide_approval(&task, &run, &id, true);
                    });
                }
                Some("question.requested") => {
                    let (reach, id, answer) = (
                        reach.clone(),
                        wire["requestId"].as_str().unwrap().to_string(),
                        answer.clone(),
                    );
                    let ids: Vec<String> = wire["request"]["questions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|q| q["id"].as_str().unwrap().to_string())
                        .collect();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        let coordinator = reach.lock().unwrap().clone().unwrap();
                        for question in ids {
                            coordinator.answer_question(&task, &run, &id, &question, &answer);
                        }
                    });
                }
                Some("run.status")
                    if matches!(
                        wire["status"].as_str(),
                        Some("succeeded" | "failed" | "cancelled")
                    ) =>
                {
                    let _ = done_tx.send(());
                }
                _ => {}
            }
        }),
        CoordinatorOptions::default(),
    );
    *slot.lock().unwrap() = Some(coordinator.clone());
    coordinator.start(StartRun {
        channel: RunChannel::Main,
        task_id: "coord-thread".into(),
        title: "Coord".into(),
        run_id: "run-1".into(),
        prompt: s["prompt"].as_str().unwrap().into(),
        workspace_root: s["workspace"].as_str().unwrap().into(),
        project_root: None,
        projectless: true,
        computer_use: ComputerUseRunConfig::Unavailable {
            message: "off".into(),
        },
        policy: serde_json::from_value(s["policy"].clone()).unwrap_or(ExecutionPolicy::Confirm),
        engine,
        model: s["model"].as_str().unwrap_or("haiku").into(),
        effort: AgentEffort::Low,
        fast_mode: None,
        operation: None,
        chrome_browser: false,
        continuation: None,
        fork_continuation: false,
        fresh_session: false,
        unattended: false,
        coordination_role: None,
        toolkit: None,
    });
    done_rx.recv().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    coordinator.close_all();
    tokio::time::sleep(Duration::from_millis(1000)).await;
}
