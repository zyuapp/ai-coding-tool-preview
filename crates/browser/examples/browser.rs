//! The browser panel in a GPUI window, driven by the real workspace reducer.
//!
//! `cargo run -p act_browser --example browser [--url URL] [--ask URL] [--tools] [--find TEXT] [--picker] [--import]`
//! - `--url` opens a page as the user would, by typing it.
//! - `--ask` has a run ask for a page, which puts the approval in the panel.
//! - `--tools` drives the panel with the browser tools against a local test page and prints what
//!   each tool answers.
//! - `--frames` has a run open a page on one origin whose frame comes from another, then leave it
//!   for that other origin: the frame loads freely, leaving asks first.
//! - `--find` searches the page; `--picker` shows the import site picker; `--import` lists the
//!   browser profiles this machine has and exits.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use act_browser::{workspace_event, BrowserEvent, BrowserHost, BrowserPanel, BrowserScope, BrowserWorkspace, PanelBridge};
use act_core::domain::platform::BrowserApproval;
use act_core::events::WorkspaceInput;
use act_core::reducer::browser_permissions;
use act_core::state::{browser_target, dock_for, dock_owner};
use act_core::{reduce_transition, AppCommand, CommandResult, WorkspaceEvent, WorkspaceState};
use act_host::tools::browser::bound_browser_tools;
use act_ui::theme::{ActiveTheme, Environment, Theme};
use act_ui::Assets;
use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use gpui::{div, prelude::*, px, size, App, Application, AsyncApp, Bounds, Context, Entity, WeakEntity, Window, WindowBounds, WindowOptions};

const THREAD: &str = "demo-thread";

struct Demo {
    state: WorkspaceState,
    host: Entity<BrowserHost>,
    panel: Entity<BrowserPanel>,
    picker: Option<Entity<act_browser::ImportPicker>>,
}

enum Request {
    Execute { command: serde_json::Value, reply: oneshot::Sender<Result<(), String>> },
    Scope { task_id: String, tab_id: Option<String>, reply: oneshot::Sender<BrowserScope> },
}

struct Workspace {
    requests: mpsc::UnboundedSender<Request>,
}

impl BrowserWorkspace for Workspace {
    fn execute(&self, command: serde_json::Value) -> BoxFuture<'_, Result<(), String>> {
        async move {
            let (reply, answer) = oneshot::channel();
            self.requests.unbounded_send(Request::Execute { command, reply }).map_err(|_| "closed".to_string())?;
            answer.await.map_err(|_| "closed".to_string())?
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

impl Demo {
    fn apply(&mut self, input: impl Into<WorkspaceInput>, cx: &mut Context<Self>) -> Option<CommandResult> {
        let transition = reduce_transition(&mut self.state, input);
        for effect in transition.effects {
            let Some(task) = self.host.update(cx, |host, cx| host.perform(&effect.work, cx)) else { continue };
            cx.spawn(async move |this: WeakEntity<Demo>, cx: &mut AsyncApp| {
                for event in task.await {
                    if let WorkspaceEvent::ActionFailed { message, .. } = &event {
                        eprintln!("action failed: {message}");
                    }
                    let _ = this.update(cx, |this, cx| this.apply(event, cx));
                }
            })
            .detach();
        }
        self.sync(cx);
        transition.result
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let owner = dock_owner(&self.state, None);
        let dock = dock_for(&self.state, &owner);
        let tab = browser_target(dock, None).cloned();
        let approval: Option<BrowserApproval> =
            self.state.browser_approval.clone().filter(|approval| tab.as_ref().is_some_and(|tab| approval.tab_id.as_deref() == Some(tab.id.as_str())));
        if let Some(tab) = tab {
            self.panel.update(cx, |panel, cx| panel.set_tab(tab, approval, cx));
        }
        cx.notify();
    }

    fn answer(&mut self, request: Request, cx: &mut Context<Self>) {
        match request {
            Request::Execute { command, reply } => {
                let result = match serde_json::from_value::<AppCommand>(command) {
                    Ok(command) => match self.apply(command, cx) {
                        Some(CommandResult::Failed { message, .. }) => Err(message),
                        _ => Ok(()),
                    },
                    Err(error) => Err(error.to_string()),
                };
                let _ = reply.send(result);
            }
            Request::Scope { task_id, tab_id, reply } => {
                let scope = match &self.state.browser_approval {
                    Some(approval) if approval.task_id == task_id => BrowserScope::AwaitingApproval { url: approval.url.clone() },
                    _ => {
                        let dock = dock_for(&self.state, &dock_owner(&self.state, Some(&task_id)));
                        BrowserScope::Dock {
                            tabs: dock.browser_tabs.clone(),
                            target: browser_target(dock, tab_id.as_deref()).map(|tab| tab.id.clone()),
                            permissions: browser_permissions(&self.state),
                        }
                    }
                };
                let _ = reply.send(scope);
            }
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = &cx.theme().tokens;
        act_ui::root(cx)
            .flex()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .p(px(28.0))
                    .pt(px(48.0))
                    .gap(px(10.0))
                    .text_color(t.ink_dim)
                    .child(div().text_size(px(20.0)).text_color(t.ink).child("Browser panel"))
                    .child("The page on the right is a WKWebView placed over the panel's page area.")
                    .children(self.picker.clone().map(|picker| div().mt(px(24.0)).w(px(520.0)).child(picker))),
            )
            .child(div().w(px(1.0)).h_full().bg(t.line_soft))
            .child(div().w(px(760.0)).h_full().pt(px(52.0)).bg(t.canvas).child(self.panel.clone()))
    }
}

/// A page with something to read, click, type into, log and fetch.
fn serve_test_page() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            println!("served {path}");
            let (status, kind, body) = match path.as_str() {
                "/frames" => ("200 OK", "text/html", FRAMES_PAGE.to_string()),
                "/inner" => ("200 OK", "text/html", "<!doctype html><p>Inner frame</p><script>setTimeout(() => location.href = '/inner2', 300)</script>".to_string()),
                "/inner2" => ("200 OK", "text/html", "<!doctype html><p>Inner frame, moved on</p>".to_string()),
                "/away" => ("200 OK", "text/html", "<!doctype html><title>Away</title><p>Away</p>".to_string()),
                "/api" => ("200 OK", "application/json", r#"{"answer":42}"#.to_string()),
                "/missing" => ("404 Not Found", "text/plain", "missing".to_string()),
                "/second" => ("200 OK", "text/html", "<!doctype html><title>Second page</title><p>The second page.</p>".to_string()),
                _ => ("200 OK", "text/html", TEST_PAGE.to_string()),
            };
            let mut stream = stream;
            let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    port
}

const TEST_PAGE: &str = r#"<!doctype html><html><head><title>Panel test page</title>
<style>body{font:15px -apple-system,system-ui;margin:32px;color:#222;background:#fafafa} input,button{font:inherit;padding:6px 10px;margin:4px}</style></head>
<body><h1>Panel test page</h1><p>Type a name, then fetch.</p>
<label for="name">Name</label><input id="name" placeholder="Your name">
<input type="password" placeholder="Secret" value="hunter2">
<button id="go" onclick="go()">Fetch the answer</button>
<p id="out"></p><p id="visits"></p><a href="/second">Second page</a> <a href="/second" target="_blank">Second page in a window</a>
<script>
const visits = Number((document.cookie.match(/visits=(\d+)/) || [])[1] || 0) + 1;
document.cookie = 'visits=' + visits + '; max-age=3600; path=/';
document.getElementById('visits').textContent = 'Visits: ' + visits;
console.info('page ready');
async function go() {
  const name = document.getElementById('name').value;
  console.warn('fetching for', name);
  const answer = await (await fetch('/api')).json();
  await fetch('/missing');
  document.getElementById('out').textContent = 'Fetched ' + answer.answer + ' for ' + name;
  console.error('done', { answer: answer.answer });
}
</script></body></html>"#;

/// Served from 127.0.0.1; its frame and its link go to localhost, another origin on the same server.
const FRAMES_PAGE: &str = r#"<!doctype html><html><head><title>Frames page</title></head><body><h1>Frames page</h1>
<script>const other = 'http://localhost:' + location.port;
document.write('<iframe src="' + other + '/inner" width="300" height="80"></iframe><iframe src="about:blank"></iframe><p><a href="' + other + '/away">Leave</a></p>');</script>
</body></html>"#;

async fn run_frames(remote: act_browser::BrowserRemote, requests: mpsc::UnboundedSender<Request>, decide: mpsc::UnboundedSender<bool>, port: u16, cx: &mut AsyncApp) {
    let bridge = Arc::new(PanelBridge::new(THREAD, Arc::new(Workspace { requests: requests.clone() }), remote));
    let tools = bound_browser_tools(bridge, false);
    let executor = cx.background_executor().clone();
    let call = async |name: &str, args: serde_json::Value| {
        let tool = tools.iter().find(|tool| tool.name == name).cloned().unwrap();
        let arguments = args.as_object().cloned();
        let result = executor.spawn(async move { tool.call(arguments).await }).await;
        println!("── {name} {args}{}\n{}\n", if result.is_error { " (error)" } else { "" }, result.text);
        result.text
    };
    let scope = async || {
        let (reply, answer) = oneshot::channel();
        let _ = requests.unbounded_send(Request::Scope { task_id: THREAD.into(), tab_id: None, reply });
        match answer.await {
            Ok(BrowserScope::AwaitingApproval { url }) => format!("awaiting approval for {url}"),
            Ok(_) => "no approval pending".to_string(),
            Err(_) => "closed".to_string(),
        }
    };
    call("browser_open", serde_json::json!({ "url": format!("http://127.0.0.1:{port}/frames") })).await;
    executor.timer(Duration::from_millis(1500)).await;
    let _ = decide.unbounded_send(true);
    executor.timer(Duration::from_millis(2500)).await;
    println!("after load: {}", scope().await);
    let snapshot = call("browser_read", serde_json::json!({})).await;
    let leave = snapshot.lines().find(|line| line.contains("Leave")).and_then(|line| line.strip_prefix('[')).and_then(|line| line.split(']').next()).unwrap_or("1").to_string();
    call("browser_click", serde_json::json!({ "ref": leave })).await;
    executor.timer(Duration::from_millis(2000)).await;
    println!("after leaving: {}", scope().await);
    println!("frames done");
}

async fn run_tools(remote: act_browser::BrowserRemote, requests: mpsc::UnboundedSender<Request>, decide: mpsc::UnboundedSender<bool>, port: u16, cx: &mut AsyncApp) {
    let bridge = Arc::new(PanelBridge::new(THREAD, Arc::new(Workspace { requests }), remote));
    let tools = bound_browser_tools(bridge, false);
    let executor = cx.background_executor().clone();
    let mut log = String::new();
    let mut call = async |name: &str, args: serde_json::Value| {
        let tool = tools.iter().find(|tool| tool.name == name).cloned().unwrap();
        let arguments = args.as_object().cloned();
        let result = executor.spawn(async move { tool.call(arguments).await }).await;
        let entry = format!("── {name} {args}{}\n{}\n\n", if result.is_error { " (error)" } else { "" }, result.text);
        print!("{entry}");
        log.push_str(&entry);
        result.text
    };
    let page = format!("http://127.0.0.1:{port}/");
    call("browser_open", serde_json::json!({ "url": page })).await;
    executor.timer(Duration::from_millis(1500)).await;
    let _ = decide.unbounded_send(true);
    executor.timer(Duration::from_millis(300)).await;
    let snapshot = call("browser_read", serde_json::json!({})).await;
    let reference = |label: &str| {
        snapshot.lines().filter(|line| line.starts_with('[')).find(|line| line.contains(label)).and_then(|line| line.strip_prefix('[')).and_then(|line| line.split(']').next()).unwrap_or("1").to_string()
    };
    let (name, button) = (reference("input:text"), reference("Fetch the answer"));
    call("browser_type", serde_json::json!({ "ref": name, "text": "Ada" })).await;
    call("browser_click", serde_json::json!({ "ref": button })).await;
    call("browser_wait", serde_json::json!({ "condition": "text", "value": "Fetched 42", "timeoutSeconds": 5 })).await;
    call("browser_wait", serde_json::json!({ "condition": "network-idle", "timeoutSeconds": 5 })).await;
    call("browser_console", serde_json::json!({})).await;
    call("browser_console", serde_json::json!({ "minimumLevel": "warning" })).await;
    call("browser_network", serde_json::json!({})).await;
    call("browser_network", serde_json::json!({ "failuresOnly": true })).await;
    call("browser_screenshot", serde_json::json!({})).await;
    call("browser_screenshot", serde_json::json!({ "fullPage": true })).await;
    call("browser_tabs", serde_json::json!({})).await;
    let popup = reference("in a window");
    call("browser_click", serde_json::json!({ "ref": popup })).await;
    call("browser_open", serde_json::json!({ "url": format!("{page}second") })).await;
    call("browser_back", serde_json::json!({})).await;
    call("browser_back", serde_json::json!({ "forward": true })).await;
    call("browser_search", serde_json::json!({ "query": "gpui rust" })).await;
    let _ = std::fs::write("/tmp/act-browser-tools.txt", &log);
    println!("tools done");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).cloned();
    let url = value("--url");
    let ask = value("--ask");
    let tools = args.iter().any(|arg| arg == "--tools");
    let frames = args.iter().any(|arg| arg == "--frames");
    let find = value("--find");
    let show_picker = args.iter().any(|arg| arg == "--picker");
    if args.iter().any(|arg| arg == "--import") {
        let sources = act_browser::import::list_import_sources();
        println!("{} browser profiles hold cookies", sources.len());
        for source in sources.iter().take(3) {
            let sites = act_browser::import::list_import_sites(&source.id).map(|sites| sites.len());
            println!("{} / {}: {:?} sites", source.browser, source.profile, sites);
        }
        return;
    }
    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        act_ui::init(cx);
        act_browser::init(cx);
        Theme::set_environment(cx, Environment::macos_glass());
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions { title: Some("Browser".into()), appears_transparent: true, traffic_light_position: Some(gpui::point(px(16.0), px(16.0))) }),
            ..Default::default()
        };
        let (url, ask) = (url.clone(), ask.clone());
        cx.open_window(options, move |window, cx| {
            let host = cx.new(|cx| BrowserHost::new(window, cx));
            let (requests, mut incoming) = mpsc::unbounded::<Request>();
            let (decide, mut decisions) = mpsc::unbounded::<bool>();
            let demo = cx.new(|cx| {
                let weak = cx.entity().downgrade();
                let panel = cx.new(|cx| {
                    BrowserPanel::new(
                        host.clone(),
                        move |command, _window, cx| {
                            let _ = weak.update(cx, |demo: &mut Demo, cx| demo.apply(command, cx));
                        },
                        window,
                        cx,
                    )
                });
                cx.subscribe(&host, |demo: &mut Demo, _, event: &BrowserEvent, cx| {
                    if let Some(event) = workspace_event(event) {
                        demo.apply(event, cx);
                    }
                })
                .detach();
                let state = WorkspaceState { current_id: Some(THREAD.into()), ..Default::default() };
                let picker = show_picker.then(|| {
                    let sites = ["github.com", "google.com", "linear.app", "notion.so", "vercel.com"]
                        .iter()
                        .enumerate()
                        .map(|(index, domain)| act_core::domain::terminal::BrowserImportSite { domain: domain.to_string(), cookies: (index as i64 + 1) * 7 })
                        .collect();
                    let state = act_core::state::BrowserImportPicker { source_id: "chrome:0:Default".into(), sites: Some(sites), selected: vec!["github.com".into(), "linear.app".into()], filter: String::new() };
                    cx.new(|cx| act_browser::ImportPicker::new(state, false, |command, _, _| println!("picker: {command:?}"), window, cx))
                });
                Demo { state, host: host.clone(), panel, picker }
            });
            demo.update(cx, |demo: &mut Demo, cx| {
                demo.apply(AppCommand::BrowserNewTab, cx);
                if let Some(url) = url {
                    let tab_id = demo.panel.read(cx).tab().id.clone();
                    demo.apply(AppCommand::BrowserOpen { task_id: None, url, tab_id: Some(tab_id), new_tab: None }, cx);
                }
                if let Some(url) = ask {
                    demo.apply(AppCommand::BrowserOpen { task_id: Some(THREAD.into()), url, tab_id: None, new_tab: None }, cx);
                }
            });
            let weak = demo.downgrade();
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Some(request) = incoming.next().await {
                    let _ = weak.update(cx, |demo: &mut Demo, cx| demo.answer(request, cx));
                }
            })
            .detach();
            let weak = demo.downgrade();
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Some(allow) = decisions.next().await {
                    let _ = weak.update(cx, |demo: &mut Demo, cx| {
                        if let Some(approval) = demo.state.browser_approval.clone() {
                            demo.apply(AppCommand::BrowserDecide { allow, approval_id: approval.approval_id }, cx);
                        }
                    });
                }
            })
            .detach();
            if let Some(query) = find.clone() {
                let finder = host.clone();
                cx.spawn(async move |cx: &mut AsyncApp| {
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    for find_next in [false, true, true] {
                        let task = finder.update(cx, |host, cx| {
                            let tab_id = host.pages().and_then(|pages| pages.tab_ids().first().cloned()).unwrap_or_default();
                            host.perform(&act_core::EffectWork::FindInPage { tab_id, query: query.clone(), forward: true, find_next }, cx)
                        });
                        if let Ok(Some(task)) = task {
                            task.await;
                        }
                    }
                })
                .detach();
                cx.subscribe(&host, |_, event: &BrowserEvent, _| {
                    if let BrowserEvent::Find { results, .. } = event {
                        println!("find: {} matches, on {:?}", results.matches, results.index);
                    }
                })
                .detach();
            }
            if tools {
                let remote = host.update(cx, |host, cx| host.remote(cx));
                let port = serve_test_page();
                cx.spawn(async move |cx: &mut AsyncApp| run_tools(remote, requests, decide, port, cx).await).detach();
            } else if frames {
                let remote = host.update(cx, |host, cx| host.remote(cx));
                let port = serve_test_page();
                cx.spawn(async move |cx: &mut AsyncApp| run_frames(remote, requests, decide, port, cx).await).detach();
            }
            demo
        })
        .unwrap();
        cx.activate(true);
    });
}
