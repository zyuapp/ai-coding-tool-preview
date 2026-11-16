//! Projects (ProjectSettings.tsx): every computer's listed projects, each edited or removed here.

use act_core::domain::project::project_name;
use act_core::domain::sidebar::SettingsSection;
use act_core::state::WorkspaceState;
use act_core::view::settings::project_settings_rows;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use super::{description, empty, error, page, Marks, Row, Section};
use super::Clip;
use crate::views::root::Root;
use crate::workspace::dispatch;

fn remove_label(threads: usize) -> String {
    match threads {
        0 => "Remove".into(),
        1 => "Remove and archive 1 thread".into(),
        count => format!("Remove and archive {count} threads"),
    }
}

pub fn render(state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let rows = project_settings_rows(state);
    let add = Button::new("project-add", "Add project").icon(IconName::Plus).icon_size(14.0).on_click(|_, _, cx| dispatch(AppCommand::ProjectOpen { start: false }, cx));
    let mut list = Section::new(if rows.len() == 1 { "1 project".to_string() } else { format!("{} projects", rows.len()) }).first().action(add);
    if let (Some(message), Some(SettingsSection::Projects)) = (&state.action_error, state.action_error_page) {
        list = list.row(error(message.clone(), &t).child(div().ml(px(8.0)).child(Button::new("project-error-dismiss", "Dismiss").variant(ButtonVariant::Link).on_click(|_, _, cx| dispatch(AppCommand::DismissActionError, cx)))));
    }
    if rows.is_empty() {
        list = list.row(empty("No projects yet.", &t));
    }
    for row in &rows {
        let project = row.project;
        let name = project_name(project);
        let key = format!("{}:{}", row.host.as_ref().map(|host| host.id.as_str()).unwrap_or("this"), project.id);
        let path = match &row.host {
            Some(host) => format!("{}{} · {}", host.name, if host.offline { if host.reconnecting { " (reconnecting)" } else { " (offline)" } } else { "" }, project.root),
            None => project.root.clone(),
        };
        let mut line = Row::new(name.clone(), &t).describe(div().mt(px(5.0)).text_color(t.quiet_strong).text_size(px(12.0)).line_height(gpui::relative(1.45)).clip().child(path)).action_column(150.0, 12.0);
        let armed = marks.armed(&format!("remove-{key}"));
        let mut actions = div().flex().items_center().gap(px(12.0));
        if row.host.is_none() && !armed.get() {
            let id = project.id.clone();
            actions = actions.child(Button::new(SharedString::from(format!("edit-{key}")), "Edit…").name(format!("Edit {name}")).on_click(move |_, _, cx| dispatch(AppCommand::EditProject { project_id: Some(id.clone()) }, cx)));
        }
        let id = project.id.clone();
        actions = actions.child(
            ConfirmAction::new(SharedString::from(format!("remove-{key}")), "Remove", remove_label(row.threads), move |_, cx| dispatch(AppCommand::ProjectRemove { project_id: id.clone() }, cx))
                .variant(ButtonVariant::Danger)
                .name(format!("Remove {name}"))
                .armed_into(&armed)
                .disabled(row.host.as_ref().is_some_and(|host| host.offline)),
        );
        line = line.action(actions);
        list = list.row(line.build(marks, &t));
    }
    page("Projects", Some(description("The folders threads start in.")), None, window, cx).child(list.build(marks, &t)).into_any_element()
}
