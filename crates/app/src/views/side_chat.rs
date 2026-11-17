//! A side chat and a coordinator's thread tab: a conversation drawn in the dock, its approval, and a
//! composer of its own (DockSideChat.tsx, SideChat.tsx, DockThreadTab.tsx, ThreadTabStrip.tsx).

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use act_core::domain::coordination::is_coordinator;
use act_core::domain::pull_request::{PullRequestAnswer, PullRequestState};
use act_core::events::ChangedFilesResult;
use act_core::state::{dock_side_chats, front_dock, ThreadRunStatus, WorkspaceState, AUTOMATION_PANEL};
use act_core::view::dock::{dock_conversation_view, thread_tab_view, DockConversationView, ThreadTabView};
use act_core::workspace::coordinated_status::{can_resolve, CoordinatedThreadStatus};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::components::panel_header::PanelHeader;
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, list, prelude::*, px, AnyElement, Context, FontWeight, ListAlignment, ListOffset, ListState, SharedString, Window};
use serde_json::json;

use super::common::{engine_icon, icon, ink_icon, rimmed};
use super::grouping::{group_timeline, Entry, Group, Options};
use super::root::Root;
use super::composer::ComposerState;
use super::sidebar::send;
use super::timeline_row::{self, Folds, RowModel};
use crate::workspace::dispatch;

/// One conversation's transcript list, kept while its tab lives.
struct Chat {
    list: ListState,
    signatures: Vec<u64>,
    follow: Rc<std::cell::Cell<bool>>,
    folds: Rc<std::cell::RefCell<Folds>>,
    /// The find match last scrolled to, and how many more frames may centre it.
    found: Option<(act_core::domain::find::FindHit, u8)>,
    /// The run status on show and when it first showed, so its clock keeps counting.
    shown_since: Option<(String, i64)>,
    selection: act_markdown::SelectionGroup,
    /// The scrollbar's room beside the rows, once the transcript overflows.
    gutter: f32,
}

pub struct DockChats {
    chats: HashMap<String, Chat>,
    /// Each chat's composer, lent to `root.composer` while it draws or handles an event.
    composers: HashMap<String, ComposerState>,
    branch_probe: BoundsCell,
    approval_probe: BoundsCell,
    /// Thread tabs whose brief is folded open.
    briefs: HashSet<String>,
}

impl Default for DockChats {
    fn default() -> Self {
        DockChats { chats: HashMap::new(), composers: HashMap::new(), branch_probe: bounds_cell(), approval_probe: bounds_cell(), briefs: HashSet::new() }
    }
}

impl DockChats {
    /// Hands the keyboard to a chat's composer. False when `tab` is no chat.
    pub fn focus(&mut self, tab: &str, window: &mut Window, cx: &mut Context<Root>) -> bool {
        let Some(composer) = self.composers.get(tab) else { return false };
        composer.input.update(cx, |input, cx| {
            input.move_to_end(cx);
            input.focus(window);
        });
        true
    }

    pub fn take_composer(&mut self, task: &str) -> Option<ComposerState> {
        self.composers.remove(task)
    }

    pub fn put_composer(&mut self, task: &str, composer: ComposerState) {
        self.composers.insert(task.to_string(), composer);
    }

    fn chat(&mut self, id: &str) -> &mut Chat {
        if !self.chats.contains_key(id) {
            let list = ListState::new(0, ListAlignment::Top, px(600.0));
            let follow = Rc::new(std::cell::Cell::new(true));
            let watched = follow.clone();
            list.set_scroll_handler(move |event, _, _| {
                if event.is_scrolled {
                    watched.set(event.visible_range.end >= event.count);
                }
            });
            self.chats.insert(id.to_string(), Chat { list, signatures: Vec::new(), follow, folds: Rc::default(), found: None, shown_since: None, selection: act_markdown::SelectionGroup::new(), gutter: 0.0 });
        }
        self.chats.get_mut(id).unwrap()
    }

    /// Drops chats whose tabs are gone.
    fn retain(&mut self, alive: &HashSet<String>) {
        self.chats.retain(|id, _| alive.contains(id));
        self.composers.retain(|id, _| alive.contains(id));
    }
}

fn status_label(status: CoordinatedThreadStatus) -> &'static str {
    match status {
        CoordinatedThreadStatus::Approval => "Needs approval",
        CoordinatedThreadStatus::Asking => "Needs you",
        CoordinatedThreadStatus::Working => "Working",
        CoordinatedThreadStatus::Resolved => "Resolved",
        CoordinatedThreadStatus::Blocked => "Blocked",
        CoordinatedThreadStatus::Failed => "Failed",
        CoordinatedThreadStatus::NeedsWork => "Needs work",
        CoordinatedThreadStatus::Review => "Ready for review",
        CoordinatedThreadStatus::Done => "Done",
        CoordinatedThreadStatus::Idle => "Idle",
    }
}

fn status_dot(status: CoordinatedThreadStatus, t: &Tokens) -> gpui::Rgba {
    match status {
        CoordinatedThreadStatus::Working => t.info,
        CoordinatedThreadStatus::Approval | CoordinatedThreadStatus::Asking => t.accent,
        CoordinatedThreadStatus::Review | CoordinatedThreadStatus::Done | CoordinatedThreadStatus::Resolved => t.success,
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => t.danger,
        CoordinatedThreadStatus::Idle => t.quiet_strong,
    }
}

type Empty = (IconName, &'static str, &'static str);

pub fn render(root: &mut Root, state: &WorkspaceState, active: &str, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let (owner, dock) = front_dock(state);
    let side = dock_side_chats(state, &owner).into_iter().find(|chat| chat.id == active).cloned();
    let alive: HashSet<String> = dock_side_chats(state, &owner).iter().map(|chat| chat.id.clone()).chain(dock.thread_tabs.iter().cloned()).collect();
    root.dock.chats.retain(&alive);
    let Some(thread) = state.thread(active) else { return div().size_full().into_any_element() };
    let view = dock_conversation_view(state, thread);
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let id = active.to_string();

    let (header, empty, disabled, error, strip): (AnyElement, Empty, bool, Option<String>, Option<AnyElement>) = match &side {
        Some(chat) => {
            let source = state.thread(&chat.source_thread_id);
            let available = source.is_some_and(|source| source.continuation.is_some()) || thread.continuation.is_some();
            let close = id.clone();
            let mark = rimmed(div().flex_none().size(px(32.0)).flex().items_center().justify_center().rounded(px(10.0)).border_1().border_color(t.info_line).bg(t.info_surface_soft), &t, 10.0).child(icon(IconName::GitFork, 18.0, t.info));
            let header = PanelHeader::new(SharedString::from(format!("side-chat-header-{id}")), view.title.clone())
                .icon(mark)
                .subtitle(format!("Temporary · forked from {}", source.map(|source| source.title.clone()).unwrap_or_default()))
                .close_label("Close side chat")
                .on_close(move |_, _, cx| send(json!({ "type": "side-chat.close", "chatId": close }), cx))
                .into_any_element();
            let empty = if available {
                (IconName::GitFork, "Work from a copy of this thread", "This conversation starts from the main thread's context, then continues on its own branch.")
            } else {
                (IconName::GitFork, "Main context unavailable", "Send a message in the main thread to enable this chat.")
            };
            (header, empty, !available, chat.error.clone(), None)
        }
        None => {
            let busy: HashSet<String> = state.active_runs.keys().cloned().collect();
            let blocked: HashSet<String> = state.active_runs.iter().filter(|(_, run)| run.phase == act_core::state::RunPhase::AwaitingApproval).map(|(id, _)| id.clone()).collect();
            let tab = thread_tab_view(state, &id, &busy, &blocked);
            let (standing, summary) = tab.as_ref().map(|tab| (tab.standing, tab.summary.clone())).unwrap_or((CoordinatedThreadStatus::Idle, None));
            let lead = state.current_thread().map(|lead| lead.title.clone()).unwrap_or_default();
            let line = match &summary {
                Some(summary) => format!("{} · {summary}", status_label(standing)),
                None => status_label(standing).to_string(),
            };
            let asking = matches!(standing, CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval);
            let status = if standing == CoordinatedThreadStatus::Working {
                div().absolute().right(px(-5.5)).bottom(px(-5.5)).p(px(2.5)).rounded_full().bg(t.sunken).child(super::spinner::bead(SharedString::from(format!("thread-tab-spin-{id}")), 11.0, &t, false)).into_any_element()
            } else {
                let dot = div().size(px(9.0)).rounded_full();
                let dot = match standing {
                    CoordinatedThreadStatus::Review => dot.border(px(1.6)).border_color(t.success),
                    CoordinatedThreadStatus::NeedsWork => dot.border(px(1.6)).border_color(t.danger),
                    other => dot.bg(status_dot(other, &t)),
                };
                div().absolute().right(px(-5.5)).bottom(px(-5.5)).p(px(2.5)).rounded_full().bg(t.sunken).child(dot).into_any_element()
            };
            let mark = rimmed(div().relative().flex_none().size(px(32.0)).flex().items_center().justify_center().rounded(px(10.0)).border_1().border_color(if asking { t.accent_line } else { t.line }).bg(if asking { t.accent_surface } else { t.surface }), &t, 10.0)
                .child(icon(engine_icon(thread.engine), 16.0, t.ink_dim))
                .child(status);
            let line_color = match standing {
                CoordinatedThreadStatus::Review => Some(t.success_soft),
                CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => Some(t.accent_text),
                CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => Some(t.danger_soft),
                _ => None,
            };
            let close = id.clone();
            let resolve = id.clone();
            let thread_title = tab.as_ref().map(|tab| tab.conversation.thread.title.clone()).unwrap_or_default();
            let action = if standing == CoordinatedThreadStatus::Resolved {
                Some(Button::bare("thread-tab-reopen").name(format!("Reopen {thread_title}")).variant(ButtonVariant::Soft).child(icon(IconName::Undo2, 14.0, t.muted)).child("Reopen").on_click(move |_, _, cx| dispatch(AppCommand::TaskReopen { task_id: resolve.clone() }, cx)).into_any_element())
            } else if can_resolve(standing) {
                Some(Button::bare("thread-tab-resolve").name(format!("Resolve {thread_title}")).variant(ButtonVariant::Soft).group("thread-tab-resolve").child(div().text_color(t.success).group_hover("thread-tab-resolve", |style| style.text_color(t.success_soft)).child(ink_icon(IconName::Check, 14.0))).child("Resolve").on_click(move |_, _, cx| dispatch(AppCommand::TaskResolve { task_id: resolve.clone() }, cx)).into_any_element())
            } else {
                None
            };
            let mut header = PanelHeader::new(SharedString::from(format!("thread-tab-header-{id}")), view.title.clone())
                .icon(mark)
                .subtitle(line)
                .borderless()
                .close_label(format!("Close {} and return to {lead}", thread.title))
                .close_tip(format!("Close · back to {lead}"))
                .on_close(move |_, _, cx| dispatch(AppCommand::CloseThreadTab { task_id: close.clone() }, cx));
            if let Some(action) = action {
                header = header.action(action);
            }
            if let Some(color) = line_color {
                header = header.subtitle_color(color);
            }
            let strip = tab.as_ref().map(|tab| thread_tab_strip(root, &id, tab, cx));
            (header.into_any_element(), (IconName::Waypoints, "Nothing here yet", "This thread has not started talking."), false, None, strip)
        }
    };

    let transcript = transcript(root, &view, empty, window, cx);
    let composer = composer(root, state, &view, disabled, side.is_some(), window, cx);
    let dropping = cx.has_active_drag() && root.dock.chats.composers.get(&id).is_some_and(|composer| composer.dropping);
    let (over_id, drop_id) = (id.clone(), id.clone());
    let m = theme.metrics.clone();
    div()
        .id(SharedString::from(format!("side-chat-{id}")))
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(t.dock_sunken)
        .child(div().flex_none().child(header))
        .children(strip)
        .child(transcript)
        .children(error.map(|error| div().mx(px(14.0)).mt(px(8.0)).child(Notice::new("side-chat-error", error).tone(NoticeTone::Danger))))
        .child(composer)
        // Files dragged in from the desktop attach to this chat's own composer.
        .on_drag_move::<gpui::ExternalPaths>(cx.listener(move |root, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
            let inside = event.bounds.contains(&event.event.position);
            if let Some(composer) = root.dock.chats.composers.get_mut(&over_id) {
                if composer.dropping != inside {
                    composer.dropping = inside;
                    cx.notify();
                }
            }
        }))
        .on_drop(cx.listener(move |root, paths: &gpui::ExternalPaths, _, cx| {
            root.in_composer(Some(&drop_id), |root| root.drop_files(paths, cx));
        }))
        .when(dropping, |chat| {
            chat.child(div().absolute().top(px(6.0)).left(px(6.0)).right(px(6.0)).bottom(px(6.0)).border_2().border_dashed().border_color(t.line_bright)).child(
                div().absolute().bottom(px(18.0)).left_0().right_0().flex().justify_center().child(
                    act_ui::style::glass(div(), &t, act_ui::style::GlassKind::Capsule, None, None).px(px(14.0)).py(px(6.0)).text_color(t.ink_soft).text_size(px(m.text_meta)).child("Drop to attach"),
                ),
            )
        })
        .into_any_element()
}

/// A web link's click: the browser, or with ⌘ held, the dock's own browser (WebLink in MarkdownMessage.tsx).
pub(super) fn web_link(url: String) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
    move |event, _, cx| {
        if event.modifiers().secondary() {
            send(json!({ "type": "browser.open", "url": url, "newTab": true }), cx);
        } else {
            dispatch(AppCommand::LinkOpen { url: url.clone() }, cx);
        }
    }
}

/// A web link's context menu: open it either way, or copy it.
pub(super) fn web_link_menu(url: String) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
    move |event, window, cx| {
        use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
        let Some(root) = window.root::<Root>().flatten() else { return };
        let (browser, app, copy) = (url.clone(), url.clone(), url.clone());
        let entries: Vec<MenuEntry> = vec![
            MenuItem::new("Open in browser").on_select(move |_, cx| dispatch(AppCommand::LinkOpen { url: browser.clone() }, cx)).into(),
            MenuItem::new(concat!("Open in ", act_core::app_name!())).shortcut(if cfg!(target_os = "macos") { "⌘Click" } else { "Ctrl+Click" }).on_select(move |_, cx| send(json!({ "type": "browser.open", "url": app, "newTab": true }), cx)).into(),
            MenuEntry::Separator,
            MenuItem::new("Copy link").on_select(move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))).into(),
        ];
        let menu = Menu::build(entries, act_ui::overlay::Placement::At { position: event.position }, window, cx, |menu| menu.top_level());
        root.update(cx, |root, cx| root.menus.show("web-link", menu, cx));
        cx.stop_propagation();
    }
}

/// What the schedule chip says: when the next run is, or why there is none.
pub fn schedule_chip_label(automation: Option<&act_core::domain::automation::AutomationView>, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(view) = automation else { return "Schedule".into() };
    if view.automation.paused {
        return "Paused".into();
    }
    let Some(next) = view.next_run_at else { return "Missed".into() };
    let at = Local.timestamp_millis_opt(next).single().unwrap_or_else(Local::now);
    let today = Local.timestamp_millis_opt(now).single().unwrap_or_else(Local::now);
    if at.date_naive() == today.date_naive() {
        return format!("Next {}", at.format("%-I:%M %p"));
    }
    if next - now < 6 * 86_400_000 {
        format!("Next {}", at.format("%a %-I:%M %p"))
    } else {
        format!("Next {}", at.format("%b %-d"))
    }
}

/// What stands in for the checkout's chips until Git has answered: the text, whether Git refused,
/// and whether it is still reading.
fn environment_note(tab: &ThreadTabView) -> Option<(String, bool, bool)> {
    if tab.folder.is_empty() {
        return Some(("No project".into(), false, false));
    }
    if tab.workspace_id.is_none() {
        return Some(("Reopen the project to inspect Git".into(), false, false));
    }
    match tab.environment {
        None => Some(("Reading Git…".into(), false, true)),
        Some(ChangedFilesResult::Error { message }) => Some((message.clone(), true, false)),
        Some(ChangedFilesResult::Unknown { .. }) => Some(("Workspace is no longer registered".into(), false, false)),
        Some(ChangedFilesResult::Unavailable { reason }) => Some((format!("Workspace is {}", serde_json::to_value(reason).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default()), false, false)),
        Some(ChangedFilesResult::Available { .. }) => None,
    }
}

/// The thread's checkout, pull request and schedule as one line of chips under its header, each doing
/// what the same row in its session panel does, with its brief folded open beneath (ThreadTabStrip.tsx).
fn thread_tab_strip(root: &mut Root, id: &str, tab: &ThreadTabView, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let probe = root.dock.chats.branch_probe.clone();
    let brief_open = root.dock.chats.briefs.contains(id);
    let glyph = |name: IconName, color: gpui::Rgba| icon(name, 14.0, color);
    let mut row = div().flex_none().min_w_0().flex().flex_wrap().items_center().gap(px(6.0)).pl(px(20.0)).pr(px(17.0)).pb(px(12.0)).border_b_1().border_color(t.line);
    if let Some(ChangedFilesResult::Available { additions, deletions, branch, baseline, files }) = tab.environment {
        let branch = branch.clone().unwrap_or_else(|| "—".into());
        let (workspace_id, current, task) = (tab.workspace_id.clone(), branch.clone(), id.to_string());
        let anchor = probe.clone();
        let open = root.menus.is_open(&format!("branch:{}", tab.workspace_id.clone().unwrap_or_default()));
        row = row.child(
            Button::bare(SharedString::from(format!("thread-tab-branch-{id}")))
                .child(glyph(IconName::GitBranch, t.muted))
                .child(div().min_w_0().max_w(px(160.0)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t.ink_dim).font_family(theme.typography.mono_family.clone()).text_size(px(m.text_meta)).child(branch.clone()))
                .child(glyph(IconName::ChevronDown, t.quiet).size(px(12.0)))
                .child(bounds_probe(&probe))
                .tip(branch.clone())
                .name(format!("Branch {branch}"))
                .pressed(open)
                .on_click(cx.listener(move |root, _, window, cx| {
                    let Some(workspace_id) = workspace_id.clone() else { return };
                    let bounds = anchor.get().unwrap_or_default();
                    root.open_branch_menu(&workspace_id, bounds, Some(current.clone()), Some(task.clone()), window, cx);
                })),
        );
        let clean = files.is_empty();
        let changes_id = id.to_string();
        let mut changes = Button::bare(SharedString::from(format!("thread-tab-changes-{id}")))
            .variant(if clean { ButtonVariant::Ghost } else { ButtonVariant::Secondary })
            .child(glyph(IconName::FileDiff, if clean { t.quiet_strong } else { t.muted }))
            .tip(baseline.as_ref().map(|baseline| format!("Changes since {baseline}")).unwrap_or_else(|| "Uncommitted changes".into()))
            .name(if clean { "Review changes, none".to_string() } else { format!("Review changes, +{additions} −{deletions}") })
            .on_click(move |_, _, cx| dispatch(AppCommand::DiffToggle { task_id: Some(changes_id.clone()) }, cx));
        changes = if clean {
            changes.child("No changes")
        } else {
            changes.child(div().flex().gap(px(6.0)).child(div().text_color(t.success).font_weight(FontWeight(600.0)).child(format!("+{additions}"))).child(div().text_color(t.danger).font_weight(FontWeight(600.0)).child(format!("−{deletions}"))))
        };
        row = row.child(changes);
    }
    if let Some((text, failed, reading)) = environment_note(tab) {
        row = row.child(if failed {
            rimmed(div().min_w_0().flex().items_center().gap(px(6.0)).h(px(28.0)).pl(px(9.0)).pr(px(2.0)).rounded(px(8.0)).border_1().border_color(t.danger_line).bg(t.danger_surface).text_color(t.danger_soft).text_size(px(m.text_meta)), &t, 8.0)
                .child(div().id(SharedString::from(format!("thread-tab-note-{id}"))).min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(text.clone()).tip(text))
                .child(Button::new(SharedString::from(format!("thread-tab-retry-{id}")), "Retry").variant(ButtonVariant::Ghost).size(ButtonSize::Sm).on_click(|_, _, cx| dispatch(AppCommand::RefreshEnvironment, cx)))
                .into_any_element()
        } else if reading {
            div().min_w_0().text_color(t.quiet).text_size(px(m.text_meta)).child(super::sweep::sweep(SharedString::from(format!("thread-tab-reading-{id}")), text, cx)).into_any_element()
        } else {
            div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t.quiet).text_size(px(m.text_meta)).child(text).into_any_element()
        });
    }
    match &tab.pull_request {
        PullRequestAnswer::GhMissing => {
            row = row.child(
                div().id(SharedString::from(format!("thread-tab-gh-link-{id}"))).on_mouse_down(gpui::MouseButton::Right, web_link_menu("https://cli.github.com".into())).child(Button::bare(SharedString::from(format!("thread-tab-gh-{id}")))
                    .variant(ButtonVariant::Ghost)
                    .child(glyph(IconName::GitPullRequest, t.quiet_strong))
                    .child("Install gh")
                    .tip("Pull requests are read with the GitHub CLI, which is not installed")
                    .on_click(web_link("https://cli.github.com".into()))),
            );
        }
        PullRequestAnswer::Found { pull_request } => {
            let url = pull_request.url.clone();
            let state_label = match pull_request.state {
                PullRequestState::Draft => "Draft",
                PullRequestState::Open => "Open",
                PullRequestState::Merged => "Merged",
                PullRequestState::Closed => "Closed",
            };
            let color = match pull_request.state {
                PullRequestState::Draft => t.muted,
                _ => super::coordination::pull_request_color(pull_request.state, cx),
            };
            row = row.child(
                div().id(SharedString::from(format!("thread-tab-pull-request-link-{id}"))).on_mouse_down(gpui::MouseButton::Right, web_link_menu(url.clone())).child(Button::bare(SharedString::from(format!("thread-tab-pull-request-{id}")))
                    .child(glyph(super::coordination::pull_request_icon(pull_request.state), color))
                    .child(format!("#{} · {state_label}", pull_request.number))
                    .tip(pull_request.title.clone())
                    .on_click(web_link(url))),
            );
        }
        PullRequestAnswer::None => {}
    }
    let scheduled = tab.automation.is_some_and(|view| !view.automation.paused);
    let schedule_id = id.to_string();
    row = row.child(
        Button::bare(SharedString::from(format!("thread-tab-schedule-{id}")))
            .child(glyph(IconName::AlarmClock, if scheduled { t.info } else { t.muted }))
            .child(schedule_chip_label(tab.automation, act_core::util::now()))
            .tip("Automation")
            .on_click(move |_, _, cx| send(json!({ "type": "view.open-dock-panel", "panel": AUTOMATION_PANEL, "taskId": schedule_id }), cx)),
    );
    let brief = tab.conversation.thread.brief.clone();
    if brief.is_some() {
        let toggle = id.to_string();
        row = row.child(div().flex_1().min_w_0()).child(
            div().child(
                Button::bare(SharedString::from(format!("thread-tab-brief-{id}")))
                    .child(glyph(IconName::ScrollText, t.muted))
                    .child("Brief")
                    .child(super::common::turning("chevron", if brief_open { 180.0 } else { 0.0 }, 140).child(glyph(IconName::ChevronDown, t.quiet).size(px(12.0))))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        if !root.dock.chats.briefs.remove(&toggle) {
                            root.dock.chats.briefs.insert(toggle.clone());
                        }
                        cx.notify();
                    })),
            ),
        );
    }
    let mut strip = div().flex_none().flex().flex_col().child(row);
    if let Some(brief) = brief.filter(|_| brief_open) {
        let term = |text: &'static str| div().text_color(t.quiet).text_size(px(11.0)).font_weight(FontWeight(600.0)).child(text.to_uppercase());
        let value = |text: String| div().mt(px(4.0)).text_color(t.ink_dim).text_size(px(m.text_meta)).line_height(gpui::relative(1.45)).child(text);
        strip = strip.child(
            div()
                .id(SharedString::from(format!("thread-tab-brief-card-{id}")))
                .when(!t.reduced_motion, |card| card.enter(gpui::Transition::ease_out(140), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(-4.0)))))
                .flex_none()
                .max_h(gpui::relative(0.4))
                .overflow_y_scroll()
                .border_b_1()
                .border_color(t.line)
                .bg(t.surface)
                .pt(px(14.0))
                .px(px(20.0))
                .pb(px(16.0))
                .child(div().border_l_2().border_color(t.accent_line).pl(px(12.0)).text_color(t.ink_soft).text_size(px(m.text_menu)).line_height(gpui::relative(1.5)).child(brief.intent.clone()))
                .child(
                    div()
                        .mt(px(14.0))
                        .flex()
                        .gap(px(16.0))
                        .child(div().flex_1().min_w_0().child(term("Done when")).child(value(brief.done_when.clone())))
                        .child(div().flex_none().child(term("Delivers")).child(value(brief.delivers.label().to_string()))),
                ),
        );
    }
    strip.into_any_element()
}

fn signature(group: &Group, tail: Option<&str>, folds: &Folds, hit: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    group.id().hash(&mut hasher);
    hit.hash(&mut hasher);
    match group {
        Group::Entry(Entry::Message { message, .. }) => message.text.len().hash(&mut hasher),
        Group::Entry(Entry::Turn { steps, final_message, ends_at, live, .. }) => {
            steps.len().hash(&mut hasher);
            final_message.as_ref().map(|message| message.text.len()).hash(&mut hasher);
            ends_at.hash(&mut hasher);
            live.hash(&mut hasher);
            if *live {
                tail.hash(&mut hasher);
            }
        }
        Group::Entry(Entry::Threads { thread_ids, .. }) => thread_ids.hash(&mut hasher),
        Group::Updates { count, .. } => count.hash(&mut hasher),
    }
    folds.opened_in(group.id(), &super::grouping::group_message_ids(group)).hash(&mut hasher);
    hasher.finish()
}

fn transcript(root: &mut Root, view: &DockConversationView, empty: Empty, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let thread = view.thread;
    let tail = view.streaming_tail.cloned();
    if thread.history_summary.is_some() {
        return div().flex_1().min_h_0().flex().items_center().justify_center().text_color(t.muted).child("Loading conversation…").into_any_element();
    }
    if thread.messages.is_empty() && tail.is_none() {
        return div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .pt(px(30.0))
            .px(px(22.0))
            .pb(px(34.0 + f32::from(window.viewport_size().height) * 0.07))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(18.0))
                    .child(icon(empty.0, 32.0, t.info_soft))
                    .child(div().mt(px(15.0)).text_color(t.ink_soft).text_size(px(theme.metrics.text_heading)).font_weight(FontWeight(560.0)).line_height(gpui::relative(1.25)).map(|title| act_ui::style::letter_spacing(title, -0.025)).child(empty.1))
                    .child(div().mt(px(8.0)).max_w(px(310.0)).text_center().text_color(t.quiet_strong).text_size(px(theme.metrics.text_content)).line_height(gpui::relative(1.55)).child(empty.2)),
            )
            .into_any_element();
    }
    let running = view.status == ThreadRunStatus::Running;
    let groups = group_timeline(&thread.messages, &Options { running, tail_message_id: tail.as_ref().map(|tail| tail.message_id.as_str()), run_ended_at: thread.run_ended_at, coordinator: is_coordinator(Some(thread)), starts: &[] });
    // The find bar, when this chat's own thread is the one it searches.
    let find = act_core::view::find::find_view(&root.state(cx)).filter(|view| matches!(&view.find.target, act_core::domain::find::FindTarget::Thread { task_id } if task_id.as_deref() == Some(thread.id.as_str()))).map(|view| timeline_row::FindPaint { needle: view.find.query.trim().to_lowercase(), hit: view.hit });
    let rows = super::grouping::message_rows(&groups);
    let hit_row = find.as_ref().and_then(|find| find.hit.as_ref()).and_then(|hit| rows.get(&hit.message_id)).copied();
    // What the run is doing, set where the reply will begin.
    let now = act_core::util::now();
    let live_since = match groups.last() {
        Some(Group::Entry(Entry::Turn { live: true, steps, .. })) => steps.first().map(|step| step.at),
        _ => None,
    };
    let mut statuses: Vec<super::conversation::Status> = Vec::new();
    if running {
        if view.compacting {
            statuses.push(super::conversation::Status::Compacting);
        } else if let Some(retry) = view.retrying {
            statuses.push(super::conversation::Status::Retrying(super::conversation::retry_label(retry)));
        } else {
            statuses.push(super::conversation::Status::Thinking { since: now, blocked: view.approval.is_some() });
        }
    }
    let chat = root.dock.chats.chat(&thread.id);
    let status_key = statuses.iter().map(|status| status.key()).collect::<Vec<_>>().join("|");
    let since = match &chat.shown_since {
        Some((key, at)) if *key == status_key => *at,
        _ => {
            chat.shown_since = Some((status_key, now));
            now
        }
    };
    for status in &mut statuses {
        if let super::conversation::Status::Thinking { since: at, .. } = status {
            *at = live_since.unwrap_or(since).min(since);
        }
    }
    let folds = chat.folds.clone();
    let mut signatures: Vec<u64> = groups.iter().enumerate().map(|(index, group)| signature(group, tail.as_ref().map(|tail| tail.text.as_str()), &folds.borrow(), hit_row == Some(index))).collect();
    for status in &statuses {
        let mut hasher = DefaultHasher::new();
        status.key().hash(&mut hasher);
        signatures.push(hasher.finish());
    }
    let estimates: Vec<f32> = groups.iter().map(Group::estimate).chain(statuses.iter().map(|_| super::conversation::STATUS_ESTIMATE)).collect();
    super::conversation::sync_rows(&chat.list, &chat.signatures, &signatures, &estimates);
    chat.signatures = signatures;
    // Reading a match takes the view over, as scrolling by hand does, and brings its row to the middle.
    match find.as_ref().and_then(|find| find.hit.clone()).zip(hit_row) {
        Some((hit, row)) => {
            if chat.found.as_ref().is_none_or(|(found, _)| found != &hit) {
                chat.follow.set(false);
                chat.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.0) });
                chat.found = Some((hit, 3));
            } else if let Some((hit, tries)) = chat.found.clone().filter(|(_, tries)| *tries > 0) {
                chat.found = Some((hit, tries - 1));
                let view_box = chat.list.viewport_bounds();
                if let Some(bounds) = chat.list.bounds_for_item(row) {
                    let delta = bounds.origin.y + bounds.size.height.min(view_box.size.height) / 2.0 - view_box.center().y;
                    if f32::from(delta).abs() > 2.0 {
                        chat.list.scroll_by(delta);
                    }
                }
                cx.notify();
            }
        }
        None => chat.found = None,
    }
    if chat.follow.get() {
        chat.list.scroll_to(ListOffset { item_ix: chat.signatures.len(), offset_in_item: px(0.0) });
    }
    let gutter = f32::from(act_ui::components::scrollbar::list_gutter(&chat.list));
    if gutter != chat.gutter {
        chat.gutter = gutter;
        chat.list.remeasure();
    }
    let list_state = chat.list.clone();
    let selection = chat.selection.clone();
    selection.begin_frame();
    let names: HashMap<String, String> = root.state(cx).threads.iter().map(|thread| (thread.id.clone(), thread.title.clone())).collect();
    let model = Rc::new(RowModel {
        engine: thread.engine,
        task_id: thread.id.clone(),
        groups: Rc::new(groups),
        tail,
        folds,
        root: cx.entity().downgrade(),
        inset: 22.0,
        side: gutter,
        now: act_core::util::now(),
        find,
        annotations: Vec::new(),
        cards: None,
        names: Rc::new(names),
        drawn: Rc::default(),
        docked: true,
        selection: Some((selection, Rc::new(super::timeline_row::reading_order(&thread.messages)))),
        folder: act_core::workspace::location::thread_workspace_root(&root.state(cx), Some(thread)),
        width: f32::from(list_state.viewport_bounds().size.width),
    });
    let count = model.groups.len();
    let statuses = Rc::new(statuses);
    let approval = view.approval.cloned();
    let mut area = div().relative().flex_1().min_h_0().child(
        list(list_state.clone(), move |index, window, cx| match statuses.get(index.wrapping_sub(count)) {
            _ if index < count => timeline_row::render(&model, index, index + 1 == count, window, cx),
            Some(status) => status.render(&model, cx),
            None => div().into_any_element(),
        })
        .size_full()
        .pt(px(30.0))
        .pb(px(34.0)),
    );
    area = area.child(act_ui::components::scrollbar::Scrollbar::list(SharedString::from(format!("chat-scrollbar-{}", thread.id)), &list_state));
    if let Some(approval) = approval {
        let probe = root.dock.chats.approval_probe.clone();
        let state = root.state(cx);
        area = area.child(div().absolute().left(px(22.0)).right(px(22.0)).bottom(px(12.0)).flex().justify_center().child(super::conversation::approval_card(&approval, &probe, &state, cx)));
    }
    area.into_any_element()
}

/// The main composer, writing to this chat's own thread.
fn composer(root: &mut Root, state: &WorkspaceState, view: &DockConversationView, disabled: bool, side: bool, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let id = view.thread.id.clone();
    if !root.dock.chats.composers.contains_key(&id) {
        let composer = ComposerState::scoped(Some(id.clone()), window, cx);
        root.dock.chats.composers.insert(id.clone(), composer);
    }
    if let Some(composer) = root.dock.chats.composers.get(&id) {
        composer.input.update(cx, |input, _| input.set_label(if side { "Side chat prompt" } else { "Thread prompt" }));
    }
    let placeholder = match (side, disabled) {
        (true, true) => "Main context required",
        (true, false) => "Ask a side question",
        (false, _) => "Reply to this thread",
    };
    root.in_composer(Some(&id), |root| {
        root.composer.side = side;
        root.composer.held = disabled;
        root.composer.sync_draft(state, cx);
        root.composer.sync_field(placeholder.to_string(), disabled, cx);
        let panel = super::composer::panel(root, state, None, window, cx);
        div().flex_none().pt(px(10.0)).px(px(14.0)).pb(px(if side { 0.0 } else { 14.0 })).child(panel).into_any_element()
    })
    .unwrap_or_else(|| div().into_any_element())
}
