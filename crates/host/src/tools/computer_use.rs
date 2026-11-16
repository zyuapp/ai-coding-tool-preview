//! The one tool a run gets in place of computer use while the user still has to enable it.

use std::sync::Arc;

use futures::FutureExt;

use super::definition::{bind_tools, names_of, ToolDefinition, ToolResult};
use super::schema::Input;
use super::server::{Grants, ToolRun, ToolServer};

pub const COMPUTER_USE_SETUP_SERVER_NAME: &str = "aicodingtool-computer-use";

/// Opens the app's computer-use settings for the user.
pub type RequestSetup = dyn Fn() + Send + Sync;

pub fn computer_use_setup_tools() -> Vec<ToolDefinition<RequestSetup>> {
    vec![ToolDefinition {
        name: "request_setup",
        description: "Use when a task requires operating another application's interface but computer use needs to be enabled in AICodingTool. Call this rather than telling the user to install or configure anything.".into(),
        input: Input::empty(),
        read_only: true,
        run: |request, _| {
            async move {
                request();
                ToolResult::text("AICodingTool opened Settings → Computer use. Ask the user to complete the required permissions, then retry after AICodingTool restarts.")
            }
            .boxed()
        },
    }]
}

pub fn server() -> ToolServer {
    ToolServer {
        name: COMPUTER_USE_SETUP_SERVER_NAME,
        definitions: names_of(&computer_use_setup_tools()),
        grants: Grants::ReadOnly,
        serve: |run: &ToolRun| {
            let request: Arc<RequestSetup> = run.computer_use_setup.clone()?;
            Some(bind_tools(request, &computer_use_setup_tools()))
        },
        brief: |_, _, _| Vec::new(),
    }
}
