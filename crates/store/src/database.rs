//! The SQLite thread store, schema-compatible with the TypeScript build's `tasks.v3.sqlite`
//! (`src/main/task-database.mts`). Every call is synchronous; [`crate::service`] runs it on a
//! thread of its own.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use act_core::persisted::storage::{self, DecodedThreadStore};
use act_core::persisted::{
    shapes, Automation, ConversationMessage, HistorySummary, LoadedTaskStore, SubagentActivity, TaskStoreDelta,
};
use rusqlite::functions::FunctionFlags;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Map, Value};

use crate::error::{StoreError, StoreResult};

const SCHEMA: &str = "
      PRAGMA journal_mode = WAL;
      PRAGMA synchronous = NORMAL;
      PRAGMA foreign_keys = ON;
      CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, position INTEGER NOT NULL, data TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, data TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS worktrees (id TEXT PRIMARY KEY, data TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS messages (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        position INTEGER NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (task_id, id)
      );
      CREATE INDEX IF NOT EXISTS messages_task_position ON messages(task_id, position);
      CREATE TABLE IF NOT EXISTS subagents (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        position INTEGER NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (task_id, id)
      );
      CREATE INDEX IF NOT EXISTS subagents_task_position ON subagents(task_id, position);
      CREATE TABLE IF NOT EXISTS subagent_activity (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        subagent_id TEXT NOT NULL,
        id TEXT NOT NULL,
        position INTEGER NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (task_id, subagent_id, id)
      );
      CREATE INDEX IF NOT EXISTS subagent_activity_position ON subagent_activity(task_id, subagent_id, position);
      CREATE TABLE IF NOT EXISTS automations (id TEXT PRIMARY KEY, task_id TEXT NOT NULL UNIQUE, data TEXT NOT NULL);
    ";

const HISTORY_COLUMNS: &str = "
          ALTER TABLE messages ADD COLUMN message_at REAL;
          ALTER TABLE messages ADD COLUMN audible INTEGER NOT NULL DEFAULT 1;
          ALTER TABLE messages ADD COLUMN has_attachment INTEGER NOT NULL DEFAULT 0;
          ALTER TABLE tasks ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;
          ALTER TABLE tasks ADD COLUMN attachment_count INTEGER NOT NULL DEFAULT 0;
          UPDATE messages SET
            message_at = CASE WHEN json_valid(data) THEN json_extract(data, '$.at') END,
            audible = CASE WHEN json_valid(data) THEN CASE WHEN coalesce(json_extract(data, '$.withdrawn'), json_extract(data, '$.quiet'), 0) = 1 THEN 0 ELSE 1 END ELSE 1 END,
            has_attachment = CASE WHEN json_valid(data) THEN CASE WHEN json_array_length(data, '$.attachments') > 0 THEN 1 ELSE 0 END ELSE 0 END;
          UPDATE tasks SET
            message_count = (SELECT count(*) FROM messages WHERE task_id = tasks.id),
            attachment_count = (SELECT coalesce(sum(has_attachment), 0) FROM messages WHERE task_id = tasks.id);
        ";

const HISTORY_TRIGGERS: &str = "
        CREATE INDEX IF NOT EXISTS messages_task_audible_position ON messages(task_id, audible, position);
        CREATE TRIGGER IF NOT EXISTS messages_summary_insert AFTER INSERT ON messages BEGIN
          UPDATE tasks SET message_count = message_count + 1, attachment_count = attachment_count + NEW.has_attachment WHERE id = NEW.task_id;
        END;
        CREATE TRIGGER IF NOT EXISTS messages_summary_update AFTER UPDATE OF has_attachment ON messages BEGIN
          UPDATE tasks SET attachment_count = attachment_count + NEW.has_attachment - OLD.has_attachment WHERE id = NEW.task_id;
        END;
        CREATE TRIGGER IF NOT EXISTS messages_summary_delete AFTER DELETE ON messages BEGIN
          UPDATE tasks SET message_count = message_count - 1, attachment_count = attachment_count - OLD.has_attachment WHERE id = OLD.task_id;
        END;
      ";

const SAVE_TASK: &str = "INSERT INTO tasks (id, data) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data";
const SAVE_MESSAGE: &str = "INSERT INTO messages (task_id, id, position, data, message_at, audible, has_attachment) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(task_id, id) DO UPDATE SET position = excluded.position, data = excluded.data, message_at = excluded.message_at, audible = excluded.audible, has_attachment = excluded.has_attachment";
const SAVE_SUBAGENT: &str = "INSERT INTO subagents (task_id, id, position, data) VALUES (?, ?, ?, ?) ON CONFLICT(task_id, id) DO UPDATE SET position = excluded.position, data = excluded.data";
const SAVE_ACTIVITY: &str = "INSERT INTO subagent_activity (task_id, subagent_id, id, position, data) VALUES (?, ?, ?, ?, ?) ON CONFLICT(task_id, subagent_id, id) DO UPDATE SET position = excluded.position, data = excluded.data";
const SEARCH_MESSAGES: &str = "
    SELECT DISTINCT task_id FROM messages
    WHERE task_id IN (SELECT value FROM json_each(?))
      AND instr(js_lower(CASE WHEN json_valid(data) THEN json_extract(data, '$.text') END), ?) > 0
";
const HISTORY_SUMMARIES: &str = "
    SELECT id, message_count, attachment_count,
      (SELECT message_at FROM messages WHERE task_id = tasks.id ORDER BY position LIMIT 1) AS first_at,
      (SELECT message_at FROM messages WHERE task_id = tasks.id AND audible = 1 ORDER BY position DESC LIMIT 1) AS last_at
    FROM tasks WHERE message_count > 0
";

/// Marks the one cutover that dropped app records tied to the retired shared Codex home.
const PRIVATE_CODEX_HOME_MARKER: &str = "privateCodexHome.v2";

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    /// Every root the app's checkouts live under; no project folder may be recorded inside one.
    pub worktrees_roots: Vec<PathBuf>,
    /// Runs the private Codex home cutover, as every build that gives Codex a home of its own does.
    pub cut_over_codex_threads: bool,
}

impl OpenOptions {
    /// How the app opens a profile's store: refusing folders under any of its checkout roots, and
    /// with Codex on a home of its own.
    pub fn for_profile(profile: &crate::Profile) -> Self {
        OpenOptions { worktrees_roots: profile.worktrees_roots(), cut_over_codex_threads: true }
    }
}

pub struct TaskDatabase {
    connection: Connection,
    worktrees_roots: Vec<PathBuf>,
}

impl TaskDatabase {
    /// Opens or creates the store, bringing an older one up to the current shape.
    pub fn open(file: &Path, options: OpenOptions) -> StoreResult<Self> {
        if let Some(parent) = file.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(file)?;
        Self::with_connection(connection, options)
    }

    fn with_connection(connection: Connection, options: OpenOptions) -> StoreResult<Self> {
        connection.create_scalar_function("js_lower", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            Ok(match context.get_raw(0) {
                rusqlite::types::ValueRef::Text(text) => Some(String::from_utf8_lossy(text).to_lowercase()),
                _ => None,
            })
        })?;
        connection.execute_batch(SCHEMA)?;
        let mut database = TaskDatabase { connection, worktrees_roots: options.worktrees_roots };
        database.prepare_history_metadata()?;
        database.lift_embedded_subagents()?;
        database.lift_embedded_worktrees()?;
        if options.cut_over_codex_threads {
            database.cut_over_codex_threads()?;
        }
        Ok(database)
    }

    /// Counts and indexed timestamps let the thread list load without reading conversation bodies.
    fn prepare_history_metadata(&mut self) -> StoreResult<()> {
        let has_columns = {
            let mut statement = self.connection.prepare("PRAGMA table_info(messages)")?;
            let names = statement.query_map([], |row| row.get::<_, String>("name"))?.collect::<Result<Vec<_>, _>>()?;
            names.iter().any(|name| name == "message_at")
        };
        let transaction = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !has_columns {
            transaction.execute_batch(HISTORY_COLUMNS)?;
        }
        transaction.execute_batch(HISTORY_TRIGGERS)?;
        transaction.commit()?;
        Ok(())
    }

    fn task_rows(&self) -> StoreResult<Vec<(String, Value)>> {
        let mut statement = self.connection.prepare("SELECT id, data FROM tasks")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut tasks = Vec::new();
        for row in rows {
            let (id, data) = row?;
            tasks.push((id, serde_json::from_str(&data)?));
        }
        Ok(tasks)
    }

    /// Tasks written before subagents had rows of their own still carry them inside the record.
    fn lift_embedded_subagents(&mut self) -> StoreResult<()> {
        let stale: Vec<(String, Value)> =
            self.task_rows()?.into_iter().filter(|(_, task)| js_truthy(task.get("subagents"))).collect();
        if stale.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (id, mut task) in stale {
            let subagents = task.as_object_mut().and_then(|record| record.remove("subagents"));
            if let Some(Value::Array(subagents)) = subagents {
                for (index, subagent) in subagents.into_iter().enumerate() {
                    write_subagent(&transaction, &id, index, subagent)?;
                }
            }
            transaction.execute("UPDATE tasks SET data = ? WHERE id = ?", params![serde_json::to_string(&task)?, id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Tasks written while a checkout belonged to exactly one thread carry it inside themselves.
    /// Giving each checkout a row of its own is what lets a second thread claim it.
    fn lift_embedded_worktrees(&mut self) -> StoreResult<()> {
        let stale: Vec<(String, Value)> =
            self.task_rows()?.into_iter().filter(|(_, task)| js_truthy(task.get("worktree"))).collect();
        if stale.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (id, mut task) in stale {
            let record = task.as_object_mut().expect("a task with a worktree is a record");
            let worktree = record.remove("worktree").unwrap_or(Value::Null);
            let mut lifted = match worktree {
                Value::Object(worktree) => worktree,
                _ => Map::new(),
            };
            let entered_at = lifted.remove("enteredAt");
            match record.get("projectId") {
                Some(project) => lifted.insert("projectId".into(), project.clone()),
                None => lifted.remove("projectId"),
            };
            let lifted = Value::Object(lifted);
            if !shapes::is_worktree(&lifted) {
                transaction.execute("UPDATE tasks SET data = ? WHERE id = ?", params![serde_json::to_string(&task)?, id])?;
                continue;
            }
            let worktree_id = lifted["id"].as_str().unwrap_or_default().to_owned();
            transaction.execute(
                "INSERT INTO worktrees (id, data) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
                params![worktree_id, serde_json::to_string(&lifted)?],
            )?;
            record.insert("worktreeId".into(), Value::from(worktree_id));
            if let Some(entered_at) = entered_at.filter(Value::is_number) {
                record.insert("worktreeEnteredAt".into(), entered_at);
            }
            transaction.execute("UPDATE tasks SET data = ? WHERE id = ?", params![serde_json::to_string(&task)?, id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Automations are read while the app boots, so one unreadable row is skipped, never fatal.
    pub fn list_automations(&self) -> StoreResult<Vec<Automation>> {
        let mut statement = self.connection.prepare_cached("SELECT data FROM automations ORDER BY task_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut automations = Vec::new();
        for data in rows {
            let parsed = serde_json::from_str::<Value>(&data?).ok().filter(shapes::is_automation);
            if let Some(automation) = parsed.and_then(|value| serde_json::from_value(value).ok()) {
                automations.push(automation);
            }
        }
        Ok(automations)
    }

    pub fn save_automation(&self, automation: &Automation) -> StoreResult<()> {
        self.connection
            .prepare_cached("INSERT INTO automations (id, task_id, data) VALUES (?, ?, ?) ON CONFLICT(id) DO UPDATE SET task_id = excluded.task_id, data = excluded.data")?
            .execute(params![automation.id, automation.task_id, serde_json::to_string(automation)?])?;
        Ok(())
    }

    pub fn delete_automation(&self, id: &str) -> StoreResult<()> {
        self.connection.prepare_cached("DELETE FROM automations WHERE id = ?")?.execute([id])?;
        Ok(())
    }

    /// One clean cutover: discard app records tied to the old Codex home, never Codex's own sessions.
    pub fn cut_over_codex_threads(&mut self) -> StoreResult<()> {
        let done: Option<i64> = self
            .connection
            .query_row("SELECT 1 FROM settings WHERE key = ?", [PRIVATE_CODEX_HOME_MARKER], |row| row.get(0))
            .optional()?;
        if done.is_some() {
            return Ok(());
        }
        let transaction = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = "SELECT id FROM tasks WHERE json_valid(data) AND json_extract(data, '$.engine') = 'codex'";
        for table in ["automations", "messages", "subagent_activity", "subagents"] {
            transaction.execute_batch(&format!("DELETE FROM {table} WHERE task_id IN ({old})"))?;
        }
        transaction.execute_batch(&format!("DELETE FROM tasks WHERE id IN ({old})"))?;
        // A thread now on Claude can still carry an older Codex continuation.
        transaction.execute_batch("UPDATE tasks SET data = json_set(json_remove(data, '$.continuation'), '$.continuationStatus', 'none') WHERE json_valid(data) AND json_extract(data, '$.continuation.provider') = 'codex'")?;
        transaction.execute("INSERT INTO settings (key, value) VALUES (?, '1')", [PRIVATE_CODEX_HOME_MARKER])?;
        transaction.commit()?;
        Ok(())
    }

    /// Closes the connection, checkpointing the write-ahead log.
    pub fn close(self) -> StoreResult<()> {
        self.connection.close().map_err(|(_, error)| error.into())
    }

    /// The stored workspace, or none when nothing was ever stored. `summaries_only` leaves every
    /// conversation on disk and gives each thread that has one a history summary instead.
    pub fn load(&self, summaries_only: bool) -> StoreResult<Option<LoadedTaskStore>> {
        let task_records: Vec<Value> = self.task_rows()?.into_iter().map(|(_, task)| task).collect();
        let projects = self.json_rows("SELECT data FROM projects ORDER BY position", [])?;
        let last_folder_row: Option<String> = self
            .connection
            .query_row("SELECT value FROM settings WHERE key = 'lastFolder'", [], |row| row.get(0))
            .optional()?;
        if task_records.is_empty() && projects.is_empty() && last_folder_row.is_none() {
            return Ok(None);
        }

        let mut messages: HashMap<String, Vec<Value>> = HashMap::new();
        if !summaries_only {
            let mut statement = self.connection.prepare("SELECT task_id, data FROM messages ORDER BY task_id, position")?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            for row in rows {
                let (task_id, data) = row?;
                messages.entry(task_id).or_default().push(serde_json::from_str(&data)?);
            }
        }
        let mut subagents: HashMap<String, Vec<Value>> = HashMap::new();
        {
            let mut statement = self.connection.prepare("SELECT task_id, id, data FROM subagents ORDER BY task_id, position")?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(2)?)))?;
            for row in rows {
                let (task_id, data) = row?;
                let mut subagent = spread(serde_json::from_str(&data)?);
                subagent.insert("activity".into(), Value::Array(Vec::new()));
                subagents.entry(task_id).or_default().push(Value::Object(subagent));
            }
        }
        let mut tasks: Vec<Value> = task_records
            .into_iter()
            .map(|task| {
                let mut task = spread(task);
                let id = task.get("id").and_then(Value::as_str).map(str::to_owned);
                let thread_messages = id.as_ref().and_then(|id| messages.remove(id)).unwrap_or_default();
                task.insert("messages".into(), Value::Array(thread_messages));
                if let Some(held) = id.as_ref().and_then(|id| subagents.remove(id)) {
                    task.insert("subagents".into(), Value::Array(held));
                }
                Value::Object(task)
            })
            .collect();
        let updated = |task: &Value| task.get("updatedAt").and_then(Value::as_f64).unwrap_or(f64::NAN);
        tasks.sort_by(|left, right| updated(right).partial_cmp(&updated(left)).unwrap_or(std::cmp::Ordering::Equal));
        let worktrees = self.json_rows("SELECT data FROM worktrees", [])?;
        let last_folder = match last_folder_row {
            Some(value) => serde_json::from_str(&value)?,
            None => Value::Null,
        };
        let decoded = DecodedThreadStore {
            tasks: Value::Array(tasks),
            projects: Value::Array(projects),
            worktrees: Value::Array(worktrees),
            last_folder,
        };
        let validated = storage::validate_thread_store_data(decoded).map_err(|errors| StoreError::Invalid(errors.join(" ")))?;
        let (mut data, hidden_tasks) = storage::typed_store(validated).map_err(StoreError::Invalid)?;
        let mut unloaded_task_ids = None;
        if summaries_only {
            let summaries = self.history_summaries()?;
            let mut unloaded = Vec::new();
            for task in &mut data.tasks {
                if let Some(summary) = summaries.get(&task.task.id) {
                    unloaded.push(task.task.id.clone());
                    task.history_summary = Some(summary.clone());
                }
            }
            unloaded_task_ids = Some(unloaded);
        }
        Ok(Some(LoadedTaskStore { data, hidden_tasks, unloaded_task_ids, resume_after_update: None }))
    }

    fn json_rows<P: rusqlite::Params>(&self, sql: &str, params: P) -> StoreResult<Vec<Value>> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
        let mut values = Vec::new();
        for data in rows {
            values.push(serde_json::from_str(&data?)?);
        }
        Ok(values)
    }

    fn history_summaries(&self) -> StoreResult<HashMap<String, HistorySummary>> {
        let mut statement = self.connection.prepare(HISTORY_SUMMARIES)?;
        let rows = statement.query_map([], |row| {
            let number = |index: usize| -> rusqlite::Result<Option<f64>> {
                Ok(match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Integer(value) => Some(value as f64),
                    rusqlite::types::ValueRef::Real(value) => Some(value),
                    _ => None,
                })
            };
            Ok((
                row.get::<_, String>(0)?,
                HistorySummary {
                    message_count: row.get::<_, i64>(1)?.max(0) as u64,
                    attachment_count: row.get::<_, i64>(2)?.max(0) as u64,
                    first_message_at: number(3)?,
                    last_audible_at: number(4)?,
                },
            ))
        })?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// Every image a stored message still names, as the absolute paths the messages carry.
    pub fn attachment_paths(&self) -> StoreResult<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT value FROM messages, json_each(json_extract(messages.data, '$.attachments')) WHERE messages.has_attachment = 1 AND type = 'text'",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// A thread's whole conversation, oldest first.
    pub fn load_thread_messages(&self, task_id: &str) -> StoreResult<Vec<ConversationMessage>> {
        let exists: Option<i64> =
            self.connection.query_row("SELECT 1 FROM tasks WHERE id = ?", [task_id], |row| row.get(0)).optional()?;
        if exists.is_none() {
            return Err(StoreError::ThreadGone);
        }
        let messages = self.json_rows("SELECT data FROM messages WHERE task_id = ? ORDER BY position", [task_id])?;
        storage::parse_stored_conversation_messages(messages).map_err(StoreError::Invalid)
    }

    /// Which of these threads have a stored message whose text contains `search`, which the caller
    /// has already lowercased.
    pub fn search_thread_messages(&self, search: &str, task_ids: &[String]) -> StoreResult<Vec<String>> {
        let mut statement = self.connection.prepare_cached(SEARCH_MESSAGES)?;
        let rows = statement.query_map(params![serde_json::to_string(task_ids)?, search], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// A subagent's activity, read only when someone opens it. An item this build cannot read is skipped.
    pub fn subagent_activity(&self, task_id: &str, subagent_id: &str) -> StoreResult<Vec<SubagentActivity>> {
        let items = self.json_rows(
            "SELECT data FROM subagent_activity WHERE task_id = ? AND subagent_id = ? ORDER BY position",
            [task_id, subagent_id],
        )?;
        Ok(items.into_iter().filter_map(|item| serde_json::from_value(item).ok()).collect())
    }

    /// Writes a delta in one transaction. A project or last folder inside the app's own checkouts is
    /// refused and the rest still lands: losing transcripts is worse than keeping a folder on disk.
    pub fn persist(&mut self, delta: &TaskStoreDelta) -> StoreResult<()> {
        let writes_projects = delta
            .projects
            .as_ref()
            .is_none_or(|projects| project_roots_are_own(projects.iter().map(|project| project.root.as_str()), &self.worktrees_roots));
        let writes_last_folder = match &delta.last_folder {
            None => true,
            Some(folder) => project_roots_are_own(folder.as_deref().filter(|folder| !folder.is_empty()), &self.worktrees_roots),
        };
        if !writes_projects || !writes_last_folder {
            eprintln!(
                "Refused to record a folder inside the app's worktrees directory. projects: {:?}, lastFolder: {:?}",
                delta.projects.as_ref().map(|projects| projects.iter().map(|project| project.root.as_str()).collect::<Vec<_>>()),
                delta.last_folder,
            );
        }
        let transaction = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(projects) = delta.projects.as_ref().filter(|_| writes_projects) {
            transaction.execute("DELETE FROM projects", [])?;
            let mut insert = transaction.prepare_cached("INSERT INTO projects (id, position, data) VALUES (?, ?, ?)")?;
            for (index, project) in projects.iter().enumerate() {
                insert.execute(params![project.id, index as i64, serde_json::to_string(project)?])?;
            }
        }
        if let Some(worktrees) = &delta.worktrees {
            transaction.execute("DELETE FROM worktrees", [])?;
            let mut insert = transaction.prepare_cached("INSERT INTO worktrees (id, data) VALUES (?, ?)")?;
            for worktree in worktrees {
                insert.execute(params![worktree.id, serde_json::to_string(worktree)?])?;
            }
        }
        if let Some(folder) = delta.last_folder.as_ref().filter(|_| writes_last_folder) {
            transaction.execute(
                "INSERT INTO settings (key, value) VALUES ('lastFolder', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [serde_json::to_string(folder)?],
            )?;
        }
        for id in &delta.removed_tasks {
            transaction.execute("DELETE FROM messages WHERE task_id = ?", [id])?;
            transaction.execute("DELETE FROM subagent_activity WHERE task_id = ?", [id])?;
            transaction.execute("DELETE FROM subagents WHERE task_id = ?", [id])?;
            transaction.execute("DELETE FROM tasks WHERE id = ?", [id])?;
        }
        {
            let mut save_task = transaction.prepare_cached(SAVE_TASK)?;
            let mut save_message = transaction.prepare_cached(SAVE_MESSAGE)?;
            let mut save_subagent = transaction.prepare_cached(SAVE_SUBAGENT)?;
            let mut save_activity = transaction.prepare_cached(SAVE_ACTIVITY)?;
            for change in &delta.tasks {
                let task_id = &change.task.id;
                save_task.execute(params![task_id, serde_json::to_string(&change.task)?])?;
                for indexed in &change.messages {
                    let message = &indexed.message;
                    save_message.execute(params![
                        task_id,
                        message.id,
                        indexed.index as i64,
                        serde_json::to_string(message)?,
                        message.at,
                        message.audible() as i64,
                        message.has_attachments() as i64,
                    ])?;
                }
                for indexed in &change.subagents {
                    save_subagent.execute(params![
                        task_id,
                        indexed.subagent.id,
                        indexed.index as i64,
                        serde_json::to_string(&indexed.subagent)?
                    ])?;
                }
                for indexed in &change.activity {
                    save_activity.execute(params![
                        task_id,
                        indexed.subagent_id,
                        indexed.item.id,
                        indexed.index as i64,
                        serde_json::to_string(&indexed.item)?
                    ])?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

/// `{ ...value }`: a record's own fields, or none for anything else.
fn spread(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(record) => record,
        _ => Map::new(),
    }
}

fn js_truthy(value: Option<&Value>) -> bool {
    act_core::persisted::js::truthy(value)
}

fn write_subagent(transaction: &rusqlite::Transaction<'_>, task_id: &str, index: usize, subagent: Value) -> StoreResult<()> {
    let mut record = spread(subagent);
    let activity = record.remove("activity");
    let subagent_id = record.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
    transaction
        .prepare_cached(SAVE_SUBAGENT)?
        .execute(params![task_id, subagent_id, index as i64, serde_json::to_string(&record)?])?;
    if let Some(Value::Array(items)) = activity {
        let mut save = transaction.prepare_cached(SAVE_ACTIVITY)?;
        for (position, item) in items.into_iter().enumerate() {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            save.execute(params![task_id, subagent_id, item_id, position as i64, serde_json::to_string(&item)?])?;
        }
    }
    Ok(())
}

/// A project folder can never be a checkout the app made for a thread, under any root it has used.
pub fn project_roots_are_own<'a>(roots: impl IntoIterator<Item = &'a str>, worktrees_roots: &[PathBuf]) -> bool {
    let owned: Vec<String> = worktrees_roots
        .iter()
        .map(|root| format!("{}{}", resolve(root).display(), std::path::MAIN_SEPARATOR))
        .collect();
    let roots: HashSet<String> = roots.into_iter().map(|root| resolve(Path::new(root)).display().to_string()).collect();
    !roots.iter().any(|root| owned.iter().any(|owned| root.starts_with(owned.as_str())))
}

/// `path.resolve`: absolute against the working directory, with `.` and `..` folded and no
/// trailing separator.
fn resolve(path: &Path) -> PathBuf {
    let base = if path.is_absolute() { PathBuf::new() } else { std::env::current_dir().unwrap_or_default() };
    let mut resolved = PathBuf::new();
    for component in base.components().chain(path.components()) {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => resolved.push(part),
        }
    }
    resolved
}

#[cfg(test)]
pub(crate) fn open_in_memory(options: OpenOptions) -> StoreResult<TaskDatabase> {
    TaskDatabase::with_connection(Connection::open_in_memory()?, options)
}

#[cfg(test)]
mod tests {
    use act_core::persisted::{IndexedActivity, IndexedMessage, IndexedSubagent, TaskChange};
    use serde_json::json;

    use super::*;

    fn typed<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    fn task(id: &str, project: Option<&str>) -> act_core::persisted::PersistedTask {
        let mut task = json!({ "id": id, "title": id, "executionPolicy": "confirm", "engine": "claude", "rankedAt": 1,
            "continuationStatus": "none", "lastChangeSnapshot": { "files": [], "capturedAt": 1 }, "updatedAt": 1 });
        if let Some(project) = project {
            task["projectId"] = json!(project);
        }
        typed(task)
    }

    fn message(id: &str, text: &str, at: f64, extra: Value) -> IndexedMessage {
        let mut message = json!({ "id": id, "kind": "assistant", "text": text, "at": at });
        message.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        IndexedMessage { index: 0, message: typed(message) }
    }

    fn change(id: &str, messages: Vec<IndexedMessage>) -> TaskChange {
        let messages = messages.into_iter().enumerate().map(|(index, indexed)| IndexedMessage { index, ..indexed }).collect();
        TaskChange { task: task(id, Some("p")), messages, subagents: Vec::new(), activity: Vec::new() }
    }

    fn workspace(tasks: Vec<TaskChange>) -> TaskStoreDelta {
        TaskStoreDelta {
            tasks,
            projects: Some(vec![typed(json!({ "id": "p", "root": "/repo" }))]),
            worktrees: Some(Vec::new()),
            last_folder: Some(Some("/repo".to_owned())),
            ..TaskStoreDelta::default()
        }
    }

    #[test]
    fn summaries_count_what_the_conversation_holds_without_reading_it() {
        let mut database = open_in_memory(OpenOptions::default()).unwrap();
        let mut thread = change("t", vec![
            message("m1", "first", 10.0, json!({})),
            message("m2", "shot", 20.0, json!({ "attachments": ["/a.png"] })),
            message("m3", "quiet tick", 30.0, json!({ "withdrawn": true })),
        ]);
        thread.subagents.push(IndexedSubagent { index: 0, subagent: typed(json!({ "id": "s", "description": "d", "status": "completed", "startedAt": 1 })) });
        thread.activity.push(IndexedActivity { subagent_id: "s".into(), index: 0, item: typed(json!({ "id": "a", "kind": "text", "text": "x", "at": 1 })) });
        database.persist(&workspace(vec![thread])).unwrap();

        let summaries = database.load(true).unwrap().unwrap();
        let loaded = &summaries.data.tasks[0];
        assert!(loaded.messages.is_empty());
        let summary = loaded.history_summary.as_ref().unwrap();
        assert_eq!((summary.message_count, summary.attachment_count), (3, 1));
        assert_eq!((summary.first_message_at, summary.last_audible_at), (Some(10.0), Some(20.0)));
        assert_eq!(summaries.unloaded_task_ids, Some(vec!["t".to_owned()]));
        assert!(loaded.subagents.as_ref().unwrap()[0].activity.is_empty());
        assert_eq!(database.subagent_activity("t", "s").unwrap().len(), 1);
        assert_eq!(database.attachment_paths().unwrap(), vec!["/a.png".to_owned()]);

        let mut replaced = change("t", vec![message("m2", "shot", 20.0, json!({}))]);
        replaced.messages[0].index = 1;
        database.persist(&TaskStoreDelta { tasks: vec![replaced], ..TaskStoreDelta::default() }).unwrap();
        let summary = database.load(true).unwrap().unwrap().data.tasks[0].history_summary.clone().unwrap();
        assert_eq!((summary.message_count, summary.attachment_count), (3, 0));

        database.persist(&TaskStoreDelta { removed_tasks: vec!["t".into()], ..TaskStoreDelta::default() }).unwrap();
        let emptied = database.load(false).unwrap().unwrap();
        assert!(emptied.data.tasks.is_empty());
        assert!(database.subagent_activity("t", "s").unwrap().is_empty());
        assert_eq!(database.load_thread_messages("t"), Err(StoreError::ThreadGone));
    }

    #[test]
    fn search_reads_message_text_case_insensitively_in_the_named_threads_only() {
        let mut database = open_in_memory(OpenOptions::default()).unwrap();
        database
            .persist(&workspace(vec![
                change("a", vec![message("m", "Fixed the ÜBER Bug", 1.0, json!({}))]),
                change("b", vec![message("m", "über bug again", 1.0, json!({}))]),
            ]))
            .unwrap();
        assert_eq!(database.search_thread_messages("über bug", &["a".to_owned()]).unwrap(), vec!["a".to_owned()]);
        let mut both = database.search_thread_messages("über bug", &["a".to_owned(), "b".to_owned()]).unwrap();
        both.sort();
        assert_eq!(both, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn a_folder_inside_the_apps_checkouts_is_refused_while_the_threads_still_land() {
        let mut database = open_in_memory(OpenOptions { worktrees_roots: vec![PathBuf::from("/home/u/.wt")], ..OpenOptions::default() }).unwrap();
        database.persist(&workspace(Vec::new())).unwrap();
        let mut delta = workspace(vec![change("t", Vec::new())]);
        delta.projects = Some(vec![typed(json!({ "id": "p", "root": "/home/u/.wt/../.wt/repo-1" }))]);
        delta.last_folder = Some(Some("/home/u/.wt/repo-1".to_owned()));
        database.persist(&delta).unwrap();
        let loaded = database.load(false).unwrap().unwrap();
        assert_eq!(loaded.data.projects[0].root, "/repo");
        assert_eq!(loaded.data.last_folder.as_deref(), Some("/repo"));
        assert_eq!(loaded.data.tasks.len(), 1);
        assert!(project_roots_are_own(["/home/u/.wtx/repo"], &[PathBuf::from("/home/u/.wt")]));
    }

    #[test]
    fn an_older_store_gains_history_counts_and_its_embedded_records_get_rows() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("tasks.v3.sqlite");
        {
            let old = Connection::open(&file).unwrap();
            old.execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, data TEXT NOT NULL);
                 CREATE TABLE messages (task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE, id TEXT NOT NULL, position INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (task_id, id));",
            )
            .unwrap();
            let embedded = json!({ "id": "t", "title": "t", "projectId": "p", "executionPolicy": "confirm", "engine": "claude",
                "continuationStatus": "none", "lastChangeSnapshot": { "files": [], "capturedAt": 1 }, "updatedAt": 1,
                "subagents": [{ "id": "s", "description": "d", "status": "working", "startedAt": 1, "activity": [{ "id": "a", "kind": "text", "text": "x", "at": 1 }] }],
                "worktree": { "id": "w", "root": "/w", "workspaceId": "ws", "baseCommit": "abc", "createdAt": 1, "lastUsedAt": 1, "enteredAt": 7 } });
            old.execute("INSERT INTO tasks VALUES ('t', ?)", [embedded.to_string()]).unwrap();
            for (position, data) in [json!({ "id": "m1", "kind": "user", "text": "a", "at": 3, "attachments": ["/x.png"] }), json!({ "id": "m2", "kind": "assistant", "text": "b", "at": 4, "quiet": true })].iter().enumerate() {
                old.execute("INSERT INTO messages VALUES ('t', ?, ?, ?)", params![data["id"].as_str().unwrap(), position as i64, data.to_string()]).unwrap();
            }
        }
        let mut database = TaskDatabase::open(&file, OpenOptions::default()).unwrap();
        database.persist(&TaskStoreDelta { projects: Some(vec![typed(json!({ "id": "p", "root": "/p" }))]), ..TaskStoreDelta::default() }).unwrap();
        let loaded = database.load(true).unwrap().unwrap();
        let thread = &loaded.data.tasks[0];
        let summary = thread.history_summary.as_ref().unwrap();
        assert_eq!((summary.message_count, summary.attachment_count, summary.last_audible_at), (2, 1, Some(3.0)));
        assert_eq!(thread.task.worktree_id.as_deref(), Some("w"));
        assert_eq!(thread.task.worktree_entered_at, Some(7.0));
        assert_eq!(loaded.data.worktrees[0].project_id, "p");
        let subagent = &thread.subagents.as_ref().unwrap()[0];
        assert_eq!(subagent.record.status.as_str(), "stopped");
        assert_eq!(database.subagent_activity("t", "s").unwrap()[0].id, "a");
        assert!(!thread.task.extra.contains_key("subagents") && !thread.task.extra.contains_key("worktree"));
    }

    #[test]
    fn the_codex_cutover_drops_codex_threads_once() {
        let mut database = open_in_memory(OpenOptions::default()).unwrap();
        let mut codex = change("codex", vec![message("m", "x", 1.0, json!({}))]);
        codex.task.engine = act_core::persisted::AgentEngine::Codex;
        let mut claude = change("claude", Vec::new());
        claude.task.continuation = Some(typed(json!({ "provider": "codex", "value": "old" })));
        claude.task.continuation_status = act_core::persisted::ContinuationStatus::Available;
        database.persist(&workspace(vec![codex, claude])).unwrap();
        database.cut_over_codex_threads().unwrap();
        let loaded = database.load(false).unwrap().unwrap();
        assert_eq!(loaded.data.tasks.len(), 1);
        assert!(loaded.data.tasks[0].task.continuation.is_none());
        assert_eq!(loaded.data.tasks[0].task.continuation_status.as_str(), "none");

        let mut again = change("codex", Vec::new());
        again.task.engine = act_core::persisted::AgentEngine::Codex;
        database.persist(&TaskStoreDelta { tasks: vec![again], ..TaskStoreDelta::default() }).unwrap();
        database.cut_over_codex_threads().unwrap();
        assert_eq!(database.load(false).unwrap().unwrap().data.tasks.len(), 2);
    }

    #[test]
    fn automations_round_trip_and_an_unreadable_row_is_skipped() {
        let database = open_in_memory(OpenOptions::default()).unwrap();
        let automation: Automation = typed(json!({ "id": "a", "taskId": "t", "prompt": "p", "schedule": "* * * * *", "paused": false,
            "createdAt": 1, "updatedAt": 2, "runCount": 0, "surfaceWhen": "found" }));
        database.save_automation(&automation).unwrap();
        database.connection.execute("INSERT INTO automations VALUES ('b', 'u', '{\"id\":\"b\"}')", []).unwrap();
        assert_eq!(database.list_automations().unwrap(), vec![automation]);
        database.delete_automation("a").unwrap();
        assert!(database.list_automations().unwrap().is_empty());
    }
}
