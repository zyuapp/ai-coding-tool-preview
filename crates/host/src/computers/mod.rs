//! Computers running this app that drive each other over the bridge: discovery on the tailnet, the
//! lines to paired computers, the replica of each one's workspace, and the reads it answers.

pub mod client;
pub mod discovery;
pub mod links;
pub mod queries;
pub mod replica;
