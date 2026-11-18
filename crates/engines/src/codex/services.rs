//! What the app asks of Codex outside a run: access and sign-in, readiness, a thread title, and a
//! saved subagent's settings.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::binary::{installed_engine, upgrade_engine, version_readiness, CODEX_BINARY};
use crate::title::{readable_images, title_of, title_question, title_schema, TITLE_INSTRUCTIONS};
use crate::types::{AvailableCommand, EngineAccess, EngineReadiness, SubagentMetadata};
use crate::version::is_older_than;

use super::app_server::{client_info, AppServerClient};
use super::config::toml;
use super::home::{private_codex_home, shared_codex_home};
use super::home_config::read_home_config;
use super::server::{codex_app_server, codex_executable, ServerOptions};
use super::skills::discover_codex_commands;

/// The `codex --version` the app's protocol knowledge was taken from. An older command is turned away.
pub const CODEX_PROTOCOL_VERSION: &str = "0.169.0";
/// How long the browser has to bring the sign-in back before it is given up on.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TITLE_MODEL: &str = "gpt-6-luna";
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

async fn account_access(client: &AppServerClient) -> anyhow::Result<EngineAccess> {
    let account = client
        .request("account/read", json!({ "refreshToken": false }))
        .await?;
    Ok(if account["account"].is_null() {
        EngineAccess::SignedOut
    } else {
        EngineAccess::Ready
    })
}

/// Whether Codex can take a run, asked of a short-lived app server.
pub async fn read_codex_access() -> EngineAccess {
    let Ok(command) = codex_app_server(
        &[],
        ServerOptions {
            shared_home: true,
            ..Default::default()
        },
    )
    .await
    else {
        return EngineAccess::Unavailable;
    };
    let client = AppServerClient::connect(command);
    let access = async {
        client.initialize(client_info()).await?;
        account_access(&client).await
    }
    .await
    .unwrap_or(EngineAccess::Unavailable);
    let closing = client.clone();
    tokio::spawn(async move {
        closing.close().await;
    });
    access
}

/// The browser sign-in Codex runs itself: it hands back a URL to open and says when the browser has
/// come back. The server outlives the round trip, since it is what the browser returns to.
pub async fn sign_in_to_codex(
    open_url: impl FnOnce(String) -> BoxFuture<'static, anyhow::Result<()>>,
) -> anyhow::Result<EngineAccess> {
    let client = AppServerClient::connect(
        codex_app_server(
            &[],
            ServerOptions {
                shared_home: true,
                ..Default::default()
            },
        )
        .await?,
    );
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Value>();
    let done = Mutex::new(Some(done_tx));
    let listener = client.on("account/login/completed", move |params| {
        if let Some(done) = done.lock().unwrap().take() {
            let _ = done.send(params);
        }
    });
    let outcome = async {
        client.initialize(client_info()).await?;
        let started = client.request("account/login/start", json!({ "type": "chatgpt" })).await?;
        if started["type"] != "chatgpt" {
            anyhow::bail!("Codex offered no browser sign-in.");
        }
        open_url(started["authUrl"].as_str().unwrap_or_default().to_string()).await?;
        let completed = tokio::select! {
            completed = done_rx => completed.map_err(|_| anyhow::anyhow!("Codex stopped before the sign-in finished."))?,
            _ = client.exited() => anyhow::bail!("Codex stopped before the sign-in finished."),
            _ = tokio::time::sleep(SIGN_IN_TIMEOUT) => anyhow::bail!("Codex sign-in timed out. Try again."),
        };
        if completed["success"] != true {
            anyhow::bail!("{}", completed["error"].as_str().unwrap_or("Codex sign-in failed."));
        }
        account_access(&client).await
    }
    .await;
    client.off(listener);
    let closing = client.clone();
    tokio::spawn(async move {
        closing.close().await;
    });
    outcome
}

/// Codex speaks the protocol its own version generated, so a command older than the app's copy is
/// turned away rather than asked to answer messages it lacks.
pub async fn read_codex_readiness() -> EngineReadiness {
    let Some(installed) = installed_engine(&CODEX_BINARY).await else {
        return EngineReadiness {
            fix: Some(CODEX_BINARY.install.into()),
            ..EngineReadiness::of(EngineAccess::Missing)
        };
    };
    if is_older_than(installed.version.as_deref(), CODEX_PROTOCOL_VERSION) {
        return version_readiness(
            &CODEX_BINARY,
            &installed,
            CODEX_PROTOCOL_VERSION,
            EngineAccess::Outdated,
        )
        .await;
    }
    let (access, version) = tokio::join!(
        read_codex_access(),
        version_readiness(
            &CODEX_BINARY,
            &installed,
            CODEX_PROTOCOL_VERSION,
            EngineAccess::Ready
        )
    );
    EngineReadiness { access, ..version }
}

pub async fn update_codex() -> anyhow::Result<()> {
    upgrade_engine(&CODEX_BINARY).await
}

/// The composer's commands: the native goal command, then the workspace's skills.
pub async fn codex_commands(workspace_root: &str) -> anyhow::Result<Vec<AvailableCommand>> {
    let mut commands = vec![AvailableCommand {
        name: "goal".into(),
        description: "Set a goal — keep working until the condition is met".into(),
        argument_hint: "condition".into(),
        aliases: None,
    }];
    commands.extend(discover_codex_commands(workspace_root).await?);
    Ok(commands)
}

/// One-shot, read-only, ephemeral, and blind to the user's own Codex config.
pub fn codex_title_args(schema_file: &str, output_file: &str, images: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--skip-git-repo-check",
        "-s",
        "read-only",
        "-m",
        TITLE_MODEL,
        "-c",
        "model_reasoning_effort=\"low\"",
        "--output-schema",
        schema_file,
        "--output-last-message",
        output_file,
    ]
    .map(String::from)
    .to_vec();
    for image in images {
        args.push("-i".into());
        args.push(image.clone());
    }
    args.push("-".into());
    args
}

async fn exec_codex(args: Vec<String>, input: String, cwd: &Path) -> anyhow::Result<()> {
    let environment: HashMap<String, String> = act_platform::process::environment();
    let private_home = private_codex_home(&environment).await?;
    let mut env = environment.clone();
    let shared = shared_codex_home(&environment);
    env.insert("CODEX_HOME".into(), shared.clone());
    let config = read_home_config(&shared).await.unwrap_or(json!({}));
    let mut overrides = Vec::new();
    if let Some(store) = config["cli_auth_credentials_store"].as_str() {
        overrides.extend([
            "-c".to_string(),
            format!("cli_auth_credentials_store={}", toml(&json!(store))),
        ]);
    }
    if let Some(secret) = config["features"]["secret_auth_storage"].as_bool() {
        overrides.extend([
            "-c".to_string(),
            format!("features.secret_auth_storage={secret}"),
        ]);
    }
    if let Some(home) = private_home {
        let home = home.to_string_lossy().into_owned();
        overrides.extend([
            "-c".to_string(),
            format!("sqlite_home={}", toml(&json!(home))),
            "-c".to_string(),
            format!("log_dir={}", toml(&json!(format!("{home}/log")))),
        ]);
    }
    let mut full = args[..args.len() - 1].to_vec();
    full.extend(overrides);
    full.push(args[args.len() - 1].clone());
    let mut child = act_platform::process::command(codex_executable()?)
        .args(full)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes()).await;
    }
    let _ = tokio::time::timeout(EXEC_TIMEOUT, child.wait()).await;
    Ok(())
}

/// Names a thread from its first message and its screenshots with `codex exec`, read back from
/// the file it writes its last message to, shaped by a schema that allows nothing but a title.
pub async fn suggest_codex_title(text: &str, attachments: &[String]) -> Option<String> {
    let images = readable_images(attachments).await;
    if text.trim().is_empty() && images.is_empty() {
        return None;
    }
    let directory = tempfile::Builder::new()
        .prefix("aicodingtool-title-")
        .tempdir()
        .ok()?;
    let schema_file = directory.path().join("title.schema.json");
    let output_file = directory.path().join("title.json");
    tokio::fs::write(&schema_file, title_schema().to_string())
        .await
        .ok()?;
    let args = codex_title_args(
        &schema_file.to_string_lossy(),
        &output_file.to_string_lossy(),
        &images,
    );
    exec_codex(
        args,
        format!("{TITLE_INSTRUCTIONS}\n\n{}", title_question(text)),
        directory.path(),
    )
    .await
    .ok()?;
    let answer: Value =
        serde_json::from_str(&tokio::fs::read_to_string(&output_file).await.ok()?).ok()?;
    title_of(&answer)
}

type MetadataRead = Shared<BoxFuture<'static, SubagentMetadata>>;

/// Inspecting a saved child recovers settings even when its session is no longer running. Reads share
/// four process slots, and a child with no settings is not asked again for thirty seconds.
pub async fn read_codex_subagent_metadata(id: &str) -> SubagentMetadata {
    static PENDING: OnceLock<Mutex<HashMap<String, MetadataRead>>> = OnceLock::new();
    static UNAVAILABLE: OnceLock<Mutex<Vec<(String, Instant, SubagentMetadata)>>> = OnceLock::new();
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    let pending = PENDING.get_or_init(Default::default);
    let unavailable = UNAVAILABLE.get_or_init(Default::default);
    let held = pending.lock().unwrap().get(id).cloned();
    if let Some(held) = held {
        return held.await;
    }
    {
        let mut cached = unavailable.lock().unwrap();
        if let Some((_, at, metadata)) = cached.iter().find(|(known, _, _)| known == id) {
            if at.elapsed() < Duration::from_secs(30) {
                return metadata.clone();
            }
        }
        cached.retain(|(known, _, _)| known != id);
    }
    let thread = id.to_string();
    let reading = async move {
        let _slot = SLOTS.get_or_init(|| Semaphore::new(4)).acquire().await;
        tokio::time::timeout(Duration::from_secs(20), read_metadata(&thread))
            .await
            .unwrap_or_default()
    }
    .boxed()
    .shared();
    pending
        .lock()
        .unwrap()
        .insert(id.to_string(), reading.clone());
    let metadata = reading.await;
    pending.lock().unwrap().remove(id);
    if metadata.model.is_none() || metadata.effort.is_none() {
        let mut cached = unavailable.lock().unwrap();
        cached.push((id.to_string(), Instant::now(), metadata.clone()));
        if cached.len() > 256 {
            cached.remove(0);
        }
    }
    metadata
}

async fn read_metadata(id: &str) -> SubagentMetadata {
    let Ok(command) = codex_app_server(
        &[
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--disable",
            "hooks",
        ]
        .map(String::from),
        ServerOptions::default(),
    )
    .await
    else {
        return SubagentMetadata::default();
    };
    let client = AppServerClient::connect(command);
    let read = async {
        client.initialize(client_info()).await.ok()?;
        let read = client
            .request(
                "thread/read",
                json!({ "threadId": id, "includeTurns": false }),
            )
            .await
            .ok()?;
        let thread = &read["thread"];
        (thread["id"] == id).then(|| SubagentMetadata {
            model: thread["model"]
                .as_str()
                .filter(|m| !m.is_empty())
                .map(String::from),
            effort: thread["reasoningEffort"]
                .as_str()
                .filter(|e| !e.is_empty())
                .map(String::from),
            prompt: None,
        })
    }
    .await;
    client.close().await;
    read.unwrap_or_default()
}
