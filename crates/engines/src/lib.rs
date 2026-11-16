//! Agent engine layer: drives Claude Code (stream-json control protocol), Codex (app-server
//! JSON-RPC) and OpenCode (private `opencode serve`) as processes and reports what they do in
//! engine-neutral terms.
//!
//! - `worker::AgentWorker`: main and side channels, each a `coordinator::RunCoordinator` over an
//!   `engines::EngineRouter`; takes `worker::RunCommand`s and emits `coordinator::AgentEvent`s.
//! - `provider`: `RunInput`, `ProviderEvent`, `ProviderResult` and the `AgentProvider` trait every
//!   engine implements.
//! - `brief::RunToolkit` and `tools`: the app's own tools as the host offers them to a run.
//! - `engines`: commands, titles, plan usage, readiness, updates and sign-in outside a run.

pub mod app_plugin;
pub mod background;
pub mod binary;
pub mod brief;
pub mod catalog;
pub mod claude;
pub mod codex;
pub mod coordinator;
pub mod engines;
pub mod markdown;
pub mod mcp_http;
pub mod opencode;
pub mod provider;
pub mod running_totals;
pub mod session_pool;
pub mod session_resources;
pub mod session_turn;
pub mod steer;
pub mod title;
pub mod tools;
pub mod types;
pub mod version;
pub mod worker;
pub mod workflow;
