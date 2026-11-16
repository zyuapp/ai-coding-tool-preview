//! The task database: opened and read on a thread of its own, written with the deltas the
//! workspace computes, and asked for one thread's conversation when it comes on screen.

use std::sync::Arc;

use act_core::domain::conversation::ConversationMessage;
use act_core::events::WorkspaceEvent;
use act_core::persisted::{LoadedTaskStore, TaskStoreDelta};
use act_store::database::OpenOptions;
use act_store::service::TaskDatabaseService;

use super::{convert, Host, Inbound};

pub type LoadedStore = Result<Option<LoadedTaskStore>, String>;

/// What the database answered.
pub enum StoreReply {
    Loaded(LoadedStore),
    Messages { task_id: String, result: Result<Vec<ConversationMessage>, String> },
    Persisted(Result<(), String>),
}

impl Host {
    /// Opens the database and reads every thread's summary, off the window's thread.
    pub fn load_store(self: &Arc<Self>) {
        let host = self.clone();
        std::thread::Builder::new()
            .name("act-store-open".into())
            .spawn(move || {
                let reply = match TaskDatabaseService::open(host.profile.tasks_database(), OpenOptions::for_profile(&host.profile)) {
                    Ok(database) => {
                        let loaded = database.load_summaries().wait().map_err(|error| error.to_string());
                        let _ = host.database.set(database);
                        if loaded.is_ok() {
                            host.automations.start(host.clone());
                            host.sweep_attachments();
                        }
                        loaded
                    }
                    Err(error) => Err(error.to_string()),
                };
                host.sink.send(Inbound::Store(StoreReply::Loaded(reply)));
            })
            .expect("the store thread starts");
    }

    /// Housekeeping the window never waits for: attachments no stored message names go once old.
    fn sweep_attachments(self: &Arc<Self>) {
        let host = self.clone();
        self.spawn(async move {
            let Some(database) = host.database.get() else { return };
            let paths = match database.attachment_paths().await {
                Ok(paths) => paths,
                Err(error) => return eprintln!("Could not sweep unused attachments: {error}"),
            };
            let referenced = act_host::user_data::attachment_names(paths.iter().map(String::as_str));
            let directory = host.profile.attachments();
            let swept = tokio::task::spawn_blocking(move || act_host::user_data::sweep_orphan_attachments(&directory, &referenced, std::time::SystemTime::now(), act_host::user_data::ORPHAN_ATTACHMENT_MIN_AGE)).await;
            match swept {
                Ok(Ok(swept)) if swept.files > 0 => println!("Removed {} unused attachment file(s), {} MB.", swept.files, (swept.bytes as f64 / 1024.0 / 1024.0).round()),
                Ok(Err(error)) => eprintln!("Could not sweep unused attachments: {error}"),
                _ => {}
            }
        });
    }

    pub fn load_thread_messages(self: &Arc<Self>, task_id: String) {
        let host = self.clone();
        self.spawn(async move {
            let result = match host.database.get() {
                Some(database) => database.load_thread_messages(task_id.clone()).await.map_err(|error| error.to_string()).and_then(|messages| messages.iter().map(convert).collect()),
                None => Err("The task database is not open.".into()),
            };
            host.sink.send(Inbound::Store(StoreReply::Messages { task_id, result }));
        });
    }

    pub fn persist(self: &Arc<Self>, delta: TaskStoreDelta) {
        let host = self.clone();
        self.spawn(async move {
            let result = match host.database.get() {
                Some(database) => database.persist(delta).await.map_err(|error| error.to_string()),
                None => Err("The task database is not open.".into()),
            };
            host.sink.send(Inbound::Store(StoreReply::Persisted(result)));
        });
    }
}

pub async fn subagent_activity(host: &Arc<Host>, task_id: String, subagent_id: String) {
    let Some(database) = host.database.get() else { return };
    match database.subagent_activity(task_id.clone(), subagent_id.clone()).await {
        Ok(activity) if !activity.is_empty() => match activity.iter().map(convert).collect::<Result<Vec<_>, _>>() {
            Ok(activity) => host.sink.event(WorkspaceEvent::SubagentActivityLoaded { task_id, subagent_id, activity }),
            Err(error) => host.sink.failed(error),
        },
        Ok(_) => {}
        Err(error) => host.sink.failed(error.to_string()),
    }
}
