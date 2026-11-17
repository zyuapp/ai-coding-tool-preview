//! A coordinator's Overview tab: its purpose, what its threads run on, its threads grouped by what
//! they need from the user, its memory, and what it all spent (Coordination.tsx, coordination.css).

use std::collections::{HashMap, HashSet};

use act_core::domain::coordination::{is_coordinator, MemoryNote, OverviewSection, MAX_MEMORY_NOTE, MAX_MEMORY_NOTES, MAX_PURPOSE};
use act_core::domain::engine::{efforts_for, engine_has_model, model_label};
use act_core::state::{RunPhase, WorkspaceState};
use act_core::view::coordination::{coordination_view, overview_groups, CoordinatedThreadView, CoordinationUsage};
use act_core::workspace::coordinated_status::{can_resolve, CoordinatedThreadStatus};
use act_core::workspace::coordination::stoppable_coordinators;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::list::{ListHeading, ListRow};
use act_ui::components::meter::Meter;
use act_ui::input::{Autosize, InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::PaneFill;
use act_ui::theme::{ActiveTheme, Tokens, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, Entity, FocusHandle, FontWeight, SharedString, Subscription, Window};

use super::common::{engine_icon, icon, ink_icon, rimmed, Ellipsis};
use super::model_library::LibraryTarget;
use super::root::Root;
use crate::workspace::dispatch;

/// How many threads a group shows before it asks to show the rest.
const OVERVIEW_LIMIT: usize = 5;

/// What is being written in place: the purpose, a note, or a new note.
#[derive(Clone, PartialEq)]
enum Editing {
    Purpose,
    Note(String),
}

impl Editing {
    /// The key of the button that opens this edit.
    fn key(&self) -> String {
        match self {
            Editing::Purpose => "edit-purpose".into(),
            Editing::Note(id) => format!("edit-note-{id}"),
        }
    }
}

/// What the keyboard last pressed in the Overview that takes itself off the page: a row's Resolve or
/// Reopen, or a Show more.
#[derive(Clone)]
enum Place {
    Row { thread_id: String, section: String, index: usize },
    More { section: String, index: usize },
}

#[derive(Default)]
pub struct OverviewState {
    editing: Option<(Editing, Entity<TextInput>, Subscription)>,
    add: Option<(Entity<TextInput>, Subscription)>,
    pub(super) model_probe: Option<BoundsCell>,
    /// Set each frame the model chip is drawn.
    pub(super) model_drawn: bool,
    focus_edit: bool,
    /// The edited text's button, which takes focus back once Enter or Escape ends the edit.
    refocus: Option<String>,
    /// Each row's, Resolve's, Show more's and group head's keyboard stop, by what it is for.
    keys: HashMap<String, FocusHandle>,
    /// The press whose control may go, and the key of that control.
    held: Option<(Place, String)>,
}

impl OverviewState {
    fn key(&mut self, key: &str, cx: &mut Context<Root>) -> FocusHandle {
        self.keys.entry(key.to_string()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone()
    }
}

/// What one frame drew of the Overview's stops, so a press whose control went can find its place.
#[derive(Default)]
struct Drawn {
    keys: HashSet<String>,
    rows: HashMap<String, Vec<String>>,
    heads: Vec<(String, String)>,
}

fn group_label(group: OverviewSection) -> &'static str {
    match group {
        OverviewSection::Needs => "Needs you",
        OverviewSection::Review => "Ready for review",
        OverviewSection::Working => "Working",
        OverviewSection::Done => "Done",
        OverviewSection::Resolved => "Resolved",
        OverviewSection::Memory => "Memory",
        OverviewSection::Usage => "Usage",
    }
}

fn busy_sets(state: &WorkspaceState) -> (HashSet<String>, HashSet<String>) {
    let busy = state.active_runs.keys().cloned().collect();
    let blocked = state.active_runs.iter().filter(|(_, run)| run.phase == RunPhase::AwaitingApproval).map(|(id, _)| id.clone()).collect();
    (busy, blocked)
}

/// What waits on the user under the coordinator in front: threads that need them, and suggestions
/// and requests from elsewhere, counted as the sidebar counts them.
pub fn needs_you(state: &WorkspaceState) -> usize {
    let lead = state.current_thread();
    if !is_coordinator(lead) {
        return 0;
    }
    let (busy, blocked) = busy_sets(state);
    let view = coordination_view(state, lead, &busy, &blocked);
    let needs = overview_groups(&view.members).into_iter().find(|(group, _)| *group == OverviewSection::Needs).map(|(_, members)| members.len()).unwrap_or(0);
    needs + view.waiting.map(|waiting| waiting.suggestions + waiting.requests.len()).unwrap_or(0)
}

fn dot(status: CoordinatedThreadStatus, t: &Tokens) -> impl IntoElement {
    let base = div().size(px(8.0)).rounded_full();
    match status {
        CoordinatedThreadStatus::Done => base.bg(t.success),
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed => base.bg(t.danger),
        CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => base.bg(t.accent),
        CoordinatedThreadStatus::Review => base.border_2().border_color(t.success),
        CoordinatedThreadStatus::NeedsWork => base.border_2().border_color(t.danger),
        CoordinatedThreadStatus::Resolved => base.bg(t.quiet),
        CoordinatedThreadStatus::Working => base.bg(t.info),
        CoordinatedThreadStatus::Idle => base.bg(t.line_bright),
    }
}

fn compact(tokens: i64) -> String {
    super::composer_meters::compact_tokens(tokens)
}

impl Root {
    fn start_edit(&mut self, editing: Editing, value: &str, cx: &mut Context<Root>) {
        let multiline = matches!(editing, Editing::Note(_));
        let input = cx.new(|cx| {
            let input = TextInput::new(cx).with_text(value).label(if multiline { "Note" } else { "Purpose" }).placeholder(if multiline { "" } else { "Add a purpose" });
            if multiline {
                input.multiline(Autosize { min_rows: 1, max_height: None }).submit_on_enter(true)
            } else {
                input
            }
        });
        let kind = editing.clone();
        let opened = value.to_string();
        let subscription = cx.subscribe(&input, move |root, input, event: &InputEvent, cx| match event {
            InputEvent::Submit | InputEvent::Blur => {
                let text = input.read(cx).text().trim().to_string();
                let submit = matches!(event, InputEvent::Submit);
                // A note cannot be saved empty: Enter waits for words, leaving puts it back.
                if submit && text.is_empty() && matches!(kind, Editing::Note(_)) {
                    return;
                }
                if submit && root.dock.overview.editing.as_ref().is_some_and(|(editing, _, _)| *editing == kind) {
                    root.dock.overview.refocus = Some(kind.key());
                }
                root.finish_edit(&kind, &opened, Some(text), cx);
            }
            _ => {}
        });
        self.dock.overview.editing = Some((editing, input, subscription));
        self.dock.overview.focus_edit = true;
        cx.notify();
    }

    /// Saves what was typed, unless it is unchanged; None puts the text back.
    fn finish_edit(&mut self, kind: &Editing, opened: &str, text: Option<String>, cx: &mut Context<Root>) {
        if self.dock.overview.editing.as_ref().is_none_or(|(editing, _, _)| editing != kind) {
            return;
        }
        self.dock.overview.editing = None;
        let Some(lead) = self.state(cx).current_id.clone() else { return };
        if let Some(text) = text.filter(|text| text != opened) {
            match kind {
                Editing::Purpose => dispatch(AppCommand::MemorySetPurpose { task_id: lead, purpose: (!text.is_empty()).then_some(text.chars().take(MAX_PURPOSE).collect()) }, cx),
                Editing::Note(note_id) if !text.is_empty() => dispatch(AppCommand::MemoryEdit { task_id: lead, note_id: note_id.clone(), text: text.chars().take(MAX_MEMORY_NOTE).collect() }, cx),
                Editing::Note(_) => {}
            }
        }
        cx.notify();
    }

    fn note_adder(&mut self, cx: &mut Context<Root>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.dock.overview.add {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::new(cx).multiline(Autosize { min_rows: 1, max_height: None }).submit_on_enter(true).label("Add a note").placeholder("Add a note"));
        let subscription = cx.subscribe(&input, |root, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                let text = input.read(cx).text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                if let Some(lead) = root.state(cx).current_id.clone() {
                    dispatch(AppCommand::MemoryAdd { task_id: lead, text: text.chars().take(MAX_MEMORY_NOTE).collect() }, cx);
                    input.update(cx, |input, cx| input.reset("", cx));
                }
            }
        });
        self.dock.overview.add = Some((input.clone(), subscription));
        input
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let Some(lead) = state.current_thread().filter(|thread| is_coordinator(Some(thread))) else { return div().size_full().into_any_element() };
    let (busy, blocked) = busy_sets(state);
    let view = coordination_view(state, Some(lead), &busy, &blocked);
    let folds = state.overview_folds.get(&lead.id).cloned().unwrap_or_default();
    let lead_id = lead.id.clone();
    if root.dock.overview.editing.is_none() {
        if let Some(key) = root.dock.overview.refocus.take() {
            window.focus(&root.dock.overview.key(&key, cx), cx);
        }
    }
    if let Some((_, input, _)) = &root.dock.overview.editing {
        if root.dock.overview.focus_edit {
            root.dock.overview.focus_edit = false;
            let input = input.clone();
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.move_to_end(cx);
            });
        }
    }

    // The purpose and what the threads run on, on one pane at the head.
    let purpose = view.memory.as_ref().and_then(|memory| memory.purpose).map(String::from);
    let editing_purpose = root.dock.overview.editing.as_ref().filter(|(editing, _, _)| *editing == Editing::Purpose).map(|(_, input, _)| input.clone());
    let purpose_line = match editing_purpose {
        Some(input) => div()
            .min_h(px(32.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(t.line_strong)
            .bg(t.canvas)
            .text_color(t.ink)
            .text_size(px(m.text_control))
            .font_weight(FontWeight(600.0))
            .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    root.dock.overview.refocus = Some(Editing::Purpose.key());
                    root.finish_edit(&Editing::Purpose, "", None, cx);
                }
            }))
            .child(input)
            .into_any_element(),
        None => {
            let value = purpose.clone().unwrap_or_default();
            let group: SharedString = "coordination-purpose".into();
            let name = purpose.as_ref().map(|purpose| format!("Edit purpose: {purpose}")).unwrap_or_else(|| "Add a purpose".into());
            let focus = root.dock.overview.key(&Editing::Purpose.key(), cx);
            div()
                .id("coordination-purpose")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, name))
                .track_focus(&focus)
                .group(group.clone())
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_h(px(32.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .cursor_text()
                .hover(|style| style.bg(t.surface_hover))
                .text_size(px(m.text_control))
                .child(match &purpose {
                    Some(purpose) => div().flex_1().min_w_0().ellipsis().text_color(t.ink).font_weight(FontWeight(600.0)).child(purpose.clone()),
                    None => div().flex_1().text_color(t.quiet).font_weight(FontWeight(450.0)).child("Add a purpose"),
                })
                .child(div().flex_none().ml_auto().text_color(t.quiet_strong).group_hover(group, |style| style.text_color(t.ink_dim)).child(ink_icon(IconName::Pencil, 12.0)))
                .when_some(purpose.clone(), |line, purpose| line.tip(purpose))
                .on_click(cx.listener(move |root, _, _, cx| root.start_edit(Editing::Purpose, &value, cx)))
                .into_any_element()
        }
    };
    let catalog = state.catalog();
    let model_header = view.thread_model.as_ref().map(|choice| {
        let engine = choice.engine.filter(|engine| engine_has_model(&catalog, *engine, &choice.model));
        let probe = root.dock.overview.model_probe.get_or_insert_with(bounds_cell).clone();
        let label = model_label(&catalog, engine, &choice.model);
        let effort = choice.effort.and_then(|effort| engine.and_then(|engine| efforts_for(&catalog, engine, &choice.model, None).into_iter().find(|spec| spec.id == effort)).map(|spec| spec.label));
        let mut row = div().flex().flex_wrap().items_center().gap_x(px(4.0)).gap_y(px(2.0)).pl(px(8.0)).child(div().text_color(t.muted).text_size(px(m.text_control)).whitespace_nowrap().child("Threads run on"));
        match engine {
            None => row = row.child(div().text_color(t.ink_dim).text_size(px(m.text_meta)).child(choice.model.clone())),
            Some(_) => {
                root.dock.overview.model_drawn = true;
                let target = LibraryTarget::Overview(lead_id.clone());
                let spoken = match &effort {
                    Some(effort) => format!("Model and effort: {label}, {effort}"),
                    None => format!("Model: {label}"),
                };
                let open = root.library_open(&target);
                row = row.child(
                    div()
                        .id("coordination-thread-model")
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(30.0))
                        .px(px(8.0))
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .group("coordination-thread-model")
                        .transition(act_ui::style::touch())
                        .when(open, |chip| chip.bg(t.surface_hover))
                        .hover(|style| style.bg(t.surface_hover))
                        .text_size(px(m.text_control))
                        .child(div().text_color(t.quiet_strong).text_size(px(m.text_meta)).child("Model"))
                        .child(div().text_color(if open { t.ink_soft } else { t.ink_faint }).group_hover("coordination-thread-model", |style| style.text_color(t.ink_soft)).child(label))
                        .when_some(effort, |chip, effort| {
                            chip.child(div().text_color(t.quiet_soft).child("·")).child(div().text_color(if open { t.ink_faint } else { t.muted }).group_hover("coordination-thread-model", |style| style.text_color(t.ink_faint)).child(effort))
                        })
                        .child(icon(IconName::ChevronDown, 12.0, t.quiet))
                        .child(bounds_probe(&probe))
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, spoken).expanded(open))
                        .on_click(cx.listener(move |root, _, window, cx| root.toggle_library_for(target.clone(), window, cx))),
                );
            }
        }
        row
    });
    let hero = rimmed(div().pane(t.glass_control).flex().flex_col().gap(px(4.0)).mb(px(14.0)).pt(px(8.0)).pr(px(8.0)).pb(px(8.0)).pl(px(6.0)), &t, 14.0).child(purpose_line).children(model_header);

    let mut column = div().flex().flex_col().child(hero);
    if view.members.is_empty() {
        column = column.child(div().px(px(8.0)).py(px(6.0)).text_color(t.quiet).text_size(px(m.text_control)).child("No threads yet"));
    }
    let stoppable = stoppable_coordinators(state).contains(&lead.id);
    let mut first = true;
    let mut drawn = Drawn::default();
    let mut went: HashMap<String, String> = HashMap::new();
    for (group, members) in overview_groups(&view.members) {
        for member in &members {
            went.insert(member.thread.id.clone(), group_label(group).to_string());
        }
        if members.is_empty() {
            continue;
        }
        let fold = folds.get(&group).cloned().unwrap_or_default();
        let open = fold.open.unwrap_or(match group {
            OverviewSection::Resolved => false,
            OverviewSection::Done => members.len() <= OVERVIEW_LIMIT,
            _ => true,
        });
        let shown: Vec<&CoordinatedThreadView> = if fold.all { members.clone() } else { members.iter().take(OVERVIEW_LIMIT).copied().collect() };
        let toggle_lead = lead_id.clone();
        let head_key = format!("head:{}", group_label(group));
        let head_focus = root.dock.overview.key(&head_key, cx);
        drawn.keys.insert(head_key.clone());
        drawn.heads.push((group_label(group).to_string(), head_key));
        let mut heading = ListHeading::new(SharedString::from(format!("overview-{}", group_label(group))), group_label(group)).count(members.len().to_string()).track_focus(&head_focus).fold(open, move |_, _, cx| dispatch(AppCommand::SetOverviewGroup { task_id: toggle_lead.clone(), group, open: Some(!open), all: None }, cx));
        if group == OverviewSection::Needs {
            heading = heading.count_tone(Tone::Accent);
        }
        if group == OverviewSection::Working {
            let stop_lead = lead_id.clone();
            heading = heading.action(if stoppable {
                ConfirmAction::new("coordination-stop-all", "Stop all", "Stop all", move |_, cx| dispatch(AppCommand::CoordinationStopAll { task_id: stop_lead.clone() }, cx)).variant(ButtonVariant::Danger).size(ButtonSize::Sm).into_any_element()
            } else {
                div().id("coordination-stop-all-off").child(Button::new("coordination-stop-all-idle", "Stop all").variant(ButtonVariant::Danger).size(ButtonSize::Sm).disabled(true)).tip("Nothing here is running").into_any_element()
            });
        }
        let mut section = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, group_label(group))).when(!first, |section| section.mt(px(12.0))).child(heading);
        first = false;
        if open {
            let mut list = div().mt(px(2.0)).flex().flex_col().gap(px(2.0));
            let label = group_label(group);
            let count = shown.len();
            for (index, member) in shown.into_iter().enumerate() {
                list = list.child(thread_row(root, member, label, index, state, &mut drawn, &t, m.text_meta, window, cx));
            }
            if members.len() > OVERVIEW_LIMIT && !fold.all {
                let more_lead = lead_id.clone();
                let more_key = format!("more:{label}");
                let place = Place::More { section: label.to_string(), index: count };
                list = list.child(show_more(root, &more_key, place, &mut drawn, format!("Show {} more", members.len() - OVERVIEW_LIMIT), 40.0, window, cx, move |cx| dispatch(AppCommand::SetOverviewGroup { task_id: more_lead.clone(), group, open: None, all: Some(true) }, cx)));
            }
            section = section.child(list);
        }
        column = column.child(section);
    }
    let notes: Vec<MemoryNote> = view.memory.as_ref().map(|memory| memory.notes.to_vec()).unwrap_or_default();
    column = column.child(memory_section(root, &lead_id, &notes, folds.get(&OverviewSection::Memory).and_then(|fold| fold.open).unwrap_or(true), first, window, cx));
    if let Some(usage) = &view.usage {
        let fold = folds.get(&OverviewSection::Usage).cloned().unwrap_or_default();
        column = column.child(usage_section(root, &lead_id, usage, fold.open.unwrap_or(false), fold.all, &catalog, &mut drawn, &t, window, cx));
    }
    settle(root, &drawn, &went, window, cx);
    div().id("coordination-overview").size_full().overflow_y_scroll().pt(px(10.0)).px(px(10.0)).pb(px(18.0)).child(column).into_any_element()
}

fn thread_row(root: &mut Root, member: &CoordinatedThreadView, section: &str, index: usize, state: &WorkspaceState, drawn: &mut Drawn, t: &Tokens, text: f32, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let thread = member.thread;
    let status = member.status;
    let line = super::coordination::status_line(status, member.summary.as_deref(), member.last_heard);
    let id = thread.id.clone();
    let group: SharedString = format!("overview-row-{}", thread.id).into();
    let row_key = format!("row:{}", thread.id);
    let row_focus = root.dock.overview.key(&row_key, cx);
    drawn.keys.insert(row_key.clone());
    drawn.rows.entry(section.to_string()).or_default().push(row_key);
    let reopen = status == CoordinatedThreadStatus::Resolved;
    // Resolve and Reopen are different buttons, so pressing one takes it off the page.
    let act_key = format!("{}:{}", if reopen { "reopen" } else { "resolve" }, thread.id);
    let mut within = row_focus.is_focused(window);
    let resolution = (reopen || can_resolve(status)).then(|| {
        let focus = root.dock.overview.key(&act_key, cx);
        drawn.keys.insert(act_key.clone());
        let task = thread.id.clone();
        let place = Place::Row { thread_id: thread.id.clone(), section: section.to_string(), index };
        let pressed = act_key.clone();
        let hover: SharedString = format!("overview-action-{}", thread.id).into();
        within |= focus.is_focused(window);
        let ring = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
        let glyph = if reopen && ring {
            ink_icon(IconName::Undo2, 14.0).into_any_element()
        } else if reopen {
            div()
                .child(div().group_hover(hover.clone(), |style| style.invisible()).child(ink_icon(IconName::CircleCheck, 14.0)))
                .child(div().absolute().top_0().left_0().invisible().group_hover(hover.clone(), |style| style.visible()).child(ink_icon(IconName::Undo2, 14.0)))
                .relative()
                .into_any_element()
        } else {
            ink_icon(IconName::Check, 14.0).into_any_element()
        };
        let lit = if reopen { t.ink } else { t.info };
        div()
            .id(SharedString::from(format!("overview-resolution-{}", thread.id)))
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("{} {}", if reopen { "Reopen" } else { "Resolve" }, thread.title)))
            .group(hover.clone())
            .track_focus(&focus)
            .relative()
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .transition(act_ui::style::touch())
            .text_color(if reopen { t.success } else { t.muted_strong })
            .hover(move |style| style.bg(t.wash).text_color(lit))
            .when(ring, |button| button.child(div().absolute().top_0().left_0().size_full().rounded(px(6.0)).border_2().border_color(t.focus_ring)))
            .child(glyph)
            .tip(if reopen { "Reopen" } else { "Resolve" })
            .on_click(cx.listener(move |root, _: &ClickEvent, _, cx| {
                root.dock.overview.held = Some((place.clone(), pressed.clone()));
                if reopen {
                    dispatch(AppCommand::TaskReopen { task_id: task.clone() }, cx);
                } else {
                    dispatch(AppCommand::TaskResolve { task_id: task.clone() }, cx);
                }
            }))
            .into_any_element()
    });
    let detail_ink = match status {
        CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => Some(t.accent),
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => Some(t.danger_soft),
        _ => None,
    };
    let worktree = thread.worktree_id.as_ref().map(|worktree_id| {
        let name = state.worktrees.iter().find(|worktree| &worktree.id == worktree_id).map(act_core::domain::worktree::worktree_name).unwrap_or_else(|| "a worktree".into());
        let hue = t.worktree[act_core::domain::worktree::worktree_hue(worktree_id).rem_euclid(6) as usize];
        div().id(SharedString::from(format!("overview-worktree-{}", thread.id))).child(icon(IconName::FolderSymlink, 12.0, hue)).tip(format!("Works in {name}"))
    });
    let host = member.host.as_ref().map(|host| {
        let away = if host.offline { Some(if host.reconnecting { "Reconnecting" } else { "Offline" }) } else { None };
        div()
            .id(SharedString::from(format!("overview-host-{}", thread.id)))
            .flex_none()
            .max_w(px(96.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .text_color(t.muted)
            .text_size(px(text))
            .font_weight(FontWeight(500.0))
            .child(div().flex().when_some(away, |mark, away| mark.accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, away))).child(icon(if away.is_some() { IconName::MonitorOff } else { IconName::Monitor }, 12.0, t.muted)))
            .child(div().min_w_0().ellipsis().child(host.name.clone()))
            .tip(match away {
                Some(away) => format!("{} · {away}", host.name),
                None => host.name.clone(),
            })
    });
    // A row's Resolve takes the place of its marks while the row is pointed at; a resolved row's
    // Reopen always does.
    let hide_marks = resolution.is_some();
    let marks = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .children(host)
        .children(worktree)
        .child(icon(engine_icon(thread.engine), 12.0, t.quiet))
        .id("marks")
        .transition(gpui::Transition::ease(150).only(&[gpui::TransitionProperty::Opacity]))
        .when(reopen || (hide_marks && within && act_ui::focus::keyboard_focus_visible(cx)), |marks| marks.opacity(0.0))
        .when(hide_marks && !reopen, |marks| marks.group_hover(group.clone(), |style| style.opacity(0.0)));
    let mark = if status == CoordinatedThreadStatus::Working && !member.last_heard {
        super::spinner::bead(SharedString::from(format!("overview-spin-{}", thread.id)), 10.0, t, false).into_any_element()
    } else {
        dot(status, t).into_any_element()
    };
    let mut row = ListRow::new(SharedString::from(format!("overview-thread-{}", thread.id)), thread.title.clone())
        .leading(div().size(px(22.0)).flex().items_center().justify_center().child(mark))
        .detail(line)
        .trailing(marks)
        .track_focus(&row_focus)
        .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx));
    if let Some(ink) = detail_ink {
        row = row.detail_color(ink);
    }
    div()
        .id(group.clone())
        .group(group.clone())
        .relative()
        .child(div().when(reopen && !(within && act_ui::focus::keyboard_focus_visible(cx)), |item| item.opacity(0.62).group_hover(group.clone(), |style| style.opacity(1.0))).child(row))
        .when_some(resolution, |item, button| {
            let shown = reopen || (within && act_ui::focus::keyboard_focus_visible(cx));
            item.child(
                div()
                    .id("resolution")
                    .absolute()
                    .right(px(6.0))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .transition(act_ui::style::touch().only(&[gpui::TransitionProperty::Opacity]))
                    .when(!shown, |slot| slot.opacity(0.0).group_hover(group, |style| style.opacity(1.0)))
                    .child(button),
            )
        })
        .text_size(px(text))
        .into_any_element()
}

fn memory_section(root: &mut Root, lead: &str, notes: &[MemoryNote], open: bool, first: bool, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let toggle_lead = lead.to_string();
    let heading = ListHeading::new("overview-memory", "Memory").count(notes.len().to_string()).fold(open, move |_, _, cx| dispatch(AppCommand::SetOverviewGroup { task_id: toggle_lead.clone(), group: OverviewSection::Memory, open: Some(!open), all: None }, cx));
    let mut section = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Memory")).mt(px(if first { 0.0 } else { 12.0 })).child(heading);
    if !open {
        return section.into_any_element();
    }
    let mut list = div().mt(px(2.0)).flex().flex_col().gap(px(2.0));
    for note in notes {
        let editing = root.dock.overview.editing.as_ref().filter(|(editing, _, _)| *editing == Editing::Note(note.id.clone())).map(|(_, input, _)| input.clone());
        let row = match editing {
            Some(input) => {
                let kind = Editing::Note(note.id.clone());
                div()
                    .min_h(px(30.0))
                    .pl(px(39.0))
                    .pr(px(8.0))
                    .py(px(5.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t.line_strong)
                    .bg(t.canvas)
                    .text_color(t.ink)
                    .text_size(px(m.text_control))
                    .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            root.dock.overview.refocus = Some(kind.key());
                            root.finish_edit(&kind, "", None, cx);
                        }
                    }))
                    .child(input)
                    .into_any_element()
            }
            None => {
                let (note_id, text) = (note.id.clone(), note.text.clone());
                let forget = (lead.to_string(), note.id.clone());
                let group: SharedString = format!("note-{}", note.id).into();
                let focus = root.dock.overview.key(&Editing::Note(note.id.clone()).key(), cx);
                div()
                    .id(SharedString::from(format!("note-{}", note.id)))
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Edit note: {}", note.text)))
                    .track_focus(&focus)
                    .group(group.clone())
                    .relative()
                    .min_h(px(30.0))
                    .pl(px(39.0))
                    .pr(px(34.0))
                    .py(px(5.0))
                    .rounded(px(8.0))
                    .text_color(t.ink_dim)
                    .text_size(px(m.text_control))
                    .cursor_text()
                    .hover(|style| style.bg(t.surface_hover).text_color(t.ink_soft))
                    .child(note.text.clone())
                    .child(
                        div().absolute().top(px(3.0)).right(px(3.0)).child(
                            ConfirmAction::new(SharedString::from(format!("forget-{}", note.id)), "", "Forget", move |_, cx| dispatch(AppCommand::MemoryForget { task_id: forget.0.clone(), note_id: forget.1.clone() }, cx)).icon(IconName::X).variant(ButtonVariant::Secondary).size(ButtonSize::Sm).tip("Forget").name(format!("Forget note: {}", note_name(&note.text))),
                        ),
                    )
                    .on_click(cx.listener(move |root, _, _, cx| root.start_edit(Editing::Note(note_id.clone()), &text, cx)))
                    .into_any_element()
            }
        };
        list = list.child(row);
    }
    let adder = root.note_adder(cx);
    let full = notes.len() >= MAX_MEMORY_NOTES;
    adder.update(cx, |input, cx| {
        if input.is_disabled() != full {
            input.set_disabled(full, cx);
            input.set_placeholder(if full { "Memory is full" } else { "Add a note" }, cx);
        }
    });
    let clear = adder.clone();
    let adding = gpui::Focusable::focus_handle(adder.read(cx), cx).is_focused(window);
    list = list.child(
        div()
            .relative()
            .min_h(px(30.0))
            .pl(px(39.0))
            .pr(px(if adding { 52.0 } else { 8.0 }))
            .py(px(5.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(if adding { t.line_strong } else { gpui::transparent_black().into() })
            .text_color(if adding { t.ink } else { t.ink_dim })
            .text_size(px(m.text_control))
            .when(adding, |row| row.bg(t.canvas))
            .when(!adding && !full, |row| row.hover(|style| style.bg(t.surface_hover)))
            .on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !clear.read(cx).text().is_empty() {
                    cx.stop_propagation();
                    clear.update(cx, |input, cx| input.reset("", cx));
                }
            })
            .child(div().absolute().left(px(12.0)).top(px(8.0)).child(icon(IconName::Plus, 14.0, if adding { t.muted } else { t.quiet_strong })))
            .child(adder),
    );
    section = section.child(list);
    section.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn usage_section(root: &mut Root, lead: &str, usage: &CoordinationUsage, open: bool, all: bool, catalog: &act_core::domain::engine::ModelCatalog, drawn: &mut Drawn, t: &Tokens, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let toggle_lead = lead.to_string();
    let heading = ListHeading::new("overview-usage", "Usage").count(compact(usage.total)).tip(format!("{} tokens", with_commas(usage.total))).fold(open, move |_, _, cx| dispatch(AppCommand::SetOverviewGroup { task_id: toggle_lead.clone(), group: OverviewSection::Usage, open: Some(!open), all: None }, cx));
    let mut section = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Usage")).mt(px(12.0)).child(heading);
    if !open {
        return section.into_any_element();
    }
    let share = |tokens: i64| if usage.total > 0 { tokens as f32 / usage.total as f32 } else { 0.0 };
    let own = share(usage.coordinator) * 100.0;
    let own_label = if own > 0.0 && own < 1.0 { "<1".to_string() } else { format!("{}", own.round() as i64) };
    let m = cx.theme().metrics.clone();
    let label = |text_: &str| div().mt(px(10.0)).mb(px(2.0)).text_color(t.quiet).text_size(px(m.text_meta)).font_weight(FontWeight(550.0)).child(text_.to_string());
    let tokens_ink = |tokens: i64| act_ui::style::tabular_nums(div().flex_none().text_color(t.quiet_strong).text_size(px(m.text_meta))).child(compact(tokens));
    let mut body = rimmed(div().pane(t.glass_control).mt(px(2.0)).pt(px(4.0)).px(px(10.0)).pb(px(8.0)).flex().flex_col().text_color(t.muted).text_size(px(m.text_menu)), t, 12.0)
        .child(div().mt(px(2.0)).flex().gap(px(4.0)).child("This coordinator").child(div().text_color(t.ink_soft).font_weight(FontWeight(600.0)).child(compact(usage.coordinator))).child(format!("· {own_label}%")))
        .child(label("By model"));
    for model in &usage.by_model {
        body = body.child(
            div()
                .id(SharedString::from(format!("usage-model-{}-{}", act_core::domain::engine::engine_label(model.engine), model.model)))
                .flex()
                .items_center()
                .gap(px(10.0))
                .min_h(px(26.0))
                .child(div().w(px(7.5 * m.text_menu)).flex_none().ellipsis().child(model_label(catalog, Some(model.engine), &model.model)))
                .child(div().flex_1().child(Meter::new(share(model.tokens)).tone(Tone::Info)))
                .child(tokens_ink(model.tokens))
                .tip(format!("{} tokens", with_commas(model.tokens))),
        );
    }
    if !usage.by_thread.is_empty() {
        body = body.child(label("By thread"));
        let shown = if all { usage.by_thread.len() } else { usage.by_thread.len().min(OVERVIEW_LIMIT) };
        for entry in &usage.by_thread[..shown] {
            let id = entry.thread.id.clone();
            let key = format!("usage:{}", entry.thread.id);
            let focus = root.dock.overview.key(&key, cx);
            let ring = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
            drawn.keys.insert(key.clone());
            drawn.rows.entry("Usage".into()).or_default().push(key);
            body = body.child(
                div()
                    .id(SharedString::from(format!("usage-thread-{}", entry.thread.id)))
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {}", entry.thread.title)))
                    .track_focus(&focus)
                    .relative()
                    .when(ring, |row| row.child(div().absolute().top_0().left_0().size_full().rounded(px(8.0)).border_2().border_color(t.focus_ring)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .min_h(px(26.0))
                    .mx(px(-8.0))
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .text_color(t.ink_dim)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.surface_hover).text_color(t.ink_soft))
                    .child(div().flex_1().min_w_0().ellipsis().child(entry.thread.title.clone()))
                    .child(tokens_ink(entry.tokens))
                    .tip(format!("{} tokens", with_commas(entry.tokens)))
                    .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx)),
            );
        }
        if usage.by_thread.len() > shown {
            let more_lead = lead.to_string();
            let place = Place::More { section: "Usage".into(), index: shown };
            body = body.child(div().ml(px(-10.0)).mr(px(-10.0)).child(show_more(root, "more:Usage", place, drawn, format!("Show {} more", usage.by_thread.len() - shown), 10.0, window, cx, move |cx| dispatch(AppCommand::SetOverviewGroup { task_id: more_lead.clone(), group: OverviewSection::Usage, open: None, all: Some(true) }, cx))));
        }
    }
    section = section.child(body);
    section.into_any_element()
}


/// A group's Show more: a row lined up under the titles above it.
fn show_more(root: &mut Root, key: &str, place: Place, drawn: &mut Drawn, label: String, indent: f32, window: &Window, cx: &mut Context<Root>, on_press: impl Fn(&mut gpui::App) + 'static) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let focus = root.dock.overview.key(key, cx);
    let ring = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    drawn.keys.insert(key.to_string());
    let pressed = key.to_string();
    div()
        .id(SharedString::from(format!("overview-{key}")))
        .track_focus(&focus)
        .relative()
        .w_full()
        .rounded(px(8.0))
        .pt(px(6.0))
        .pb(px(6.0))
        .pr(px(8.0))
        .pl(px(indent))
        .text_color(t.quiet_strong)
        .text_size(px(m.text_meta))
        .cursor_pointer()
        .hover(|style| style.bg(t.wash).text_color(t.muted))
        .when(ring, |row| row.child(div().absolute().top_0().left_0().size_full().rounded(px(8.0)).border_2().border_color(t.focus_ring)))
        .child(label)
        .on_click(cx.listener(move |root, _: &ClickEvent, _, cx| {
            root.dock.overview.held = Some((place.clone(), pressed.clone()));
            on_press(cx);
        }))
        .into_any_element()
}

/// Where the keyboard goes once what it pressed is gone. A resolved or reopened row hands focus to the
/// row that took its place, else the one before it, else its own row or the head of the group it went
/// to; a Show more to the first row it brought in.
fn settle(root: &mut Root, drawn: &Drawn, went: &HashMap<String, String>, window: &mut Window, cx: &mut Context<Root>) {
    let _ = cx;
    let Some((place, pressed)) = root.dock.overview.held.clone() else { return };
    let pressed_focus = root.dock.overview.keys.get(&pressed).cloned();
    if drawn.keys.contains(&pressed) {
        // Still on the page: the press is over once focus moves elsewhere.
        if !pressed_focus.is_some_and(|focus| focus.is_focused(window)) {
            root.dock.overview.held = None;
        }
        return;
    }
    root.dock.overview.held = None;
    root.dock.overview.keys.retain(|key, _| drawn.keys.contains(key));
    if !pressed_focus.is_some_and(|focus| focus.is_focused(window)) {
        return;
    }
    let place_of = |rows: Option<&Vec<String>>, index: usize| rows.and_then(|rows| rows.get(index).or_else(|| index.checked_sub(1).and_then(|before| rows.get(before)))).cloned();
    let target = match place {
        Place::More { section, index } => drawn.rows.get(&section).and_then(|rows| rows.get(index).or(rows.last())).cloned(),
        Place::Row { thread_id, section, index } => {
            let own = format!("row:{thread_id}");
            place_of(drawn.rows.get(&section), index)
                .or_else(|| drawn.keys.contains(&own).then_some(own))
                .or_else(|| went.get(&thread_id).and_then(|went| drawn.heads.iter().find(|(label, _)| label == went)).map(|(_, key)| key.clone()))
        }
    };
    if let Some(focus) = target.and_then(|key| root.dock.overview.keys.get(&key).cloned()) {
        window.focus(&focus, cx);
    }
}

/// A count as en-US writes it, in groups of three.
fn with_commas(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}

/// A note as its Forget button names it: its first line, cut to 60 characters.
fn note_name(text: &str) -> String {
    let line = text.trim().split('\n').next().unwrap_or_default();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(59).collect::<String>().trim_end())
    } else {
        line.to_string()
    }
}
