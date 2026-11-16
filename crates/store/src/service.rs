//! The thread store on one ordered worker thread, so SQLite and JSON decoding never block the UI
//! (`src/main/task-database-service.mts`). Requests run in the order they are sent: a flush or
//! close answers once every write sent before it has landed.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;

use act_core::persisted::{Automation, ConversationMessage, LoadedTaskStore, SubagentActivity, TaskStoreDelta};
use futures_channel::oneshot;

use crate::database::{OpenOptions, TaskDatabase};
use crate::error::{StoreError, StoreResult};

/// A request's answer. Await it, or [`Pending::wait`] for it off the UI thread.
#[must_use = "a request's answer says whether it landed"]
pub struct Pending<T> {
    receiver: Result<oneshot::Receiver<StoreResult<T>>, StoreError>,
}

impl<T> Pending<T> {
    fn failed(error: StoreError) -> Self {
        Pending { receiver: Err(error) }
    }

    /// Blocks the calling thread until the answer arrives.
    pub fn wait(self) -> StoreResult<T> {
        block_on(self)
    }
}

impl<T> Future for Pending<T> {
    type Output = StoreResult<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match &mut this.receiver {
            Err(error) => Poll::Ready(Err(error.clone())),
            Ok(receiver) => Pin::new(receiver).poll(context).map(|answer| answer.unwrap_or(Err(StoreError::WorkerExited))),
        }
    }
}

struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

type Reply<T> = oneshot::Sender<StoreResult<T>>;

enum Request {
    Load { summaries_only: bool, reply: Reply<Option<LoadedTaskStore>> },
    Messages { task_id: String, reply: Reply<Vec<ConversationMessage>> },
    Search { search: String, task_ids: Vec<String>, reply: Reply<Vec<String>> },
    Attachments { reply: Reply<Vec<String>> },
    Activity { task_id: String, subagent_id: String, reply: Reply<Vec<SubagentActivity>> },
    Persist { delta: Box<TaskStoreDelta>, reply: Reply<()> },
    Automations { reply: Reply<Vec<Automation>> },
    SaveAutomation { automation: Box<Automation>, reply: Reply<()> },
    DeleteAutomation { id: String, reply: Reply<()> },
    Flush { reply: Reply<()> },
    Close { reply: Reply<()> },
}

struct Shared {
    sender: Mutex<Option<mpsc::Sender<Request>>>,
    failure: Mutex<Option<StoreError>>,
    closing: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// A handle on the store's worker. Clones share one worker and one order of requests.
#[derive(Clone)]
pub struct TaskDatabaseService {
    shared: Arc<Shared>,
}

impl TaskDatabaseService {
    /// Starts the worker and waits for the store to open, which may migrate an older one. Call it
    /// off the UI thread.
    pub fn open(file: PathBuf, options: OpenOptions) -> StoreResult<Self> {
        let (sender, receiver) = mpsc::channel::<Request>();
        let (ready_sender, ready_receiver) = mpsc::channel::<StoreResult<()>>();
        let shared = Arc::new(Shared {
            sender: Mutex::new(Some(sender)),
            failure: Mutex::new(None),
            closing: AtomicBool::new(false),
            worker: Mutex::new(None),
        });
        let worker_shared = Arc::downgrade(&shared);
        let worker = std::thread::Builder::new()
            .name("task-database".into())
            .spawn(move || {
                let database = match TaskDatabase::open(&file, options) {
                    Ok(database) => {
                        let _ = ready_sender.send(Ok(()));
                        database
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                run(database, receiver, worker_shared);
            })?;
        *shared.worker.lock().expect("worker lock") = Some(worker);
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(TaskDatabaseService { shared }),
            Ok(Err(error)) => {
                shared.sender.lock().expect("sender lock").take();
                Err(error)
            }
            Err(_) => Err(StoreError::WorkerExited),
        }
    }

    pub fn load(&self) -> Pending<Option<LoadedTaskStore>> {
        self.request(|reply| Request::Load { summaries_only: false, reply })
    }

    /// The workspace with every conversation left on disk behind a history summary.
    pub fn load_summaries(&self) -> Pending<Option<LoadedTaskStore>> {
        self.request(|reply| Request::Load { summaries_only: true, reply })
    }

    pub fn load_thread_messages(&self, task_id: impl Into<String>) -> Pending<Vec<ConversationMessage>> {
        let task_id = task_id.into();
        self.request(|reply| Request::Messages { task_id, reply })
    }

    /// `search` must already be lowercased.
    pub fn search_thread_messages(&self, search: impl Into<String>, task_ids: Vec<String>) -> Pending<Vec<String>> {
        let search = search.into();
        self.request(|reply| Request::Search { search, task_ids, reply })
    }

    pub fn attachment_paths(&self) -> Pending<Vec<String>> {
        self.request(|reply| Request::Attachments { reply })
    }

    pub fn subagent_activity(&self, task_id: impl Into<String>, subagent_id: impl Into<String>) -> Pending<Vec<SubagentActivity>> {
        let (task_id, subagent_id) = (task_id.into(), subagent_id.into());
        self.request(|reply| Request::Activity { task_id, subagent_id, reply })
    }

    pub fn persist(&self, delta: TaskStoreDelta) -> Pending<()> {
        self.request(|reply| Request::Persist { delta: Box::new(delta), reply })
    }

    pub fn list_automations(&self) -> Pending<Vec<Automation>> {
        self.request(|reply| Request::Automations { reply })
    }

    pub fn save_automation(&self, automation: Automation) -> Pending<()> {
        self.request(|reply| Request::SaveAutomation { automation: Box::new(automation), reply })
    }

    pub fn delete_automation(&self, id: impl Into<String>) -> Pending<()> {
        let id = id.into();
        self.request(|reply| Request::DeleteAutomation { id, reply })
    }

    /// Answers once every request sent before it has run.
    pub fn flush(&self) -> Pending<()> {
        self.request(|reply| Request::Flush { reply })
    }

    /// Closes the store once every request sent before it has run. Later requests are refused.
    pub fn close(&self) -> Pending<()> {
        if self.shared.closing.swap(true, Ordering::SeqCst) {
            return Pending::failed(StoreError::Closing);
        }
        let (reply, receiver) = oneshot::channel();
        let sent = self
            .shared
            .sender
            .lock()
            .expect("sender lock")
            .take()
            .map(|sender| sender.send(Request::Close { reply }).is_ok())
            .unwrap_or(false);
        if !sent {
            return Pending::failed(self.failure().unwrap_or(StoreError::WorkerExited));
        }
        Pending { receiver: Ok(receiver) }
    }

    fn failure(&self) -> Option<StoreError> {
        self.shared.failure.lock().expect("failure lock").clone()
    }

    fn request<T>(&self, build: impl FnOnce(Reply<T>) -> Request) -> Pending<T> {
        if let Some(failure) = self.failure() {
            return Pending::failed(failure);
        }
        if self.shared.closing.load(Ordering::SeqCst) {
            return Pending::failed(StoreError::Closing);
        }
        let (reply, receiver) = oneshot::channel();
        let sender = self.shared.sender.lock().expect("sender lock");
        match sender.as_ref().map(|sender| sender.send(build(reply))) {
            Some(Ok(())) => Pending { receiver: Ok(receiver) },
            _ => Pending::failed(self.failure().unwrap_or(StoreError::WorkerExited)),
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.sender.get_mut().expect("sender lock").take();
        if let Some(worker) = self.worker.get_mut().expect("worker lock").take() {
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

fn run(database: TaskDatabase, receiver: mpsc::Receiver<Request>, shared: std::sync::Weak<Shared>) {
    let mut database = Some(database);
    while let Ok(request) = receiver.recv() {
        let Some(open) = database.as_mut() else { break };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(open, request)));
        match outcome {
            Ok(Some(close)) => {
                let result = database.take().expect("open until closed").close();
                let _ = close.send(result);
                break;
            }
            Ok(None) => {}
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "Task database worker failed.".to_owned());
                if let Some(shared) = shared.upgrade() {
                    *shared.failure.lock().expect("failure lock") = Some(StoreError::Sqlite(message));
                }
                break;
            }
        }
    }
}

/// Runs one request, answering it. A close is handed back for the caller to finish.
fn execute(database: &mut TaskDatabase, request: Request) -> Option<Reply<()>> {
    match request {
        Request::Load { summaries_only, reply } => {
            let _ = reply.send(database.load(summaries_only));
        }
        Request::Messages { task_id, reply } => {
            let _ = reply.send(database.load_thread_messages(&task_id));
        }
        Request::Search { search, task_ids, reply } => {
            let _ = reply.send(database.search_thread_messages(&search, &task_ids));
        }
        Request::Attachments { reply } => {
            let _ = reply.send(database.attachment_paths());
        }
        Request::Activity { task_id, subagent_id, reply } => {
            let _ = reply.send(database.subagent_activity(&task_id, &subagent_id));
        }
        Request::Persist { delta, reply } => {
            let _ = reply.send(database.persist(&delta));
        }
        Request::Automations { reply } => {
            let _ = reply.send(database.list_automations());
        }
        Request::SaveAutomation { automation, reply } => {
            let _ = reply.send(database.save_automation(&automation));
        }
        Request::DeleteAutomation { id, reply } => {
            let _ = reply.send(database.delete_automation(&id));
        }
        Request::Flush { reply } => {
            let _ = reply.send(Ok(()));
        }
        Request::Close { reply } => return Some(reply),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_answer_in_order_and_a_closed_store_refuses_more() {
        let directory = tempfile::tempdir().unwrap();
        let service = TaskDatabaseService::open(directory.path().join("tasks.v3.sqlite"), OpenOptions::default()).unwrap();
        let delta = TaskStoreDelta { last_folder: Some(Some("/repo".into())), ..TaskStoreDelta::default() };
        let written = service.persist(delta);
        let loaded = service.clone().load();
        assert_eq!(loaded.wait().unwrap().unwrap().data.last_folder.as_deref(), Some("/repo"));
        written.wait().unwrap();
        service.close().wait().unwrap();
        assert_eq!(service.flush().wait(), Err(StoreError::Closing));
        assert_eq!(service.close().wait(), Err(StoreError::Closing));
    }

    #[test]
    fn a_store_that_cannot_open_says_why() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("tasks.v3.sqlite"), "not a database, just text long enough to have a header").unwrap();
        assert!(TaskDatabaseService::open(directory.path().join("tasks.v3.sqlite"), OpenOptions::default()).is_err());
    }
}
