//! Serves the app's tools over MCP streamable HTTP on the loopback interface, to any MCP client that
//! can carry a bearer token. Each token names one set of tools, so a caller sees exactly the set it
//! was given. The listener opens on the first set served, on a port of the system's choosing.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::future::{BoxFuture, FutureExt, Shared};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::tools::{BoundTool, ServedTools, ToolHost, ToolResult};

const PATH: &str = "/mcp";
const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: [&str; 4] = ["2025-06-18", "2025-03-26", "2024-11-05", "2025-11-25"];

struct Registration {
    tools: Vec<BoundTool>,
    /// Live MCP sessions opened with the token.
    sessions: HashSet<String>,
}

#[derive(Default)]
struct Inner {
    registrations: Mutex<HashMap<String, Registration>>,
    listening: Mutex<Option<Shared<BoxFuture<'static, Result<String, String>>>>>,
    server: Mutex<Option<AbortHandle>>,
}

#[derive(Clone, Default)]
pub struct McpHttpHost {
    inner: Arc<Inner>,
}

impl McpHttpHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops listening and forgets every set served.
    pub fn close(&self) {
        self.inner.registrations.lock().unwrap().clear();
        self.inner.listening.lock().unwrap().take();
        if let Some(server) = self.inner.server.lock().unwrap().take() {
            server.abort();
        }
    }

    fn listen(&self) -> Shared<BoxFuture<'static, Result<String, String>>> {
        let mut listening = self.inner.listening.lock().unwrap();
        if let Some(listening) = listening.as_ref() {
            return listening.clone();
        }
        let host = self.clone();
        let opening = async move {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|error| error.to_string())?;
            let port = listener
                .local_addr()
                .map_err(|_| "The tool service has no port.".to_string())?
                .port();
            let serving = host.clone();
            let task = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        continue;
                    };
                    let host = serving.clone();
                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(move |request| {
                            let host = host.clone();
                            async move { Ok::<_, Infallible>(host.handle(request).await) }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                            .await;
                    });
                }
            });
            *host.inner.server.lock().unwrap() = Some(task.abort_handle());
            Ok(format!("http://127.0.0.1:{port}{PATH}"))
        }
        .boxed()
        .shared();
        *listening = Some(opening.clone());
        opening
    }

    async fn handle(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let status = |code: StatusCode| {
            Response::builder()
                .status(code)
                .body(Full::new(Bytes::new()))
                .unwrap()
        };
        if request.uri().path() != PATH {
            return status(StatusCode::NOT_FOUND);
        }
        let token = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(String::from);
        let Some(token) =
            token.filter(|token| self.inner.registrations.lock().unwrap().contains_key(token))
        else {
            return status(StatusCode::UNAUTHORIZED);
        };
        let session = request
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let known = session.as_ref().is_some_and(|id| {
            self.inner
                .registrations
                .lock()
                .unwrap()
                .get(&token)
                .is_some_and(|registration| registration.sessions.contains(id))
        });
        if session.is_some() && !known {
            return status(StatusCode::NOT_FOUND);
        }
        match *request.method() {
            Method::DELETE => {
                if let (Some(id), Some(registration)) = (
                    &session,
                    self.inner.registrations.lock().unwrap().get_mut(&token),
                ) {
                    registration.sessions.remove(id);
                }
                status(if known {
                    StatusCode::OK
                } else {
                    StatusCode::NOT_FOUND
                })
            }
            Method::GET => status(StatusCode::METHOD_NOT_ALLOWED),
            Method::POST => {
                let Ok(body) = request.into_body().collect().await else {
                    return status(StatusCode::BAD_REQUEST);
                };
                let Ok(message) = serde_json::from_slice::<Value>(&body.to_bytes()) else {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
                        None,
                    );
                };
                let messages = match &message {
                    Value::Array(batch) => batch.clone(),
                    single => vec![single.clone()],
                };
                let initializing = messages
                    .iter()
                    .any(|message| message["method"] == "initialize");
                let opened = match (&session, initializing) {
                    (None, true) => {
                        let id = uuid::Uuid::new_v4().to_string();
                        if let Some(registration) =
                            self.inner.registrations.lock().unwrap().get_mut(&token)
                        {
                            registration.sessions.insert(id.clone());
                        }
                        Some(id)
                    }
                    (None, false) => {
                        return json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32000, "message": "Bad Request: No valid session ID provided" } }),
                            None,
                        )
                    }
                    _ => None,
                };
                let tools = self
                    .inner
                    .registrations
                    .lock()
                    .unwrap()
                    .get(&token)
                    .map(|registration| registration.tools.clone())
                    .unwrap_or_default();
                let mut answers = Vec::new();
                for message in &messages {
                    if let Some(answer) = answer(&tools, message).await {
                        answers.push(answer);
                    }
                }
                if answers.is_empty() {
                    let mut accepted = status(StatusCode::ACCEPTED);
                    if let Some(id) = &opened {
                        accepted
                            .headers_mut()
                            .insert("mcp-session-id", id.parse().unwrap());
                    }
                    return accepted;
                }
                let body = if message.is_array() {
                    Value::Array(answers)
                } else {
                    answers.remove(0)
                };
                json_response(StatusCode::OK, &body, opened.as_deref())
            }
            _ => status(StatusCode::METHOD_NOT_ALLOWED),
        }
    }
}

fn json_response(code: StatusCode, body: &Value, session: Option<&str>) -> Response<Full<Bytes>> {
    let mut response = Response::builder()
        .status(code)
        .header("content-type", "application/json");
    if let Some(session) = session {
        response = response.header("mcp-session-id", session);
    }
    response
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

/// Answers one JSON-RPC message; notifications and responses get no answer.
async fn answer(tools: &[BoundTool], message: &Value) -> Option<Value> {
    let id = message.get("id").filter(|id| !id.is_null())?.clone();
    let method = message["method"].as_str()?;
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION);
            let version = if SUPPORTED_VERSIONS.contains(&asked) {
                asked
            } else {
                PROTOCOL_VERSION
            };
            Ok(
                json!({ "protocolVersion": version, "capabilities": { "tools": { "listChanged": true } }, "serverInfo": { "name": "aicodingtool", "version": "1.0.0" } }),
            )
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(
            json!({ "tools": tools.iter().map(|tool| json!({ "name": tool.name, "description": tool.description, "inputSchema": tool.input_schema })).collect::<Vec<_>>() }),
        ),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            match tools.iter().find(|tool| tool.name == name) {
                Some(tool) => Ok(serde_json::to_value(
                    (tool.handler)(params.get("arguments").cloned().unwrap_or(json!({}))).await,
                )
                .unwrap_or(json!({}))),
                None => Ok(serde_json::to_value(ToolResult::error(format!(
                    "Tool {name} not found"
                )))
                .unwrap_or(json!({}))),
            }
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err(json!({ "code": -32601, "message": format!("Method not found: {other}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

impl ToolHost for McpHttpHost {
    fn serve(&self, tools: Vec<BoundTool>) -> BoxFuture<'static, anyhow::Result<ServedTools>> {
        let host = self.clone();
        async move {
            let url = host.listen().await.map_err(anyhow::Error::msg)?;
            let token = uuid::Uuid::new_v4().to_string();
            host.inner.registrations.lock().unwrap().insert(
                token.clone(),
                Registration {
                    tools,
                    sessions: HashSet::new(),
                },
            );
            let registrations = Arc::downgrade(&host.inner);
            let released = token.clone();
            Ok(ServedTools::new(url, token, move || {
                if let Some(inner) = registrations.upgrade() {
                    inner.registrations.lock().unwrap().remove(&released);
                }
            }))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_a_token_its_own_tools_and_nobody_else() {
        let host = McpHttpHost::new();
        let tool = BoundTool {
            name: "echo".into(),
            description: "Echoes".into(),
            input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
            read_only: true,
            handler: Arc::new(|args| {
                async move { ToolResult::text(args["text"].as_str().unwrap_or_default()) }.boxed()
            }),
        };
        let served = host.serve(vec![tool]).await.unwrap();
        let client = reqwest::Client::new();
        let url = served.url.clone();
        let post = |body: Value, session: Option<String>, token: String| {
            let mut request = client
                .post(&url)
                .bearer_auth(token)
                .header("accept", "application/json, text/event-stream")
                .json(&body);
            if let Some(session) = session {
                request = request.header("mcp-session-id", session);
            }
            request.send()
        };
        let opened = post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }), None, served.token.clone()).await.unwrap();
        let session = opened.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_string();
        let called: Value = post(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo", "arguments": { "text": "hi" } } }), Some(session.clone()), served.token.clone()).await.unwrap().json().await.unwrap();
        assert_eq!(called["result"]["content"][0]["text"], "hi");
        assert_eq!(
            post(
                json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
                Some(session),
                "other".into()
            )
            .await
            .unwrap()
            .status(),
            401
        );
        let token = served.token.clone();
        served.release();
        assert_eq!(
            post(
                json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
                None,
                token
            )
            .await
            .unwrap()
            .status(),
            401
        );
        host.close();
    }
}
