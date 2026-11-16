//! Keychain access stays in Codex, under the original home. A conversation in a private home receives
//! only an in-memory access token, read from one credential connection per source shared by every
//! concurrent conversation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};

use super::app_server::{AppServerClient, AppServerCommand, AppServerError};

type Ready = Shared<BoxFuture<'static, Result<AppServerClient, String>>>;

struct Connection {
    client: Ready,
    users: usize,
    refreshing: Option<Shared<BoxFuture<'static, Result<Option<Value>, String>>>>,
}

fn connections() -> &'static Mutex<HashMap<String, Connection>> {
    static CONNECTIONS: OnceLock<Mutex<HashMap<String, Connection>>> = OnceLock::new();
    CONNECTIONS.get_or_init(Default::default)
}

/// The login a private conversation takes from the shared home's auth status. Refresh tokens never
/// leave Codex.
pub fn shared_auth_login(status: &Value) -> Result<Option<Value>, String> {
    let Some(method) = status["authMethod"].as_str() else {
        return Ok(None);
    };
    let Some(token) = status["authToken"].as_str() else {
        return Err(format!(
            "Codex cannot share {method} authentication with a private conversation home."
        ));
    };
    match method {
        "apikey" => Ok(Some(json!({ "type": "apiKey", "apiKey": token }))),
        "chatgpt" | "chatgptAuthTokens" => {
            let payload = token.split('.').nth(1).unwrap_or_default();
            let claims: Value = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload.trim_end_matches('='))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .ok_or_else(|| "Codex returned an unreadable ChatGPT access token.".to_string())?;
            let auth = &claims["https://api.openai.com/auth"];
            let account = auth["chatgpt_account_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    "Codex's ChatGPT access token has no account identifier.".to_string()
                })?;
            Ok(Some(json!({
                "type": "chatgptAuthTokens",
                "accessToken": token,
                "chatgptAccountId": account,
                "chatgptPlanType": auth["chatgpt_plan_type"].as_str().map(Value::from).unwrap_or(Value::Null),
            })))
        }
        other => Err(format!(
            "Codex cannot share {other} authentication with a private conversation home."
        )),
    }
}

/// One user's hold on a shared credential connection.
#[derive(Clone)]
pub struct SharedAuth {
    key: String,
    released: Arc<Mutex<bool>>,
}

impl SharedAuth {
    /// The login to hand a conversation, refreshed when asked.
    pub async fn read(&self, refresh: bool) -> Result<Option<Value>, String> {
        let (client, refreshing) = {
            let connections = connections().lock().unwrap();
            let Some(connection) = connections.get(&self.key) else {
                return Err("The shared Codex sign-in was released.".into());
            };
            (connection.client.clone(), connection.refreshing.clone())
        };
        if let Some(refreshing) = refreshing {
            return refreshing.await;
        }
        let reading = async move {
            let client = client.await?;
            let status = client
                .request(
                    "getAuthStatus",
                    json!({ "includeToken": true, "refreshToken": refresh }),
                )
                .await
                .map_err(|error| error.to_string())?;
            shared_auth_login(&status)
        }
        .boxed()
        .shared();
        if refresh {
            if let Some(connection) = connections().lock().unwrap().get_mut(&self.key) {
                connection.refreshing = Some(reading.clone());
            }
        }
        let result = reading.await;
        if refresh {
            if let Some(connection) = connections().lock().unwrap().get_mut(&self.key) {
                connection.refreshing = None;
            }
        }
        result
    }

    pub fn release(&self) -> BoxFuture<'static, ()> {
        {
            let mut released = self.released.lock().unwrap();
            if *released {
                return async {}.boxed();
            }
            *released = true;
        }
        let closing = {
            let mut connections = connections().lock().unwrap();
            match connections.get_mut(&self.key) {
                None => None,
                Some(connection) => {
                    connection.users -= 1;
                    if connection.users > 0 {
                        None
                    } else {
                        connections
                            .remove(&self.key)
                            .map(|connection| connection.client)
                    }
                }
            }
        };
        async move {
            if let Some(client) = closing {
                if let Ok(client) = client.await {
                    client.close().await;
                }
            }
        }
        .boxed()
    }
}

/// One credential connection per source, shared by all concurrent conversations.
pub fn acquire_shared_auth(
    command: AppServerCommand,
) -> BoxFuture<'static, Result<SharedAuth, AppServerError>> {
    async move {
    let key = json!([command.executable, command.env.get("CODEX_HOME"), command.args]).to_string();
    let client = {
        let mut connections = connections().lock().unwrap();
        let connection = connections.entry(key.clone()).or_insert_with(|| {
            let client = crate::session_resources::in_resource_scope_sync(None, || AppServerClient::connect(command));
            let ready = async move {
                match client.initialize(json!({ "name": "aicodingtool_auth", "title": "AICodingTool authentication", "version": "1" })).await {
                    Ok(_) => Ok(client),
                    Err(error) => {
                        client.close().await;
                        Err(error.to_string())
                    }
                }
            }
            .boxed()
            .shared();
            Connection { client: ready, users: 0, refreshing: None }
        });
        connection.users += 1;
        connection.client.clone()
    };
    let auth = SharedAuth { key, released: Arc::new(Mutex::new(false)) };
    if let Err(message) = client.await {
        auth.release().await;
        return Err(AppServerError::Rpc { method: "initialize".into(), code: -32603, message, data: Value::Null });
    }
    Ok(auth)
    }
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chatgpt_token_names_its_account() {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct", "chatgpt_plan_type": "pro" } }).to_string());
        let login = shared_auth_login(
            &json!({ "authMethod": "chatgpt", "authToken": format!("h.{claims}.s") }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(login["chatgptAccountId"], "acct");
        assert_eq!(login["chatgptPlanType"], "pro");
        assert!(shared_auth_login(&json!({ "authMethod": "chatgpt" })).is_err());
        assert_eq!(shared_auth_login(&json!({})).unwrap(), None);
    }
}
