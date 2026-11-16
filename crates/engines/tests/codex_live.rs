//! Runs real Codex turns with a cheap model in a private Codex home under /tmp. Needs `codex`
//! signed in: `cargo test -p act_engines --test codex_live -- --ignored --nocapture`

use std::sync::{Arc, Mutex};

use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::provider::{
    AgentProvider, ProviderEvent, ResultStatus, RunHooks, RunInput, ToolDecision,
};
use act_engines::session_pool::SessionPool;
use act_engines::steer::SteerChannel;
use act_engines::types::*;
use tokio_util::sync::CancellationToken;

fn run_input(
    workspace: &str,
    prompt: &str,
    policy: ExecutionPolicy,
    continuation: Option<Continuation>,
    events: Arc<Mutex<Vec<ProviderEvent>>>,
    asked: Arc<Mutex<Vec<ToolIntent>>>,
) -> RunInput {
    let mut hooks = RunHooks::silent();
    hooks.emit = Arc::new(move |event| events.lock().unwrap().push(event));
    hooks.authorize = Arc::new(move |intent| {
        asked.lock().unwrap().push(intent);
        Box::pin(async { ToolDecision::Allow })
    });
    RunInput {
        channel: RunChannel::Main,
        task_id: "codex-live".into(),
        title: "Codex live test".into(),
        prompt: prompt.into(),
        workspace_root: workspace.into(),
        project_root: None,
        projectless: true,
        computer_use: ComputerUseRunConfig::Unavailable {
            message: "off".into(),
        },
        policy,
        engine: AgentEngine::Codex,
        model: "gpt-6-luna".into(),
        effort: AgentEffort::Low,
        fast_mode: Some(false),
        operation: None,
        chrome_browser: false,
        continuation,
        fork_continuation: false,
        fresh_session: false,
        coordination_role: None,
        toolkit: None,
        steering: SteerChannel::new(),
        cancel: CancellationToken::new(),
        hooks,
    }
}

fn text_of(events: &[ProviderEvent]) -> String {
    events
        .iter()
        .filter_map(|event| {
            if let ProviderEvent::Assistant { text, .. } = event {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn runs_codex_turns_with_an_approved_command() {
    let private = tempfile::Builder::new()
        .prefix("act-engines-codex-home-")
        .tempdir_in("/tmp")
        .unwrap();
    // SAFETY: set before any thread reads the environment for a Codex launch.
    unsafe { std::env::set_var("AICODINGTOOL_CODEX_HOME", private.path().join("home")) };
    let workspace = tempfile::Builder::new()
        .prefix("act-engines-codex-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = std::fs::canonicalize(workspace.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let provider = CodexAgentProvider::new(
        SessionPool::default(),
        CodexProviderOptions::new(None, None),
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let first = provider
        .execute(run_input(
            &root,
            "Reply with exactly the word ready.",
            ExecutionPolicy::Bypass,
            None,
            events.clone(),
            asked.clone(),
        ))
        .await;
    let seen = events.lock().unwrap().clone();
    println!("first: {first:?}\ntext: {}", text_of(&seen));
    assert_eq!(first.status, ResultStatus::Succeeded, "{first:?}");
    assert!(text_of(&seen).to_lowercase().contains("ready"));
    let continuation = seen
        .iter()
        .find_map(|event| {
            if let ProviderEvent::Continuation(c) = event {
                Some(c.clone())
            } else {
                None
            }
        })
        .expect("thread id");
    assert!(seen
        .iter()
        .any(|event| matches!(event, ProviderEvent::Usage { tokens, .. } if *tokens > 0)));

    let events = Arc::new(Mutex::new(Vec::new()));
    let second = provider
        .execute(run_input(&root, "Run the shell command `touch made.txt` in the current directory, then reply with done.", ExecutionPolicy::Confirm, Some(continuation), events.clone(), asked.clone()))
        .await;
    println!(
        "second: {second:?}\ntext: {}\nasked: {:?}",
        text_of(&events.lock().unwrap()),
        asked.lock().unwrap()
    );
    assert_eq!(second.status, ResultStatus::Succeeded);
    assert!(
        !asked.lock().unwrap().is_empty(),
        "a confirm run asks before writing"
    );
    assert!(workspace.path().join("made.txt").exists());
    provider.close_all();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}
