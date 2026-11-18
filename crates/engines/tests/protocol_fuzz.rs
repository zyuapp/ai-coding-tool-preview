//! Engine processes are outside the app: whatever they print must not panic it. A stand-in Claude
//! Code and Codex app-server (`fake_engine.py`, needs python3) answer each turn with seeded, mutated
//! protocol messages; every turn must settle and nothing may panic.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use act_engines::claude::ClaudeAgentProvider;
use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::provider::AgentProvider;
use act_engines::session_pool::SessionPool;
use act_engines::types::*;
use common::*;

const TURNS: u64 = 60;

async fn turns(engine: AgentEngine, provider: &dyn AgentProvider, root: &str) {
    for seed in 1..=TURNS {
        std::env::set_var("FAKE_SEED", seed.to_string());
        let recorder = Recorder::new();
        let mut turn = Turn::new(engine, if engine == AgentEngine::Claude { "haiku" } else { "gpt-6-luna" }, "hello");
        turn.task_id = format!("fuzz-{engine:?}-{seed}");
        let settled = tokio::time::timeout(Duration::from_secs(30), provider.execute(turn.input(root, &recorder))).await;
        assert!(settled.is_ok(), "{engine:?} turn with seed {seed} never settled");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_engine_output_never_panics() {
    let panics = Arc::new(AtomicUsize::new(0));
    let counted = panics.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        counted.fetch_add(1, Ordering::SeqCst);
        previous(info);
    }));
    fake_engines();
    let (_dir, root) = workspace("act-fuzz-");

    let claude = ClaudeAgentProvider::new(SessionPool::default());
    turns(AgentEngine::Claude, &claude, &root).await;
    claude.close_all();

    let codex = CodexAgentProvider::new(SessionPool::default(), CodexProviderOptions::new(None, None));
    turns(AgentEngine::Codex, &codex, &root).await;
    codex.close_all();

    assert_eq!(panics.load(Ordering::SeqCst), 0, "engine output panicked the app");
}
