//! Prints what one engine's services answer outside a run, as JSON lines, for comparing against the
//! TypeScript services. `cargo run -p act_engines --example services_harness -- <engine> <workspace> [image]`

use act_engines::engines::{
    close_engine_services, engine_commands, plan_usage, readiness, suggest_title,
};
use act_engines::types::AgentEngine;
use serde_json::json;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let engine: AgentEngine = serde_json::from_value(json!(args[1])).unwrap();
    let workspace = &args[2];
    let images: Vec<String> = args.iter().skip(3).cloned().collect();
    let model = std::env::var("TITLE_MODEL").ok();
    let out = |k: &str, v: serde_json::Value| println!("{}", json!({ "k": k, "v": v }));
    out(
        "usage",
        serde_json::to_value(plan_usage(engine).await).unwrap(),
    );
    out(
        "commands",
        match engine_commands(engine, workspace, true).await {
            Ok(commands) => serde_json::to_value(commands).unwrap(),
            Err(error) => json!({ "error": error.to_string() }),
        },
    );
    out(
        "title",
        json!(
            suggest_title(
                engine,
                "Fix the login button that does nothing when clicked on Safari",
                &images,
                model.as_deref()
            )
            .await
        ),
    );
    out(
        "readiness",
        serde_json::to_value(readiness(engine).await).unwrap(),
    );
    close_engine_services().await;
}
