//! Real Codex turns on a cheap model, in a private Codex home under /tmp. Needs `codex` signed in:
//! `cargo test -p act_engines --test codex_scenarios -- --ignored --nocapture --test-threads=1`
//! Codex cannot open one fresh home from several sessions at once, so they run in turn.

mod common;

use std::sync::Once;
use std::time::Duration;

use act_engines::codex::{CodexAgentProvider, CodexProviderOptions};
use act_engines::provider::{AgentProvider, ProviderEvent, ResultStatus};
use act_engines::session_pool::SessionPool;
use act_engines::types::*;
use common::*;

const MODEL: &str = "gpt-6-luna";

/// One private Codex home under /tmp, kept between runs so later runs start warm.
fn provider() -> CodexAgentProvider {
    static HOME: Once = Once::new();
    HOME.call_once(|| {
        // SAFETY: set once, before any Codex launch reads the environment.
        unsafe { std::env::set_var("AICODINGTOOL_CODEX_HOME", "/tmp/act-codex-scenarios-home") };
    });
    CodexAgentProvider::new(
        SessionPool::default(),
        CodexProviderOptions::new(None, None),
    )
}

fn luna(prompt: &str) -> Turn {
    Turn::new(AgentEngine::Codex, MODEL, prompt)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn asks_the_user_and_answers_with_their_choice() {
    let (_dir, root) = workspace("act-codex-question-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = luna("Before doing anything, ask me which color I prefer using your request_user_input tool, offering red and blue. Then reply with a sentence naming the color I chose.");
    turn.answer = Some("Blue".into());
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let questions = rec.questions.lock().unwrap().clone();
    let request = questions.first().expect("a question");
    assert!(!request.blocking);
    assert!(request.questions[0]
        .options
        .iter()
        .any(|option| option.label.to_lowercase() == "blue"));
    assert!(text(&rec.events()).to_lowercase().contains("blue"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn an_interrupted_turn_is_cancelled_and_the_thread_goes_on() {
    let (_dir, root) = workspace("act-codex-interrupt-");
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = luna("My secret word is walnut. Run the shell command `sleep 30 && echo finished` and tell me what it printed.");
    turn.on_event = Some(cancel_when(&turn.cancel, |event| {
        matches!(event, ProviderEvent::Tool { .. })
    }));
    let started = std::time::Instant::now();
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Cancelled, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(25));
    let thread = continuation(&rec.take_events()).expect("thread id");

    let result = provider
        .execute(
            luna("What is my secret word? Answer with the word only.")
                .continuing(Some(thread))
                .input(&root, &rec),
        )
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(text(&rec.events()).to_lowercase().contains("walnut"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn compacts_on_request_and_takes_a_steered_message() {
    let (_dir, root) = workspace("act-codex-compact-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(luna("Remember the word lantern. Reply with ok.").input(&root, &rec))
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded);
    let thread = continuation(&rec.take_events());

    let mut turn = luna("").continuing(thread.clone());
    turn.operation = Some(RunOperation::Compact { pre_tokens: 20_000 });
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.take_events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderEvent::CompactionStatus {
            compacting: true,
            ..
        }
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, ProviderEvent::Compaction { .. })));

    let mut turn =
        luna("Run the shell command `sleep 5`, then tell me which word I asked you to remember.")
            .continuing(thread);
    turn.on_event = Some(steer_when(
        &turn.steering,
        "Also end your reply with the word banana.",
        |event| matches!(event, ProviderEvent::Tool { .. }),
    ));
    let result = provider.execute(turn.input(&root, &rec)).await;
    let events = rec.events();
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Steered { .. })),
        "{events:#?}"
    );
    let reply = text(&events).to_lowercase();
    assert!(
        reply.contains("lantern") && reply.contains("banana"),
        "{reply}"
    );
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reports_a_subagent_it_spawns() {
    let (_dir, root) = workspace("act-codex-subagent-");
    let provider = provider();
    let rec = Recorder::new();
    let result = provider
        .execute(luna("You must delegate: call your spawn_agent tool to start exactly one subagent with the task: read seed.txt and report its contents. Do not read the file yourself. Wait for the subagent, then tell me what it reported.").input(&root, &rec))
        .await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let reports = rec.subagents();
    let id = reports
        .iter()
        .find_map(|report| match report {
            SubagentReport::Started { id, .. } => Some(id.clone()),
            _ => None,
        })
        .expect("the subagent starts");
    assert!(reports.iter().any(|report| matches!(report, SubagentReport::Metadata { id: of, metadata } if *of == id && metadata.model.as_deref() == Some(MODEL))));
    assert!(reports.iter().any(
        |report| matches!(report, SubagentReport::Activity { id: of, kind: ActivityKind::Tool, .. } if *of == id)
    ));
    assert!(reports.iter().any(|report| matches!(report, SubagentReport::Status { id: of, status: SubagentLiveStatus::Idle, summary: Some(_) } if *of == id)));
    assert!(text(&rec.events()).contains("seed"));
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn reviews_uncommitted_changes() {
    let (dir, root) = workspace("act-codex-review-");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap()
    };
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() { println!(\"{}\", 6 / 3); }\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() { println!(\"{}\", 6 / 0); }\n",
    )
    .unwrap();
    let provider = provider();
    let rec = Recorder::new();
    let mut turn = luna("");
    turn.operation = Some(RunOperation::Review {
        target: serde_json::json!({ "type": "uncommittedChanges" }),
    });
    let result = provider.execute(turn.input(&root, &rec)).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let review = text(&rec.events());
    assert!(review.contains("main.rs"), "{review}");
    provider.close_all();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn works_toward_a_goal_until_it_is_met() {
    let (dir, root) = workspace("act-codex-goal-");
    let provider = provider();
    let rec = Recorder::new();
    let goals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut input = luna(
        "/goal a file named goal.txt containing the word done exists in the current directory",
    )
    .input(&root, &rec);
    let seen = goals.clone();
    input.hooks.report_goal = std::sync::Arc::new(move |report| seen.lock().unwrap().push(report));
    let result = provider.execute(input).await;
    assert_eq!(result.status, ResultStatus::Succeeded, "{result:?}");
    let goals = goals.lock().unwrap().clone();
    assert!(goals
        .iter()
        .any(
            |GoalReport::Changed { goal }| goal.as_ref().is_some_and(|goal| goal.status
                == GoalStatus::Active
                && goal.objective.contains("goal.txt"))
        ));
    assert!(matches!(
        goals.last(),
        Some(GoalReport::Changed { goal: None })
    ));
    assert!(std::fs::read_to_string(dir.path().join("goal.txt"))
        .unwrap()
        .contains("done"));
    provider.close_all();
}
