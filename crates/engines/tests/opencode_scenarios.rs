//! Real OpenCode turns with OpenCode's data copied to /tmp/act-opencode-scenarios. Needs `opencode`
//! with a connected provider; `ACT_OPENCODE_MODEL` picks the model, else the catalog's first:
//! `cargo test -p act_engines --test opencode_scenarios -- --ignored --nocapture --test-threads=1`

mod common;

use std::sync::Once;
use std::time::Duration;

use act_engines::opencode::server::connect_open_code;
use act_engines::opencode::services::OpenCodeServices;
use act_engines::opencode::OpenCodeAgentProvider;
use act_engines::provider::{AgentProvider, ProviderEvent, ResultStatus, ToolDecision};
use act_engines::session_pool::SessionPool;
use act_engines::types::*;
use common::*;

/// Points OpenCode at a copy of the user's data and names the model the tests run on.
async fn model() -> String {
    static DATA: Once = Once::new();
    DATA.call_once(|| {
        let data = std::path::Path::new("/tmp/act-opencode-scenarios");
        let home = std::env::var("HOME").unwrap();
        let _ = std::fs::remove_dir_all(data);
        std::fs::create_dir_all(data.join("share/opencode")).unwrap();
        for file in [
            "auth.json",
            "opencode.db",
            "opencode.db-wal",
            "opencode.db-shm",
        ] {
            std::fs::copy(
                format!("{home}/.local/share/opencode/{file}"),
                data.join("share/opencode").join(file),
            )
            .ok();
        }
        // SAFETY: set once, before any OpenCode server starts.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", data.join("share"));
            std::env::set_var("XDG_STATE_HOME", data.join("state"));
            std::env::set_var("XDG_CACHE_HOME", data.join("cache"));
        }
    });
    if let Ok(model) = std::env::var("ACT_OPENCODE_MODEL") {
        return model;
    }
    let services = OpenCodeServices::new(connect_open_code());
    let catalog = services.catalog().await.expect("catalog");
    services.close().await;
    catalog.first().expect("a connected provider").id.clone()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn denies_interrupts_steers_and_asks_on_one_thread() {
    let model = model().await;
    let (dir, root) = workspace("act-opencode-scenarios-");
    let provider = OpenCodeAgentProvider::new(SessionPool::default(), None);
    let rec = Recorder::new();
    let turn = |prompt: &str| Turn::new(AgentEngine::OpenCode, &model, prompt);

    let mut denied = turn("Use your bash tool to run `echo denied-test > d.txt`. If it is refused, reply with the single word refused.")
        .policy(ExecutionPolicy::Confirm);
    denied.decision = ToolDecision::Deny;
    let result = provider.execute(denied.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(!rec.asked.lock().unwrap().is_empty(), "a confirm run asks");
    assert!(!dir.path().join("d.txt").exists());
    let thread = continuation(&rec.take_events()).expect("session id");

    let mut cancelled = turn(
        "Use your bash tool to run `sleep 30 && echo finished`, then tell me what it printed.",
    )
    .continuing(Some(thread.clone()));
    cancelled.on_event = Some(cancel_when(&cancelled.cancel, |event| {
        matches!(event, ProviderEvent::Tool { .. })
    }));
    let started = std::time::Instant::now();
    let result = provider.execute(cancelled.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Cancelled, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(25));
    rec.take_events();

    let mut steered = turn("Use your bash tool to run `sleep 6`, then reply with the word again.")
        .continuing(Some(thread.clone()));
    steered.on_event = Some(steer_when(
        &steered.steering,
        "Also add the word banana to your reply.",
        |event| matches!(event, ProviderEvent::Tool { .. }),
    ));
    let result = provider.execute(steered.input(&root, &rec)).await;
    let events = rec.take_events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(events
        .iter()
        .any(|event| matches!(event, ProviderEvent::Steered { .. })));

    let mut asked = turn("Ask me which color I prefer using your question tool, offering red and blue. Then reply naming the color I chose.")
        .continuing(Some(thread));
    asked.answer = Some("Blue".into());
    let result = provider.execute(asked.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let questions = rec.questions.lock().unwrap().clone();
    assert!(questions.first().is_some_and(|request| request.blocking));
    assert!(text(&rec.events()).to_lowercase().contains("blue"));
    provider.close_all();
    tokio::time::sleep(Duration::from_secs(1)).await;
}
