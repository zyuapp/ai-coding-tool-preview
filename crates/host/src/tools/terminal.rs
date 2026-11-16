//! The terminal panel, which a run may only read: the shell is the user's, and a run has Bash of its own.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{number, string, Args, Input};
use super::server::{Grants, Offers, ToolRun, ToolServer};
use super::types::*;

pub const TERMINAL_SERVER_NAME: &str = "aicodingtool-terminal";

pub trait TerminalBridge: Send + Sync {
    fn read(&self, read: TerminalRead) -> BoxFuture<'_, Result<TerminalReadResult, String>>;
}

const TERMINAL_ERROR: &str = "Terminal error: ";

fn snapshot_text(snapshot: &TerminalSnapshot) -> String {
    let state = match snapshot.status {
        TerminalStatus::Running => "running".to_string(),
        TerminalStatus::Exited => snapshot.error.clone().unwrap_or_else(|| format!("exited{}", snapshot.exit_code.map(|code| format!(" ({code})")).unwrap_or_default())),
    };
    let mut heading = vec![format!("{} — {}", snapshot.title, snapshot.cwd), state];
    if let Some(matched) = snapshot.matched {
        heading.push(format!("{matched} matching lines"));
    }
    if snapshot.omitted > 0 {
        heading.push(format!("{} earlier lines not shown", snapshot.omitted));
    }
    let body = if snapshot.lines.is_empty() { "(this terminal has printed nothing)".to_string() } else { snapshot.lines.join("\n") };
    format!("{}\n\n{body}", heading.join(" · "))
}

pub fn read_text(result: &TerminalReadResult) -> String {
    match result {
        TerminalReadResult::Snapshot { snapshot } => snapshot_text(snapshot),
        TerminalReadResult::Terminals { terminals } if !terminals.is_empty() => terminals.iter().map(TerminalSession::describe).collect::<Vec<_>>().join("\n"),
        _ => "The terminal panel has no terminal open.".into(),
    }
}

pub fn terminal_tools() -> Vec<ToolDefinition<dyn TerminalBridge>> {
    vec![
        ToolDefinition {
            name: "terminal_list",
            description: "List the terminals the AICodingTool terminal panel has open, with the folder each runs in and whether its shell is still alive.".into(),
            input: Input::empty(),
            read_only: true,
            run: |bridge, _| async move { report(TERMINAL_ERROR, async { Ok(read_text(&bridge.read(TerminalRead::Terminals).await?)) }).await }.boxed(),
        },
        ToolDefinition {
            name: "terminal_read",
            description: "Read what a terminal in the AICodingTool terminal panel has printed, as plain text with the escape sequences resolved. This is the user's own shell, not yours: you can read it but never type into it, so use Bash to run anything yourself. Prefer `match` over a large `lines` when hunting for an error — it filters before the output is returned.".into(),
            input: Input::new(vec![
                ("terminalId", string().max(MAX_ID_LENGTH).optional().describe("Which terminal to read. Defaults to the one this thread opened, else the one on screen.")),
                ("lines", number().nonnegative().optional().describe(format!("How many of the newest lines to return. Defaults to {DEFAULT_TERMINAL_LINES}, and never exceeds {MAX_TERMINAL_LINES}."))),
                ("match", string().max(MAX_SEARCH_TEXT).optional().describe("Keep only lines containing this text, case-insensitively.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(TERMINAL_ERROR, async {
                        let read = TerminalRead::Snapshot {
                            terminal_id: args.text("terminalId").filter(|id| !id.is_empty()),
                            lines: args.float("lines"),
                            r#match: args.text("match").filter(|text| !text.is_empty()),
                        };
                        Ok(read_text(&bridge.read(read).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
    ]
}

pub fn bound_terminal_tools(bridge: Arc<dyn TerminalBridge>) -> Vec<BoundTool> {
    bind_tools(bridge, &terminal_tools())
}

const TERMINAL_INSTRUCTIONS: &str = "Use terminal_list and terminal_read to see what the user's terminals in the AICodingTool terminal panel have printed. Those shells are the user's; run your own commands in your own.";

pub fn server() -> ToolServer {
    ToolServer {
        name: TERMINAL_SERVER_NAME,
        definitions: names_of(&terminal_tools()),
        grants: Grants::None,
        serve: |run: &ToolRun| Some(bound_terminal_tools(run.terminal.clone()?)),
        brief: |offers: &Offers, _, _| if offers.any() { vec![TERMINAL_INSTRUCTIONS.to_string()] } else { Vec::new() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exited_shell_reads_its_error_before_its_code() {
        let snapshot = TerminalSnapshot { terminal_id: "t".into(), title: "zsh".into(), cwd: "/r".into(), status: TerminalStatus::Exited, exit_code: Some(1), error: None, lines: vec![], omitted: 3, matched: Some(0) };
        assert_eq!(snapshot_text(&snapshot), "zsh — /r · exited (1) · 0 matching lines · 3 earlier lines not shown\n\n(this terminal has printed nothing)");
    }
}
