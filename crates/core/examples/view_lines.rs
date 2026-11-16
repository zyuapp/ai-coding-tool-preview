//! The view projection over stdin, for comparing it with the TypeScript `deriveView`: each line is a
//! workspace state, answered with one line holding the parts of `derive_view` both sides draw from,
//! named and shaped as the TypeScript view names them (threads by id). `view_lines <requests-fifo>
//! <answers-fifo>` reads and answers through those pipes instead.

#![recursion_limit = "256"]

use std::io::{BufRead, BufReader, Write};

use act_core::view::workspace::derive_view;
use act_core::workspace::activity::ThreadWait;
use act_core::workspace::location::ThreadLocation;
use act_core::WorkspaceState;
use serde_json::{json, Value};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [requests, answers] = arguments.as_slice() {
        let input = BufReader::new(std::fs::File::open(requests).expect("requests pipe"));
        let output = std::fs::OpenOptions::new().write(true).open(answers).expect("answers pipe");
        serve(input, output);
    } else {
        let stdout = std::io::stdout();
        serve(std::io::stdin().lock(), stdout.lock());
    }
}

fn serve(input: impl BufRead, mut out: impl Write) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(asked) => {
                act_core::util::set_fixed_now(asked["now"].as_i64());
                match asked.get("state").map_or(Ok(WorkspaceState::default()), |state| serde_json::from_value::<WorkspaceState>(state.clone())) {
                    Ok(state) => answer(&state, &asked),
                    Err(error) => json!({ "error": format!("state: {error}") }),
                }
            }
            Err(_) => json!({ "error": "unreadable line" }),
        };
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}

fn answer(state: &WorkspaceState, asked: &Value) -> Value {
    use act_core::view::mobile;
    let task_id = asked["taskId"].as_str().unwrap_or("");
    let value = match asked["kind"].as_str() {
        Some("mobile") => serde_json::to_value(mobile::project_mobile_view(state, asked["at"].as_i64().unwrap_or(0))),
        Some("mobile-archived") => serde_json::to_value(mobile::mobile_archived_threads(state)),
        Some("mobile-read") => serde_json::to_value(mobile::mobile_thread_read(state, task_id)),
        Some("mobile-history") => serde_json::to_value(mobile::mobile_thread_history(state, task_id, asked["before"].as_u64().unwrap_or(0) as usize)),
        Some("summaries") => {
            let filter = &asked["filter"];
            let thread_filter = act_core::view::thread_projection::ThreadFilter {
                scope: serde_json::from_value(filter["scope"].clone()).ok(),
                archived: filter["archived"] == true,
                idle_for_ms: filter["idleForMs"].as_i64(),
                search: filter["search"].as_str().map(String::from),
                stored: filter["stored"].as_array().map(|ids| ids.iter().filter_map(|id| id.as_str().map(String::from)).collect()),
                attachments: filter["attachments"] == true,
                limit: filter["limit"].as_u64().map(|limit| limit as usize),
            };
            serde_json::to_value(act_core::view::thread_projection::thread_summaries(state, &thread_filter, asked["at"].as_i64().unwrap_or(0)))
        }
        Some("transcript") => serde_json::to_value(act_core::view::thread_projection::thread_transcript(state, task_id, asked["limit"].as_u64().map(|n| n as usize), asked["textLimit"].as_u64().map(|n| n as usize), asked["end"].as_u64().map(|n| n as usize))),
        Some("wait") => serde_json::to_value(act_core::view::thread_projection::thread_wait_result(state, task_id, asked["timedOut"] == true)),
        Some("matches") => {
            let (rank, threads) = act_core::view::thread_projection::thread_matches(state, asked["reference"].as_str().unwrap_or(""));
            Ok(json!({ "rank": rank, "threads": ids(threads) }))
        }
        Some("find") => Ok(json!(act_core::view::thread_projection::find_thread(state, asked["reference"].as_str().unwrap_or("")).map(|thread| thread.id.clone()))),
        Some("scope") => Ok(match act_core::view::thread_projection::resolve_scope(state, asked["caller"].as_str().unwrap_or(""), asked["project"].as_str()) {
            Ok(scope) => serde_json::to_value(scope).unwrap_or_default(),
            Err(error) => json!({ "failed": error }),
        }),
        Some("fn") => return call(state, asked["name"].as_str().unwrap_or(""), asked["args"].as_array().cloned().unwrap_or_default()),
        _ => return project(state),
    };
    value.unwrap_or_else(|error| json!({ "error": error.to_string() }))
}

fn arg<T: serde::de::DeserializeOwned>(args: &[Value], index: usize) -> Result<T, String> {
    serde_json::from_value(args.get(index).cloned().unwrap_or(Value::Null)).map_err(|error| format!("argument {index}: {error}"))
}

fn set(args: &[Value], index: usize) -> std::collections::HashSet<String> {
    arg::<Vec<String>>(args, index).unwrap_or_default().into_iter().collect()
}

fn stroke_json(stroke: &act_core::domain::shortcuts::Keystroke) -> Value {
    json!({ "key": stroke.key, "mod": stroke.r#mod, "ctrl": stroke.ctrl, "alt": stroke.alt, "shift": stroke.shift })
}

fn stroke_of(value: Option<&Value>) -> Result<act_core::domain::shortcuts::Keystroke, String> {
    let value = value.ok_or("no keystroke")?;
    let flag = |name: &str| value[name] == true;
    Ok(act_core::domain::shortcuts::Keystroke { key: value["key"].as_str().unwrap_or("").into(), r#mod: flag("mod"), ctrl: flag("ctrl"), alt: flag("alt"), shift: flag("shift") })
}

fn sections(sections: &act_core::workspace::order::ActivitySections) -> Value {
    json!({ "priority": ids(sections.priority.iter().copied()), "running": ids(sections.running.iter().copied()), "threads": ids(sections.threads.iter().copied()), "snoozed": ids(sections.snoozed.iter().copied()) })
}

/// One of the pure helpers the TypeScript tests call directly, by its TypeScript name, answered in the shape the shim compares.
fn call(state: &WorkspaceState, name: &str, args: Vec<Value>) -> Value {
    use act_core::domain::thread::Thread;
    use act_core::shared::Shared;
    use act_core::domain::shortcuts;
    use act_core::reducer::run_queue;
    use act_core::workspace::{activity, coordinated_status, coordination, order};
    let answered = (|| -> Result<Value, String> {
        Ok(match name {
            "orderThreads" => json!(ids(order::order_threads(&arg::<Vec<Shared<Thread>>>(&args, 0)?).iter().map(|thread| &**thread))),
            "activitySections" => {
                let threads: Vec<Thread> = arg(&args, 0)?;
                let listed: Vec<&Thread> = threads.iter().collect();
                sections(&order::activity_sections(&listed, &set(&args, 1), &set(&args, 2)))
            }
            "coordinationSections" => {
                let threads: Vec<Thread> = arg(&args, 0)?;
                let listed: Vec<&Thread> = threads.iter().collect();
                sections(&coordination::coordination_sections(&listed, &set(&args, 1), &set(&args, 2), &arg::<Option<Vec<String>>>(&args, 3)?.unwrap_or_default()))
            }
            "recentIn" => {
                let threads: Vec<Shared<Thread>> = arg(&args, 0)?;
                json!(ids(order::recent_in(&threads, arg::<Option<String>>(&args, 1)?.as_deref(), arg::<f64>(&args, 2)? as usize)))
            }
            "backfillSortIndex" => {
                let mut threads: Vec<Shared<Thread>> = arg(&args, 0)?;
                order::backfill_sort_index(&mut threads);
                json!(threads.iter().map(|thread| json!([thread.id, thread.sort_index])).collect::<Vec<_>>())
            }
            "nextSortIndex" => json!(order::next_sort_index(&arg::<Vec<Shared<Thread>>>(&args, 0)?)),
            "threadMoveBlock" => json!(order::thread_move_block(&arg::<Thread>(&args, 0)?, args.get(1).and_then(|host| host["name"].as_str()), args.get(2) == Some(&Value::Bool(true)))),
            "moveThread" => {
                let mut threads: Vec<Shared<Thread>> = arg(&args, 0)?;
                order::move_thread(&mut threads, &arg::<String>(&args, 1)?, &arg(&args, 2)?);
                json!(threads.iter().map(|thread| json!([thread.id, thread.sort_index, thread.project_id])).collect::<Vec<_>>())
            }
            "orderProjects" => json!(act_core::view::sidebar::order_projects(&arg::<Vec<act_core::domain::project::Project>>(&args, 0)?).iter().map(|project| project.id.clone()).collect::<Vec<_>>()),
            "threadStatus" => json!(match activity::thread_status(&activity::thread_activity(state), &arg::<String>(&args, 1)?) {
                activity::ThreadStatus::Blocked => "blocked",
                activity::ThreadStatus::Paused => "paused",
                activity::ThreadStatus::Working => "working",
                activity::ThreadStatus::Idle => "idle",
            }),
            "waitingOn" => json!(activity::waiting_on(state, arg::<Option<String>>(&args, 1)?.as_deref(), &arg::<String>(&args, 2)?).map(|wait| match wait {
                ThreadWait::Worktree => "worktree",
                ThreadWait::WorktreeRelease => "worktree-release",
                ThreadWait::Run => "run",
            })),
            "threadLists" => {
                let lists = activity::thread_lists(state);
                json!({ "listedThreads": ids(lists.listed_threads.iter().copied()), "visibleThreads": ids(lists.visible_threads.iter().copied()), "archivedThreads": ids(lists.archived_threads.iter().copied()), "worktreeThreadIds": sorted(&lists.worktree_thread_ids), "sideChatAttention": sorted(&lists.side_chat_attention), "unreadCount": lists.unread_count })
            }
            "threadActivity" => {
                let activity = activity::thread_activity(state);
                json!({ "working": sorted(&activity.working), "blocked": sorted(&activity.blocked), "paused": sorted(&activity.paused), "monitoring": sorted(&activity.monitoring), "ranked": sorted(&activity.ranked) })
            }
            "nextSteps" => json!(coordinated_status::next_steps(&arg::<Thread>(&args, 0)?, arg(&args, 1)?, arg::<Option<act_core::domain::pull_request::PullRequestAnswer>>(&args, 2)?.as_ref())),
            "coordinatedThreadStatus" => {
                let thread: Thread = arg(&args, 0)?;
                let standing = coordinated_status::coordinated_thread_status(&thread, &set(&args, 1), &set(&args, 2));
                json!({ "status": standing.status, "summary": standing.summary })
            }
            "promptWithPastes" => json!(run_queue::prompt_with_pastes(&arg::<String>(&args, 0)?, &arg::<Vec<_>>(&args, 1)?)),
            "promptWithAnnotations" => json!(run_queue::prompt_with_annotations(&arg::<String>(&args, 0)?, &arg::<Vec<_>>(&args, 1)?)),
            "promptWithAttachments" => json!(run_queue::prompt_with_attachments(&arg::<String>(&args, 0)?, &arg::<Vec<_>>(&args, 1)?)),
            "promptWithFiles" => json!(run_queue::prompt_with_files(&arg::<String>(&args, 0)?, &arg::<Vec<_>>(&args, 1)?)),
            "textAfterSend" => json!(run_queue::text_after_send(&arg::<String>(&args, 0)?, &arg::<String>(&args, 1)?)),
            "clampQuote" => json!(run_queue::clamp_quote(&arg::<String>(&args, 0)?)),
            "threadTitleFor" => json!(act_core::reducer::runs::thread_title_for(&arg::<String>(&args, 0)?, arg::<Vec<Value>>(&args, 1)?.len())),
            "isExternalCommand" => json!(act_core::validation::is_external_command(args.first().unwrap_or(&Value::Null))),
            "isPhoneCommand" => json!(act_core::validation::phone_command(args.first().unwrap_or(&Value::Null)).is_ok()),
            "supportsComputerCommand" => {
                let capabilities: Option<Vec<String>> = arg(&args, 0)?;
                match act_core::validation::parse_command(args.get(1).unwrap_or(&Value::Null)) {
                    Ok(command) => json!(act_core::validation::supports_computer_command(capabilities.as_deref(), &command)),
                    Err(error) => return Err(format!("unparsed command: {error}")),
                }
            }
            "findHits" => json!(act_core::domain::find::find_hits(&arg::<Vec<Shared<act_core::domain::conversation::ConversationMessage>>>(&args, 0)?, &arg::<String>(&args, 1)?).into_iter().map(|hit| json!({ "messageId": hit.message_id, "field": hit.field, "start": hit.start, "occurrence": hit.occurrence })).collect::<Vec<_>>()),
            "stepMatch" => json!(act_core::domain::find::step_match(arg(&args, 0)?, arg(&args, 1)?, arg(&args, 2)?)),
            "parseShortcut" => json!(args.first().and_then(Value::as_str).and_then(shortcuts::parse_shortcut).map(|stroke| stroke_json(&stroke))),
            "formatShortcut" => json!(shortcuts::format_shortcut(&stroke_of(args.first())?)),
            "keystrokeOf" => {
                let input = args.first().cloned().unwrap_or_default();
                let flag = |name: &str| input[name] == true;
                let key_input = shortcuts::KeyInput { key: input["key"].as_str().unwrap_or("").into(), code: input["code"].as_str().unwrap_or("").into(), meta: flag("meta"), control: flag("control"), alt: flag("alt"), shift: flag("shift") };
                json!(shortcuts::keystroke_of(&key_input, args.get(1) == Some(&Value::Bool(true))).map(|stroke| stroke_json(&stroke)))
            }
            "displayShortcut" => json!(shortcuts::display_shortcut(&arg::<String>(&args, 0)?, args.get(1) == Some(&Value::Bool(true)))),
            "desktopAccelerator" => json!(shortcuts::desktop_accelerator(&arg::<String>(&args, 0)?)),
            "shortcutProblem" => json!(shortcuts::shortcut_problem(&arg::<String>(&args, 0)?)),
            "resolveShortcuts" => json!(shortcuts::resolve_shortcuts(&arg(&args, 0)?)),
            "withShortcut" => json!(shortcuts::with_shortcut(&arg(&args, 0)?, &arg::<String>(&args, 1)?, arg::<Option<String>>(&args, 2)?.as_deref())),
            "shortcutFor" => {
                let bindings: Vec<shortcuts::ShortcutBinding> = arg(&args, 0)?;
                json!(shortcuts::shortcut_for(&bindings, &stroke_of(args.get(1))?, arg(&args, 2)?))
            }
            "isSimulatorInput" => json!(arg::<act_core::domain::platform::SimulatorInput>(&args, 0).is_ok_and(|input| act_core::domain::platform::is_simulator_input(&input))),
            "hidUsage" => json!(act_core::domain::platform::hid_usage(&arg::<String>(&args, 0)?)),
            "parseThreadStore" | "migrateV1ToV2" => {
                use act_core::persisted::legacy::{migrate_v1_to_v2, parse_thread_store, ErrorKind, ThreadStoreParseResult};
                let raw = arg(&args, 0)?;
                match if name == "parseThreadStore" { parse_thread_store(raw) } else { migrate_v1_to_v2(raw) } {
                    ThreadStoreParseResult::Ok { data, hidden_tasks, source_version, preserved_v1 } => json!({ "ok": true, "data": data, "hiddenTasks": hidden_tasks, "sourceVersion": source_version, "preservedV1": preserved_v1 }),
                    ThreadStoreParseResult::Failed { source_version, error_kind, errors, preserved_v1, raw } => json!({ "ok": false, "canWrite": false, "sourceVersion": source_version, "errorKind": if error_kind == ErrorKind::Corrupt { "corrupt" } else { "storage" }, "errors": errors, "preservedV1": preserved_v1, "raw": raw }),
                }
            }
            "serializeThreadStore" => {
                let written = serde_json::to_value(act_core::persisted::legacy::serialize_thread_store(&arg(&args, 0)?).map_err(|error| error.to_string())?).unwrap_or_default();
                let Value::Object(fields) = written else { return Ok(written) };
                Value::Object(fields.into_iter().map(|(key, value)| (key, value.as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or(value))).collect())
            }
            "commandDefinitions" => json!(act_core::validation::DECLARATIONS.iter().map(|declared| json!({ "type": declared.kind, "fields": declared.fields, "at": format!("{:?}", declared.at).to_lowercase(), "leaves": declared.leaves })).collect::<Vec<_>>()),
            "computerCapabilities" => json!(act_core::validation::computer_capabilities()),
            _ => return Err(format!("no Rust counterpart wired for {name}")),
        })
    })();
    answered.unwrap_or_else(|error| json!({ "error": error }))
}

fn ids<'a>(threads: impl IntoIterator<Item = &'a act_core::domain::thread::Thread>) -> Vec<String> {
    threads.into_iter().map(|thread| thread.id.clone()).collect()
}

fn sorted<'a>(ids: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.into_iter().cloned().collect();
    ids.sort();
    ids
}

fn project(state: &WorkspaceState) -> Value {
    let view = derive_view(state);
    let collections = &view.collections;
    let thread = &view.thread;
    let lists = &collections.lists;
    let coordination = &thread.coordination;
    let location = match &thread.location {
        ThreadLocation::Local => json!({ "kind": "local" }),
        ThreadLocation::Creating => json!({ "kind": "creating" }),
        ThreadLocation::Releasing => json!({ "kind": "releasing" }),
        ThreadLocation::Worktree { worktree, threads } => json!({ "kind": "worktree", "worktree": worktree.id, "threads": threads }),
    };
    let waiting_on = thread.waiting_on.map(|wait| match wait {
        ThreadWait::Worktree => "worktree",
        ThreadWait::WorktreeRelease => "worktree-release",
        ThreadWait::Run => "run",
    });
    json!({
        "currentThread": thread.current_thread.map(|thread| thread.id.clone()),
        "activityThreads": {
            "priority": ids(lists.activity_threads.priority.iter().copied()),
            "running": ids(lists.activity_threads.running.iter().copied()),
            "threads": ids(lists.activity_threads.threads.iter().copied()),
            "snoozed": ids(lists.activity_threads.snoozed.iter().copied()),
        },
        "activityProject": lists.activity_project,
        "orderedThreads": ids(lists.ordered_threads.iter().copied()),
        "threadSlots": lists.thread_slots,
        "threads": ids(collections.listed_threads.iter().copied()),
        "archivedThreads": ids(collections.archived_threads.iter().copied()),
        "unreadCount": collections.unread_count,
        "sideChatAttention": sorted(&collections.side_chat_attention),
        "worktreeThreadIds": sorted(&collections.worktree_thread_ids),
        "runningThreadIds": sorted(&collections.every_busy),
        "monitoringThreadIds": sorted(&collections.every_monitoring),
        "blockedThreadIds": sorted(&collections.every_blocked),
        "stoppableCoordinatorIds": sorted(&collections.every_stoppable),
        "status": thread.status,
        "runActive": thread.run_active,
        "compacting": thread.compacting,
        "fastMode": thread.fast_mode,
        "engineLocked": thread.engine_locked,
        "policy": thread.policy,
        "engine": thread.engine,
        "model": thread.model,
        "effort": thread.effort,
        "canGoBack": view.can_go_back,
        "canGoForward": view.can_go_forward,
        "waitingOn": waiting_on,
        "limitPositions": thread.limit_positions,
        "threadTabs": thread.thread_tabs.iter().map(|tab| tab.conversation.id.clone()).collect::<Vec<_>>(),
        "sideChats": thread.side_chats.iter().map(|chat| chat.chat.id.clone()).collect::<Vec<_>>(),
        "location": location,
        "workspaceId": thread.workspace_id,
        "zenAvailable": thread.zen_available,
        "prompt": view.window.prompts.get(&view.prompt_key).cloned().unwrap_or_default(),
        "find": act_core::view::find::find_view(view.window).map(|find| json!({ "matches": find.matches, "index": find.find.index, "counting": find.counting, "hit": find.hit.map(|hit| json!({ "messageId": hit.message_id, "field": hit.field, "start": hit.start, "occurrence": hit.occurrence })) })),
        "coordination": {
            "coordinator": coordination.coordinator,
            "lead": coordination.lead.map(|lead| lead.id.clone()),
            "remoteLead": coordination.remote_lead.as_ref().map(|lead| json!({ "title": lead.title, "computer": lead.computer, "threadId": lead.thread_id })),
            "members": coordination.members.iter().map(|member| json!({ "id": member.thread.id, "status": member.status, "summary": member.summary, "steps": member.steps, "startedAt": member.started_at, "lastHeard": member.last_heard, "host": member.host.as_ref().map(|host| host.name.clone()) })).collect::<Vec<_>>(),
            "starts": coordination.starts.iter().map(|start| json!({ "id": start.id, "at": start.at })).collect::<Vec<_>>(),
            "approvals": coordination.approvals.iter().map(|approval| approval.thread.id.clone()).collect::<Vec<_>>(),
            "decisions": coordination.decisions.iter().map(|decision| json!([decision.thread.id, decision.decision.id])).collect::<Vec<_>>(),
            "proposals": coordination.proposals.iter().map(|proposal| json!({ "id": proposal.proposal.id, "starting": proposal.starting })).collect::<Vec<_>>(),
            "waiting": coordination.waiting.as_ref().map(|waiting| json!({ "decisions": waiting.decisions, "suggestions": waiting.suggestions, "requests": waiting.requests, "blocked": waiting.blocked, "failed": waiting.failed })),
            "asking": coordination.asking,
            "usage": coordination.usage.as_ref().map(|usage| usage.total),
        },
    })
}
