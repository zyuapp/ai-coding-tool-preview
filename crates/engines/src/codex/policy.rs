//! How an execution mode reads in Codex's approval and sandbox terms.

use serde_json::{json, Value};

use crate::tools::role_limits;
use crate::types::{CoordinationRole, ExecutionPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexSandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandbox {
    pub fn id(self) -> &'static str {
        match self {
            CodexSandbox::ReadOnly => "read-only",
            CodexSandbox::WorkspaceWrite => "workspace-write",
            CodexSandbox::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexPolicy {
    pub approval_policy: Value,
    pub sandbox: CodexSandbox,
    pub approvals_reviewer: &'static str,
}

/// Codex puts MCP tool calls to the reviewer and refuses escalations out of the sandbox without asking.
fn tool_calls_only() -> Value {
    json!({ "granular": { "sandbox_approval": false, "rules": false, "skill_approval": false, "request_permissions": false, "mcp_elicitations": true } })
}

fn policy_grants(policy: ExecutionPolicy) -> CodexPolicy {
    match policy {
        ExecutionPolicy::Confirm | ExecutionPolicy::Plan => CodexPolicy {
            approval_policy: json!("untrusted"),
            sandbox: CodexSandbox::ReadOnly,
            approvals_reviewer: "user",
        },
        ExecutionPolicy::AllowEdits => CodexPolicy {
            approval_policy: json!("on-request"),
            sandbox: CodexSandbox::WorkspaceWrite,
            approvals_reviewer: "user",
        },
        ExecutionPolicy::Autonomous => CodexPolicy {
            approval_policy: json!("on-request"),
            sandbox: CodexSandbox::WorkspaceWrite,
            approvals_reviewer: "auto_review",
        },
        ExecutionPolicy::Bypass => CodexPolicy {
            approval_policy: json!("never"),
            sandbox: CodexSandbox::DangerFullAccess,
            approvals_reviewer: "user",
        },
    }
}

/// Codex has no plan mode, so a plan run is held to what a confirm run may do. A read-only run's
/// shell is read-only, and every escalation out of it is refused.
pub fn codex_policy(policy: ExecutionPolicy, role: Option<CoordinationRole>) -> CodexPolicy {
    let granted = policy_grants(policy);
    if !role_limits(role).read_only {
        return granted;
    }
    if policy == ExecutionPolicy::Autonomous {
        CodexPolicy {
            approval_policy: tool_calls_only(),
            sandbox: CodexSandbox::ReadOnly,
            ..granted
        }
    } else {
        CodexPolicy {
            sandbox: CodexSandbox::ReadOnly,
            approvals_reviewer: "user",
            ..granted
        }
    }
}

/// The temporary folders workspace-write lets every command write in beside its own checkout.
#[derive(Debug, Clone, Default)]
pub struct TemporaryFolders {
    pub slash_tmp: Vec<String>,
    pub tmpdir: Vec<String>,
}

fn both_ways(folder: &str) -> Vec<String> {
    match std::fs::canonicalize(folder) {
        Ok(real) => vec![folder.to_string(), real.to_string_lossy().into_owned()],
        Err(_) => vec![folder.to_string()],
    }
}

fn temporary_folders() -> TemporaryFolders {
    TemporaryFolders {
        slash_tmp: both_ways("/tmp"),
        tmpdir: std::env::var("TMPDIR")
            .ok()
            .filter(|dir| !dir.is_empty())
            .map(|dir| both_ways(&dir))
            .unwrap_or_default(),
    }
}

/// Whether `candidate` is `root` or lies inside it, read lexically.
pub fn is_path_inside(root: &str, candidate: &str) -> bool {
    let root = super::home::normalize(std::path::Path::new(root));
    let candidate = super::home::normalize(std::path::Path::new(candidate));
    candidate.starts_with(&root)
}

/// The sandbox a turn runs in. A run that must leave `kept` alone gives up whichever temporary
/// folder holds it.
pub fn sandbox_policy(
    sandbox: CodexSandbox,
    kept: Option<&str>,
    temporary: Option<TemporaryFolders>,
) -> Value {
    match sandbox {
        CodexSandbox::ReadOnly => json!({ "type": "readOnly", "networkAccess": false }),
        CodexSandbox::DangerFullAccess => json!({ "type": "dangerFullAccess" }),
        CodexSandbox::WorkspaceWrite => {
            let (exclude_slash_tmp, exclude_tmpdir) = match kept {
                None => (false, false),
                Some(kept) => {
                    let folders = temporary.unwrap_or_else(temporary_folders);
                    let holds = |candidates: &[String]| {
                        candidates.iter().any(|folder| is_path_inside(folder, kept))
                    };
                    (holds(&folders.slash_tmp), holds(&folders.tmpdir))
                }
            };
            json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false, "excludeTmpdirEnvVar": exclude_tmpdir, "excludeSlashTmp": exclude_slash_tmp })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinator_is_read_only_whatever_the_mode() {
        assert_eq!(
            codex_policy(ExecutionPolicy::Bypass, Some(CoordinationRole::Coordinator)).sandbox,
            CodexSandbox::ReadOnly
        );
        let auto = codex_policy(
            ExecutionPolicy::Autonomous,
            Some(CoordinationRole::Coordinator),
        );
        assert_eq!(auto.approvals_reviewer, "auto_review");
        assert!(auto.approval_policy["granular"].is_object());
        let kept = sandbox_policy(
            CodexSandbox::WorkspaceWrite,
            Some("/tmp/project"),
            Some(TemporaryFolders {
                slash_tmp: vec!["/tmp".into()],
                tmpdir: vec!["/var/t".into()],
            }),
        );
        assert_eq!(kept["excludeSlashTmp"], true);
        assert_eq!(kept["excludeTmpdirEnvVar"], false);
    }
}
