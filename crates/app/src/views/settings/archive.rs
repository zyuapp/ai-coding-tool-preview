//! Archived threads (ArchiveSettings.tsx): what is archived, how long it stays, and putting it back.

use act_core::state::WorkspaceState;
use act_core::workspace::activity::thread_lists;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::SearchField;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, AnyElement, Context, KeyDownEvent, SharedString, Window};

use super::{description, page, Held, Marks, Row, Section};
use crate::views::root::Root;
use crate::workspace::dispatch;

/// How many archived threads the page lists before it offers a filter.
const ARCHIVE_FILTER_FROM: usize = 8;
const ARCHIVE_RETENTION_MS: i64 = 5 * 24 * 60 * 60 * 1000;

#[derive(Default)]
pub struct ArchiveState {
    filter: Option<Held>,
}

fn days_left(archived_at: i64, now: i64) -> String {
    let remaining = ((archived_at + ARCHIVE_RETENTION_MS - now) as f64 / 86_400_000.0).ceil() as i64;
    match remaining {
        ..=0 => "Deletes on next launch".into(),
        1 => "Deletes in 1 day".into(),
        days => format!("Deletes in {days} days"),
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let lists = thread_lists(state);
    let archived = &lists.archived_threads;
    let filtering = archived.len() > ARCHIVE_FILTER_FROM;
    if filtering && root.settings.archive.filter.is_none() {
        root.settings.archive.filter = Some(Held::new("", "Filter archived threads", |_, _, _, _| {}, cx));
    }
    let query = if filtering { root.settings.archive.filter.as_ref().map(|held| held.text(cx)).unwrap_or_default() } else { String::new() };
    let term = query.trim().to_lowercase();
    let shown: Vec<_> = archived.iter().filter(|thread| term.is_empty() || thread.title.to_lowercase().contains(&term)).collect();
    let count = if archived.len() == 1 { "1 thread".to_string() } else { format!("{} threads", archived.len()) };
    let armed = marks.armed("archive-clear");
    // Clear all leaves with the last archived thread, and its question with it.
    if archived.is_empty() {
        armed.set(false);
    }
    let mut action = div().flex().items_center().gap(px(10.0)).when(!armed.get(), |action| action.child(div().text_color(t.quiet_strong).text_size(px(12.0)).whitespace_nowrap().child(format!("{} archived", archived.len()))));
    if !archived.is_empty() {
        action = action.child(ConfirmAction::new("archive-clear", "Clear all", format!("Delete {count}"), |_, cx| dispatch(AppCommand::TaskClearArchive, cx)).variant(ButtonVariant::Danger).armed_into(&armed));
    }
    let mut section = Section::new("Archive").first().description(description("Restore a thread to put it back in the sidebar. Its automation stays off.")).action(action);
    if filtering {
        let input = root.settings.archive.filter.as_ref().map(|held| held.input.clone()).expect("made above");
        let escape = input.clone();
        section = section.row(
            div()
                .mb(px(14.0))
                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" && !escape.read(cx).text().is_empty() {
                        cx.stop_propagation();
                        escape.update(cx, |input, cx| input.reset("", cx));
                    }
                })
                .child(SearchField::new("archive-filter", &input)),
        );
    }
    let empty = |content: AnyElement| div().flex().flex_wrap().items_center().border_t_1().border_color(t.line).py(px(19.0)).text_color(t.quiet_strong).text_size(px(14.0)).child(content).into_any_element();
    if archived.is_empty() {
        section = section.row(empty(div().child("Nothing archived.").into_any_element()));
    } else if shown.is_empty() {
        let input = root.settings.archive.filter.as_ref().map(|held| held.input.clone());
        section = section.row(empty(
            div()
                .flex()
                .items_center()
                .child(format!("No archived thread matches “{}”.", query.trim()))
                .child(div().ml(px(8.0)).child(Button::new("archive-clear-filter", "Clear filter").variant(ButtonVariant::Link).on_click(move |_, window, cx| {
                    if let Some(input) = &input {
                        input.update(cx, |input, cx| {
                            input.reset("", cx);
                            input.focus(window);
                        });
                    }
                })))
                .into_any_element(),
        ));
    }
    let now = act_core::util::now();
    for thread in shown {
        let id = thread.id.clone();
        section = section.row(
            Row::new(thread.title.clone(), &t)
                .line(days_left(thread.archived_at.unwrap_or(now), now), &t)
                .action(Button::new(SharedString::from(format!("restore-{}", thread.id)), "Restore").name(format!("Restore {}", thread.title)).on_click(move |_, _, cx| dispatch(AppCommand::TaskRestore { task_id: id.clone() }, cx)))
                .build(marks, &t),
        );
    }
    page("Archived threads", Some(description(concat!("Archived threads stay here for 5 days, then ", act_core::app_name!(), " deletes them on the next launch."))), None, window, cx).child(section.build(marks, &t)).into_any_element()
}
