//! OpenCode permission rules for a run's mode and the app's tools.

use serde::Serialize;

use crate::tools::{
    grants_tool, mcp_tool_name, role_limits, tool_reach, ApprovalScope, ServedToolSet, ToolReach,
};
use crate::types::{CoordinationRole, ExecutionPolicy};

/// The name OpenCode files the app's own tools under.
pub const APP_SERVER_NAME: &str = "aicodingtool";
pub const COMPUTER_USE_SERVER_NAME: &str = "cua-driver";

/// One OpenCode permission rule. Rules are read in order and the last that matches decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionRule {
    pub action: String,
    pub resource: String,
    pub effect: &'static str,
}

/// How OpenCode names a tool an MCP server offers.
pub fn open_code_tool_name(server: &str, tool: &str) -> String {
    format!("{server}_{tool}")
}

/// Tools that only look, which every mode but a coordinator's lets run unasked.
const LOOKS: [&str; 9] = [
    "read",
    "glob",
    "grep",
    "list",
    "skill",
    "question",
    "todowrite",
    "todoread",
    "lsp",
];

fn rule(action: &str, effect: &'static str) -> PermissionRule {
    PermissionRule {
        action: action.to_string(),
        resource: "*".into(),
        effect,
    }
}

/// What a mode lets OpenCode's own tools do. Auto has no reviewer to stand on here, so it is held to Edits.
fn mode_rules(policy: ExecutionPolicy) -> Vec<PermissionRule> {
    let looks = || LOOKS.iter().map(|action| rule(action, "allow"));
    match policy {
        ExecutionPolicy::Bypass => vec![rule("*", "allow")],
        ExecutionPolicy::Plan => std::iter::once(rule("*", "ask"))
            .chain(looks())
            .chain([rule("edit", "deny")])
            .collect(),
        ExecutionPolicy::Confirm => std::iter::once(rule("*", "ask")).chain(looks()).collect(),
        ExecutionPolicy::AllowEdits | ExecutionPolicy::Autonomous => {
            std::iter::once(rule("*", "ask"))
                .chain(looks())
                .chain([rule("edit", "allow"), rule("external_directory", "ask")])
                .collect()
        }
    }
}

/// The rules a session runs under: the mode's, then the app's tools each granted as Claude grants
/// them, then what the thread's role keeps from it.
pub fn permission_rules(
    scope: ApprovalScope,
    role: Option<CoordinationRole>,
    sets: &[ServedToolSet],
    computer_use: bool,
) -> Vec<PermissionRule> {
    let mut rules = mode_rules(scope.policy);
    for set in sets {
        for tool in &set.tools {
            let unasked = grants_tool(
                tool_reach(&mcp_tool_name(&set.server, &tool.name), sets),
                Some(scope),
            );
            rules.push(rule(
                &open_code_tool_name(APP_SERVER_NAME, &tool.name),
                if unasked { "allow" } else { "ask" },
            ));
        }
    }
    if computer_use {
        rules.push(rule(
            &format!("{COMPUTER_USE_SERVER_NAME}_*"),
            if grants_tool(ToolReach::ComputerUse, Some(scope)) {
                "allow"
            } else {
                "ask"
            },
        ));
    }
    let limits = role_limits(role);
    if limits.read_only {
        rules.extend([rule("edit", "deny"), rule("shell", "deny")]);
    }
    if limits.no_subagents {
        rules.push(rule("subagent", "deny"));
    }
    if limits.no_native_questions {
        rules.push(rule("question", "deny"));
    }
    rules
}

/// The agent a mode runs as: OpenCode's own planner for Plan, its builder otherwise.
pub fn agent_for(policy: ExecutionPolicy) -> &'static str {
    if policy == ExecutionPolicy::Plan {
        "plan"
    } else {
        "build"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RunChannel;

    /// What the last rule matching `action` says, as OpenCode reads them.
    fn effect(rules: &[PermissionRule], action: &str) -> &'static str {
        rules
            .iter()
            .rev()
            .find(|rule| rule.action == action || rule.action == "*")
            .map_or("ask", |rule| rule.effect)
    }

    #[test]
    fn a_role_keeps_what_it_may_not_do_whatever_the_mode() {
        let scope = ApprovalScope {
            policy: ExecutionPolicy::Bypass,
            channel: RunChannel::Main,
        };
        let coordinator = permission_rules(scope, Some(CoordinationRole::Coordinator), &[], false);
        for action in ["edit", "shell", "subagent", "question"] {
            assert_eq!(effect(&coordinator, action), "deny", "{action}");
        }
        let member = permission_rules(scope, Some(CoordinationRole::Member), &[], false);
        assert_eq!(effect(&member, "question"), "deny");
        assert_eq!(effect(&member, "edit"), "allow");
        assert_eq!(
            effect(&permission_rules(scope, None, &[], false), "question"),
            "allow"
        );
    }
}
