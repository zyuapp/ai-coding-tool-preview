//! The user-owned Codex config, read only for what a private home has to carry over.

use std::path::Path;

use serde_json::{json, Map, Value};

use super::config::toml;

/// The user's `config.toml` as JSON; empty when there is none.
pub async fn read_home_config(home: &str) -> anyhow::Result<Value> {
    match tokio::fs::read_to_string(Path::new(home).join("config.toml")).await {
        Ok(text) => {
            let parsed: toml::Value = text.parse::<toml::Table>().map(toml::Value::Table)?;
            Ok(serde_json::to_value(parsed)?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

fn table(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn resolve(source: &str, relative: &str) -> String {
    super::home::normalize(&Path::new(source).join(relative))
        .to_string_lossy()
        .into_owned()
}

/// Keeps file references anchored to the config that declared them, including custom agent layers,
/// and aliases hook trust from the shared home to the private one.
pub fn private_home_config(config: &Value, source: &str, destination: &str) -> Vec<String> {
    let mut overrides = Vec::new();
    for key in [
        "model_instructions_file",
        "experimental_compact_prompt_file",
        "model_catalog_json",
    ] {
        if let Some(value) = config[key]
            .as_str()
            .filter(|value| !value.is_empty() && !Path::new(value).is_absolute())
        {
            overrides.push("-c".into());
            overrides.push(format!("{key}={}", toml(&json!(resolve(source, value)))));
        }
    }
    let mut agents = Map::new();
    for (name, agent) in table(&config["agents"]) {
        if let Some(file) = agent["config_file"]
            .as_str()
            .filter(|file| !file.is_empty() && !Path::new(file).is_absolute())
        {
            agents.insert(name, json!({ "config_file": resolve(source, file) }));
        }
    }
    if !agents.is_empty() {
        overrides.push("-c".into());
        overrides.push(format!("agents={}", toml(&Value::Object(agents))));
    }
    let separator = std::path::MAIN_SEPARATOR;
    let mut states = Map::new();
    for (key, state) in table(&config["hooks"]["state"]) {
        if !key.starts_with(&format!("{source}{separator}"))
            || key.starts_with(&format!("{destination}{separator}"))
        {
            continue;
        }
        let alias = format!("{destination}{}", &key[source.len()..]);
        for field in ["trusted_hash", "enabled"] {
            let value = &state[field];
            if value.is_string() || value.is_boolean() {
                states.entry(alias.clone()).or_insert_with(|| json!({}))[field] = value.clone();
            }
        }
    }
    if !states.is_empty() {
        overrides.push("-c".into());
        overrides.push(format!("hooks.state={}", toml(&Value::Object(states))));
    }
    overrides
}

pub fn uses_shared_keyring(config: &Value) -> bool {
    matches!(
        config["cli_auth_credentials_store"].as_str(),
        Some("keyring" | "auto")
    )
}

pub fn encrypted_mcp_credentials(config: &Value) -> bool {
    config["features"]["secret_auth_storage"] == true
        && config["mcp_oauth_credentials_store"] != "file"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_relative_files_to_the_shared_home() {
        let config = json!({ "model_instructions_file": "notes.md", "agents": { "r": { "config_file": "agents/r.toml" } }, "hooks": { "state": { "/src/hooks/a": { "trusted_hash": "h", "other": 1 } } } });
        let overrides = private_home_config(&config, "/src", "/dst");
        assert_eq!(overrides[1], "model_instructions_file=\"/src/notes.md\"");
        assert_eq!(
            overrides[3],
            "agents={ \"r\" = { \"config_file\" = \"/src/agents/r.toml\" } }"
        );
        assert_eq!(
            overrides[5],
            "hooks.state={ \"/dst/hooks/a\" = { \"trusted_hash\" = \"h\" } }"
        );
    }
}
