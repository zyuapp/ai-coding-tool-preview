//! OpenCode, driven over the HTTP API and event feed of a private `opencode serve`.

pub mod catalog;
pub mod policy;
pub mod provider;
pub mod server;
pub mod services;
pub mod session;

pub use provider::OpenCodeAgentProvider;
pub use session::{OpenCodeSession, OPENCODE_DIALECT};
