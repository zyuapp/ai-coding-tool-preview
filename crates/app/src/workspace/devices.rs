//! The window's browser pages and simulator: their effects, and the bridges a run's browser tools
//! reach them through.

use std::sync::Arc;

use act_browser::{BrowserEvent, BrowserHost, BrowserScope, BrowserWorkspace, PanelBridge};
use act_core::effects::EffectWork;
use act_core::events::WorkspaceInput;
use act_core::CommandResult;
use act_core::state::{browser_target, dock_for, dock_owner};
use act_core::AppCommand;
use act_devices::simulator::SimulatorHost;
use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use gpui::{AppContext as _, Context, Entity};

use super::history::Reply;
use super::Workspace;
use crate::effects::tracking::Tracker;
use crate::effects::Inbound;

/// What a run's browser tools ask the window for.
enum Request {
    Execute { command: serde_json::Value, reply: oneshot::Sender<Result<(), String>> },
    Scope { task_id: String, tab_id: Option<String>, reply: oneshot::Sender<BrowserScope> },
}

/// The workspace side of the browser tools, answered on the window's thread.
#[derive(Clone)]
pub struct BrowserTools {
    requests: mpsc::UnboundedSender<Request>,
    remote: act_browser::BrowserRemote,
}

struct ToolWorkspace {
    requests: mpsc::UnboundedSender<Request>,
}

impl BrowserWorkspace for ToolWorkspace {
    fn execute(&self, command: serde_json::Value) -> BoxFuture<'_, Result<(), String>> {
        async move {
            let (reply, answer) = oneshot::channel();
            self.requests.unbounded_send(Request::Execute { command, reply }).map_err(|_| "The window closed.".to_string())?;
            answer.await.map_err(|_| "The window closed.".to_string())?
        }
        .boxed()
    }

    fn scope(&self, task_id: &str, tab_id: Option<&str>) -> BoxFuture<'_, BrowserScope> {
        let (task_id, tab_id) = (task_id.to_string(), tab_id.map(str::to_string));
        async move {
            let (reply, answer) = oneshot::channel();
            let _ = self.requests.unbounded_send(Request::Scope { task_id, tab_id, reply });
            answer.await.unwrap_or(BrowserScope::Dock { tabs: Vec::new(), target: None, permissions: Default::default() })
        }
        .boxed()
    }
}

/// The window's simulator, which a run's simulator tools read the screen of.
pub static SIMULATOR: std::sync::OnceLock<SimulatorHost> = std::sync::OnceLock::new();

/// The browser tools' way into the window, once the window has its pages.
pub static BROWSER_TOOLS: std::sync::OnceLock<BrowserTools> = std::sync::OnceLock::new();

impl BrowserTools {
    /// The browser tools' bridge for one run.
    pub fn bridge(&self, task_id: &str) -> Arc<dyn act_host::tools::browser::BrowserBridge> {
        Arc::new(PanelBridge::new(task_id, Arc::new(ToolWorkspace { requests: self.requests.clone() }), self.remote.clone()))
    }
}

impl Workspace {
    /// Takes the window's browser host, and starts the simulator host beside it.
    pub fn install_browser(&mut self, host: Entity<BrowserHost>, cx: &mut Context<Self>) {
        cx.subscribe(&host, |workspace, _, event: &BrowserEvent, cx| {
            if let Some(event) = act_browser::workspace_event(event) {
                workspace.dispatch(WorkspaceInput::Event(event), cx);
            }
        })
        .detach();
        let remote = host.update(cx, |host, cx| host.remote(cx));
        let (requests, mut incoming) = mpsc::unbounded::<Request>();
        let _ = BROWSER_TOOLS.set(BrowserTools { requests, remote });
        cx.spawn(async move |this, cx| {
            while let Some(request) = incoming.next().await {
                if this.update(cx, |workspace, cx| workspace.answer_browser(request, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        self.browser = Some(host);
        if cfg!(target_os = "macos") && self.simulator.is_none() {
            let simulator = {
                let _entered = act_host::runtime().enter();
                SimulatorHost::new(Default::default())
            };
            let quitting = simulator.clone();
            cx.on_app_quit(move |_, _| {
                quitting.stop_with_window();
                async {}
            })
            .detach();
            let _ = SIMULATOR.set(simulator.clone());
            self.simulator = Some(simulator);
            let sink = self.host.sink.clone();
            act_devices::simulator::effects::subscribe(&(Arc::new(move |input| sink.send(Inbound::Input(input))) as act_devices::simulator::effects::Dispatch));
        }
    }

    fn answer_browser(&mut self, request: Request, cx: &mut Context<Self>) {
        match request {
            Request::Execute { command, reply } => {
                let command = match serde_json::from_value::<AppCommand>(command) {
                    Ok(command) => command,
                    Err(error) => {
                        let _ = reply.send(Err(error.to_string()));
                        return;
                    }
                };
                let (tracker, _) = Tracker::new();
                let (accepted, answer) = oneshot::channel();
                self.run(WorkspaceInput::Command(command), Reply::Execute(tracker, accepted), cx);
                cx.background_spawn(async move {
                    let result = match answer.await {
                        Ok(CommandResult::Failed { message, .. }) => Err(message),
                        Ok(_) => Ok(()),
                        Err(_) => Err("The window closed.".to_string()),
                    };
                    let _ = reply.send(result);
                })
                .detach();
            }
            Request::Scope { task_id, tab_id, reply } => {
                let state = &self.state;
                let scope = match &state.browser_approval {
                    Some(approval) if approval.task_id == task_id => BrowserScope::AwaitingApproval { url: approval.url.clone() },
                    _ => {
                        let dock = dock_for(state, &dock_owner(state, Some(&task_id)));
                        BrowserScope::Dock {
                            tabs: dock.browser_tabs.clone(),
                            target: browser_target(dock, tab_id.as_deref()).map(|tab| tab.id.clone()),
                            permissions: act_core::reducer::browser_permissions(state),
                        }
                    }
                };
                let _ = reply.send(scope);
            }
        }
    }

    /// Performs a browser or simulator effect. False for any other effect.
    pub(super) fn perform_device(&mut self, work: &EffectWork, tracker: Option<Tracker>, cx: &mut Context<Self>) -> bool {
        if let Some(simulator) = &self.simulator {
            let sink = self.host.sink.clone();
            if act_devices::simulator::effects::perform(simulator, work, Arc::new(move |input| sink.send(Inbound::Input(input)))) {
                return true;
            }
        }
        let Some(browser) = self.browser.clone() else { return false };
        let Some(task) = browser.update(cx, |host, cx| host.perform(work, cx)) else { return false };
        cx.spawn(async move |this, cx| {
            for event in task.await {
                let reply = tracker.clone().map_or(Reply::None, Reply::Tracked);
                if this.update(cx, |workspace, cx| workspace.run(WorkspaceInput::Event(event), reply, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        true
    }
}
