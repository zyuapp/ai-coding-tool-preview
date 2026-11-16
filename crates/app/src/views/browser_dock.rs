//! The browser tab: act_browser's panel mounted in the dock, the host that owns the window's pages,
//! and the find bar placed above the page (BrowserPanel.tsx, browser-view.ts).

use act_browser::{BrowserEvent, BrowserHost, BrowserPanel};
use act_core::domain::platform::BrowserTab;
use act_core::events::WorkspaceInput;
use act_core::state::WorkspaceState;
use gpui::{div, prelude::*, px, AnyElement, AnyView, App, Context, Entity, Render, WeakEntity, Window};

use super::root::Root;
use crate::workspace::{dispatch, Workspace};

#[derive(Default)]
pub struct BrowserDock {
    pub host: Option<Entity<BrowserHost>>,
    panel: Option<Entity<BrowserPanel>>,
    find: Option<Entity<FindSlot>>,
    finding: bool,
    focus_token: u64,
    /// The page last put in front, so a tab the dock surfaces on its own (an approval) draws its page.
    fronted: Option<String>,
}

/// Starts the window's pages and hands their effects and events to the workspace.
pub fn install(root: &mut Root, workspace: &Entity<Workspace>, window: &mut Window, cx: &mut Context<Root>) {
    let host = cx.new(|cx| BrowserHost::new(window, cx));
    let target = workspace.downgrade();
    cx.subscribe(&host, move |_, _, event: &BrowserEvent, cx| {
        if let Some(event) = act_browser::workspace_event(event) {
            let _ = target.update(cx, |workspace, cx| workspace.dispatch(WorkspaceInput::Event(event), cx));
        }
    })
    .detach();
    workspace.update(cx, |workspace, cx| workspace.install_browser(host.clone(), cx));
    root.dock.browser.host = Some(host);
}

/// The find bar drawn by the window, placed in the row the panel keeps above its page.
struct FindSlot {
    root: WeakEntity<Root>,
}

impl Render for FindSlot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = self.root.update(cx, |root, cx| {
            let state = root.state(cx);
            super::find_bar::render(root, &state, false, window, cx)
        });
        div().relative().h(px(46.0)).children(bar.ok().flatten())
    }
}

impl BrowserDock {
    /// Feeds the panel the tab on top, and takes the page off the window whenever it is hidden or covered.
    pub fn sync(&mut self, state: &WorkspaceState, tab: Option<&BrowserTab>, covered: bool, window: &mut Window, cx: &mut Context<Root>) {
        let Some(tab) = tab else {
            self.hide(cx);
            return;
        };
        let Some(host) = self.host.clone() else { return };
        let front_host = host.clone();
        let panel = self
            .panel
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    BrowserPanel::new(
                        host,
                        |command, _, cx: &mut App| {
                            let command = command.clone();
                            cx.defer(move |cx| dispatch(command, cx));
                        },
                        window,
                        cx,
                    )
                })
            })
            .clone();
        let approval = state.browser_approval.clone().filter(|approval| approval.tab_id.as_deref() == Some(tab.id.as_str()));
        let finding = state.find.as_ref().is_some_and(|find| matches!(&find.target, act_core::domain::find::FindTarget::Browser { tab_id } if *tab_id == tab.id));
        if finding && self.find.is_none() {
            let root = cx.entity().downgrade();
            self.find = Some(cx.new(|_| FindSlot { root }));
        }
        let find: Option<AnyView> = finding.then(|| self.find.clone().map(Into::into)).flatten();
        let refresh_find = finding != self.finding || finding;
        self.finding = finding;
        if self.fronted.as_deref() != Some(tab.id.as_str()) {
            self.fronted = Some(tab.id.clone());
            let tab_id = tab.id.clone();
            // Opening is idempotent: a page with a view keeps it, and a restored one loads its address.
            let url = (!tab.url.is_empty()).then(|| tab.url.clone());
            front_host.update(cx, |host, cx| {
                for effect in [act_core::effects::EffectWork::BrowserOpen { task_id: None, tab_id: tab_id.clone(), url }, act_core::effects::EffectWork::BrowserShow { tab_id: Some(tab_id.clone()) }] {
                    if let Some(task) = host.perform(&effect, cx) {
                        task.detach();
                    }
                }
            });
        }
        front_host.read(cx).set_shortcuts(&state.shortcuts);
        let tab = tab.clone();
        panel.update(cx, |panel, cx| {
            panel.set_tab(tab, approval, cx);
            if refresh_find {
                panel.set_find(find, cx);
            }
            panel.set_shown(true, cx);
            panel.set_covered(covered, cx);
        });
        if let Some(slot) = self.find.clone().filter(|_| finding) {
            slot.update(cx, |_, cx| cx.notify());
        }
    }

    /// Takes the page off the window: the dock is closed, covered by settings, or on another tab.
    pub fn hide(&mut self, cx: &mut Context<Root>) {
        self.fronted = None;
        if let Some(panel) = &self.panel {
            panel.update(cx, |panel, cx| panel.set_shown(false, cx));
        }
    }

    pub fn render(&self, _tab: &BrowserTab, _cx: &mut Context<Root>) -> AnyElement {
        match &self.panel {
            Some(panel) => div().size_full().child(panel.clone()).into_any_element(),
            None => div().size_full().into_any_element(),
        }
    }

    /// Hands the keyboard to the page, or to the address bar of a tab with none.
    pub fn request_focus(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        self.focus_token += 1;
        let token = self.focus_token;
        if let Some(panel) = &self.panel {
            panel.update(cx, |panel, cx| panel.request_focus(token, window, cx));
        }
    }
}
