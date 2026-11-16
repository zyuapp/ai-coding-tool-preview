//! Round-trips a copy of a real `tasks.v3.sqlite`. Run with
//! `ACT_STORE_FIXTURE=/path/to/copy.sqlite cargo test -p act_store -- --ignored`.
//! The fixture is copied before it is opened, so the file named is never written.

use std::collections::BTreeMap;
use std::path::PathBuf;

use act_core::persisted::{
    Automation, ConversationMessage, IndexedActivity, IndexedMessage, IndexedSubagent, PersistedSubagent, PersistedTask,
    Project, SubagentActivity, TaskChange, TaskStoreDelta, Worktree,
};
use act_store::{OpenOptions, TaskDatabase, TaskDatabaseService};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let source = PathBuf::from(std::env::var("ACT_STORE_FIXTURE").expect("ACT_STORE_FIXTURE names a copy of tasks.v3.sqlite"));
    let directory = tempfile::tempdir().expect("a scratch directory");
    let copy = directory.path().join("tasks.v3.sqlite");
    let reader = Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader.execute("VACUUM INTO ?", [copy.to_str().unwrap()]).unwrap();
    (directory, copy)
}

/// Typed and back again, the record reads exactly as stored.
fn round_trips<T: Serialize + DeserializeOwned>(table: &str, data: &str) {
    let stored: Value = serde_json::from_str(data).unwrap();
    let typed: T = serde_json::from_value(stored.clone()).unwrap_or_else(|error| panic!("{table} row does not type: {error}\n{data}"));
    let written: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(written, stored, "{table} row changed on its way through its type");
}

fn rows(connection: &Connection, sql: &str) -> Vec<String> {
    let mut statement = connection.prepare(sql).unwrap();
    statement.query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

#[test]
#[ignore = "needs ACT_STORE_FIXTURE"]
fn every_row_round_trips_through_its_type() {
    let (_directory, file) = fixture();
    drop(TaskDatabase::open(&file, OpenOptions::default()).unwrap());
    let connection = Connection::open(&file).unwrap();
    let counts: BTreeMap<&str, usize> = [
        ("tasks", rows(&connection, "SELECT data FROM tasks").into_iter().map(|data| round_trips::<PersistedTask>("tasks", &data)).count()),
        ("messages", rows(&connection, "SELECT data FROM messages").into_iter().map(|data| round_trips::<ConversationMessage>("messages", &data)).count()),
        ("subagents", rows(&connection, "SELECT data FROM subagents").into_iter().map(|data| round_trips::<PersistedSubagent>("subagents", &data)).count()),
        ("subagent_activity", rows(&connection, "SELECT data FROM subagent_activity").into_iter().map(|data| round_trips::<SubagentActivity>("subagent_activity", &data)).count()),
        ("worktrees", rows(&connection, "SELECT data FROM worktrees").into_iter().map(|data| round_trips::<Worktree>("worktrees", &data)).count()),
        ("projects", rows(&connection, "SELECT data FROM projects").into_iter().map(|data| round_trips::<Project>("projects", &data)).count()),
        ("automations", rows(&connection, "SELECT data FROM automations").into_iter().map(|data| round_trips::<Automation>("automations", &data)).count()),
    ]
    .into_iter()
    .collect();
    eprintln!("round-tripped {counts:?}");
}

#[test]
#[ignore = "needs ACT_STORE_FIXTURE"]
fn a_loaded_store_written_back_loads_the_same() {
    let (_directory, file) = fixture();
    let service = TaskDatabaseService::open(file.clone(), OpenOptions { cut_over_codex_threads: true, ..OpenOptions::default() }).unwrap();
    let full = service.load().wait().unwrap().expect("the fixture holds a workspace");
    let summaries = service.load_summaries().wait().unwrap().unwrap();
    assert_eq!(full.data.tasks.len(), summaries.data.tasks.len());
    eprintln!("{} threads, {} hidden, {} projects, {} worktrees", full.data.tasks.len(), full.hidden_tasks, full.data.projects.len(), full.data.worktrees.len());

    for (loaded, summary) in full.data.tasks.iter().zip(&summaries.data.tasks) {
        let messages = service.load_thread_messages(loaded.task.id.clone()).wait().unwrap();
        assert_eq!(messages, loaded.messages);
        match &summary.history_summary {
            Some(history) => assert_eq!(history.message_count as usize, messages.len()),
            None => assert!(messages.is_empty()),
        }
    }

    let copy = file.with_file_name("rewritten.sqlite");
    let rewritten = TaskDatabaseService::open(copy, OpenOptions::default()).unwrap();
    let mut delta = TaskStoreDelta {
        projects: Some(full.data.projects.clone()),
        worktrees: Some(full.data.worktrees.clone()),
        last_folder: Some(full.data.last_folder.clone()),
        ..TaskStoreDelta::default()
    };
    for thread in &full.data.tasks {
        let mut subagents = Vec::new();
        let mut activity = Vec::new();
        for (index, subagent) in thread.subagents.iter().flatten().enumerate() {
            subagents.push(IndexedSubagent { index, subagent: subagent.record.clone() });
            let items = service.subagent_activity(thread.task.id.clone(), subagent.record.id.clone()).wait().unwrap();
            activity.extend(items.into_iter().enumerate().map(|(index, item)| IndexedActivity { subagent_id: subagent.record.id.clone(), index, item }));
        }
        delta.tasks.push(TaskChange {
            task: thread.task.clone(),
            messages: thread.messages.iter().cloned().enumerate().map(|(index, message)| IndexedMessage { index, message }).collect(),
            subagents,
            activity,
        });
    }
    rewritten.persist(delta).wait().unwrap();
    for automation in service.list_automations().wait().unwrap() {
        rewritten.save_automation(automation).wait().unwrap();
    }
    let reloaded = rewritten.load().wait().unwrap().unwrap();
    assert_eq!(reloaded.data, full.data);
    assert_eq!(rewritten.load_summaries().wait().unwrap().unwrap().data.tasks.iter().map(|task| &task.history_summary).collect::<Vec<_>>(),
        summaries.data.tasks.iter().map(|task| &task.history_summary).collect::<Vec<_>>());
    assert_eq!(rewritten.attachment_paths().wait().unwrap().len(), service.attachment_paths().wait().unwrap().len());
    rewritten.close().wait().unwrap();
    service.close().wait().unwrap();
}
