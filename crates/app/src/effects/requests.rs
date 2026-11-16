//! The window as the app's tools reach it from the host runtime: reads of its state, inputs whose
//! answers are awaited, and the tool calls held open on a state still to come
//! (src/host/thread-requests.ts, src/host/thread-reads.ts).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, Weak};

use act_core::{WorkspaceInput, WorkspaceState};
use act_host::thread_requests::{Execution, RequestHost, TerminalText, ThreadWaiters};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::Value;

use super::tracking::Tracker;
use super::{Host, Inbound, Sink};

/// What the window does on the host's behalf, on its own thread.
pub enum WindowWork {
    /// Runs against the state as it stands once every earlier input is in.
    Read(Box<dyn FnOnce(&WorkspaceState) + Send>),
    /// Loads a thread's transcript, answering once it is in (at once when it already is).
    Hydrate(String, oneshot::Sender<()>),
    /// Something about the app's own update the user asked for, shown as a dialog.
    UpdateNotice(act_host::updates::UpdateNotice),
    /// Quits and starts again: into an installed update, or to take up new permissions.
    Relaunch { update: bool },
    /// Brings the window back to the user.
    Reveal,
    /// The updater started or stopped looking, which the menu bar shows.
    UpdateChecking(bool),
    /// Asks where to save an image the window shows, then writes it there.
    SaveImage(act_host::image_download::ImageDownload),
}

/// A borrowed read, run on the window's thread while the caller waits for it.
struct BorrowedRead(*mut (dyn FnMut(&WorkspaceState) + 'static));

// SAFETY: the pointer is used only while the thread that lent it blocks until the read has run or
// been dropped unrun.
unsafe impl Send for BorrowedRead {}

pub struct WindowRequests {
    sink: Sink,
    host: Weak<Host>,
    waiters: ThreadWaiters,
    hydrations: Mutex<HashMap<String, Vec<oneshot::Sender<()>>>>,
}

impl WindowRequests {
    pub fn new(sink: Sink, host: Weak<Host>) -> Self {
        WindowRequests { sink, host, waiters: ThreadWaiters::default(), hydrations: Mutex::new(HashMap::new()) }
    }

    /// Holds `done` until the thread's transcript is in.
    pub fn await_history(&self, task_id: String, done: oneshot::Sender<()>) {
        self.hydrations.lock().unwrap().entry(task_id).or_default().push(done);
    }

    /// A transcript arrived, or failed to: whoever waited on it goes on.
    pub fn history_loaded(&self, task_id: &str) {
        for done in self.hydrations.lock().unwrap().remove(task_id).unwrap_or_default() {
            let _ = done.send(());
        }
    }

    pub fn release_waiters(&self, state: &WorkspaceState) {
        self.waiters.release(state);
    }
}

impl RequestHost for WindowRequests {
    fn read(&self, read: &mut dyn FnMut(&WorkspaceState)) {
        // SAFETY: only the lifetime is erased; `lent` is dropped or has run before this returns.
        let lent = BorrowedRead(unsafe { std::mem::transmute::<*mut (dyn FnMut(&WorkspaceState) + '_), *mut (dyn FnMut(&WorkspaceState) + 'static)>(read as *mut _) });
        let (ran, finished) = std::sync::mpsc::channel::<()>();
        self.sink.send(Inbound::Window(WindowWork::Read(Box::new(move |state| {
            let lent = lent;
            // SAFETY: the lender is blocked on `finished` until this send.
            unsafe { (*lent.0)(state) };
            let _ = ran.send(());
        }))));
        if finished.recv().is_err() {
            read(&WorkspaceState::default());
        }
    }

    fn dispatch(&self, input: WorkspaceInput) -> BoxFuture<'static, ()> {
        self.execute(input).completed.map(|_| ()).boxed()
    }

    fn execute(&self, input: WorkspaceInput) -> Execution {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (tracker, completed) = Tracker::new();
        self.sink.send(Inbound::Execute { input, accepted: accepted_tx, tracker });
        let accepted = accepted_rx.map(|result| result.unwrap_or_else(|_| act_core::CommandResult::failed("The workspace runtime is closed."))).boxed();
        Execution { accepted, completed }
    }

    fn waiters(&self) -> &ThreadWaiters {
        &self.waiters
    }

    fn hydrate(&self, thread_id: String) -> BoxFuture<'static, ()> {
        let (done, loaded) = oneshot::channel();
        self.sink.send(Inbound::Window(WindowWork::Hydrate(thread_id, done)));
        loaded.map(|_| ()).boxed()
    }

    fn search_stored(&self, search: String, ids: Vec<String>) -> BoxFuture<'static, HashSet<String>> {
        let host = self.host.clone();
        async move {
            let Some(database) = host.upgrade().and_then(|host| host.database.get().cloned()) else { return HashSet::new() };
            database.search_thread_messages(search, ids).await.map(|found| found.into_iter().collect()).unwrap_or_default()
        }
        .boxed()
    }

    fn read_terminal(&self, terminal_id: String, lines: usize, matching: Option<String>) -> BoxFuture<'static, Option<TerminalText>> {
        let text = self.host.upgrade().and_then(|host| host.terminals.read(&terminal_id, &act_terminal::contract::TerminalReadOptions { lines, matching }));
        futures::future::ready(text.map(|text| TerminalText { lines: text.lines, omitted: text.omitted as u64, matched: text.matched.map(|count| count as u64) })).boxed()
    }

    fn describe_simulator(&self, udid: String, name: String) -> BoxFuture<'static, Result<act_host::tools::types::SimulatorSnapshot, String>> {
        let Some(simulator) = crate::workspace::devices::SIMULATOR.get().cloned() else { return futures::future::ready(Err("The simulator is unavailable.".to_string())).boxed() };
        async move { simulator.describe(&udid, &name).await }.boxed()
    }

    fn capture_simulator(&self, udid: String) -> BoxFuture<'static, Option<act_host::tools::types::SimulatorShot>> {
        let Some(simulator) = crate::workspace::devices::SIMULATOR.get().cloned() else { return futures::future::ready(None).boxed() };
        async move { simulator.capture(&udid).await.ok() }.boxed()
    }

    fn query_computer_threads(&self, computer_id: String, query: Value) -> BoxFuture<'static, Result<Value, String>> {
        let host = self.host.clone();
        async move {
            let Some(host) = host.upgrade() else { return Err("That computer is unavailable.".to_string()) };
            host.computers.query(&computer_id, query).await
        }
        .boxed()
    }
}
