//! Runs real OpenCode turns on the server's default model, with OpenCode's data kept under /tmp.
//! Needs `opencode` with a connected provider:
//! `cargo test -p act_engines --test opencode_live -- --ignored --nocapture`

use std::sync::{Arc, Mutex};

use act_engines::opencode::server::connect_open_code;
use act_engines::opencode::services::OpenCodeServices;
use act_engines::opencode::OpenCodeAgentProvider;
use act_engines::provider::{
    AgentProvider, ProviderEvent, ResultStatus, RunHooks, RunInput, ToolDecision,
};
use act_engines::session_pool::SessionPool;
use act_engines::steer::SteerChannel;
use act_engines::types::*;
use tokio_util::sync::CancellationToken;

fn run_input(
    workspace: &str,
    model: &str,
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
        task_id: "opencode-live".into(),
        title: "OpenCode live test".into(),
        prompt: prompt.into(),
        workspace_root: workspace.into(),
        project_root: None,
        projectless: true,
        computer_use: ComputerUseRunConfig::Unavailable {
            message: "off".into(),
        },
        policy,
        engine: AgentEngine::Opencode,
        model: model.into(),
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
async fn runs_opencode_turns_with_an_approved_edit() {
    let data = tempfile::Builder::new()
        .prefix("act-engines-opencode-data-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = std::env::var("HOME").unwrap();
    std::fs::create_dir_all(data.path().join("share/opencode")).unwrap();
    for file in [
        "auth.json",
        "opencode.db",
        "opencode.db-wal",
        "opencode.db-shm",
    ] {
        std::fs::copy(
            format!("{home}/.local/share/opencode/{file}"),
            data.path().join("share/opencode").join(file),
        )
        .ok();
    }
    // SAFETY: set before any server is started.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", data.path().join("share"));
        std::env::set_var("XDG_STATE_HOME", data.path().join("state"));
        std::env::set_var("XDG_CACHE_HOME", data.path().join("cache"));
    }
    let services = OpenCodeServices::new(connect_open_code());
    let catalog = services.catalog().await.expect("catalog");
    services.close().await;
    println!(
        "models: {:?}",
        catalog
            .iter()
            .map(|spec| spec.id.as_str())
            .collect::<Vec<_>>()
    );
    let model = std::env::var("ACT_OPENCODE_MODEL")
        .unwrap_or_else(|_| catalog.first().expect("a connected provider").id.clone());
    println!("model: {model}");

    let workspace = tempfile::Builder::new()
        .prefix("act-engines-opencode-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = std::fs::canonicalize(workspace.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let provider = OpenCodeAgentProvider::new(SessionPool::default(), None);

    let events = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let first = provider
        .execute(run_input(
            &root,
            &model,
            "Reply with exactly the word ready and nothing else.",
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
        .expect("session id");

    let events = Arc::new(Mutex::new(Vec::new()));
    let second = provider
        .execute(run_input(&root, &model, "Create a file named made.txt in the current directory containing the word pear, using your write tool. Then reply with done.", ExecutionPolicy::Confirm, Some(continuation), events.clone(), asked.clone()))
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
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}
