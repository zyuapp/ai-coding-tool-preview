//! The window's workspace, reachable from a run: reads are projections, writes are commands.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Map, Value};

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{boolean, enumeration, number, string, Args, Field, Input};
use super::server::{BriefDialect, Grants, Offers, ToolRun, ToolServer};
use super::types::*;
use act_core::domain::engine::{is_model_id, AGENT_ENGINES, ENGINE_IDS};

pub const THREAD_SERVER_NAME: &str = "aicodingtool-threads";

pub trait ThreadBridge: Send + Sync {
    fn list(&self, query: ThreadListQuery) -> BoxFuture<'_, Result<Vec<ThreadSummary>, String>>;
    fn read(&self, thread_id: String, limit: Option<f64>, computer: Option<String>) -> BoxFuture<'_, Result<ThreadTranscript, String>>;
    fn wait(&self, thread_id: String, timeout_ms: u64) -> BoxFuture<'_, Result<ThreadWaitResult, String>>;
    /// `computer` names the paired computer a new thread starts on; a named thread is acted on wherever it is.
    fn command(&self, command: ExternalCommand, computer: Option<String>) -> BoxFuture<'_, Result<ThreadCommandResult, String>>;
}

/// The bridge with the clock idle times are measured against.
pub struct ThreadToolContext {
    pub bridge: Arc<dyn ThreadBridge>,
    pub now: fn() -> i64,
}

const MINUTE: f64 = 60_000.0;
const DEFAULT_WAIT_MS: u64 = 5 * 60_000;

pub const AGENT_EFFORTS: [&str; 6] = ["ultra", "max", "xhigh", "high", "medium", "low"];
pub const THREAD_ROLES: [&str; 4] = ["coordinator", "implementer", "reviewer", "researcher"];
pub const DELIVERIES: [&str; 3] = ["pull-request", "commit", "report"];
/// The models this build ships knowing, each with the name the picker gives it where that is not the id itself.
fn built_in_models() -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for model in AGENT_ENGINES.iter().flat_map(|engine| engine.spec().models) {
        let shown = if model.label.to_lowercase() == model.id { model.id.to_string() } else { format!("{} ({})", model.id, model.label) };
        if !named.contains(&shown) {
            named.push(shown);
        }
    }
    named
}

fn hyphenate(name: String) -> String {
    let mut hyphenated = String::with_capacity(name.len());
    let mut in_space = false;
    for character in name.chars() {
        if character.is_whitespace() {
            if !in_space {
                hyphenated.push('-');
            }
            in_space = true;
        } else {
            hyphenated.push(character);
            in_space = false;
        }
    }
    hyphenated
}

fn thread_id_field() -> Field {
    string().min(1).max(MAX_ID_LENGTH).describe("The thread's known ID, an unambiguous prefix of it, or its exact title, on this computer or a paired one. Use list_threads only if you need to discover the target.")
}

fn project_field() -> Field {
    string().min(1).max(MAX_PROPOSAL_PROJECT).optional().describe("\"current\" (the default) for the project this thread belongs to, \"all\" for every project, or a project named by its folder name or its path.")
}

/// A name said with spaces, as "GLM 5.3 Flash", is taken hyphenated, which the window reads as the same name.
fn model_field(omitted: &str) -> Field {
    string()
        .trim()
        .max(256)
        .transform(hyphenate)
        .refine(is_model_id, "Not a model this thread can start on.")
        .optional()
        .describe(format!(
            "Model for the new thread: one of {}; an engine ({}) for that engine's default model; or any other model in the app's model picker. Name a model by its id or by the name the picker shows; a name several providers offer resolves to the one the user pinned. Omit to {omitted}",
            built_in_models().join(", "),
            ENGINE_IDS.join(", ")
        ))
}

fn effort_field(omitted: &str) -> Field {
    enumeration(&AGENT_EFFORTS).optional().describe(format!("Effort for the new thread. Omit to {omitted}"))
}

fn role_field() -> Field {
    enumeration(&THREAD_ROLES).optional().describe("The part the new thread plays beside the others: coordinator, implementer, reviewer, or researcher. Set reviewer for review threads; a reviewer starts on Auto, or on Bypass where its engine has no Auto, and stays in Threads unless it needs the user's approval.")
}

/// What a start takes beyond the computer it runs on. Blank optional fields are trimmed to nothing and dropped.
fn new_thread_input() -> Input {
    Input::new(vec![
        ("prompt", string().trim().min(1).max(MAX_PROMPT_LENGTH).describe("The first message of the new thread. It has none of this conversation's context, so say everything it needs.")),
        ("title", string().trim().min(1).max(MAX_PROPOSAL_TITLE).optional().describe("A few words naming what the new thread does, shown wherever it is listed. Omit to have it named from its prompt.")),
        ("project", string().trim().max(MAX_PROPOSAL_PROJECT).optional().describe("Which project to start it in: its folder name, its path, or its id. Defaults to this thread's project, or on a paired computer the project there at the same path or with the same folder name.")),
        ("worktree", boolean().optional().describe("Run the new thread in its own new git worktree, detached at whatever the project has checked out, so its edits never touch this thread's checkout. Name files in its prompt relative to the project, never by this checkout's absolute path, which would send it back here.")),
        ("worktreeId", string().trim().max(MAX_ID_LENGTH).optional().describe("Start the thread in another worktree that already exists, as list_threads reports it, on the computer it starts on. Omit both worktree fields to share this thread's checkout. Takes precedence over worktree.")),
        ("model", model_field("inherit the calling thread's model.")),
        ("engine", enumeration(&ENGINE_IDS).optional().describe("The engine to run model on, needed only when more than one engine offers it.")),
        ("effort", effort_field("inherit the calling thread's effort when the selected model supports it, otherwise use that engine's default.")),
        ("role", role_field()),
        ("intent", string().trim().max(MAX_BRIEF_FIELD).optional().describe("Part of the brief a coordinator must give: the user's own words for this piece of work.")),
        ("doneWhen", string().trim().max(MAX_BRIEF_FIELD).optional().describe("Part of the brief a coordinator must give: what finished looks like, checkably.")),
        ("delivers", enumeration(&DELIVERIES).optional().describe("Part of the brief a coordinator must give: pull-request, commit, or report (knowledge only, no code changes).")),
    ])
}

fn start_input() -> Input {
    new_thread_input().with("computer", string().trim().max(MAX_PROPOSAL_COMPUTER).optional().describe("A paired computer to start it on, named by its name or ID, or \"this\" (the default). There it starts in the project checkout unless worktree or worktreeId says otherwise."))
}

/// A start for a thread that will work under a coordinator runs on what the coordinator's threads run on.
fn led_start_input(whose: &str) -> Input {
    new_thread_input()
        .with("model", model_field(&format!("run it on the model {whose} threads run on.")))
        .with("effort", effort_field(&format!("run it at the effort {whose} threads run at, or the nearest one the selected model takes.")))
}

/// What a coordinator's start_thread takes, and so each thread it suggests.
pub fn coordinator_start_input() -> Input {
    led_start_input("this coordinator's").with("computer", string().trim().max(MAX_PROPOSAL_COMPUTER).optional().describe("A paired computer to run it on. It starts there once the user allows it."))
}

fn member_start_input() -> Input {
    led_start_input("your coordinator's").with(
        "computer",
        string().trim().max(MAX_PROPOSAL_COMPUTER).optional().describe("A paired computer to run it on. It starts there once the user allows it. A thread whose coordinator is on another computer starts its threads on this one."),
    )
}

fn present(args: &Map<String, Value>, name: &str) -> Option<String> {
    args.text(name).filter(|text| !text.is_empty())
}

/// What a start's arguments ask of the new thread beyond its prompt. A brief counts only once it is whole.
pub fn new_thread_fields(args: &Map<String, Value>) -> Map<String, Value> {
    let mut fields = Map::new();
    if let (Some(intent), Some(done_when), Some(delivers)) = (present(args, "intent"), present(args, "doneWhen"), present(args, "delivers")) {
        fields.insert("brief".into(), json!({ "intent": intent, "doneWhen": done_when, "delivers": delivers }));
    }
    for name in ["title", "project"] {
        if let Some(value) = present(args, name) {
            fields.insert(name.into(), json!(value));
        }
    }
    if let Some(worktree_id) = present(args, "worktreeId") {
        fields.insert("worktreeId".into(), json!(worktree_id));
    } else if args.flag("worktree") == Some(true) {
        fields.insert("worktree".into(), json!(true));
    }
    for name in ["model", "engine", "effort", "role"] {
        if let Some(value) = present(args, name) {
            fields.insert(name.into(), json!(value));
        }
    }
    fields
}

fn elapsed(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 48 {
        let rest = minutes % 60;
        return if rest > 0 { format!("{hours}h {rest}m") } else { format!("{hours}h") };
    }
    format!("{}d", hours / 24)
}

fn iso(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default()
}

pub fn describe(thread: &ThreadSummary, at: i64) -> String {
    let mut parts = vec![format!("{} [{}]", thread.title, thread.id)];
    if let Some(role) = &thread.role {
        parts.push(format!("role {role}"));
    }
    if let Some(computer) = &thread.computer {
        parts.push(format!("computer {} [{}]{}", computer.name, computer.id, if computer.offline { " (offline, cached)" } else { "" }));
    }
    parts.push(thread.worktree_root.clone().or_else(|| thread.project_root.clone()).unwrap_or_else(|| "no project".into()));
    if let Some(worktree) = &thread.worktree_id {
        parts.push(format!("worktree {worktree}"));
    }
    parts.push(thread.status.as_str().into());
    if let Some(until) = thread.paused_until {
        parts.push(format!("paused by its usage limit until {}", iso(until)));
    }
    parts.push(format!("{} messages", thread.message_count));
    if thread.attachment_count > 0 {
        parts.push(format!("{} with images", thread.attachment_count));
    }
    parts.push(format!("idle {}", elapsed((at - thread.last_activity_at).max(0))));
    if thread.archived {
        parts.push("archived".into());
    }
    parts.join(" · ")
}

fn transcript_text(transcript: &ThreadTranscript, at: i64) -> String {
    let mut lines = vec![describe(&transcript.thread, at)];
    if transcript.omitted > 0 {
        lines.push(format!("({} earlier messages not shown)", transcript.omitted));
    }
    lines.push(String::new());
    for message in &transcript.messages {
        let origin = message.origin.as_ref().map(|origin| format!(" · {origin}")).unwrap_or_default();
        lines.push(format!("[{}{origin}] {}", message.kind, message.text));
    }
    lines.join("\n")
}

const THREAD_ERROR: &str = "Thread error: ";

fn thread_tools(start: Input) -> Vec<ToolDefinition<ThreadToolContext>> {
    vec![
        ToolDefinition {
            name: "list_threads",
            description: "List the other AICodingTool threads, newest activity first. Set computer to \"all\" to include paired computers. Use when the user asks what else is going on, points at recent or related work, or describes threads by age rather than by name.".into(),
            input: Input::new(vec![
                ("computer", string().min(1).max(MAX_PROPOSAL_COMPUTER).optional().describe("\"this\" (the default), \"all\" paired computers and this one, or a paired computer named by its name or ID. With \"all\" or a paired computer selected, project defaults to \"all\". Remote rows include their computer and whether it is offline.")),
                ("project", project_field()),
                ("archived", boolean().optional().describe("List archived threads instead of active ones.")),
                ("idleMinutes", number().nonnegative().optional().describe("Only threads that have done nothing for at least this many minutes.")),
                ("search", string().min(1).max(MAX_SEARCH_TEXT).optional().describe("Only threads whose title or messages contain this text.")),
                ("hasImages", boolean().optional().describe("Only threads where a message carries an image.")),
                ("limit", number().nonnegative().optional().describe("How many threads to return. Defaults to 20.")),
            ]),
            read_only: true,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let at = (context.now)();
                        let query = ThreadListQuery {
                            computer: args.text("computer"),
                            project: args.text("project"),
                            archived: args.flag("archived"),
                            idle_for_ms: args.float("idleMinutes").map(|minutes| minutes * MINUTE),
                            search: args.text("search"),
                            attachments: args.flag("hasImages"),
                            limit: Some(args.float("limit").unwrap_or(20.0)),
                        };
                        let threads = context.bridge.list(query).await?;
                        Ok(if threads.is_empty() { "No thread matches.".into() } else { threads.iter().map(|thread| describe(thread, at)).collect::<Vec<_>>().join("\n") })
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "read_thread",
            description: "Read another thread's transcript, including threads on paired computers. Use a known ID directly, including one supplied in app context or an aicodingtool://thread/<id> link. The app resolves its computer automatically. Use list_threads first only when the target is unknown.".into(),
            input: Input::new(vec![
                ("threadId", thread_id_field()),
                ("computer", string().min(1).max(MAX_PROPOSAL_COMPUTER).optional().describe("Optional computer name or ID, or \"this\" for local threads. Omit to resolve the thread across this computer and paired computers. Use to disambiguate matching threads.")),
                ("limit", number().nonnegative().optional().describe("How many of the newest messages to read. Defaults to 30.")),
            ]),
            read_only: true,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let transcript = context.bridge.read(args.text("threadId").unwrap_or_default(), args.float("limit"), args.text("computer")).await?;
                        Ok(transcript_text(&transcript, (context.now)()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "wait_for_thread",
            description: "Wait until a thread stops working and report what it last said. Use after start_thread or message_thread when the user is waiting on that work; polling read_thread instead just burns turns. A wait that runs out says so, and calling it again keeps waiting.".into(),
            input: Input::new(vec![
                ("threadId", thread_id_field()),
                ("timeoutSeconds", number().optional().describe("How long to wait before reporting back regardless. Defaults to 300, and cannot exceed 900.")),
            ]),
            read_only: true,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let timeout_ms = args.float("timeoutSeconds").map_or(DEFAULT_WAIT_MS, |seconds| (seconds.max(0.0) * 1_000.0) as u64).min(MAX_THREAD_WAIT_MS);
                        let waited = context.bridge.wait(args.text("threadId").unwrap_or_default(), timeout_ms).await?;
                        let heading = if waited.timed_out { format!("Still working after {}s", (timeout_ms as f64 / 1_000.0).round()) } else { "Finished".into() };
                        let mut lines = vec![format!("{heading}: {}", describe(&waited.thread, (context.now)()))];
                        if let Some(reply) = waited.reply.filter(|reply| !reply.is_empty()) {
                            lines.push(String::new());
                            lines.push(reply);
                        }
                        Ok(lines.join("\n"))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "start_thread",
            description: "Start a new AICodingTool thread on its own prompt and run it. Use when the user asks for separate pieces of work to run side by side, one thread per piece. The new thread runs with the permission policy the app is set to on the computer it runs on, except a reviewer, which starts on Auto, or on Bypass where its engine has no Auto; write a prompt that stands on its own. It starts in this thread's checkout, worktree included. Pass worktree to give it an isolated checkout instead, which is what you want when it edits the same files as this thread.".into(),
            input: start,
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let mut command = Map::new();
                        command.insert("type".into(), json!("task.send"));
                        command.insert("text".into(), json!(args.text("prompt").unwrap_or_default()));
                        command.extend(new_thread_fields(&args));
                        let result = context.bridge.command(Value::Object(command), present(&args, "computer")).await?;
                        Ok(match result.thread {
                            Some(thread) => format!("Started {}", describe(&thread, (context.now)())),
                            None => result.notice.unwrap_or_else(|| "The thread did not start.".into()),
                        })
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "message_thread",
            description: "Send a message to another thread, on this computer or a paired one. A thread that is working reads it in the run already going, and an idle one starts a run on it. To learn where a thread stands, read it with read_thread instead.".into(),
            input: Input::new(vec![("threadId", thread_id_field()), ("text", string().trim().min(1).max(MAX_PROMPT_LENGTH).describe("The message to send."))]),
            read_only: false,
            // A message from another thread is meant for the work in hand, so it joins the run rather than waiting behind it.
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let command = json!({ "type": "task.send", "taskId": args.text("threadId"), "text": args.text("text"), "steer": true });
                        let result = context.bridge.command(command, None).await?;
                        Ok(result.thread.map(|thread| format!("Sent to {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The message was not delivered.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "set_thread_role",
            description: "Give a thread its part beside the others, or take it off. The role shows on the thread's row and in list_threads. A coordinator changes no files itself and delegates to threads it starts, and keeps its role until the user changes it; the other roles change nothing about what the thread may do.".into(),
            input: Input::new(vec![("threadId", thread_id_field()), ("role", enumeration(&THREAD_ROLES).nullable().describe("coordinator, implementer, reviewer, or researcher. null takes the role off."))]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let role = args.text("role");
                        let command = json!({ "type": "task.set-role", "taskId": args.text("threadId"), "role": role });
                        let result = context.bridge.command(command, None).await?;
                        let verb = if role.is_some() { "Set the role of" } else { "Took the role off" };
                        Ok(result.thread.map(|thread| format!("{verb} {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The thread's role was not changed.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "archive_thread",
            description: "Archive a thread, which also cancels a run it still has going and retires its automation. Archived threads stay recoverable for five days. This throws away work in progress, so only archive when the user asked for it.".into(),
            input: Input::new(vec![("threadId", thread_id_field())]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let result = context.bridge.command(json!({ "type": "task.archive", "taskId": args.text("threadId") }), None).await?;
                        Ok(result.thread.map(|thread| format!("Archived {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The thread was not archived.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "dismiss_thread",
            description: "Dismiss a thread, taking it out of Priority as the sidebar's Dismiss does. Use when the user asks to dismiss a thread, such as this one once its last step is done. Dismissing this thread while it runs takes effect when this turn finishes; a turn that fails still surfaces.".into(),
            input: Input::new(vec![("threadId", string().min(1).max(MAX_ID_LENGTH).optional().describe("The thread's ID, an unambiguous prefix of it, or its exact title. Omit to dismiss this thread."))]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let mut command = json!({ "type": "task.dismiss" });
                        if let Some(thread) = present(&args, "threadId") {
                            command["taskId"] = json!(thread);
                        }
                        let result = context.bridge.command(command, None).await?;
                        Ok(result.thread.map(|thread| format!("Dismissed {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The thread was not dismissed.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "move_to_worktree",
            description: "Move a thread into a new worktree, one the project already has, or back to the project checkout, as the thread menu's Move to worktree does. This is how a thread works in a worktree: use it whenever the user asks for one, never git worktree, so the app knows where the thread works. File changes already made stay in the old checkout. Moving this thread waits for this turn to end, and its next turn starts in the new checkout, on its own when you pass then. Another thread must not be running.".into(),
            input: Input::new(vec![
                ("threadId", string().max(MAX_ID_LENGTH).nullable().optional().describe("The thread to move: its ID, an unambiguous prefix of it, or its exact title. Omit or null to move this thread.")),
                ("worktreeId", string().max(MAX_ID_LENGTH).nullable().describe("\"new\" for a new worktree, the ID of one list_threads reports, or null for the project checkout.")),
                ("then", string().trim().max(MAX_PROMPT_LENGTH).nullable().describe("When moving this thread, whatever else the user asked for along with the move, written as instructions to yourself. This thread's next turn starts in the new checkout on its own with it. null when the move is all the user asked for, or when moving another thread.")),
            ]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let destination = match present(&args, "worktreeId").as_deref() {
                            Some("new") => json!({ "kind": "new" }),
                            Some(id) => json!({ "kind": "worktree", "id": id }),
                            None => json!({ "kind": "local" }),
                        };
                        let mut command = json!({ "type": "task.move-worktree", "destination": destination });
                        if let Some(thread) = present(&args, "threadId") {
                            command["taskId"] = json!(thread);
                        }
                        if let Some(then) = present(&args, "then") {
                            command["then"] = json!(then);
                        }
                        let result = context.bridge.command(command, None).await?;
                        if let Some(notice) = result.notice {
                            return Ok(notice);
                        }
                        Ok(result.thread.map(|thread| format!("Moved {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The thread was not moved.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "delete_worktree",
            description: "Delete a worktree the app made, by the id list_threads reports. Uncommitted work is committed first so it stays recoverable, then the folder is removed; git-ignored files are lost. Every thread in it moves back to the project checkout. Refused while any of those threads is running. Only delete when the user asked for it, such as cleaning up after coordinated work is done.".into(),
            input: Input::new(vec![("worktreeId", string().min(1).max(MAX_ID_LENGTH).describe("The worktree's ID, as list_threads reports it."))]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let worktree = args.text("worktreeId").unwrap_or_default();
                        let result = context.bridge.command(json!({ "type": "worktree.delete", "worktreeId": worktree }), None).await?;
                        Ok(result.notice.unwrap_or_else(|| format!("Deleted worktree {worktree}.")))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "stop_thread",
            description: "Stop the run a thread has going, leaving the thread itself alone. This throws away work in progress, so only stop a run when the user asked for it.".into(),
            input: Input::new(vec![("threadId", thread_id_field())]),
            read_only: false,
            run: |context, args| {
                async move {
                    report(THREAD_ERROR, async {
                        let result = context.bridge.command(json!({ "type": "run.cancel", "taskId": args.text("threadId") }), None).await?;
                        Ok(result.thread.map(|thread| format!("Asked this thread to stop: {}", describe(&thread, (context.now)()))).unwrap_or_else(|| "The thread was not stopped.".into()))
                    })
                    .await
                }
                .boxed()
            },
        },
    ]
}

/// The tools for a run's role. A coordinator is woken with its threads' news, so it never waits on
/// one, and it never moves into a worktree; a member's starts work under its coordinator too.
pub fn thread_definitions(role: Option<CoordinationRole>) -> Vec<ToolDefinition<ThreadToolContext>> {
    match role {
        Some(CoordinationRole::Coordinator) => {
            let mut tools = thread_tools(coordinator_start_input());
            tools.retain(|tool| tool.name != "wait_for_thread" && tool.name != "move_to_worktree");
            tools
        }
        Some(CoordinationRole::Member) => thread_tools(member_start_input()),
        None => thread_tools(start_input()),
    }
}

pub fn bound_thread_tools(bridge: Arc<dyn ThreadBridge>, now: fn() -> i64, role: Option<CoordinationRole>) -> Vec<BoundTool> {
    bind_tools(Arc::new(ThreadToolContext { bridge, now }), &thread_definitions(role))
}

fn thread_instructions(dialect: &BriefDialect) -> String {
    format!("Use the AICodingTool thread tools, such as start_thread, list_threads and read_thread, when the user's request requires creating, fetching or acting on another AICodingTool thread. App task IDs identify AICodingTool threads, not {}. Conversation history and app-supplied thread context are already available evidence; answering from them does not require a tool call.", dialect.native_work)
}

fn worktree_instructions(dialect: &BriefDialect, moves: bool) -> String {
    let how = if moves {
        "To do the work in this thread, call move_to_worktree with \"new\" and the rest of the request as then; this thread moves into a new worktree when this turn ends and its next turn starts there on its own with then, so end the turn right after the call. Start a thread with start_thread and worktree only when the user wants the work in a separate thread."
    } else {
        "Start a thread with start_thread and worktree to do it."
    };
    let native = dialect.native_worktree.as_ref().map(|native| format!(" or {native}")).unwrap_or_default();
    format!("When the user asks for work in a worktree, use the AICodingTool worktree tools so the app knows where each thread works. {how} Never make one yourself for that with git worktree{native}.")
}

pub fn server() -> ToolServer {
    ToolServer {
        name: THREAD_SERVER_NAME,
        definitions: names_of(&thread_definitions(None)),
        // Reading the workspace changes nothing, so it needs no approval; starting or stopping a run does.
        grants: Grants::ReadOnly,
        // A native review is already isolated; app thread tools would create sidebar tasks instead.
        serve: |run: &ToolRun| {
            let threads = run.threads.clone()?;
            (!run.review).then(|| bound_thread_tools(threads, run.now, run.coordination_role))
        },
        brief: |offers: &Offers, dialect, _| {
            let mut lines = Vec::new();
            if offers.any() {
                lines.push(thread_instructions(dialect));
            }
            if offers.has("start_thread") {
                lines.push(worktree_instructions(dialect, offers.has("move_to_worktree")));
            }
            lines
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> ThreadSummary {
        ThreadSummary {
            computer: None,
            id: "t1".into(),
            title: "Fix".into(),
            role: Some("reviewer".into()),
            project_id: None,
            project_root: Some("/repo".into()),
            worktree_id: None,
            worktree_root: None,
            status: ThreadStatus::Idle,
            paused_until: None,
            archived: false,
            created_at: 0,
            last_activity_at: 0,
            message_count: 3,
            attachment_count: 0,
        }
    }

    #[test]
    fn describes_a_thread_with_its_idle_time() {
        assert_eq!(describe(&summary(), 90 * 60_000), "Fix [t1] · role reviewer · /repo · idle · 3 messages · idle 1h 30m");
        assert_eq!(elapsed(49 * 3_600_000), "2d");
        assert_eq!(elapsed(120 * 60_000), "2h");
    }

    #[test]
    fn a_brief_counts_only_once_whole_and_blank_fields_are_dropped() {
        let args = json!({ "intent": "x", "doneWhen": "y", "title": "", "worktree": true, "worktreeId": "" });
        let fields = new_thread_fields(args.as_object().unwrap());
        assert_eq!(Value::Object(fields), json!({ "worktree": true }));
        let whole = json!({ "intent": "x", "doneWhen": "y", "delivers": "commit", "worktree": true, "worktreeId": "abc" });
        assert_eq!(Value::Object(new_thread_fields(whole.as_object().unwrap())), json!({ "brief": { "intent": "x", "doneWhen": "y", "delivers": "commit" }, "worktreeId": "abc" }));
    }

    #[test]
    fn a_model_named_with_spaces_is_hyphenated_and_checked() {
        let input = start_input();
        let args = json!({ "prompt": "go", "model": " GLM 5.3  Flash " });
        assert_eq!(input.parse(args.as_object().unwrap()).unwrap()["model"], "GLM-5.3-Flash");
        let bad = json!({ "prompt": "go", "model": "toString" });
        assert!(input.parse(bad.as_object().unwrap()).is_err());
    }

    #[test]
    fn a_coordinator_neither_waits_nor_moves() {
        let names: Vec<&str> = thread_definitions(Some(CoordinationRole::Coordinator)).iter().map(|tool| tool.name).collect();
        assert!(!names.contains(&"wait_for_thread") && !names.contains(&"move_to_worktree"));
        assert!(names.contains(&"start_thread"));
    }
}
