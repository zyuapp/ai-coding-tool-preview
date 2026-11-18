//! Turns on stand-in engines (`fake_engine.py`, needs python3) for what every engine's session
//! reports the same way.

mod common;

use std::time::Duration;

use act_engines::claude::ClaudeAgentProvider;
use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::provider::{AgentProvider, ProviderEvent, ResultStatus};
use act_engines::session_pool::SessionPool;
use act_engines::steer::SteerMessage;
use act_engines::types::*;
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_the_engine_no_longer_has_is_reported_lost() {
    fake_engines();
    let (_dir, root) = workspace("act-fake-lost-");
    let claude = ClaudeAgentProvider::new(SessionPool::default());
    let codex = CodexAgentProvider::new(
        SessionPool::default(),
        CodexProviderOptions::new(None, None),
    );
    let engines: [(AgentEngine, &str, &dyn AgentProvider); 2] = [
        (AgentEngine::Claude, "haiku", &claude),
        (AgentEngine::Codex, "gpt-6-luna", &codex),
    ];
    for (engine, model, provider) in engines {
        let recorder = Recorder::new();
        let turn = Turn::new(engine, model, "hello")
            .continuing(Some(Continuation::new(engine, "lost-thread")));
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            provider.execute(turn.input(&root, &recorder)),
        )
        .await
        .unwrap();
        assert_eq!(result.status, ResultStatus::Failed, "{engine:?}: {result:?}");
        assert!(
            recorder
                .events()
                .iter()
                .any(|event| matches!(event, ProviderEvent::ContinuationLost)),
            "{engine:?} did not report the continuation lost"
        );
        provider.close_all();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_steered_message_the_engine_cannot_take_is_reported_failed() {
    fake_engines();
    let (_dir, root) = workspace("act-fake-steer-");
    let provider = ClaudeAgentProvider::new(SessionPool::default());
    let recorder = Recorder::new();
    let turn = Turn::new(AgentEngine::Claude, "haiku", "close input");
    let steering = turn.steering.clone();
    let run = tokio::spawn({
        let input = turn.input(&root, &recorder);
        let provider = provider.clone();
        async move { provider.execute(input).await }
    });
    let events = || recorder.events();
    assert!(
        eventually(Duration::from_secs(20), || events()
            .iter()
            .any(|event| matches!(event, ProviderEvent::AssistantTail { .. })))
        .await
    );
    let steer = |id: &str| {
        steering.push(SteerMessage {
            message_id: id.into(),
            prompt: "and then".into(),
        })
    };
    // The first write after the engine stops reading is the one that finds its input closed.
    steer("first");
    tokio::time::sleep(Duration::from_millis(500)).await;
    steer("second");
    assert!(
        eventually(Duration::from_secs(5), || events().iter().any(|event| matches!(
            event,
            ProviderEvent::SteerFailed { message_id, .. } if message_id == "second"
        )))
        .await,
        "a steer Claude could not take was dropped"
    );
    provider.close_all();
    let _ = tokio::time::timeout(Duration::from_secs(20), run).await;
}
