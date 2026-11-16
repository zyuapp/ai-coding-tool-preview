//! One tool the app offers an agent, described apart from whatever host serves it.

use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Map, Value};

use super::schema::Input;

/// What a tool answers with: one text block, flagged when it reports a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

/// Runs a tool's work, answering a failure as `{prefix}{message}` the way each server's `report` did.
pub async fn report(prefix: &str, work: impl Future<Output = Result<String, String>>) -> ToolResult {
    match work.await {
        Ok(text) => ToolResult::text(text),
        Err(message) => ToolResult::error(format!("{prefix}{message}")),
    }
}

pub type Run<B> = fn(Arc<B>, Map<String, Value>) -> BoxFuture<'static, ToolResult>;

/// `read_only` says a call changes nothing the user can see, which lets a host grant it without asking.
pub struct ToolDefinition<B: ?Sized> {
    pub name: &'static str,
    pub description: String,
    pub input: Input,
    pub read_only: bool,
    pub run: Run<B>,
}

impl<B: ?Sized> Clone for ToolDefinition<B> {
    fn clone(&self) -> Self {
        Self { name: self.name, description: self.description.clone(), input: self.input.clone(), read_only: self.read_only, run: self.run }
    }
}

impl<B: ?Sized + Send + Sync + 'static> ToolDefinition<B> {
    pub fn bind(&self, bridge: Arc<B>) -> BoundTool {
        let run = self.run;
        BoundTool {
            name: self.name.to_string(),
            description: self.description.clone(),
            input: self.input.clone(),
            handler: Arc::new(move |args| run(bridge.clone(), args)),
        }
    }
}

pub fn bind_tools<B: ?Sized + Send + Sync + 'static>(bridge: Arc<B>, definitions: &[ToolDefinition<B>]) -> Vec<BoundTool> {
    definitions.iter().map(|definition| definition.bind(bridge.clone())).collect()
}

/// A definition with its bridge supplied, which is all a host needs to serve it.
#[derive(Clone)]
pub struct BoundTool {
    pub name: String,
    pub description: String,
    pub input: Input,
    pub handler: Arc<dyn Fn(Map<String, Value>) -> BoxFuture<'static, ToolResult> + Send + Sync>,
}

impl BoundTool {
    /// Checks the arguments against the input, then runs the tool. A refusal reads the way the
    /// TypeScript MCP server worded it.
    pub async fn call(&self, arguments: Option<Map<String, Value>>) -> ToolResult {
        match self.input.parse(&arguments.unwrap_or_default()) {
            Ok(arguments) => (self.handler)(arguments).await,
            Err(issues) => ToolResult::error(format!("MCP error -32602: Input validation error: Invalid arguments for tool {}: {issues}", self.name)),
        }
    }
}

/// A server's tool, as its name and whether it only reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolName {
    pub name: &'static str,
    pub read_only: bool,
}

pub fn names_of<B: ?Sized>(definitions: &[ToolDefinition<B>]) -> Vec<ToolName> {
    definitions.iter().map(|definition| ToolName { name: definition.name, read_only: definition.read_only }).collect()
}
