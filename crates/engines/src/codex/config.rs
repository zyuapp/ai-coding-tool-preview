//! The `-c key=value` overrides a Codex app server is spawned with.

use serde_json::{json, Map, Value};

use crate::provider::RunInput;
use crate::tools::{grants_tool, mcp_tool_name, tool_reach, ServedToolSet, ServedTools, ToolReach};
use crate::types::{ComputerUseRunConfig, CoordinationRole};

/// The name Codex files the app's own tools under.
pub const APP_SERVER_NAME: &str = "aicodingtool";
/// Where the app server finds the bearer token for the app's tool service.
pub const TOOL_TOKEN_ENV: &str = "AICODINGTOOL_MCP_TOKEN";
pub const COMPUTER_USE_SERVER_NAME: &str = "cua-driver";

/// These bundled plugins target Codex's desktop surfaces. User plugins retain their own settings.
const APP_SURFACE_PLUGINS: [&str; 5] = [
    "browser",
    "chrome",
    "computer-use",
    "unified-computer-use",
    "codex-app-tools",
];

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One TOML value, as `-c key=value` takes it. Objects become inline tables.
pub fn toml(value: &Value) -> String {
    match value {
        Value::String(text) => toml_string(text),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(toml).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(entries) if entries.is_empty() => "{}".to_string(),
        Value::Object(entries) => format!(
            "{{ {} }}",
            entries
                .iter()
                .map(|(key, entry)| format!("{} = {}", toml_string(key), toml(entry)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Null => "\"\"".to_string(),
    }
}

/// Features every app server the app starts runs with.
pub fn app_features() -> Vec<String> {
    let plugins: Map<String, Value> = APP_SURFACE_PLUGINS
        .iter()
        .map(|name| {
            (
                format!("{name}@openai-bundled"),
                json!({ "enabled": false }),
            )
        })
        .collect();
    vec![
        "--enable".into(),
        "fast_mode".into(),
        "--enable".into(),
        "goals".into(),
        "-c".into(),
        format!("plugins={}", toml(&Value::Object(plugins))),
    ]
}

/// The config overrides a Codex app server is spawned with. The app's own tools are served flat
/// under one server and granted tool by tool as Claude grants them; the rest prompt.
pub fn codex_config(
    input: &RunInput,
    served: Option<&ServedTools>,
    sets: &[ServedToolSet],
) -> Vec<String> {
    let mut config: Vec<(String, Value)> = Vec::new();
    if let Some(served) = served {
        config.push((
            format!("mcp_servers.{APP_SERVER_NAME}.url"),
            json!(served.url),
        ));
        config.push((
            format!("mcp_servers.{APP_SERVER_NAME}.bearer_token_env_var"),
            json!(TOOL_TOKEN_ENV),
        ));
        let unasked = grants_tool(ToolReach::Workspace, Some(input.scope()));
        config.push((
            format!("mcp_servers.{APP_SERVER_NAME}.default_tools_approval_mode"),
            json!(if unasked { "approve" } else { "prompt" }),
        ));
        for set in sets {
            for tool in &set.tools {
                let granted = unasked
                    || grants_tool(
                        tool_reach(&mcp_tool_name(&set.server, &tool.name), sets),
                        None,
                    );
                config.push((
                    format!(
                        "mcp_servers.{APP_SERVER_NAME}.tools.{}.approval_mode",
                        tool.name
                    ),
                    json!(if granted { "approve" } else { "prompt" }),
                ));
            }
        }
    }
    if let ComputerUseRunConfig::Available { mcp } = &input.computer_use {
        config.push((
            format!("mcp_servers.{COMPUTER_USE_SERVER_NAME}.command"),
            json!(mcp.command),
        ));
        config.push((
            format!("mcp_servers.{COMPUTER_USE_SERVER_NAME}.args"),
            json!(mcp.args),
        ));
        config.push((
            format!("mcp_servers.{COMPUTER_USE_SERVER_NAME}.env"),
            json!(mcp.env),
        ));
        if grants_tool(ToolReach::ComputerUse, Some(input.scope())) {
            config.push((
                format!("mcp_servers.{COMPUTER_USE_SERVER_NAME}.default_tools_approval_mode"),
                json!("approve"),
            ));
        }
    }
    let mut args = app_features();
    args.push(
        if input.coordination_role.is_some() {
            "--disable"
        } else {
            "--enable"
        }
        .into(),
    );
    args.push("default_mode_request_user_input".into());
    if input.coordination_role == Some(CoordinationRole::Coordinator) {
        args.extend(["--disable", "multi_agent", "--disable", "multi_agent_v2"].map(String::from));
    }
    for (key, value) in config {
        args.push("-c".into());
        args.push(format!("{key}={}", toml(&value)));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_inline_toml() {
        assert_eq!(
            toml(&json!({ "a b": [1, "x\"y"], "c": {} })),
            r#"{ "a b" = [1, "x\"y"], "c" = {} }"#
        );
        assert_eq!(toml(&json!("tab\tend\u{1}")), "\"tab\\tend\\u0001\"");
    }
}
