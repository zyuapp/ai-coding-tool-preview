//! Other computers and phones: the paired computers this one drives, the workspace it hands to the
//! ones that drive it, and the phone bridge (src/host/computer-effects.ts, mobile-bridge.ts,
//! src/main/computer-bridge.ts, src/host/runtime-publisher.ts).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use act_core::effects::EffectWork;
use act_core::events::WorkspaceEvent;
use act_core::view::mobile::{apply_mobile_patch, diff_mobile_view, project_mobile_view, MobileView};
use act_core::{AppCommand, WorkspaceInput, WorkspaceState};
use act_host::attachments::AttachmentStore;
use act_host::computers::links::{ComputerLinks, ComputerLinksOptions};
use act_host::computers::queries::{ComputerReads, QueryHost};
use act_host::mobile::host::{MobileHost, MobileHostOptions, TailscaleHooks};
use act_host::mobile::relay::MobileResponse;
use act_host::mobile::server::{WorkspaceHooks, WorkspaceUpdate};
use act_host::thread_requests::{query_local_threads, with_state, RequestHost};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

use super::{phones, Host, Sink};

type Listener = Arc<dyn Fn(WorkspaceUpdate) + Send + Sync>;

/// The workspace as another computer is handed it: whole once, then as numbered patches.
pub struct Publisher {
    requests: Arc<dyn RequestHost>,
    computers: Weak<Inner>,
    held: Mutex<Option<super::published::Published>>,
    revision: AtomicU64,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next: AtomicU64,
    me: Weak<Publisher>,
}

impl Publisher {
    pub fn watched(&self) -> bool {
        !self.listeners.lock().unwrap().is_empty()
    }

    /// Takes `state` as the published state, hands every watcher what changed since the state they
    /// last had, and answers the revision now.
    fn sync(&self, state: &WorkspaceState) -> u64 {
        let mut held = self.held.lock().unwrap();
        let patches = match held.as_mut() {
            Some(published) => published.update(state),
            None => {
                *held = Some(super::published::Published::new(state));
                Vec::new()
            }
        };
        drop(held);
        if patches.is_empty() {
            return self.revision.load(Ordering::SeqCst);
        }
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let listeners: Vec<Listener> = self.listeners.lock().unwrap().iter().map(|(_, listener)| listener.clone()).collect();
        for listener in listeners {
            listener(WorkspaceUpdate::Patches { revision, patches: patches.clone() });
        }
        revision
    }

    /// Publishes the state as it stands now; answers the revision it is at.
    fn flush(&self) -> u64 {
        with_state(self.requests.as_ref(), |state| self.sync(state))
    }

    /// Publishes the window's state from the window's own thread.
    pub fn publish_state(&self, state: &WorkspaceState) {
        if self.watched() {
            self.sync(state);
        }
    }
}

impl WorkspaceHooks for Publisher {
    fn name(&self) -> Option<String> {
        let inner = self.computers.upgrade()?;
        if let Some(links) = inner.links.get() {
            return Some(links.name());
        }
        let file = inner.links_file.get()?;
        Some(act_host::computers::links::stored_computer_name(file, act_platform::ipc::hostname().trim_end_matches(".local")))
    }

    fn snapshot(&self) -> WorkspaceUpdate {
        let (revision, state) = with_state(self.requests.as_ref(), |state| {
            let revision = self.sync(state);
            (revision, self.held.lock().unwrap().as_ref().map(super::published::Published::value).unwrap_or(Value::Null))
        });
        WorkspaceUpdate::State { revision, state }
    }

    fn subscribe(&self, listener: Listener) -> Box<dyn FnOnce() + Send> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.listeners.lock().unwrap().push((id, listener));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(publisher) = me.upgrade() {
                publisher.listeners.lock().unwrap().retain(|(held, _)| *held != id);
            }
        })
    }

    /// Runs the inputs one after another, each to completion, and answers with the revision the
    /// state was left at.
    fn input(&self, inputs: Vec<Value>) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.me.upgrade();
        // The first input is handed over now, so requests reach the workspace in the order they were made.
        let mut inputs = inputs.into_iter();
        let first = me.as_ref().zip(inputs.next()).map(|(me, input)| WorkspaceInput::from_json(&input).map(|parsed| me.requests.execute(parsed).completed));
        async move {
            let Some(me) = me else { return Err("This computer is shutting down.".into()) };
            let mut result = json!({ "ok": true });
            let mut pending = first;
            loop {
                let completed = match pending.take() {
                    Some(Ok(completed)) => completed,
                    Some(Err(error)) => return Ok(json!({ "ok": false, "message": format!("Invalid input: {error}"), "revision": me.revision.load(Ordering::SeqCst) })),
                    None => match inputs.next() {
                        Some(input) => match WorkspaceInput::from_json(&input) {
                            Ok(parsed) => me.requests.execute(parsed).completed,
                            Err(error) => return Ok(json!({ "ok": false, "message": format!("Invalid input: {error}"), "revision": me.revision.load(Ordering::SeqCst) })),
                        },
                        None => break,
                    },
                };
                let answered = completed.await;
                let mut answered = serde_json::to_value(&answered).unwrap_or(Value::Null);
                if answered.get("ok") != Some(&Value::Bool(true)) {
                    answered["revision"] = json!(me.flush());
                    return Ok(answered);
                }
                if let (Some(result), Some(fields)) = (result.as_object_mut(), answered.as_object()) {
                    for (key, value) in fields {
                        result.insert(key.clone(), value.clone());
                    }
                }
            }
            result["revision"] = json!(me.flush());
            Ok(result)
        }
        .boxed()
    }

    fn query(&self, query: Value) -> BoxFuture<'static, Result<Value, String>> {
        let reads = self.computers.upgrade().and_then(|inner| inner.reads.get().cloned());
        async move {
            match reads {
                Some(reads) => reads.answer(query).await,
                None => Err("This computer is still starting.".into()),
            }
        }
        .boxed()
    }
}

/// Answers the reads only this app can: its own threads, its terminals, its engines' commands.
struct Queries {
    host: Weak<Host>,
}

impl QueryHost for Queries {
    fn threads(&self, query: Value) -> BoxFuture<'static, Result<Value, String>> {
        let host = self.host.clone();
        async move {
            let Some(host) = host.upgrade() else { return Err("This computer is shutting down.".into()) };
            query_local_threads(host.requests.as_ref(), query).await
        }
        .boxed()
    }

    fn terminal_output(&self, terminal_id: String, after: Option<u64>) -> BoxFuture<'static, Result<Value, String>> {
        let read = self.host.upgrade().map(|host| host.terminals.read_output(&terminal_id, after));
        async move {
            match read {
                Some(Ok(Some(output))) => serde_json::to_value(output).map_err(|error| error.to_string()),
                Some(Err(error)) => Err(error.to_string()),
                _ => Ok(Value::Null),
            }
        }
        .boxed()
    }

    fn commands(&self, root: String, projectless: bool, engine: String) -> BoxFuture<'static, Result<Value, String>> {
        async move {
            let Some(engine) = act_core::domain::engine::AgentEngine::parse(&engine).and_then(act_engines::engines::engine) else { return Ok(json!({ "status": "error", "message": "Unknown engine." })) };
            match engine.commands(&root, projectless).await {
                Ok(commands) => Ok(json!({ "status": "available", "commands": commands })),
                Err(error) => Ok(json!({ "status": "error", "message": error.to_string() })),
            }
        }
        .boxed()
    }

    fn state(&self, read: &mut dyn FnMut(&WorkspaceState)) {
        match self.host.upgrade() {
            Some(host) => host.requests.read(read),
            None => read(&WorkspaceState::default()),
        }
    }

    fn ask_computer(&self, computer_id: String, query: Value) -> BoxFuture<'static, Result<Value, String>> {
        let host = self.host.clone();
        async move {
            let Some(host) = host.upgrade() else { return Err("That computer is unavailable.".into()) };
            host.computers.query(&computer_id, query).await
        }
        .boxed()
    }
}

/// How the phone bridge is started: by the window, or by `aic serve`.
#[derive(Clone, Default)]
pub struct BridgeOptions {
    pub port: Option<u16>,
    /// Loopback only: Tailscale is left alone.
    pub local: bool,
    /// Lines a headless server prints about the bridge.
    pub say: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

struct Inner {
    publisher: Arc<Publisher>,
    links: OnceLock<Arc<ComputerLinks>>,
    /// Where this computer's own name is kept, which a host with no links still announces.
    links_file: OnceLock<PathBuf>,
    reads: OnceLock<Arc<ComputerReads>>,
    mobile: OnceLock<Arc<MobileHost>>,
    /// The view every phone holds, so a change goes out as its difference.
    phones: Mutex<Option<MobileView>>,
    /// The socket `aic pair` asks for a code, while a server holds it open.
    pairing_socket: Mutex<Option<tokio_util::sync::CancellationToken>>,
}

/// This computer's paired computers, the workspace it hands out, and its phone bridge.
#[derive(Clone)]
pub struct Computers(Arc<Inner>);

impl Computers {
    pub fn new(requests: Arc<dyn RequestHost>) -> Computers {
        Computers(Arc::new_cyclic(|inner| Inner {
            publisher: Arc::new_cyclic(|me| Publisher { requests, computers: inner.clone(), held: Mutex::new(None), revision: AtomicU64::new(0), listeners: Mutex::new(Vec::new()), next: AtomicU64::new(0), me: me.clone() }),
            links: OnceLock::new(),
            links_file: OnceLock::new(),
            reads: OnceLock::new(),
            mobile: OnceLock::new(),
            phones: Mutex::new(None),
            pairing_socket: Mutex::new(None),
        }))
    }

    /// Opens the lines to paired computers (`pair` false for a host no computer is paired with),
    /// then starts the phone bridge.
    pub fn start(&self, host: &Arc<Host>, pair: bool, bridge: BridgeOptions) {
        let _guard = act_host::runtime().enter();
        let reads = Arc::new(ComputerReads::new(host.services.clone(), Arc::new(Queries { host: Arc::downgrade(host) })));
        let _ = self.0.reads.set(reads);
        let _ = self.0.links_file.set(host.profile.computers());
        if pair {
            let file: PathBuf = host.profile.computers();
            let (changed, state, notices) = (host.sink.clone(), host.sink.clone(), host.sink.clone());
            let named = Arc::downgrade(&self.0);
            let links = ComputerLinks::new(ComputerLinksOptions {
                file,
                device_name: act_platform::ipc::hostname().trim_end_matches(".local").to_string(),
                on_changed: Arc::new(move |links| {
                    let name = named.upgrade().and_then(|inner| inner.links.get().map(|links| links.name())).unwrap_or_default();
                    changed.json(json!({ "type": "computers.changed", "name": name, "links": links }));
                }),
                on_state: Arc::new(move |id, workspace| state.event(WorkspaceEvent::ComputerState { id, state: Box::new(Arc::unwrap_or_clone(workspace)) })),
                on_notice: Arc::new(move |id, notice| notices.json(json!({ "type": "computer.notice", "id": id, "notice": notice }))),
                connect: None,
                peers: None,
            });
            let _ = self.0.links.set(links.clone());
            links.start();
        }
        let computers = self.clone();
        let host = host.clone();
        act_host::runtime().spawn(async move { computers.start_bridge(&host, bridge).await });
        let woken = Arc::downgrade(&self.0);
        act_host::runtime().spawn(async move {
            // A clock that jumped far past the tick says the machine slept in between.
            let tick = std::time::Duration::from_secs(10);
            let mut last = std::time::SystemTime::now();
            loop {
                tokio::time::sleep(tick).await;
                let Some(inner) = woken.upgrade() else { return };
                let now = std::time::SystemTime::now();
                if now.duration_since(last).is_ok_and(|gap| gap > tick * 3) {
                    Computers(inner).resumed();
                }
                last = now;
            }
        });
    }

    async fn start_bridge(&self, host: &Arc<Host>, bridge: BridgeOptions) {
        let answering = host.clone();
        let changed = host.sink.clone();
        let say = bridge.say.clone();
        let announced: Arc<Mutex<(Option<String>, Option<String>)>> = Arc::default();
        let paired_from = Arc::downgrade(&self.0);
        let tailscale: Option<Arc<dyn TailscaleHooks>> = bridge.local.then(|| Arc::new(act_host::serve_args::NoTailscale) as Arc<dyn TailscaleHooks>);
        let mobile = MobileHost::start(MobileHostOptions {
            user_data: host.profile.user_data.clone(),
            send: Arc::new(move |request| {
                let host = answering.clone();
                act_host::runtime().spawn(async move {
                    let request_id = request.request_id.clone();
                    let result = phones::answer(&host, request).await;
                    if let Some(mobile) = host.computers.0.mobile.get() {
                        mobile.answer(MobileResponse { request_id, result });
                    }
                });
                true
            }),
            on_state: Arc::new(move |remote| {
                if let Some(say) = &say {
                    let mut announced = announced.lock().unwrap();
                    let served = remote.tailscale.magic_dns_name.clone().filter(|_| remote.tailscale.serving);
                    if let Some(name) = served.filter(|name| announced.0.as_ref() != Some(name)) {
                        announced.0 = Some(name.clone());
                        say(format!("Serving as https://{name}"));
                        if let Some(mobile) = paired_from.upgrade().and_then(|inner| inner.mobile.get().cloned()) {
                            if let Ok(offer) = mobile.create_pairing_code() {
                                say(crate::serve::pairing_text(&offer));
                            }
                        }
                    }
                    if let Some(error) = &remote.error {
                        say(format!("Bridge: {error}"));
                    }
                    if remote.tailscale.error.is_some() && remote.tailscale.error != announced.1 {
                        say(format!("Tailscale: {}", remote.tailscale.error.clone().unwrap_or_default()));
                    }
                    announced.1 = remote.tailscale.error.clone();
                }
                changed.json(json!({ "type": "remote.changed", "remote": remote }));
            }),
            workspace: Some(self.0.publisher.clone()),
            tailscale,
            port: bridge.port,
        })
        .await;
        let _ = self.0.mobile.set(mobile.clone());
        host.sink.json(json!({ "type": "remote.changed", "remote": mobile.mobile_state() }));
    }

    pub fn mobile(&self) -> Option<Arc<MobileHost>> {
        self.0.mobile.get().cloned()
    }

    pub async fn query(&self, id: &str, query: Value) -> Result<Value, String> {
        match self.0.links.get() {
            Some(links) => links.query(id, query).await,
            None => Err("That computer is unavailable.".into()),
        }
    }

    /// What reads this computer answers, or forwards to the one holding what they name.
    pub fn reads(&self) -> Option<Arc<ComputerReads>> {
        self.0.reads.get().cloned()
    }

    /// Whether anything outside this window watches the state: a paired computer or a phone.
    pub fn watched(&self, state: &WorkspaceState) -> bool {
        self.0.publisher.watched() || !state.remote.sessions.is_empty() || self.0.phones.lock().unwrap().is_some()
    }

    /// Hands what changed to the computers and phones watching, from the window's own thread.
    pub fn publish(&self, state: &WorkspaceState) {
        self.0.publisher.publish_state(state);
        let Some(mobile) = self.0.mobile.get() else { return };
        let mut held = self.0.phones.lock().unwrap();
        if state.remote.sessions.is_empty() {
            *held = None;
            return;
        }
        let view = project_mobile_view(state, act_core::util::now());
        let update = match held.as_ref() {
            None => {
                let update = json!({ "kind": "snapshot", "view": view });
                *held = Some(view);
                update
            }
            Some(previous) => {
                let Some(patch) = diff_mobile_view(previous, &view) else { return };
                let next = apply_mobile_patch(previous, &patch).unwrap_or(view);
                *held = Some(next);
                json!({ "kind": "patch", "patch": patch })
            }
        };
        mobile.publish_view(&update);
    }

    /// Tells paired computers and phones about a thread, as a host nobody sits at does.
    pub fn notice(&self, notice: &act_host::notify::ThreadNotice) {
        if let Some(mobile) = self.0.mobile.get() {
            mobile.notice_computers(notice);
        }
        self.notice_phones(notice);
    }

    /// Hands a notice to the phones that take them while closed.
    pub fn notice_phones(&self, notice: &act_host::notify::ThreadNotice) {
        if let Some(mobile) = self.0.mobile.get().cloned() {
            let notice = notice.clone();
            act_host::runtime().spawn(async move { mobile.notice_phones(&notice).await });
        }
    }

    /// A machine that slept has lines that only look open.
    pub fn resumed(&self) {
        if let Some(links) = self.0.links.get() {
            links.reconnect(None);
        }
        if let Some(mobile) = self.0.mobile.get() {
            mobile.recheck();
        }
    }

    pub async fn perform(&self, sink: &Sink, attachments: &AttachmentStore, work: EffectWork) {
        let links = self.0.links.get().cloned();
        let link_failed = || "Computers are not ready.".to_string();
        match work {
            EffectWork::ComputerDiscover => match links.ok_or_else(link_failed) {
                Ok(links) => match links.discover().await {
                    Ok(found) => sink.json(json!({ "type": "computers.found", "found": found })),
                    Err(message) => sink.json(json!({ "type": "computers.search-failed", "message": message })),
                },
                Err(message) => sink.json(json!({ "type": "computers.search-failed", "message": message })),
            },
            EffectWork::ComputerPair { host, name, code } => {
                let paired = match links {
                    Some(links) => links.pair(&host, &name, &code).await,
                    None => Err(link_failed()),
                };
                if let Err(message) = paired {
                    sink.json(json!({ "type": "computers.pair-failed", "message": message }));
                }
            }
            EffectWork::ComputerForget { id } => match links {
                Some(links) => links.forget(&id),
                None => sink.failed(link_failed()),
            },
            EffectWork::ComputerReconnect { id } => match links {
                Some(links) => links.reconnect(id.as_deref()),
                None => sink.failed(link_failed()),
            },
            EffectWork::ComputerRename { name } => match links {
                Some(links) => {
                    links.rename(&name);
                    if let Some(mobile) = self.0.mobile.get() {
                        mobile.announce_computer_name();
                    }
                }
                None => sink.failed(link_failed()),
            },
            EffectWork::ComputerLabel { id, name } => match links {
                Some(links) => links.label(&id, &name),
                None => sink.failed(link_failed()),
            },
            EffectWork::ComputerForward { id, inputs, project_add_request, draft, proposal, silent } => forward(sink, links, attachments, id, inputs, project_add_request, draft, proposal, silent).await,
            EffectWork::RemoteSetEnabled { enabled } => self.bridge(sink, |mobile| async move { Ok(mobile.set_enabled(enabled).await) }.boxed()).await,
            EffectWork::RemoteCreatePairingCode => self.bridge(sink, |mobile| async move { mobile.create_pairing_code().map(|_| mobile.mobile_state()) }.boxed()).await,
            EffectWork::RemoteRevokeDevice { device_id } => self.bridge(sink, |mobile| async move { Ok(mobile.revoke_device(&device_id)) }.boxed()).await,
            EffectWork::RemoteRefresh => self.bridge(sink, |mobile| async move { Ok(mobile.refresh_tailscale().await) }.boxed()).await,
            _ => {}
        }
    }

    /// Each bridge setting answers with the whole of what the bridge is doing.
    async fn bridge(&self, sink: &Sink, change: impl FnOnce(Arc<MobileHost>) -> BoxFuture<'static, Result<act_core::domain::mobile::MobileServerState, String>>) {
        let Some(mobile) = self.0.mobile.get().cloned() else { return sink.failed("The phone bridge is still starting.") };
        match change(mobile).await {
            Ok(remote) => sink.json(json!({ "type": "remote.changed", "remote": remote })),
            Err(message) => sink.failed(message),
        }
    }

    pub fn hold_pairing_socket(&self, stop: tokio_util::sync::CancellationToken) {
        *self.0.pairing_socket.lock().unwrap() = Some(stop);
    }

    pub async fn stop(&self) {
        if let Some(stop) = self.0.pairing_socket.lock().unwrap().take() {
            stop.cancel();
        }
        if let Some(links) = self.0.links.get() {
            links.stop();
        }
        if let Some(mobile) = self.0.mobile.get() {
            mobile.stop().await;
        }
    }
}

/// Another computer cannot read this one's files, so each staged image goes as its bytes and never by its path.
async fn carry_images(attachments: &AttachmentStore, inputs: &mut [WorkspaceInput]) -> Result<(), String> {
    for input in inputs {
        let WorkspaceInput::Command(AppCommand::AttachmentsSend { attachments: outgoing, .. }) = input else { continue };
        for attachment in outgoing {
            if let Some(path) = attachment.path.take().filter(|_| attachment.source.is_empty()) {
                let data = attachments.read_kept(&path).await.map_err(|_| "That image is no longer available.".to_string())?;
                attachment.source = format!("data:image/png;base64,{data}");
            }
        }
    }
    Ok(())
}

/// What the other computer refused comes back as this window's own error, since it is the one that
/// asked. A draft a send carried is let go of only once that computer has taken it.
#[allow(clippy::too_many_arguments)]
async fn forward(sink: &Sink, links: Option<Arc<ComputerLinks>>, attachments: &AttachmentStore, id: String, mut inputs: Vec<WorkspaceInput>, project_add_request: Option<i64>, draft: Option<act_core::events::SentDraft>, proposal: Option<act_core::effects::ForwardProposal>, silent: bool) {
    let housekeeping = inputs.iter().all(|input| {
        let kind = input.to_json().ok().and_then(|value| value.get("type").and_then(Value::as_str).map(String::from)).unwrap_or_default();
        matches!(kind.as_str(), "view.set-focused" | "usage.read" | "pull-request.read-member")
    });
    let removes_project = inputs.iter().any(|input| input.to_json().ok().and_then(|value| value.get("type").and_then(Value::as_str).map(|kind| kind == "project.remove")).unwrap_or(false));
    let carried = carry_images(attachments, &mut inputs).await;
    let values: Vec<Value> = inputs.iter().filter_map(|input| input.to_json().ok()).collect();
    let sent = match (carried, links) {
        (Err(message), _) => Err(message),
        (Ok(()), Some(links)) => links.send(&id, values).await,
        (Ok(()), None) => Err("That computer is no longer paired.".to_string()),
    };
    let answered = sent.and_then(|result| {
        if result.get("ok") == Some(&Value::Bool(true)) {
            Ok(result)
        } else if let Some(proposal) = &proposal {
            let message = result.get("message").and_then(Value::as_str).unwrap_or("That computer refused it.").to_string();
            sink.json(json!({ "type": "proposal.settled", "taskId": proposal.task_id, "proposalId": proposal.proposal_id, "message": message, "refused": true }));
            Err(String::new())
        } else {
            Err(result.get("message").and_then(Value::as_str).unwrap_or("That computer refused it.").to_string())
        }
    });
    match answered {
        Ok(result) => {
            let task_id = result.get("taskId").and_then(Value::as_str).map(String::from);
            if let Some(proposal) = proposal {
                match task_id {
                    Some(thread_id) => sink.json(json!({ "type": "proposal.settled", "taskId": proposal.task_id, "proposalId": proposal.proposal_id, "threadId": thread_id })),
                    None => sink.json(json!({ "type": "proposal.settled", "taskId": proposal.task_id, "proposalId": proposal.proposal_id, "message": "That computer did not say which thread it started." })),
                }
                return;
            }
            if let Some(task_id) = task_id {
                sink.json(json!({ "type": "computers.answered", "taskId": task_id }));
            }
            if let Some(request) = project_add_request {
                sink.json(json!({ "type": "project.add-finished", "request": request }));
            }
            if let Some(draft) = draft {
                sink.event(WorkspaceEvent::ComputersForwarded { draft });
            }
        }
        Err(message) if message.is_empty() => {}
        Err(message) => {
            if silent || housekeeping {
                return;
            }
            if let Some(proposal) = proposal {
                sink.json(json!({ "type": "proposal.settled", "taskId": proposal.task_id, "proposalId": proposal.proposal_id, "message": message }));
                return;
            }
            if let Some(request) = project_add_request {
                sink.json(json!({ "type": "project.add-finished", "request": request, "error": message }));
                return;
            }
            match draft.and_then(|draft| draft.attachments).map(|strip| strip.key) {
                Some(key) => sink.json(json!({ "type": "attachments.failed", "taskId": if key.is_empty() { Value::Null } else { json!(key) }, "message": message })),
                None if removes_project => sink.json(json!({ "type": "action.failed", "message": message, "page": "projects" })),
                None => sink.failed(message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use act_core::domain::conversation::OutgoingAttachment;

    #[tokio::test]
    async fn a_staged_image_goes_to_another_computer_as_its_bytes() {
        let dir = std::env::temp_dir().join(format!("act-carry-images-{}", std::process::id()));
        let store = AttachmentStore::new(&dir);
        let path = store.write("aGk=", None).await.unwrap().to_string_lossy().into_owned();
        let staged = OutgoingAttachment { id: "a".into(), source: String::new(), annotations: Vec::new(), path: Some(path), context: None };
        let mut inputs = vec![WorkspaceInput::Command(AppCommand::AttachmentsSend { task_id: None, steer: None, attachments: vec![staged], text: None })];
        let carried = carry_images(&store, &mut inputs).await;
        let _ = std::fs::remove_dir_all(&dir);
        carried.unwrap();
        let WorkspaceInput::Command(AppCommand::AttachmentsSend { attachments, .. }) = &inputs[0] else { unreachable!() };
        assert_eq!(attachments[0].source, "data:image/png;base64,aGk=");
        assert!(attachments[0].path.is_none());
    }
}
