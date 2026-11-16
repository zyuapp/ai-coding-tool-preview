//! Worktrees (WorktreeSettings.tsx): every checkout the app keeps, grouped by project, what deleting
//! each would cost, and the folders already gone.

use std::collections::HashSet;

use act_core::state::WorkspaceState;
use act_core::view::worktrees::{worktree_settings_page, worktree_settings_views, WorktreeSettingsView};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant, IconButton, IconButtonSize};
use act_ui::components::chip::Chip;
use act_ui::components::list::ListHeading;
use act_ui::components::select::{Select, SelectOption};
use act_ui::theme::{ActiveTheme, Tokens, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnimationExt, AnyElement, Context, FontWeight, Rgba, SharedString, Window};

use super::{empty, error, glyph, notice, page, spinning_button, Clip, Marks, Row, Section};
use crate::views::common::{icon, ink_icon, with_alpha};
use crate::views::root::Root;
use crate::views::timeline_row::copy_button;
use crate::workspace::dispatch;

fn plural(count: usize, one: &str) -> String {
    format!("{count} {}", if count == 1 { one.to_string() } else { format!("{one}s") })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Safety {
    Safe,
    Warning,
    Busy,
    Unknown,
}

/// What deleting would cost, in a word or two, with the full reading as the tip.
fn safety(worktree: &WorktreeSettingsView) -> (Safety, String, String) {
    if worktree.busy {
        return (Safety::Busy, "Run active".into(), "Wait for the active run to finish".into());
    }
    if worktree.repository.is_none() {
        return (Safety::Warning, "Not a Git checkout".into(), "Deleting removes every file in the folder".into());
    }
    let changed = worktree.status.changed_files;
    let comparison = worktree.status.comparison.as_ref();
    let changes = match changed {
        None => "Working tree status unavailable".to_string(),
        Some(0) => "No uncommitted changes".to_string(),
        Some(count) => plural(count as usize, "uncommitted file"),
    };
    let commits = match comparison {
        None => "Commit comparison unavailable".to_string(),
        Some(comparison) if comparison.ahead == 0 => format!("All commits in {}", comparison.branch),
        Some(comparison) => format!("{} not in {}", plural(comparison.ahead as usize, "commit"), comparison.branch),
    };
    let tip = format!("{changes}. {commits}.");
    if worktree.safe {
        return (Safety::Safe, "Safe to delete".into(), tip);
    }
    let mut risks = Vec::new();
    if let Some(count) = changed.filter(|count| *count > 0) {
        risks.push(plural(count as usize, "uncommitted file"));
    }
    if let Some(ahead) = comparison.map(|comparison| comparison.ahead).filter(|ahead| *ahead > 0) {
        risks.push(plural(ahead as usize, "unmerged commit"));
    }
    if !risks.is_empty() {
        return (Safety::Warning, risks.join(" · "), tip);
    }
    (Safety::Unknown, "Status unknown".into(), tip)
}

/// `moving` is false while the window is not in use or motion is reduced, and the busy dot rests.
fn safety_badge(worktree: &WorktreeSettingsView, t: &Tokens, moving: bool) -> Option<AnyElement> {
    if !worktree.available {
        return None;
    }
    let (tone, label, tip) = safety(worktree);
    let (border, color, fill): (Rgba, Rgba, Option<Rgba>) = match tone {
        Safety::Safe => (t.success_line, t.success_soft, Some(t.success_surface)),
        Safety::Warning => (with_alpha(t.warning, 0.35), t.warning, Some(with_alpha(t.warning, 0.09))),
        Safety::Busy => (t.info_line, t.info_soft, Some(t.info_surface_soft)),
        Safety::Unknown => (t.line, t.muted, None),
    };
    let mark: AnyElement = match tone {
        Safety::Safe => icon(IconName::Check, 12.0, color).into_any_element(),
        Safety::Busy if !moving => div().size(px(6.0)).rounded_full().bg(color).into_any_element(),
        Safety::Busy => div()
            .size(px(6.0))
            .rounded_full()
            .bg(color)
            .with_animation(SharedString::from(format!("worktree-pulse-{}", worktree.root)), gpui::Animation::new(std::time::Duration::from_millis(1400)).repeat(), |dot, delta| {
                let step = (delta * 14.0).floor() / 14.0;
                dot.opacity(1.0 - 0.65 * (1.0 - (2.0 * step - 1.0).abs()))
            })
            .into_any_element(),
        _ => icon(IconName::CircleAlert, 12.0, color).into_any_element(),
    };
    Some(
        div()
            .id(SharedString::from(format!("worktree-safety-{}", worktree.root)))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(9.0))
            .py(px(3.0))
            .border_1()
            .border_color(border)
            .rounded_full()
            .when_some(fill, |badge, fill| badge.bg(fill))
            .text_color(color)
            .text_size(px(12.0))
            .font_weight(FontWeight(550.0))
            .whitespace_nowrap()
            .child(mark)
            .child(label)
            .tip(tip)
            .into_any_element(),
    )
}

/// A `code` in a settings group: a washed chip at 0.9em of the line's 12px.
fn code_chip<E: Styled>(element: E, mono: &SharedString, t: &act_ui::theme::Tokens) -> E {
    element.px(px(5.0)).py(px(1.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono.clone()).text_size(px(10.8)).line_height(gpui::relative(1.4))
}

fn worktree_row(worktree: &WorktreeSettingsView, expanded: bool, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let group = SharedString::from(format!("worktree-row-{}", worktree.root));
    let mut metadata = div().mt(px(3.0)).flex().items_center().gap(px(12.0)).min_w_0().text_color(t.muted).text_size(px(12.0));
    if worktree.threads.is_empty() {
        metadata = metadata.child(div().flex_none().text_color(t.quiet_strong).whitespace_nowrap().child("No threads"));
    } else {
        let root = worktree.root.clone();
        metadata = metadata.child(
            div()
                .id(SharedString::from(format!("worktree-threads-{}", worktree.root)))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(5.0))
                .mx(px(-5.0))
                .px(px(5.0))
                .py(px(1.0))
                .rounded(px(5.0))
                .text_color(t.ink_dim)
                .map(act_ui::style::tabular_nums)
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(|style| style.bg(t.wash).text_color(t.ink))
                .child(ink_icon(IconName::MessageSquare, 12.0))
                .child(plural(worktree.threads.len(), "thread"))
                .child(crate::views::common::turning("chevron", if expanded { 90.0 } else { 0.0 }, 120).child(act_ui::Icon::new(IconName::ChevronRight).size(px(12.0)).color(t.muted)))
                .on_click(move |_, _, cx| dispatch(AppCommand::WorktreeSetThreadsOpen { root: root.clone(), open: !expanded }, cx)),
        );
    }
    if let Some(branch) = &worktree.branch {
        metadata = metadata.child(code_chip(div().min_w_0().max_w(px(220.0)).rounded(px(4.0)), &mono, &t).clip().child(branch.clone()));
    }
    metadata = metadata.child(
        div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(code_chip(div().id(SharedString::from(format!("worktree-path-{}", worktree.root))).min_w_0().rounded(px(5.0)), &mono, &t).clip().child(worktree.name.clone()).tip(worktree.root.clone()))
            // `.copy-affordance` here is 22px square; the button overflows that box without growing the line.
            .child(div().flex_none().size(px(22.0)).flex().items_center().justify_center().opacity(0.0).group_hover(group.clone(), |style| style.opacity(1.0)).child(copy_button(&format!("worktree-copy-{}", worktree.root), worktree.root.clone(), "Copy path", None, window, cx))),
    );
    let mut actions = div().flex().items_center().justify_end().gap(px(2.0)).min_w(px(64.0));
    if worktree.available {
        let root = worktree.root.clone();
        actions = actions.child(IconButton::new(SharedString::from(format!("reveal-{}", worktree.root)), IconName::FolderOpen, if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in file manager" }).size(IconButtonSize::Md).on_click(move |_, _, cx| dispatch(AppCommand::WorktreeReveal { root: root.clone() }, cx)));
    }
    if worktree.deleting {
        actions = actions.child(div().px(px(4.0)).text_size(px(12.0)).text_color(t.muted).child(if worktree.available { "Deleting…" } else { "Forgetting…" }));
    } else {
        let root = worktree.root.clone();
        let busy = worktree.busy;
        let tip = if busy { "Wait for the active run to finish" } else if worktree.available { "Delete" } else { "Forget" };
        // `.worktree-delete`: a ghost key that turns danger under the pointer, held off while busy.
        actions = actions.child(
            div()
                .id(SharedString::from(format!("delete-{}", worktree.root)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, if worktree.available { "Delete…" } else { "Forget…" }).disabled(busy))
                .size(px(30.0))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .text_color(t.quiet_strong)
                .map(|button| if busy { button.opacity(t.disabled_opacity) } else { button.cursor_pointer().transition(act_ui::style::touch()).hover(|style| style.bg(t.danger_surface).text_color(t.danger_soft)) })
                .child(ink_icon(IconName::Trash2, 16.0))
                .tip(tip)
                .on_click(move |_, _, cx| {
                    if !busy {
                        dispatch(AppCommand::WorktreeConfirmDelete { root: Some(root.clone()) }, cx);
                    }
                }),
        );
    }
    let mut row = Row::titled(div().id(SharedString::from(format!("worktree-title-{}", worktree.root))).text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(570.0)).clip().child(worktree.title.clone()).tip_overflow(worktree.title.clone()).into_any_element())
        .block_title()
        .lead(glyph(IconName::FolderGit2, if worktree.available { t.muted } else { t.quiet_strong }))
        .describe(metadata)
        .actions(safety_badge(worktree, &t, window.is_window_active() && !t.reduced_motion))
        .action(actions);
    if expanded && !worktree.threads.is_empty() {
        row = row.footer(
            div().mt(px(6.0)).mb(px(2.0)).pl(px(10.0)).border_l_1().border_color(t.line).flex().flex_col().gap(px(1.0)).children(worktree.threads.iter().map(|thread| {
                let id = thread.id.clone();
                div()
                    .id(SharedString::from(format!("worktree-thread-{}", thread.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .min_h(px(24.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .text_color(t.link)
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.wash).text_color(t.link_hover))
                    .child(thread.title.clone())
                    .when(thread.archived, |link| link.child(div().text_color(t.muted).text_size(px(12.0)).child("Archived")))
                    .on_click(move |_, _, cx| dispatch(AppCommand::WorktreeOpenThread { task_id: id.clone() }, cx))
            })),
        );
    }
    div().group(group).child(row.build(marks, &t)).into_any_element()
}

/// Rows arrive sorted by project, so each run of one project becomes its group.
fn groups(worktrees: &[WorktreeSettingsView], expanded: &HashSet<String>, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> Vec<AnyElement> {
    let mut runs: Vec<(String, String, Vec<&WorktreeSettingsView>)> = Vec::new();
    for worktree in worktrees {
        match runs.last_mut() {
            Some((key, _, rows)) if *key == worktree.project_key => rows.push(worktree),
            _ => runs.push((worktree.project_key.clone(), worktree.project.clone(), vec![worktree])),
        }
    }
    runs.into_iter()
        .map(|(key, name, rows)| {
            let count = rows.len();
            let rows: Vec<AnyElement> = rows.into_iter().map(|worktree| worktree_row(worktree, expanded.contains(&worktree.root), marks, window, cx)).collect();
            div().mt(px(22.0)).child(div().mx(px(-8.0)).child(ListHeading::new(SharedString::from(format!("worktree-group-{key}")), name).count(count.to_string()))).children(rows).into_any_element()
        })
        .collect()
}

pub fn render(state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let busy: HashSet<String> = state.active_runs.keys().cloned().collect();
    let views = worktree_settings_views(state, &busy);
    let page_view = worktree_settings_page(state, views.as_deref());
    let expanded: HashSet<String> = page_view.expanded_threads.iter().cloned().collect();
    let loading = page_view.loading;
    let safe = page_view.safe.len();

    let title = div()
        .flex()
        .items_center()
        .child("On this device")
        .when(!loading, |title| title.child(div().ml(px(9.0)).child(Chip::count(page_view.available.len().to_string()))))
        .into_any_element();
    let mut actions = div().flex().items_center().gap(px(10.0));
    if page_view.projects.len() > 1 {
        let mut options = vec![SelectOption::new("", if loading { "All projects".to_string() } else { format!("All projects · {}", page_view.total) })];
        options.extend(page_view.projects.iter().map(|project| SelectOption::new(project.key.clone(), if loading { project.name.clone() } else { format!("{} · {}", project.name, project.count) })));
        actions = actions.child(Select::new("worktree-filter", options, page_view.project.clone().unwrap_or_default()).disabled(loading).on_change(|value, _, cx| {
            let project = (!value.is_empty()).then(|| value.to_string());
            dispatch(AppCommand::WorktreeFilterProject { project }, cx);
        }));
    }
    actions = actions
        .child(spinning_button("worktrees-refresh", if loading { "Reading…" } else { "Refresh" }, IconName::RefreshCw, loading).disabled(loading).on_click(|_, _, cx| dispatch(AppCommand::WorktreeRefresh, cx)))
        .child(
            Button::new("worktrees-delete-safe", if safe > 0 { format!("Delete {safe} safe") } else { "None safe to delete".to_string() })
                .icon(IconName::Trash2)
                .icon_size(14.0)
                .variant(ButtonVariant::Danger)
                .disabled(loading || safe == 0)
                .on_click(|_, _, cx| dispatch(AppCommand::WorktreeConfirmDeleteSafe { open: true }, cx)),
        );
    let mut list = Section::titled(title).first().action(actions);
    if let Some(message) = &state.worktree_management_error {
        list = list.row(error(message.clone(), &t));
    }
    if let Some(message) = &state.worktree_management_notice {
        list = list.row(notice(message.clone(), &t));
    }
    if loading && page_view.available.is_empty() {
        list = list.row(empty("Reading worktrees…", &t));
    }
    if !loading && state.worktree_management_error.is_none() && page_view.available.is_empty() {
        list = list.row(empty(if page_view.project.is_some() { "No worktrees on this device for this project." } else { "No worktrees on this device." }, &t));
    }
    list = list.rows(groups(&page_view.available, &expanded, marks, window, cx));
    let mut body = page("Worktrees", None, None, window, cx).child(list.build(marks, &t));
    if !page_view.missing.is_empty() {
        let open = page_view.missing_open;
        let mut missing = div()
            .mt(px(27.0))
            .pt(px(14.0))
            .border_t_1()
            .border_color(t.line)
            .child(div().mx(px(-8.0)).child(ListHeading::new("worktree-missing", "Missing folders").count(page_view.missing.len().to_string()).count_tone(Tone::Warning).fold(open, move |_, _, cx| dispatch(AppCommand::WorktreeSetMissingOpen { open: !open }, cx))));
        if open {
            missing = missing
                .child(div().mt(px(8.0)).text_color(t.muted).text_size(px(12.0)).child("These folders are already gone. Forgetting them keeps your thread history."))
                .children(groups(&page_view.missing, &expanded, marks, window, cx));
        }
        body = body.child(missing);
    }
    body.into_any_element()
}
