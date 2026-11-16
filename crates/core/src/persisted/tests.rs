use serde_json::{json, Value};

use super::storage::{
    parse_stored_conversation_messages, readable_remote_thread, sanitize_v2_thread, validate_thread_store_data,
    DecodedThreadStore,
};
use super::*;

/// A thread carrying every field the TypeScript `Thread` names, with nested records filled out.
fn every_field() -> Value {
    let origin = json!({ "kind": "thread", "threadId": "t0", "title": "Lead", "computer": "mac" });
    let attachments = json!({
        "attachments": ["/a.png"],
        "annotations": [{ "id": "n1", "quote": "q", "note": "", "anchor": { "kind": "diff", "comparison": "c", "path": "p", "start": "1", "end": "2", "side": "new" } }],
        "pastes": [{ "id": "p1", "text": "pasted" }],
        "files": [{ "id": "f1", "path": "/x", "name": "x", "folder": true }],
    });
    let mut message = json!({ "id": "m1", "kind": "user", "text": "hi", "artifact": true, "answer": true, "origin": origin, "detail": "From Lead", "tone": "error", "withdrawn": true, "at": 1.5 });
    message.as_object_mut().unwrap().extend(attachments.as_object().unwrap().clone());
    let mut held = json!({ "id": "h1", "text": "t", "prompt": "p", "origin": { "kind": "automation", "runNumber": 3 }, "detail": "d", "answers": "d1", "usersOwn": true });
    held.as_object_mut().unwrap().extend(attachments.as_object().unwrap().clone());
    let mut thread: Value = serde_json::from_str(
        r#"{
        "id": "t1", "title": "Thread", "titleByUser": true, "role": "coordinator", "projectId": "p1",
        "executionPolicy": "autonomous", "engine": "codex", "model": "gpt-6.1-sol", "effort": "xhigh", "fastMode": true,
        "sortIndex": 2, "rankedAt": 10, "snoozedUntil": 20, "createdAt": 1, "updatedAt": 9, "archivedAt": 30,
        "parentId": "t0",
        "remoteLead": { "threadId": "r1", "title": "Remote", "computer": "c1", "proposalId": "x1", "away": true, "backAt": 4 },
        "brief": { "intent": "do", "doneWhen": "done", "delivers": "pull-request" },
        "report": { "state": "blocked", "summary": "stuck", "at": 3 },
        "resolvedAt": 5,
        "decisions": [{ "id": "d1", "question": "Which?", "context": "ctx", "options": [{ "label": "A", "description": "a", "recommended": true }], "raisedAt": 2, "answer": "A", "answeredAt": 3 }],
        "coordinationNotes": [{ "id": "c1", "threadId": "t2", "text": "news", "at": 2, "urgent": true, "runId": "r", "quiet": true }],
        "threadModel": { "engine": "claude", "model": "opus", "effort": "high" },
        "memory": { "purpose": "why", "purposeCleared": 1, "notes": [{ "id": "n1", "text": "note", "at": 1 }], "revision": 2, "heard": 1 },
        "proposals": [{ "id": "pr1", "title": "Next", "named": true, "prompt": "go", "brief": { "intent": "i", "doneWhen": "d", "delivers": "report" },
                        "place": { "project": "p1", "worktree": true, "worktreeId": "w1", "computer": { "id": "c1", "name": "Mac" } },
                        "engine": "codex", "model": "gpt-6-luna", "effort": "low", "role": "reviewer", "from": "t3", "proposedAt": 4, "error": "e", "carried": true }],
        "muted": true,
        "remoteHeard": { "r1": 5 },
        "remoteWorking": { "r1": { "computer": "c1", "title": "Remote" } },
        "remoteStops": { "r1": { "computer": "c1", "runId": "run", "answer": { "at": 6, "runId": "run2" } } },
        "caughtStarts": { "pr1": { "computer": "c1", "threadId": "r2" } },
        "pullRequest": { "number": 12, "url": "https://x/12", "state": "open", "title": "PR", "checks": "failing", "review": "required", "conflicts": true },
        "findings": [{ "id": "f1", "headline": "Found", "detail": "body", "key": "k", "at": 7, "read": true }],
        "handledIssues": ["k0"], "lastFindingAt": 7, "lastChecked": { "at": 8, "note": "fine" },
        "contextUsage": { "tokens": 10, "limit": 100, "model": "gpt-6.1-sol" },
        "usage": { "gpt-6.1-sol": 40 },
        "continuation": { "provider": "codex", "value": "session" }, "continuationStatus": "available",
        "lastChangeSnapshot": { "files": ["a.rs"], "capturedAt": 9 },
        "outcome": "failed", "outcomeUnread": true,
        "limitPause": { "resetsAt": 100, "window": "weekly", "pausedAt": 50, "nudgedAt": 60, "workflow": true, "held": [] },
        "runEndedAt": 9, "runEndedAs": "stopped", "runOpen": true, "inheritedContinuation": true,
        "worktreeId": "w1", "worktreeEnteredAt": 3,
        "messages": [],
        "subagents": [{ "id": "s1", "description": "helper", "agentType": "explore", "sessionScoped": true, "status": "idle", "stopping": true,
                        "lastToolName": "Read", "summary": "ok", "totalTokens": 5, "startedAt": 1, "finishedAt": 2, "model": "m", "effort": "e", "prompt": "p",
                        "activity": [{ "id": "a1", "kind": "tool", "title": "Read", "text": "file", "at": 1 }] }]
    }"#,
    )
    .unwrap();
    thread["limitPause"]["held"] = json!([held]);
    thread["messages"] = json!([message]);
    thread
}

fn no_unknown_fields(value: &Value, path: &str, typed: &Value) {
    assert_eq!(value, typed, "{path} changed on its way through its type");
}

#[test]
fn a_thread_with_every_field_is_readable_and_names_each_field() {
    let raw = every_field();
    let mut lifted = Vec::new();
    let sanitized = sanitize_v2_thread(raw.clone(), &mut lifted).expect("every field is readable");
    assert_eq!(sanitized, raw, "nothing readable is dropped or renamed");
    let thread: StoredThread = serde_json::from_value(raw.clone()).unwrap();
    assert!(thread.task.extra.is_empty(), "unnamed fields: {:?}", thread.task.extra.keys().collect::<Vec<_>>());
    assert!(thread.messages[0].extra.is_empty());
    let pause = thread.task.limit_pause.as_ref().unwrap();
    assert!(pause.held.as_ref().unwrap()[0].extra.is_empty());
    let subagent = &thread.subagents.as_ref().unwrap()[0];
    assert!(subagent.record.extra.is_empty() && subagent.activity[0].extra.is_empty());
    let task = &thread.task;
    let proposal = &task.proposals.as_ref().unwrap()[0];
    let decision = &task.decisions.as_ref().unwrap()[0];
    let message = &thread.messages[0];
    let nested = [
        ("remoteLead", &task.remote_lead.as_ref().unwrap().extra),
        ("brief", &task.brief.as_ref().unwrap().extra),
        ("report", &task.report.as_ref().unwrap().extra),
        ("decision", &decision.extra),
        ("decision option", &decision.options[0].extra),
        ("coordination note", &task.coordination_notes.as_ref().unwrap()[0].extra),
        ("threadModel", &task.thread_model.as_ref().unwrap().extra),
        ("memory", &task.memory.as_ref().unwrap().extra),
        ("memory note", &task.memory.as_ref().unwrap().notes[0].extra),
        ("proposal", &proposal.extra),
        ("proposal place", &proposal.place.extra),
        ("remoteWorking", &task.remote_working.as_ref().unwrap()["r1"].extra),
        ("remoteStops", &task.remote_stops.as_ref().unwrap()["r1"].extra),
        ("remoteStops answer", &task.remote_stops.as_ref().unwrap()["r1"].answer.as_ref().unwrap().extra),
        ("caughtStarts", &task.caught_starts.as_ref().unwrap()["pr1"].extra),
        ("pullRequest", &task.pull_request.as_ref().unwrap().extra),
        ("finding", &task.findings.as_ref().unwrap()[0].extra),
        ("lastChecked", &task.last_checked.as_ref().unwrap().extra),
        ("contextUsage", &task.context_usage.as_ref().unwrap().extra),
        ("continuation", &task.continuation.as_ref().unwrap().extra),
        ("lastChangeSnapshot", &task.last_change_snapshot.extra),
        ("limitPause", &pause.extra),
        ("annotation", &message.annotations.as_ref().unwrap()[0].extra),
        ("paste", &message.pastes.as_ref().unwrap()[0].extra),
        ("file", &message.files.as_ref().unwrap()[0].extra),
    ];
    for (name, extra) in nested {
        assert!(extra.is_empty(), "{name} has unnamed fields: {:?}", extra.keys().collect::<Vec<_>>());
    }
    assert!(matches!(&message.origin, Some(MessageOrigin::Thread(origin)) if origin.extra.is_empty() && origin.computer.is_some()));
    assert!(matches!(message.annotations.as_ref().unwrap()[0].anchor.as_ref().and_then(Anchor::read), Some(AnnotationAnchor::Diff(anchor)) if anchor.extra.is_empty()));
    no_unknown_fields(&raw, "thread", &serde_json::to_value(&thread).unwrap());
}

#[test]
fn unknown_fields_survive_at_every_level() {
    let mut raw = every_field();
    raw["fromNewerBuild"] = json!({ "kept": [1, 2.5] });
    raw["messages"][0]["reaction"] = json!("👍");
    raw["messages"][0]["origin"]["avatar"] = json!("x");
    raw["proposals"][0]["place"]["region"] = json!("eu");
    raw["limitPause"]["held"][0]["priority"] = json!(1);
    let thread: StoredThread = serde_json::from_value(raw.clone()).unwrap();
    no_unknown_fields(&raw, "thread", &serde_json::to_value(&thread).unwrap());
}

fn minimal(extra: Value) -> Value {
    let mut thread = json!({ "id": "t", "title": "T", "executionPolicy": "confirm", "engine": "claude", "messages": [],
        "continuationStatus": "none", "lastChangeSnapshot": { "files": [], "capturedAt": 1 }, "updatedAt": 1 });
    thread.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    thread
}

#[test]
fn older_builds_fields_are_read_under_their_current_names() {
    let raw = minimal(json!({
        "model": "gpt-6-sol", "engine": "codex", "contextWindow": 200000, "crewNotes": [{ "id": "c", "threadId": "t2", "text": "x", "at": 1 }],
        "usage": { "gpt-5.6-sol": 3, "gpt-6.1-sol": 4 },
        "messages": [{ "id": "m", "kind": "user", "text": "x", "detail": "Automation run #7", "quiet": true, "at": 1 }],
    }));
    let task = sanitize_v2_thread(raw, &mut Vec::new()).unwrap();
    assert_eq!(task["model"], "gpt-6.1-sol");
    assert_eq!(task["usage"], json!({ "gpt-6.1-sol": 7 }));
    assert!(task.get("contextWindow").is_none() && task.get("crewNotes").is_none());
    assert_eq!(task["coordinationNotes"][0]["id"], "c");
    let message = &task["messages"][0];
    assert_eq!(message["withdrawn"], true);
    assert_eq!(message["origin"], json!({ "kind": "automation", "runNumber": 7 }));
}

#[test]
fn an_unreadable_optional_field_is_dropped_and_an_unreadable_thread_is_hidden() {
    let dropped = sanitize_v2_thread(minimal(json!({ "pullRequest": { "number": -1 }, "proposals": [{ "id": "" }] })), &mut Vec::new()).unwrap();
    assert!(dropped.get("pullRequest").is_none() && dropped.get("proposals").is_none());
    assert!(sanitize_v2_thread(minimal(json!({ "engine": "claude", "model": "gpt-6.1-sol" })), &mut Vec::new()).is_none());
    assert!(sanitize_v2_thread(minimal(json!({ "engine": "claude", "fastMode": true })), &mut Vec::new()).is_none());
    let bad_continuation = sanitize_v2_thread(minimal(json!({ "continuation": { "provider": "claude" }, "continuationStatus": "available" })), &mut Vec::new()).unwrap();
    assert_eq!(bad_continuation["continuationStatus"], "invalid");
}

#[test]
fn the_store_lifts_checkouts_settles_subagents_and_counts_what_it_hides() {
    let worktree = json!({ "id": "w1", "root": "/w", "workspaceId": "ws", "baseCommit": "abc", "createdAt": 1, "lastUsedAt": 2, "enteredAt": 5 });
    let tasks = json!([
        minimal(json!({ "id": "a", "projectId": "p", "worktree": worktree })),
        minimal(json!({ "id": "b", "projectId": "gone" })),
        minimal(json!({ "id": "c", "worktreeId": "missing", "worktreeEnteredAt": 1,
            "subagents": [{ "id": "s", "description": "", "status": "working", "startedAt": 1, "activity": [] }] })),
        json!({ "id": "d" }),
    ]);
    let validated = validate_thread_store_data(DecodedThreadStore {
        tasks,
        projects: json!([{ "id": "p", "root": "/p" }]),
        worktrees: json!([]),
        last_folder: json!("/p"),
    })
    .unwrap();
    assert_eq!(validated.hidden_tasks, 2);
    assert_eq!(validated.worktrees[0]["projectId"], "p");
    assert_eq!(validated.tasks[0]["worktreeId"], "w1");
    assert_eq!(validated.tasks[0]["worktreeEnteredAt"], 5);
    let settled = &validated.tasks[1];
    assert!(settled.get("worktreeId").is_none() && settled.get("worktreeEnteredAt").is_none());
    assert_eq!(settled["subagents"][0]["status"], "stopped");
    assert_eq!(settled["subagents"][0]["finishedAt"], 1);

    let unreadable_project = validate_thread_store_data(DecodedThreadStore {
        tasks: json!([]),
        projects: json!([{ "id": "p" }]),
        worktrees: json!([]),
        last_folder: Value::Null,
    });
    assert!(unreadable_project.is_err());
}

#[test]
fn a_stored_conversation_that_cannot_be_read_names_the_message() {
    let error = parse_stored_conversation_messages(vec![
        json!({ "id": "m", "kind": "user", "text": "", "at": 1 }),
        json!({ "id": "n", "kind": "robot", "text": "", "at": 1 }),
    ])
    .unwrap_err();
    assert_eq!(error, "Stored message 2 is unreadable.");
}

#[test]
fn a_remote_thread_keeps_what_this_build_cannot_name() {
    let remote = readable_remote_thread(minimal(json!({
        "threadModel": { "engine": "future", "model": "new-model" },
        "role": "captain", "rankedAt": 0, "createdAt": 4,
        "messages": [{ "id": "m", "kind": "assistant", "text": "x", "at": 6 }],
    })));
    assert_eq!(remote["threadModel"]["engine"], "future");
    assert!(remote.get("role").is_none());
    assert_eq!(remote["rankedAt"], 6);
}
