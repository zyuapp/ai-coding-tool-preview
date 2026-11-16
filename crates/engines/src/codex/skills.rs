//! Codex's native skills: the composer's menu, and the skill inputs a slash name in a prompt adds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};

use crate::app_plugin::{app_skills_root, unqualified_skill_name};
use crate::types::AvailableCommand;

use super::app_server::{client_info, AppServerClient};
use super::config::app_features;
use super::server::{codex_app_server, ServerOptions};

/// Codex qualifies the app's skills as `aicodingtool:skill`; the app offers them by the short name.
pub async fn adopt_app_skills(client: &AppServerClient) -> anyhow::Result<()> {
    if let Some(root) = app_skills_root() {
        client
            .request("skills/extraRoots/set", json!({ "extraRoots": [root] }))
            .await?;
    }
    Ok(())
}

/// The enabled skills for one folder, each name once.
async fn read_skills(
    client: &AppServerClient,
    cwd: &str,
    force_reload: bool,
) -> anyhow::Result<Vec<Value>> {
    let result = client
        .request(
            "skills/list",
            json!({ "cwds": [cwd], "forceReload": force_reload }),
        )
        .await?;
    let mut skills: Vec<Value> = Vec::new();
    for entry in result["data"].as_array().into_iter().flatten() {
        for skill in entry["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|skill| skill["enabled"] == true)
        {
            match skills
                .iter_mut()
                .find(|known| known["name"] == skill["name"])
            {
                Some(known) => *known = skill.clone(),
                None => skills.push(skill.clone()),
            }
        }
    }
    Ok(skills)
}

type Discovery = Shared<BoxFuture<'static, Result<Vec<AvailableCommand>, String>>>;

/// The commands Codex offers in a workspace. Concurrent reads share one server.
pub async fn discover_codex_commands(cwd: &str) -> anyhow::Result<Vec<AvailableCommand>> {
    static DISCOVERING: OnceLock<Mutex<HashMap<String, Discovery>>> = OnceLock::new();
    let reading = {
        let mut discovering = DISCOVERING.get_or_init(Default::default).lock().unwrap();
        discovering
            .entry(cwd.to_string())
            .or_insert_with(|| {
                discover(cwd.to_string())
                    .map(|result| result.map_err(|error| error.to_string()))
                    .boxed()
                    .shared()
            })
            .clone()
    };
    let result = reading.await;
    DISCOVERING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(cwd);
    result.map_err(anyhow::Error::msg)
}

async fn discover(cwd: String) -> anyhow::Result<Vec<AvailableCommand>> {
    let client = AppServerClient::connect(
        codex_app_server(
            &app_features(),
            ServerOptions {
                cwd: Some(cwd.clone()),
                ..Default::default()
            },
        )
        .await?,
    );
    let reading = async {
        client.initialize(client_info()).await?;
        adopt_app_skills(&client).await?;
        let skills = read_skills(&client, &cwd, true).await?;
        Ok::<_, anyhow::Error>(
            skills
                .iter()
                .map(|skill| AvailableCommand {
                    name: unqualified_skill_name(skill["name"].as_str().unwrap_or_default())
                        .to_string(),
                    description: skill["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    argument_hint: String::new(),
                    aliases: None,
                })
                .collect(),
        )
    };
    let result = match tokio::time::timeout(Duration::from_secs(20), reading).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("Codex did not return its skills in time.")),
    };
    client.close().await;
    result
}

/// The native Codex skills available to one workspace.
#[derive(Clone)]
pub struct CodexSkills {
    client: AppServerClient,
    cwd: String,
    skills: Arc<Mutex<Vec<Value>>>,
    reading: Arc<Mutex<Option<Shared<BoxFuture<'static, ()>>>>>,
}

impl CodexSkills {
    pub fn new(client: AppServerClient, cwd: String) -> Self {
        CodexSkills {
            client,
            cwd,
            skills: Arc::default(),
            reading: Arc::default(),
        }
    }

    /// Refreshes metadata without making skill discovery a condition for running Codex.
    pub fn refresh(&self, force_reload: bool) -> Shared<BoxFuture<'static, ()>> {
        let mut reading = self.reading.lock().unwrap();
        if let Some(reading) = reading.as_ref() {
            return reading.clone();
        }
        let (client, cwd, skills, slot) = (
            self.client.clone(),
            self.cwd.clone(),
            self.skills.clone(),
            self.reading.clone(),
        );
        let read = async move {
            if let Ok(found) = read_skills(&client, &cwd, force_reload).await {
                *skills.lock().unwrap() = found;
            }
            slot.lock().unwrap().take();
        }
        .boxed()
        .shared();
        *reading = Some(read.clone());
        tokio::spawn(read.clone());
        read
    }

    /// Waits for an active refresh, then adds every slash skill named in the prompt.
    pub async fn input(&self, prompt: &str) -> Vec<Value> {
        let reading = self.reading.lock().unwrap().clone();
        if let Some(reading) = reading {
            reading.await;
        }
        let skills = self.skills.lock().unwrap().clone();
        let mut available: HashMap<String, Value> = HashMap::new();
        for skill in &skills {
            let name = skill["name"].as_str().unwrap_or_default();
            available.insert(name.to_string(), skill.clone());
            available.insert(unqualified_skill_name(name).to_string(), skill.clone());
        }
        let mut requested = Vec::new();
        let mut seen = Vec::<String>::new();
        for name in slash_names(prompt) {
            let Some(skill) = available.get(name) else {
                continue;
            };
            let skill_name = skill["name"].as_str().unwrap_or_default().to_string();
            if seen.contains(&skill_name) {
                continue;
            }
            seen.push(skill_name.clone());
            requested.push(json!({ "type": "skill", "name": skill_name, "path": skill["path"] }));
        }
        requested.push(json!({ "type": "text", "text": prompt, "text_elements": [] }));
        requested
    }
}

/// Every `/name` at the start of the prompt or after whitespace (`(?:^|\s)\/([^\s/]+)`).
pub fn slash_names(prompt: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut previous_space = true;
    let mut index = 0;
    while index < prompt.len() {
        let c = prompt[index..].chars().next().unwrap();
        if c == '/' && previous_space {
            let start = index + 1;
            let end = prompt[start..]
                .find(|c: char| c.is_whitespace() || c == '/')
                .map_or(prompt.len(), |offset| start + offset);
            if end > start {
                names.push(&prompt[start..end]);
                index = end;
                previous_space = false;
                continue;
            }
        }
        previous_space = c.is_whitespace();
        index += c.len_utf8();
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_slash_names_after_whitespace() {
        assert_eq!(
            slash_names("/review this and /plan-it now a/b"),
            ["review", "plan-it"]
        );
        assert_eq!(slash_names("x /a/b"), ["a"]);
    }
}
