//! What a thread under a coordinator, or the coordinator itself, says for itself.

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Map, Value};

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{array, boolean, enumeration, object, string, Args, Input};
use super::server::{BriefDialect, BriefRun, Grants, ToolRun, ToolServer};
use super::threads::{coordinator_start_input, new_thread_fields};
use super::types::*;

pub const COORDINATION_SERVER_NAME: &str = "aicodingtool-coordination";

pub trait CoordinationBridge: Send + Sync {
    fn report(&self, state: CoordinationState, summary: String) -> BoxFuture<'_, Result<FindingResult, String>>;
    fn decide(&self, request: DecisionRequest) -> BoxFuture<'_, Result<FindingResult, String>>;
    /// A coordinator's own memory. It fails with what to tell the coordinator when the change cannot be made.
    fn remember(&self, change: MemoryChange) -> BoxFuture<'_, Result<String, String>>;
    /// A coordinator's suggested threads, kept for the user to start. It fails with what to tell it when none could be.
    fn propose(&self, proposals: Vec<ProposalRequest>) -> BoxFuture<'_, Result<String, String>>;
}

const COORDINATION_ERROR: &str = "Coordination error: ";

/// Each option names itself, so the answer, which is only its label, says which one the user chose.
pub fn distinct_labels(options: &[Value]) -> bool {
    let labels: HashSet<String> = options.iter().filter_map(|option| option.get("label").and_then(Value::as_str)).map(|label| label.trim().to_lowercase()).collect();
    labels.len() == options.len()
}

fn present(args: &Map<String, Value>, name: &str) -> Option<String> {
    args.text(name).filter(|text| !text.is_empty())
}

fn report_status() -> ToolDefinition<dyn CoordinationBridge> {
    ToolDefinition {
        name: "report_status",
        description: "Tell the coordinator this thread works under where your work stands. Call it before you end the turn in which you finish, get blocked, or fail: your closing message is not a report. The coordinator hears it when your turn ends. A done report names what you delivered: the pull request URL, the commits, or the report.".into(),
        input: Input::new(vec![
            ("state", enumeration(&["working", "blocked", "done", "failed"]).describe("working, blocked, done, or failed.")),
            ("summary", string().trim().min(1).max(MAX_SUMMARY).describe("One or two sentences: the outcome, or what stops you.")),
        ]),
        read_only: false,
        run: |bridge, args| {
            async move {
                report(COORDINATION_ERROR, async {
                    let state: CoordinationState = serde_json::from_value(args.get("state").cloned().unwrap_or_default()).map_err(|error| error.to_string())?;
                    Ok(bridge.report(state, args.text("summary").unwrap_or_default()).await?.note)
                })
                .await
            }
            .boxed()
        },
    }
}

/// A blank question or label, or a label two options share, is refused on its own field.
fn raise_decision() -> ToolDefinition<dyn CoordinationBridge> {
    ToolDefinition {
        name: "raise_decision",
        description: "Put a choice to the user that only they can make: one that changes scope, product behaviour, or anything destructive. It shows in the app at once; their answer arrives in this thread as a message. Offer the options you see with your recommendation, then end your turn if you cannot go on without the answer.".into(),
        input: Input::new(vec![
            ("question", string().trim().min(1).max(MAX_QUESTION).describe("The decision, as one question.")),
            ("context", string().trim().max(MAX_DECISION_CONTEXT).optional().describe("What the user needs to know to decide, including the consequences either way.")),
            (
                "options",
                array(object(vec![
                    ("label", string().trim().min(1).max(MAX_OPTION_LABEL)),
                    ("description", string().trim().max(MAX_OPTION_DESCRIPTION).optional()),
                    ("recommended", boolean().optional()),
                ]))
                .max(MAX_OPTIONS)
                .refine_items(distinct_labels, "Give each option its own label.")
                .describe("The choices, each with a short label of its own and what it means. Mark the one you recommend."),
            ),
        ]),
        read_only: false,
        run: |bridge, args| {
            async move {
                report(COORDINATION_ERROR, async {
                    let options = args.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                    let request = DecisionRequest {
                        question: args.text("question").unwrap_or_default(),
                        context: present(&args, "context"),
                        options: options
                            .iter()
                            .filter_map(Value::as_object)
                            .map(|option| DecisionOption {
                                label: option.text("label").unwrap_or_default(),
                                description: present(option, "description"),
                                recommended: (option.flag("recommended") == Some(true)).then_some(true),
                            })
                            .collect(),
                    };
                    Ok(bridge.decide(request).await?.note)
                })
                .await
            }
            .boxed()
        },
    }
}

/// A refused memory change or suggestion already says what to do instead, so it is answered in its own words.
fn remember() -> ToolDefinition<dyn CoordinationBridge> {
    ToolDefinition {
        name: "remember",
        description: "Save a note to your memory: something the user asked you to remember, or how they want you to work. You and every thread you start from now on read it.".into(),
        input: Input::new(vec![("text", string().trim().min(1).max(MAX_MEMORY_NOTE).describe("The note, in one or two sentences."))]),
        read_only: false,
        run: |bridge, args| async move { report("", bridge.remember(MemoryChange::Remember { text: args.text("text").unwrap_or_default() })).await }.boxed(),
    }
}

fn forget() -> ToolDefinition<dyn CoordinationBridge> {
    ToolDefinition {
        name: "forget",
        description: "Drop a note from your memory when the user asks you to forget it or it no longer holds.".into(),
        input: Input::new(vec![("noteId", string().trim().min(1).max(MAX_MEMORY_NOTE_ID).describe("The note's ID, such as n3."))]),
        read_only: false,
        run: |bridge, args| async move { report("", bridge.remember(MemoryChange::Forget { note_id: args.text("noteId").unwrap_or_default() })).await }.boxed(),
    }
}

fn set_purpose() -> ToolDefinition<dyn CoordinationBridge> {
    ToolDefinition {
        name: "set_purpose",
        description: "Set what this coordinator is for, in the user's words, when they say what its purpose or goal is. Pass null to clear it.".into(),
        input: Input::new(vec![("purpose", string().max(MAX_PURPOSE).nullable().describe("One line, or null to clear it."))]),
        read_only: false,
        run: |bridge, args| async move { report("", bridge.remember(MemoryChange::Purpose { purpose: args.text("purpose") })).await }.boxed(),
    }
}

fn propose_threads() -> ToolDefinition<dyn CoordinationBridge> {
    let thread = coordinator_start_input().with(
        "title",
        string().trim().min(1).max(MAX_PROPOSAL_TITLE).describe("A few words naming what the thread does, which label the suggestion for the user and name the thread once it starts."),
    );
    ToolDefinition {
        name: "propose_threads",
        description: "Suggest threads for the user to start, rather than starting them. Each takes what start_thread takes, brief included, and must have a title. Nothing starts until the user starts it; you hear from each one they start once it ends a turn.".into(),
        input: Input::new(vec![("threads", array(object(thread.fields)).min(1).max(MAX_PROPOSALS).describe("The threads to suggest, one per piece of work."))]),
        read_only: false,
        run: |bridge, args| {
            async move {
                let threads = args.get("threads").and_then(Value::as_array).cloned().unwrap_or_default();
                let proposals = threads
                    .iter()
                    .filter_map(Value::as_object)
                    .map(|thread| {
                        let mut proposal = new_thread_fields(thread);
                        proposal.insert("title".into(), json!(thread.text("title")));
                        proposal.insert("prompt".into(), json!(thread.text("prompt")));
                        if let Some(computer) = present(thread, "computer") {
                            proposal.insert("computer".into(), json!(computer));
                        }
                        Value::Object(proposal)
                    })
                    .collect();
                report("", bridge.propose(proposals)).await
            }
            .boxed()
        },
    }
}

/// A coordinator reports to no one, so it raises decisions, keeps its memory, and suggests threads.
pub fn coordination_definitions(role: CoordinationRole) -> Vec<ToolDefinition<dyn CoordinationBridge>> {
    match role {
        CoordinationRole::Member => vec![report_status(), raise_decision()],
        CoordinationRole::Coordinator => vec![raise_decision(), remember(), forget(), set_purpose(), propose_threads()],
    }
}

pub fn bound_coordination_tools(bridge: Arc<dyn CoordinationBridge>, role: CoordinationRole) -> Vec<BoundTool> {
    bind_tools(bridge, &coordination_definitions(role))
}

fn coordinator_instructions(reads: &str) -> String {
    [
        "You are a coordinator in AI Coding Tool. The user talks to you; threads you start do the work.".to_string(),
        format!("Never change anything yourself, however small. {reads} Read what you need to plan, then delegate every change with start_thread, one thread per piece of work, each with a title of a few words naming what it does and its brief: intent (the user's own words for that piece), doneWhen, and delivers. Pass worktree: true for a thread that edits files; it works in a checkout of its own, so never point it at this project's folder by path in its prompt or doneWhen. Threads you start run on the model the user chose for them; name another only when a piece of work needs it."),
        "When the user asks you to make a change directly, start a thread for it anyway and say so in one line.".into(),
        "You cannot schedule automations. When the user asks to repeat, poll, or watch something on a cadence, start a thread for it and have that thread schedule itself.".into(),
        "Threads you start work under you. After delegating, end your turn: you are woken with their news when they end a turn, report back, or raise a decision, and the user can talk to you meanwhile. A turn the user started by writing to one of them is theirs: unless it fails, you hear it ended the next time you are woken.".into(),
        "To learn where a thread stands or what it found, read it with read_thread. Message a thread only to change what it is doing.".into(),
        "A thread that ends its turn without reporting tells you where it stands only in its last reply: judge from that whether it delivered its brief, and read it with read_thread when the reply says too little.".into(),
        "A message another thread sent names that thread. Credit what it asks to that thread alone, and refer to threads by their ID.".into(),
        "Tell the user only outcomes, decisions waiting on them, and real blockers. Progress, retries and mechanics are not news.".into(),
        "Put a choice only the user can make to them with raise_decision, with options and your recommendation. Never merge, discard work, or do anything destructive or irreversible without the user's explicit word, and never widen what they asked for.".into(),
        "Your memory holds the user's purpose and notes, and every thread you start reads it. When the user asks you to remember or forget something, or says how they want you to work, such as proposing threads before starting them, how many to run at once, or how long your updates are, save it with remember, drop it with forget, or set the purpose with set_purpose. Follow what it says.".into(),
        "To suggest threads rather than start them, call propose_threads with the same brief; the user starts the ones they want. Do that whenever your memory says the user wants to see threads first.".into(),
        "To run a thread on another computer, name it with computer; it starts once the user allows it there.".into(),
    ]
    .join("\n")
}

pub const MEMBER_INSTRUCTIONS: &str = "This thread works under a coordinator thread in AI Coding Tool. Its brief is in your first message; stay within it.\nBefore you end the turn in which you finish your brief, get blocked, or fail, call report_status as done, blocked, or failed: your closing message is not a report. A done report names what you delivered: the pull request URL, the commits, or the report.\nIf you can't reach something you need, say exactly what's missing and stop. Don't substitute, mock, or guess. Say it with report_status as blocked.\nWhen a choice would change the scope of the brief, or only the user can make it, call raise_decision with the options and your recommendation, then end your turn; the answer arrives here as a message.\nA message another thread sent names that thread. Credit what it asks to that thread alone, and refer to threads by their ID.";

/// A member in a checkout of its own is told so, and that the project's checkout is the user's.
fn own_checkout_instructions(workspace_root: &str, project_root: &str) -> String {
    format!("You work in your own checkout, {workspace_root}. The project's main checkout, {project_root}, is the user's: never change anything there. Where your brief or prompt names that folder, it means the same files in your own checkout: make every change and commit here.")
}

/// A coordination role is told how to work whether or not its tools are offered.
pub fn coordination_instructions(run: &BriefRun, dialect: &BriefDialect) -> Vec<String> {
    match run.coordination_role {
        Some(CoordinationRole::Coordinator) => vec![coordinator_instructions(&dialect.coordinator_reads)],
        Some(CoordinationRole::Member) => match &run.project_root {
            None => vec![MEMBER_INSTRUCTIONS.to_string()],
            Some(project_root) => {
                let mut lines = vec![MEMBER_INSTRUCTIONS.to_string(), own_checkout_instructions(&run.workspace_root, project_root)];
                lines.extend(dialect.own_checkout_commits.clone());
                lines
            }
        },
        None => Vec::new(),
    }
}

/// Claude's own tools a coordinator has, and the only ones. Named rather than withheld, so a tool a
/// newer Claude Code adds never reaches a coordinator unasked.
pub const COORDINATOR_TOOLS: [&str; 12] = ["Read", "Grep", "Glob", "LSP", "WebSearch", "WebFetch", "Skill", "ToolSearch", "ListMcpResourcesTool", "ReadMcpResourceTool", "EnterPlanMode", "ExitPlanMode"];

pub fn server() -> ToolServer {
    let mut definitions = names_of(&coordination_definitions(CoordinationRole::Member));
    definitions.extend(names_of(&coordination_definitions(CoordinationRole::Coordinator)).into_iter().filter(|tool| tool.name != "raise_decision"));
    ToolServer {
        name: COORDINATION_SERVER_NAME,
        definitions,
        // A thread speaking for itself to its coordinator and the user reaches nothing but the app.
        grants: Grants::All,
        serve: |run: &ToolRun| {
            let bridge = run.coordination.clone()?;
            let role = run.coordination_role?;
            (!run.review).then(|| bound_coordination_tools(bridge, role))
        },
        brief: |_, dialect, run| coordination_instructions(run, dialect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_need_labels_of_their_own() {
        let input = &raise_decision().input;
        let shared = json!({ "question": "Which?", "options": [{ "label": "A" }, { "label": " a " }] });
        assert!(input.parse(shared.as_object().unwrap()).unwrap_err().contains("Give each option its own label."));
        let distinct = json!({ "question": "Which?", "options": [{ "label": "A", "description": "  " }, { "label": "B", "recommended": true }] });
        assert!(input.parse(distinct.as_object().unwrap()).is_ok());
    }

    #[test]
    fn a_member_in_its_own_checkout_is_told_whose_the_project_is() {
        let run = BriefRun { coordination_role: Some(CoordinationRole::Member), workspace_root: "/wt".into(), project_root: Some("/repo".into()) };
        let dialect = BriefDialect { own_checkout_commits: Some("Commit with git.".into()), ..Default::default() };
        let lines = coordination_instructions(&run, &dialect);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("/wt") && lines[1].contains("/repo"));
    }
}
