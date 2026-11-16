//! Claude Code, driven directly over its stream-json protocol.

pub mod provider;
pub mod query;
pub mod services;
pub mod session;

pub use provider::{ClaudeAgentProvider, CLAUDE_DIALECT};
pub use query::{ClaudeOptions, ClaudeQuery, QueryOutput};
pub use session::ClaudeSession;
