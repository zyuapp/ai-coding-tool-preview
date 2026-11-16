//! The command a Codex app server is started with.

use std::collections::HashMap;
use std::path::Path;

use serde_json::json;

use crate::binary::{engine_binary_path, CODEX_BINARY};

use super::app_server::AppServerCommand;
use super::config::toml;
use super::home::{codex_child_environment, shared_codex_home, PRIVATE_CODEX_HOME_ENV};
use super::home_config::{
    encrypted_mcp_credentials, private_home_config, read_home_config, uses_shared_keyring,
};

/// The Codex the user installed. The app carries no copy of its own.
pub fn codex_executable() -> anyhow::Result<String> {
    engine_binary_path(&CODEX_BINARY)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("Codex is not installed."))
}

#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub cwd: Option<String>,
    /// The environment to start from; the process's own when None.
    pub env: Option<HashMap<String, String>>,
    /// Account operations keep the user's home; conversations use the app's private state.
    pub shared_home: bool,
}

fn listen(extra: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["app-server", "--listen", "stdio://"]
        .map(String::from)
        .to_vec();
    args.extend_from_slice(extra);
    args
}

/// Account operations keep the user's home; conversation processes use the app's private state.
pub async fn codex_app_server(
    args: &[String],
    options: ServerOptions,
) -> anyhow::Result<AppServerCommand> {
    let environment = options.env.unwrap_or_else(|| std::env::vars().collect());
    let env = if options.shared_home {
        environment.clone()
    } else {
        codex_child_environment(&environment).await?
    };
    let private = !options.shared_home
        && environment
            .get(PRIVATE_CODEX_HOME_ENV)
            .is_some_and(|home| !home.is_empty());
    let executable = codex_executable()?;
    if !private {
        return Ok(AppServerCommand {
            executable,
            args: listen(args),
            cwd: options.cwd,
            env,
            shared_auth: None,
        });
    }
    let home = env.get("CODEX_HOME").cloned().unwrap_or_default();
    let storage = vec![
        "-c".to_string(),
        format!("sqlite_home={}", toml(&json!(home))),
        "-c".to_string(),
        format!("log_dir={}", toml(&json!(format!("{home}/log")))),
    ];
    let source = std::fs::canonicalize(shared_codex_home(&environment))?
        .to_string_lossy()
        .into_owned();
    let config = read_home_config(&source).await?;
    if encrypted_mcp_credentials(&config)
        && Path::new(&source)
            .join("secrets")
            .join("mcp_oauth.age")
            .exists()
    {
        anyhow::bail!("Codex's encrypted MCP credential store cannot be shared across homes. Configure mcp_oauth_credentials_store = \"file\" in your Codex config and reconnect those MCP servers before using them in AICodingTool.");
    }
    let keyring = uses_shared_keyring(&config);
    let shared_auth = keyring.then(|| {
        let mut auth_env = environment.clone();
        auth_env.insert("CODEX_HOME".into(), source.clone());
        auth_env.insert("CODEX_SQLITE_HOME".into(), home.clone());
        let mut auth_args = listen(
            &[
                "--disable",
                "plugins",
                "--disable",
                "apps",
                "--disable",
                "hooks",
            ]
            .map(String::from),
        );
        auth_args.extend(storage.iter().cloned());
        Box::new(AppServerCommand {
            executable: executable.clone(),
            args: auth_args,
            cwd: Some(source.clone()),
            env: auth_env,
            shared_auth: None,
        })
    });
    let mut full = listen(&private_home_config(&config, &source, &home));
    full.extend_from_slice(args);
    if keyring {
        full.push("-c".into());
        full.push("cli_auth_credentials_store=\"ephemeral\"".into());
    }
    full.extend(storage);
    Ok(AppServerCommand {
        executable,
        args: full,
        cwd: options.cwd,
        env,
        shared_auth,
    })
}
