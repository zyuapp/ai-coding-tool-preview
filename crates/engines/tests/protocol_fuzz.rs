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

fn fake_bin(dir: &std::path::Path) {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_engine.py");
    for name in ["claude", "codex"] {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexec python3 -I {script} \"$@\"\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

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
    let bin = tempfile::tempdir().unwrap();
    fake_bin(bin.path());
    let home = tempfile::tempdir().unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", path);
    std::env::set_var("CODEX_HOME", home.path().join("codex"));
    std::env::set_var("AICODINGTOOL_CODEX_HOME", home.path().join("private"));
    std::fs::create_dir_all(home.path().join("codex")).unwrap();
    let (_dir, root) = workspace("act-fuzz-");

    std::env::set_var("FAKE_ENGINE", "claude");
    let claude = ClaudeAgentProvider::new(SessionPool::default());
    turns(AgentEngine::Claude, &claude, &root).await;
    claude.close_all();

    std::env::set_var("FAKE_ENGINE", "codex");
    let codex = CodexAgentProvider::new(SessionPool::default(), CodexProviderOptions::new(None, None));
    turns(AgentEngine::Codex, &codex, &root).await;
    codex.close_all();

    assert_eq!(panics.load(Ordering::SeqCst), 0, "engine output panicked the app");
}
