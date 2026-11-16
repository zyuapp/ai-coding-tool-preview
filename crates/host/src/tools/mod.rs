//! The app's MCP tools that agents call, and the loopback host that serves them.

pub mod automation;
pub mod browser;
pub mod computer_use;
pub mod coordination;
pub mod definition;
pub mod http_host;
pub mod schema;
pub mod server;
pub mod serving;
pub mod simulator;
pub mod terminal;
pub mod threads;
pub mod types;

pub use definition::{BoundTool, ToolResult};
pub use http_host::{McpHttpHost, ServedTools};
pub use serving::{serve_flat, serve_run, ServedRun, ServedServer};
pub use server::{run_tools, server_briefs, tool_reach, tool_servers, BriefDialect, BriefRun, ServedToolSet, ToolReach, ToolRun, ToolServer};
