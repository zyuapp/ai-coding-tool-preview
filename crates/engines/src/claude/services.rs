//! What the app asks of Claude outside a run: its commands, the models it can run, a thread title,
//! plan usage, readiness, and a saved subagent's settings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};

use crate::app_plugin::{unqualified_skill_name, APP_PLUGIN_NAME};
use crate::binary::{installed_engine, upgrade_engine, version_readiness, CLAUDE_BINARY};
use crate::title::{
    readable_images, title_of, title_question, title_schema, IMAGE_BYTE_LIMIT, TITLE_INSTRUCTIONS,
};
use crate::types::{
    AgentEngine, AvailableCommand, EngineAccess, EngineReadiness, PlanUsage, SubagentMetadata,
    UsagePeriod, UsageWindow,
};

use super::provider::{app_plugin_dirs, claude_process, setting_sources};
use super::query::{ClaudeOptions, ClaudeQuery, QueryOutput, SystemPrompt};

/// The Claude Code the app is built against. An older installed command is offered fewer models.
pub const CLAUDE_BASELINE_VERSION: &str = "2.1.250";
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// Claude qualifies a plugin skill as `plugin:skill`; the app's own skills are offered by their alias.
fn app_command(command: &Value) -> AvailableCommand {
    let name = command["name"].as_str().unwrap_or_default();
    let description = command["description"].as_str().unwrap_or_default();
    let argument_hint = command["argumentHint"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if name.starts_with(&format!("{APP_PLUGIN_NAME}:")) {
        return AvailableCommand {
            name: unqualified_skill_name(name).to_string(),
            description: description.replacen(&format!("({APP_PLUGIN_NAME}) "), "", 1),
            argument_hint,
            aliases: None,
        };
    }
    AvailableCommand {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint,
        aliases: command["aliases"].as_array().map(|aliases| {
            aliases
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        }),
    }
}

/// A session that runs nothing: it answers control requests and closes.
fn idle_session(options: ClaudeOptions) -> anyhow::Result<ClaudeQuery> {
    let (query, output) = ClaudeQuery::spawn(options)?;
    tokio::spawn(async move {
        let mut output = output;
        while output.recv().await.is_some() {}
    });
    Ok(query)
}

/// The commands Claude offers a composer in the workspace.
pub async fn discover_claude_commands(
    workspace_root: &str,
    projectless: bool,
) -> anyhow::Result<Vec<AvailableCommand>> {
    let mut options = claude_process()?;
    options.cwd = Some(workspace_root.to_string());
    options.setting_sources = Some(setting_sources(projectless));
    options.all_skills = true;
    options.plugin_dirs = app_plugin_dirs();
    let query = idle_session(options)?;
    let commands = query.supported_commands().await;
    query.close();
    Ok(commands?
        .as_array()
        .map(|commands| commands.iter().map(app_command).collect())
        .unwrap_or_default())
}

/// What a row calls its model: the short name a run passes and the full one it resolves to, either
/// without a `[1m]`-style suffix.
fn model_names(info: &Value) -> Vec<String> {
    ["value", "resolvedModel"]
        .iter()
        .filter_map(|field| info[*field].as_str())
        .map(|name| match name.rfind('[') {
            Some(open) if name.ends_with(']') => name[..open].to_string(),
            _ => name.to_string(),
        })
        .collect()
}

/// Which catalogue models the installed Claude Code can run. None means all of them, which a
/// command that cannot answer is given the benefit of.
pub async fn discover_claude_models() -> Option<Vec<String>> {
    let query = idle_session(claude_process().ok()?).ok()?;
    let models = tokio::time::timeout(READ_TIMEOUT, query.supported_models()).await;
    query.close();
    let offered: Vec<String> = models
        .ok()?
        .ok()?
        .as_array()?
        .iter()
        .flat_map(model_names)
        .collect();
    let supported: Vec<String> = AgentEngine::Claude
        .spec()
        .models
        .iter()
        .map(|model| model.id.to_string())
        .filter(|id| {
            offered
                .iter()
                .any(|name| name == id || name.starts_with(&format!("claude-{id}-")))
        })
        .collect();
    (!supported.is_empty()).then_some(supported)
}

async fn image_blocks(attachments: &[String]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for file in readable_images(attachments).await {
        if let Ok(bytes) = tokio::fs::read(&file).await {
            if !bytes.is_empty() && bytes.len() as u64 <= IMAGE_BYTE_LIMIT {
                blocks.push(json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(bytes) } }));
            }
        }
    }
    blocks
}

/// Names a thread from its first message and its screenshots in one small Haiku turn that carries
/// no settings, no tools and no project directory.
pub async fn suggest_task_title(text: &str, attachments: &[String]) -> Option<String> {
    let images = image_blocks(attachments).await;
    let question = title_question(text);
    let mut options = claude_process().ok()?;
    options.model = Some("haiku".into());
    options.cwd = Some(std::env::temp_dir().to_string_lossy().into_owned());
    options.setting_sources = Some(Vec::new());
    options.system_prompt = SystemPrompt::Custom(TITLE_INSTRUCTIONS.into());
    options.tools = Some(Vec::new());
    options.json_schema = Some(title_schema());
    options.max_turns = Some(1);
    let (query, mut output) = ClaudeQuery::spawn(options).ok()?;
    let only_text = images.is_empty();
    let mut content = images;
    content.push(json!({ "type": "text", "text": question }));
    let mut message = json!({ "type": "user", "message": { "role": "user", "content": content }, "parent_tool_use_id": null });
    if only_text {
        message["session_id"] = json!("");
    }
    query.send_user(&message);
    let mut title = None;
    while let Some(item) = output.recv().await {
        match item {
            QueryOutput::Message(message) if message["type"] == "result" => {
                if message["subtype"] == "success" && message["is_error"] != true {
                    title = title_of(&message["structured_output"]);
                }
                break;
            }
            QueryOutput::End(_) => break,
            _ => {}
        }
    }
    query.close();
    title
}

fn usage_window(id: &str, label: &str, raw: &Value, model: Option<&str>) -> Option<UsageWindow> {
    if !raw.is_object() {
        return None;
    }
    let session = id == "five_hour";
    Some(UsageWindow {
        id: id.to_string(),
        label: label.to_string(),
        utilization: raw["utilization"]
            .as_f64()
            .filter(|value| value.is_finite()),
        resets_at: raw["resets_at"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(String::from),
        model: model.map(String::from),
        period: Some(if session {
            UsagePeriod::Session
        } else {
            UsagePeriod::Week
        }),
        minutes: Some(if session { 300.0 } else { 10_080.0 }),
    })
}

/// None when the answer is not a usage report at all.
pub fn to_plan_usage(raw: &Value) -> Option<PlanUsage> {
    let available = raw.get("rate_limits_available")?.as_bool()?;
    let limits = &raw["rate_limits"];
    if !available || !limits.is_object() {
        return Some(PlanUsage::NotApplicable);
    }
    let mut windows: Vec<UsageWindow> = Vec::new();
    let mut add = |window: Option<UsageWindow>| {
        if let Some(window) = window {
            if !windows.iter().any(|seen| seen.label == window.label) {
                windows.push(window);
            }
        }
    };
    add(usage_window(
        "five_hour",
        "Current session",
        &limits["five_hour"],
        None,
    ));
    add(usage_window(
        "seven_day",
        "Current week (all models)",
        &limits["seven_day"],
        None,
    ));
    for entry in limits["model_scoped"].as_array().into_iter().flatten() {
        if let Some(name) = entry["display_name"]
            .as_str()
            .filter(|name| !name.is_empty())
        {
            add(usage_window(
                &format!("model:{name}"),
                &format!("Current week ({name})"),
                entry,
                Some(name),
            ));
        }
    }
    add(usage_window(
        "seven_day_opus",
        "Current week (Opus)",
        &limits["seven_day_opus"],
        Some("Opus"),
    ));
    add(usage_window(
        "seven_day_sonnet",
        "Current week (Sonnet)",
        &limits["seven_day_sonnet"],
        Some("Sonnet"),
    ));
    add(usage_window(
        "seven_day_oauth_apps",
        "Current week (apps)",
        &limits["seven_day_oauth_apps"],
        None,
    ));
    if windows.is_empty() {
        return Some(PlanUsage::Unavailable {
            message: "The plan reported no usage windows.".into(),
        });
    }
    Some(PlanUsage::Available {
        subscription: raw["subscription_type"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from),
        windows,
    })
}

/// Reads the plan's rate-limit windows over a session that runs nothing.
pub async fn read_plan_usage() -> PlanUsage {
    let mut options = match claude_process() {
        Ok(options) => options,
        Err(error) => {
            return PlanUsage::Unavailable {
                message: error.to_string(),
            }
        }
    };
    options.cwd = Some(std::env::temp_dir().to_string_lossy().into_owned());
    options.setting_sources = Some(Vec::new());
    options.tools = Some(Vec::new());
    let query = match idle_session(options) {
        Ok(query) => query,
        Err(error) => {
            return PlanUsage::Unavailable {
                message: error.to_string(),
            }
        }
    };
    let answer = tokio::time::timeout(READ_TIMEOUT, query.usage()).await;
    query.close();
    match answer {
        Ok(Ok(raw)) => to_plan_usage(&raw).unwrap_or(PlanUsage::Unavailable {
            message:
                "Claude reported usage in a shape this version of AI Coding Tool does not read."
                    .into(),
        }),
        Ok(Err(error)) => PlanUsage::Unavailable {
            message: error.to_string(),
        },
        Err(_) => PlanUsage::Unavailable {
            message: "Claude did not answer in time.".into(),
        },
    }
}

/// Claude Code names the models it can run, so a command behind the app loses the models it never
/// heard of rather than being turned away.
pub async fn read_claude_readiness() -> EngineReadiness {
    let Some(installed) = installed_engine(&CLAUDE_BINARY).await else {
        return EngineReadiness {
            fix: Some(CLAUDE_BINARY.install.into()),
            ..EngineReadiness::of(EngineAccess::Missing)
        };
    };
    let (models, version) = tokio::join!(
        discover_claude_models(),
        version_readiness(
            &CLAUDE_BINARY,
            &installed,
            CLAUDE_BASELINE_VERSION,
            EngineAccess::Ready
        )
    );
    EngineReadiness { models, ..version }
}

pub async fn update_claude() -> anyhow::Result<()> {
    upgrade_engine(&CLAUDE_BINARY).await
}

const TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// The transcript stamps the effort sent on the request; per-turn overrides take precedence.
pub fn claude_transcript_metadata(text: &str, id: &str, session_id: &str) -> SubagentMetadata {
    for line in text.lines().rev() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row["type"] != "assistant" || row["agentId"] != id || row["sessionId"] != session_id {
            continue;
        }
        let Some(model) = row["message"]["model"]
            .as_str()
            .filter(|m| !m.is_empty() && *m != "<synthetic>")
        else {
            continue;
        };
        let effort = if row["perTurnEffort"].is_null() {
            &row["effort"]
        } else {
            &row["perTurnEffort"]
        };
        let effort = match effort {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        };
        return SubagentMetadata {
            model: Some(model.to_string()),
            effort,
            prompt: None,
        };
    }
    SubagentMetadata::default()
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn claude_config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude"))
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

async fn read_metadata(config: PathBuf, id: String, session_id: String) -> SubagentMetadata {
    let projects = config.join("projects");
    let Ok(mut entries) = tokio::fs::read_dir(&projects).await else {
        return SubagentMetadata::default();
    };
    while let Ok(Some(project)) = entries.next_entry().await {
        if !project.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let file = project
            .path()
            .join(&session_id)
            .join("subagents")
            .join(format!("agent-{id}.jsonl"));
        if let Some(text) = read_tail(&file).await {
            return claude_transcript_metadata(&text, &id, &session_id);
        }
    }
    SubagentMetadata::default()
}

async fn read_tail(file: &Path) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut handle = tokio::fs::File::open(file).await.ok()?;
    let size = handle.metadata().await.ok()?.len();
    let start = size.saturating_sub(TAIL_BYTES);
    handle.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    let mut bytes = Vec::with_capacity((size - start) as usize);
    handle.read_to_end(&mut bytes).await.ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

type MetadataRead = Shared<BoxFuture<'static, SubagentMetadata>>;

/// Reads only the tail of a saved child, without starting Claude or loading its full conversation.
pub async fn read_claude_subagent_metadata(id: &str, session_id: Option<&str>) -> SubagentMetadata {
    let Some(session_id) = session_id.filter(|session| safe_id(session)) else {
        return SubagentMetadata::default();
    };
    if !safe_id(id) {
        return SubagentMetadata::default();
    }
    static PENDING: OnceLock<Mutex<HashMap<String, MetadataRead>>> = OnceLock::new();
    let config = claude_config_dir();
    let key = json!([config.to_string_lossy(), session_id, id]).to_string();
    let reading = {
        let mut pending = PENDING.get_or_init(Default::default).lock().unwrap();
        pending
            .entry(key.clone())
            .or_insert_with(|| {
                read_metadata(config, id.to_string(), session_id.to_string())
                    .boxed()
                    .shared()
            })
            .clone()
    };
    let metadata = reading.await;
    PENDING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&key);
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plan_windows_once_each() {
        let usage = to_plan_usage(&json!({
            "rate_limits_available": true,
            "subscription_type": "max",
            "rate_limits": {
                "five_hour": { "utilization": 12.5, "resets_at": "2026-01-01T00:00:00Z" },
                "seven_day": { "utilization": 40 },
                "model_scoped": [{ "display_name": "Opus", "utilization": 3 }],
                "seven_day_opus": { "utilization": 9 },
            },
        }))
        .unwrap();
        let PlanUsage::Available {
            windows,
            subscription,
        } = usage
        else {
            panic!("available")
        };
        assert_eq!(subscription.as_deref(), Some("max"));
        assert_eq!(
            windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(),
            [
                "Current session",
                "Current week (all models)",
                "Current week (Opus)"
            ]
        );
        assert_eq!(
            to_plan_usage(&json!({ "rate_limits_available": false })),
            Some(PlanUsage::NotApplicable)
        );
        assert_eq!(to_plan_usage(&json!({})), None);
    }

    #[test]
    fn transcript_metadata_reads_the_newest_matching_reply() {
        let text = [
            json!({ "type": "assistant", "agentId": "a", "sessionId": "s", "message": { "model": "claude-old" }, "effort": "low" }).to_string(),
            json!({ "type": "assistant", "agentId": "a", "sessionId": "s", "message": { "model": "claude-new" }, "effort": "low", "perTurnEffort": "high" }).to_string(),
            "{ broken".to_string(),
        ]
        .join("\n");
        let metadata = claude_transcript_metadata(&text, "a", "s");
        assert_eq!(metadata.model.as_deref(), Some("claude-new"));
        assert_eq!(metadata.effort.as_deref(), Some("high"));
    }
}
