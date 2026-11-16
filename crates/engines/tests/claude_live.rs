//! Runs real Claude turns with a cheap model. Needs the `claude` command signed in:
//! `cargo test -p act_engines --test claude_live -- --ignored --nocapture`

use std::sync::{Arc, Mutex};

use act_engines::claude::ClaudeAgentProvider;
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
        task_id: "live-thread".into(),
        title: "Live test".into(),
        prompt: prompt.into(),
        workspace_root: workspace.into(),
        project_root: None,
        projectless: true,
        computer_use: ComputerUseRunConfig::Unavailable {
            message: "off".into(),
        },
        policy: ExecutionPolicy::Confirm,
        engine: AgentEngine::Claude,
        model: "haiku".into(),
        effort: AgentEffort::Low,
        fast_mode: None,
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

fn assistant_text(events: &[ProviderEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn runs_two_turns_on_one_warm_session_with_an_approved_write() {
    let workspace = tempfile::Builder::new()
        .prefix("act-engines-claude-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = std::fs::canonicalize(workspace.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let provider = ClaudeAgentProvider::new(SessionPool::default());

    let events = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let first = provider
        .execute(run_input(&root, "Use the Write tool to create a file named note.txt in the current directory containing exactly the word pineapple. Then reply with the single word done.", None, events.clone(), asked.clone()))
        .await;
    let seen = events.lock().unwrap().clone();
    println!(
        "first: {first:?}\ntext: {}\nasked: {:?}",
        assistant_text(&seen),
        asked.lock().unwrap()
    );
    assert_eq!(first.status, ResultStatus::Succeeded, "{first:?}");
    assert!(asked
        .lock()
        .unwrap()
        .iter()
        .any(|intent| intent.name == "Write"
            && intent
                .write_path
                .as_deref()
                .is_some_and(|path| path.ends_with("note.txt"))));
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("note.txt"))
            .unwrap()
            .trim(),
        "pineapple"
    );
    let continuation = seen.iter().find_map(|event| match event {
        ProviderEvent::Continuation(continuation) => Some(continuation.clone()),
        _ => None,
    });
    let continuation = continuation.expect("the session reports its id");
    assert_eq!(continuation.provider, "claude");
    assert!(seen.iter().any(
        |event| matches!(event, ProviderEvent::Usage { tokens, limit: 200_000, .. } if *tokens > 0)
    ));
    assert!(seen
        .iter()
        .any(|event| matches!(event, ProviderEvent::Spend { .. })));
    assert!(seen
        .iter()
        .any(|event| matches!(event, ProviderEvent::Tool { intent } if intent.name == "Write")));

    let events = Arc::new(Mutex::new(Vec::new()));
    let second = provider
        .execute(run_input(
            &root,
            "What word did you write into note.txt? Answer with that word only.",
            Some(continuation.clone()),
            events.clone(),
            asked.clone(),
        ))
        .await;
    let text = assistant_text(&events.lock().unwrap());
    println!("second: {second:?}\ntext: {text}");
    assert_eq!(second.status, ResultStatus::Succeeded);
    assert!(text.to_lowercase().contains("pineapple"));
    assert!(
        events.lock().unwrap().iter().all(
            |event| !matches!(event, ProviderEvent::Continuation(other) if *other != continuation)
        ),
        "the warm session continues the same conversation"
    );
    provider.close_all();
}
