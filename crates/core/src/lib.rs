//! The application core: domain types, commands, events, effects, the reducer and the view projection.

pub mod command_fields;
pub mod commands;
pub mod contracts;
pub mod domain;
pub mod effects;
pub mod events;
pub mod identity;
mod json;
pub mod persisted;
pub mod reducer;
pub mod shared;
pub mod state;
pub mod util;
pub mod validation;
pub mod view;
pub mod workspace;

pub use commands::AppCommand;
pub use effects::{Effect, EffectWork};
pub use events::{CommandResult, WorkspaceEvent, WorkspaceInput};
pub use reducer::{reduce, reduce_transition, Transition};
pub use state::WorkspaceState;

/// The workspace state, under the name the architecture gives it.
pub type State = WorkspaceState;
