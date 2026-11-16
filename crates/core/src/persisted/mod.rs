//! The JSON records `tasks.v3.sqlite` and the profile's storage keep, typed exactly as the
//! TypeScript build writes them. Every record carries the fields this build has no name for in
//! `extra`, so writing a record back never loses what a newer build stored.

pub mod catalog;
pub mod enums;
pub mod js;
pub mod legacy;
pub mod message;
pub mod project;
pub mod shapes;
pub mod storage;
pub mod store;
pub mod thread;

pub use enums::*;
pub use message::*;
pub use project::*;
pub use store::*;
pub use thread::*;

#[cfg(test)]
mod tests;
