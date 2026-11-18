//! JSON for the types with the largest trees. A crate that reads or writes one of them through
//! `serde_json` compiles the type's whole tree again, so other crates call these.

use serde::Deserialize;
use serde_json::{Error, Value};

use crate::domain::thread::Thread;
use crate::view::mobile::{MobileThreadHistory, MobileThreadRead, MobileView};
use crate::{AppCommand, WorkspaceEvent, WorkspaceInput, WorkspaceState};

macro_rules! reads {
    ($($type:ty),*) => {$(
        impl $type {
            pub fn from_json(value: &Value) -> Result<Self, Error> {
                Self::deserialize(value)
            }
        }
    )*};
}

macro_rules! writes {
    ($($type:ty),*) => {$(
        impl $type {
            pub fn to_json(&self) -> Result<Value, Error> {
                serde_json::to_value(self)
            }
        }
    )*};
}

reads!(AppCommand, WorkspaceEvent, WorkspaceInput, WorkspaceState, Thread);
writes!(AppCommand, WorkspaceEvent, WorkspaceInput, WorkspaceState, Thread, MobileView, MobileThreadRead, MobileThreadHistory);
