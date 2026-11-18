//! The browser host as the app holds it: one entity per window that owns the pages, performs the
//! reducer's browser effects, and reports what the pages do as events.

use act_core::domain::find::FindTarget;
use act_core::domain::terminal::BrowserImportResult;
use act_core::effects::EffectWork;
use act_core::events::WorkspaceEvent;
use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{App, AppContext, AsyncApp, Context, EventEmitter, Task, Window};

use crate::import::{self, ImportError};
use crate::remote::{BrowserRemote, RemoteCall};
use crate::types::{BrowserEvent, PageBounds};
use crate::Pages;

/// What performing an effect came to: events for the reducer, failures among them.
pub type EffectOutcome = Vec<WorkspaceEvent>;

pub struct BrowserHost {
    pages: Option<Pages>,
    unavailable: Option<String>,
    _forward: Task<()>,
    _quit: gpui::Subscription,
}

impl EventEmitter<BrowserEvent> for BrowserHost {}

fn failed(message: impl Into<String>) -> WorkspaceEvent {
    WorkspaceEvent::ActionFailed { message: message.into(), page: None }
}

fn import_failed(error: ImportError) -> WorkspaceEvent {
    if matches!(error, ImportError::Blocked(_)) {
        import::open_full_disk_access();
    }
    WorkspaceEvent::BrowserImportFailed { message: error.message().to_string() }
}

impl BrowserHost {
    /// Starts the pages for this window. Pages are drawn over the window's own view.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<BrowserEvent>();
        let forward = cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.next().await {
                if this.update(cx, |_, cx| cx.emit(event)).is_err() {
                    break;
                }
            }
        });
        let (pages, unavailable) = match {
            let executor = cx.background_executor().clone();
            crate::platform_pages(window, cx, executor, sender)
        } {
            Ok(pages) => (Some(pages), None),
            Err(message) => (None, Some(message)),
        };
        // Pictures only serve the runs that asked for them, so none outlives the app.
        let quit = cx.on_app_quit(|this: &mut Self, _| {
            if let Some(pages) = &this.pages {
                pages.discard_shots();
            }
            async {}
        });
        Self { pages, unavailable, _forward: forward, _quit: quit }
    }

    pub fn pages(&self) -> Option<&Pages> {
        self.pages.as_ref()
    }

    fn require(&self) -> Result<&Pages, String> {
        self.pages.as_ref().ok_or_else(|| self.unavailable.clone().unwrap_or_else(|| "The browser panel needs the desktop app.".into()))
    }

    fn quietly(&self, work: impl FnOnce(&Pages)) -> Option<Task<EffectOutcome>> {
        if let Some(pages) = &self.pages {
            work(pages);
        }
        Some(Task::ready(Vec::new()))
    }

    /// Where the panel's page area is, in window points, or None while it is off screen.
    pub fn set_bounds(&self, bounds: Option<PageBounds>) {
        if let Some(pages) = &self.pages {
            pages.set_bounds(bounds);
        }
    }

    /// The user's shortcuts, which on Linux a page holding the keyboard hands to the app.
    pub fn set_shortcuts(&self, overrides: &act_core::domain::shortcuts::ShortcutOverrides) {
        if let Some(pages) = &self.pages {
            pages.set_shortcuts(overrides);
        }
    }

    /// Whether the app paints the pages itself (Wayland) rather than placing native views over its window.
    pub fn paints_pages(&self) -> bool {
        self.pages.as_ref().is_some_and(|pages| pages.painted_by_app())
    }

    /// The last frame of a page the app paints.
    pub fn page_frame(&self, tab_id: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        self.pages.as_ref().and_then(|pages| pages.frame(tab_id))
    }

    /// Hands pointer or focus input to a page the app paints.
    pub fn page_input(&self, tab_id: &str, input: &crate::PageInput) {
        if let Some(pages) = &self.pages {
            pages.input(tab_id, input);
        }
    }

    /// Answers a drop-down list a page the app paints opened: the row chosen, or None to close it.
    pub fn page_option(&self, tab_id: &str, choice: Option<usize>) {
        if let Some(pages) = &self.pages {
            pages.choose_option(tab_id, choice);
        }
    }

    /// The pointer a page the app paints shows over itself.
    pub fn page_cursor(&self, tab_id: &str) -> gpui::CursorStyle {
        self.pages.as_ref().map(|pages| pages.cursor(tab_id)).unwrap_or(gpui::CursorStyle::Arrow)
    }

    /// Hands a key to a page the app paints when it holds the keyboard and the key is not the app's.
    /// True when the page took it.
    pub fn page_key(&self, tab_id: &str, down: bool, keystroke: &gpui::Keystroke) -> bool {
        self.pages.as_ref().is_some_and(|pages| pages.key(tab_id, down, keystroke))
    }

    /// Hands the keyboard back to the app when a page holds it.
    pub fn focus_app(&self) {
        if let Some(pages) = &self.pages {
            pages.focus_host();
        }
    }

    /// Performs one of the reducer's browser effects. Answers None for an effect that is not the
    /// browser's; otherwise a task resolving to the events the effect produced.
    pub fn perform(&self, effect: &EffectWork, cx: &mut Context<Self>) -> Option<Task<EffectOutcome>> {
        let sync = |work: Result<(), String>| -> Option<Task<EffectOutcome>> {
            Some(Task::ready(match work {
                Ok(()) => Vec::new(),
                Err(message) => vec![failed(message)],
            }))
        };
        match effect {
            // Without pages there is nothing to configure, close, show or focus.
            EffectWork::BrowserPermissions { permissions } => self.quietly(|pages| pages.configure_permissions(permissions)),
            EffectWork::BrowserClose { tab_id } => self.quietly(|pages| pages.close_tab(tab_id)),
            EffectWork::BrowserShow { tab_id } => self.quietly(|pages| pages.show_tab(tab_id.as_deref())),
            EffectWork::FocusBrowser { tab_id } => self.quietly(|pages| pages.focus_tab(tab_id)),
            EffectWork::BrowserOpen { task_id, tab_id, url } => sync(self.require().and_then(|pages| pages.open_tab(tab_id, url.as_deref(), task_id.as_deref()))),
            EffectWork::BrowserNavigate { task_id, tab_id, url } => sync(self.require().and_then(|pages| pages.navigate(tab_id, url, task_id.as_deref()))),
            EffectWork::BrowserHistory { task_id, tab_id, delta } => sync(self.require().and_then(|pages| pages.go_history(tab_id, *delta, task_id.as_deref()))),
            EffectWork::BrowserReload { task_id, tab_id } => sync(self.require().and_then(|pages| pages.reload(tab_id, task_id.as_deref()))),
            EffectWork::BrowserAct { task_id, tab_id, action } => {
                let pages = match self.require() {
                    Ok(pages) => pages.clone(),
                    Err(message) => return sync(Err(message)),
                };
                let (tab_id, action, task_id) = (tab_id.clone(), action.clone(), task_id.clone());
                Some(cx.spawn(async move |_, _| match pages.act(&tab_id, &action, task_id.as_deref()).await {
                    Ok(_) => Vec::new(),
                    Err(message) => vec![failed(message)],
                }))
            }
            EffectWork::BrowserClearData => {
                let Some(pages) = self.pages.clone() else { return sync(Ok(())) };
                Some(cx.spawn(async move |_, _| {
                    pages.clear_data().await;
                    Vec::new()
                }))
            }
            EffectWork::FindInPage { tab_id, query, forward, find_next } => {
                let Some(pages) = self.pages.clone() else { return sync(Ok(())) };
                let (tab_id, query, forward, find_next) = (tab_id.clone(), query.clone(), *forward, *find_next);
                Some(cx.spawn(async move |_, _| {
                    pages.find_in_page(&tab_id, &query, forward, find_next).await;
                    Vec::new()
                }))
            }
            EffectWork::StopFindInPage { tab_id } => {
                let Some(pages) = self.pages.clone() else { return sync(Ok(())) };
                let tab_id = tab_id.clone();
                Some(cx.spawn(async move |_, _| {
                    pages.stop_find_in_page(&tab_id).await;
                    Vec::new()
                }))
            }
            EffectWork::BrowserImportListSources => {
                let read = cx.background_executor().spawn(async { import::list_import_sources() });
                Some(cx.spawn(async move |_, _| vec![WorkspaceEvent::BrowserImportSources { sources: read.await }]))
            }
            EffectWork::BrowserImportListSites { source_id } => {
                let id = source_id.clone();
                let read = cx.background_executor().spawn(async move { import::list_import_sites(&id) });
                let source_id = source_id.clone();
                Some(cx.spawn(async move |_, _| match read.await {
                    Ok(sites) => vec![WorkspaceEvent::BrowserImportSites { source_id, sites }],
                    Err(error) => vec![import_failed(error)],
                }))
            }
            EffectWork::BrowserImportImport { source_id, sites } => {
                let Some(pages) = self.pages.clone() else { return sync(Err(self.require().err().unwrap_or_default())) };
                let (id, chosen) = (source_id.clone(), sites.clone());
                let read = cx.background_executor().spawn(async move { import::read_import_cookies(&id, &chosen) });
                let (source_id, sites) = (source_id.clone(), sites.clone());
                Some(cx.spawn(async move |_, _| match read.await {
                    Ok((cookies, skipped)) => {
                        let details: Vec<import::CookieDetails> = cookies.iter().map(import::cookie_details).collect();
                        let imported = pages.replace_cookies(&details, &sites).await;
                        let skipped = skipped + cookies.len() - imported;
                        vec![WorkspaceEvent::BrowserImportDone { source_id, sites, result: BrowserImportResult { imported: imported as i64, skipped: skipped as i64 } }]
                    }
                    Err(error) => vec![import_failed(error)],
                }))
            }
            _ => None,
        }
    }

    /// A handle any thread can use to read pages, for the tools a run drives the panel with.
    pub fn remote(&self, cx: &mut Context<Self>) -> BrowserRemote {
        let (remote, mut calls) = BrowserRemote::channel();
        let pages = self.pages.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            while let Some(call) = calls.next().await {
                let pages = pages.clone();
                cx.spawn(async move |_| answer(pages, call).await).detach();
            }
        })
        .detach();
        remote
    }
}

async fn answer(pages: Option<Pages>, call: RemoteCall) {
    let Some(pages) = pages else {
        call.unavailable();
        return;
    };
    call.run(&pages).await;
}

/// The events a page reports, as the reducer takes them.
pub fn workspace_event(event: &BrowserEvent) -> Option<WorkspaceEvent> {
    match event {
        BrowserEvent::Page(page) => Some(WorkspaceEvent::BrowserUpdated { page: page.clone() }),
        BrowserEvent::Find { tab_id, results } => Some(WorkspaceEvent::FindResults { target: FindTarget::Browser { tab_id: tab_id.clone() }, results: results.clone() }),
        BrowserEvent::PageFocused { .. } | BrowserEvent::PageFrame { .. } | BrowserEvent::OptionMenu { .. } | BrowserEvent::OptionMenuClosed { .. } => None,
    }
}

/// Opens a browser host for a window and keeps it as the app's.
pub fn open_host(window: &mut Window, cx: &mut App) -> gpui::Entity<BrowserHost> {
    cx.new(|cx| BrowserHost::new(window, cx))
}
