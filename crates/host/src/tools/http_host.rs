//! Serves the app's tools over MCP streamable HTTP on the loopback interface, to any client that can
//! carry a bearer token. Each token names one set of tools, so a caller sees exactly the set it was
//! given and nobody else's. The listener opens on the first set served, on a port the system picks.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio::net::TcpListener;
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_service::Service;

use super::definition::BoundTool;

const SERVER_NAME: &str = "aicodingtool";
const SERVER_VERSION: &str = "1.0.0";
const PATH: &str = "/mcp";

type Body = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// One served set's MCP server, made afresh for each session.
#[derive(Clone)]
struct ToolSet {
    tools: Arc<Vec<BoundTool>>,
}

impl ServerHandler for ToolSet {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new(SERVER_NAME, SERVER_VERSION))
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                let mut described = Tool::new(tool.name.clone(), tool.description.clone(), Arc::new(tool.input.json_schema()));
                described.title = None;
                described
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = self.tools.iter().find(|tool| tool.name == request.name) else {
            let message = format!("MCP error -32602: Tool {} not found", request.name);
            return Ok(CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(message)])));
        };
        let result = tool.call(request.arguments).await;
        let content = vec![ContentBlock::text(result.text)];
        Ok(CallToolResponse::Complete(if result.is_error { CallToolResult::error(content) } else { CallToolResult::success(content) }))
    }
}

struct Registration {
    service: StreamableHttpService<ToolSet, LocalSessionManager>,
    /// Ends every session opened with the token.
    cancel: CancellationToken,
}

#[derive(Default)]
struct Shared {
    registrations: Mutex<HashMap<String, Registration>>,
}

/// What a served set is reachable as: the address, and the bearer token that names the set.
pub struct ServedTools {
    pub url: String,
    pub token: String,
    shared: Arc<Shared>,
}

impl ServedTools {
    /// Forgets the token and drops every connection made with it.
    pub fn release(&self) {
        if let Some(registration) = self.shared.registrations.lock().unwrap().remove(&self.token) {
            registration.cancel.cancel();
        }
    }
}

#[derive(Default)]
pub struct McpHttpHost {
    shared: Arc<Shared>,
    listening: OnceCell<(String, JoinHandle<()>)>,
}

impl McpHttpHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves a set under a fresh token. Must be called within a Tokio runtime.
    pub async fn serve(&self, tools: Vec<BoundTool>) -> std::io::Result<ServedTools> {
        let (url, _) = self.listening.get_or_try_init(|| listen(self.shared.clone())).await?;
        let token = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let mut config = StreamableHttpServerConfig::default();
        config.cancellation_token = cancel.clone();
        let set = ToolSet { tools: Arc::new(tools) };
        let service = StreamableHttpService::new(move || Ok(set.clone()), Arc::new(LocalSessionManager::default()), config);
        self.shared.registrations.lock().unwrap().insert(token.clone(), Registration { service, cancel });
        Ok(ServedTools { url: url.clone(), token, shared: self.shared.clone() })
    }

    /// Ends every session and stops listening.
    pub fn close(&self) {
        for (_, registration) in self.shared.registrations.lock().unwrap().drain() {
            registration.cancel.cancel();
        }
        if let Some((_, accept)) = self.listening.get() {
            accept.abort();
        }
    }
}

impl Drop for McpHttpHost {
    fn drop(&mut self) {
        self.close();
    }
}

async fn listen(shared: Arc<Shared>) -> std::io::Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://127.0.0.1:{}{PATH}", listener.local_addr()?.port());
    let accept = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let shared = shared.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| handle(shared.clone(), request));
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    Ok((url, accept))
}

fn status(code: StatusCode) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = code;
    response
}

fn bearer(request: &Request<Incoming>) -> Option<&str> {
    request.headers().get(hyper::header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}

async fn handle(shared: Arc<Shared>, request: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let service = bearer(&request).and_then(|token| shared.registrations.lock().unwrap().get(token).map(|registration| registration.service.clone()));
    let Some(mut service) = service else { return Ok(status(StatusCode::UNAUTHORIZED)) };
    // Only a fresh POST may open a session; a stale or foreign session id names nothing here.
    if request.headers().get("mcp-session-id").is_none() && request.method() != hyper::Method::POST {
        return Ok(status(StatusCode::NOT_FOUND));
    }
    service.call(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::definition::ToolResult;
    use crate::tools::schema::{string, Input};
    use futures::FutureExt;
    use serde_json::{json, Value};

    async fn post(url: &str, token: &str, session: Option<&str>, body: Value) -> (u16, Option<String>, String) {
        let address = url.trim_start_matches("http://").trim_end_matches(PATH).to_string();
        let stream = tokio::net::TcpStream::connect(&address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
        tokio::spawn(connection);
        let mut builder = Request::post(url)
            .header("host", address.as_str())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(session) = session {
            builder = builder.header("mcp-session-id", session).header("mcp-protocol-version", "2025-06-18");
        }
        let response = sender.send_request(builder.body(Full::new(Bytes::from(body.to_string()))).unwrap()).await.unwrap();
        let code = response.status().as_u16();
        let session = response.headers().get("mcp-session-id").map(|value| value.to_str().unwrap().to_string());
        let mut body = response.into_body();
        let mut text = String::new();
        // A streamed answer ends with the response it carries; stop reading once it has arrived.
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.unwrap().into_data() {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains("\"result\"") || text.contains("\"error\"") {
                    break;
                }
            }
        }
        (code, session, text)
    }

    #[tokio::test]
    async fn serves_each_set_only_to_its_own_token() {
        let host = McpHttpHost::new();
        let echo = BoundTool {
            name: "echo".into(),
            description: "Echo".into(),
            input: Input::new(vec![("text", string().min(1))]),
            handler: Arc::new(|args| async move { ToolResult::text(args["text"].as_str().unwrap().to_string()) }.boxed()),
        };
        let served = host.serve(vec![echo]).await.unwrap();
        let initialize = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } });
        assert_eq!(post(&served.url, "wrong", None, initialize.clone()).await.0, 401);

        let (code, session, text) = post(&served.url, &served.token, None, initialize).await;
        assert_eq!(code, 200, "{text}");
        assert!(text.contains("aicodingtool"));
        let session = session.unwrap();
        post(&served.url, &served.token, Some(&session), json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;

        let (_, _, listed) = post(&served.url, &served.token, Some(&session), json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await;
        assert!(listed.contains("\"echo\"") && listed.contains("\"minLength\":1"), "{listed}");
        let call = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "echo", "arguments": { "text": "hello" } } });
        let (_, _, called) = post(&served.url, &served.token, Some(&session), call).await;
        assert!(called.contains("hello"), "{called}");
        let refused = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "echo", "arguments": {} } });
        let (_, _, refused) = post(&served.url, &served.token, Some(&session), refused).await;
        assert!(refused.contains("Input validation error") && refused.contains("\"isError\":true"), "{refused}");

        served.release();
        let again = json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" });
        assert_eq!(post(&served.url, &served.token, Some(&session), again).await.0, 401);
        host.close();
    }
}
