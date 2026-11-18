//! Asks the installed engines what the app asks outside a run: commands, models, a title, plan usage
//! and readiness. Codex and OpenCode keep their state under /tmp.
//! `cargo test -p act_engines --test services_live -- --ignored --nocapture`

use act_engines::engines::engine;
use act_engines::types::{AgentEngine, EngineAccess, PlanUsage};

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn engines_answer_outside_a_run() {
    let scratch = tempfile::Builder::new()
        .prefix("act-engines-services-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = std::env::var("HOME").unwrap();
    std::fs::create_dir_all(scratch.path().join("share/opencode")).unwrap();
    for file in [
        "auth.json",
        "opencode.db",
        "opencode.db-wal",
        "opencode.db-shm",
    ] {
        std::fs::copy(
            format!("{home}/.local/share/opencode/{file}"),
            scratch.path().join("share/opencode").join(file),
        )
        .ok();
    }
    // SAFETY: set before any engine process starts.
    unsafe {
        std::env::set_var("AICODINGTOOL_CODEX_HOME", scratch.path().join("codex"));
        std::env::set_var("XDG_DATA_HOME", scratch.path().join("share"));
        std::env::set_var("XDG_STATE_HOME", scratch.path().join("state"));
        std::env::set_var("XDG_CACHE_HOME", scratch.path().join("cache"));
    }
    let workspace = scratch.path().to_string_lossy().into_owned();

    let claude_engine = engine(AgentEngine::Claude).unwrap();
    let claude = claude_engine.readiness().await;
    println!("claude readiness: {claude:?}");
    assert_eq!(claude.access, EngineAccess::Ready);
    assert!(claude.version.is_some());

    let commands = claude_engine.commands(&workspace, true).await.unwrap();
    println!("claude commands: {}", commands.len());
    assert!(commands.iter().any(|command| command.name == "compact"));

    let title = claude_engine
        .suggest_title(
            "Please fix the flaky login test in the auth service",
            &[],
            None,
        )
        .await;
    println!("claude title: {title:?}");
    assert!(title.is_some_and(|title| !title.is_empty() && title.chars().count() <= 52));

    let usage = claude_engine.plan_usage().await;
    println!("claude usage: {usage:?}");
    assert!(!matches!(usage, PlanUsage::Unavailable { .. }));

    let codex = engine(AgentEngine::Codex)
        .unwrap()
        .commands(&workspace, true)
        .await
        .unwrap();
    println!(
        "codex commands: {:?}",
        codex
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(codex[0].name, "goal");

    let opencode_engine = engine(AgentEngine::Opencode).unwrap();
    let opencode = opencode_engine.commands(&workspace, true).await.unwrap();
    println!("opencode commands: {}", opencode.len());
    let ready = opencode_engine.readiness().await;
    println!(
        "opencode readiness: {:?} {:?}",
        ready.access,
        ready.catalog.as_ref().map(Vec::len)
    );
    assert_eq!(ready.access, EngineAccess::Ready);
    act_engines::engines::close_engine_services().await;
}
