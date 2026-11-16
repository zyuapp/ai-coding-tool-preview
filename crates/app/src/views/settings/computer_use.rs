//! Computer use (ComputerUseSettings.tsx): the switch, and what the platform lets the app see and operate.

use act_core::domain::platform::ComputerUsePermission;
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::Button;
use act_ui::components::field::Switch;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use super::{description, em, error, page, pill, Marks, PillTone, Row, Section};
use crate::views::root::Root;
use crate::workspace::dispatch;

/// macOS asks the user for two permissions; elsewhere the platform reports what it can do.
const ASK_PERMISSIONS: bool = cfg!(target_os = "macos");

pub fn render(state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let access = &state.computer_use_permissions;
    let permissions = access.permissions.as_ref();
    let linux = permissions.and_then(|permissions| permissions.linux_runtime.as_ref());
    let ready = match linux {
        Some(linux) => linux.status != "unavailable",
        None => permissions.is_some_and(|permissions| permissions.accessibility && permissions.screen_recording),
    };
    let enabled = state.computer_use;
    let availability = Section::new("Availability").first().row(
        Row::setting("computer-use.availability", &t)
            .line(
                if ASK_PERMISSIONS {
                    "The agent can see and operate other applications. Off leaves it no way to reach them, whatever the permissions below say."
                } else {
                    "The agent can see and operate other applications. Off leaves it no way to reach them, whatever the platform setup below says."
                },
                &t,
            )
            .on_title_click(move |_, cx| dispatch(AppCommand::SetComputerUse { enabled: !enabled }, cx))
            .action(Switch::new("computer-use-switch", enabled, "Computer use").track_focus(&marks.stop("computer-use.availability", cx)).on_change(|value: &bool, _, cx| dispatch(AppCommand::SetComputerUse { enabled: *value }, cx)))
            .build(marks, &t),
    );
    let failure = access.error.clone().map(|message| error(message, &t).into_any_element());
    let setup = if permissions.is_none() && !ASK_PERMISSIONS {
        Section::new("Platform setup").description(description("Checking what this computer can use…")).badge(pill("Checking…", PillTone::Neutral, &t)).rows(failure)
    } else if let Some(linux) = linux {
        let word = match linux.status.as_str() {
            "available" => "Ready",
            "limited" => "Compositor-dependent",
            _ => "Unavailable",
        };
        Section::new("Linux runtime").description(description(linux.message.clone())).badge(pill(word, if ready { PillTone::Ready } else { PillTone::Accent }, &t)).rows(failure)
    } else if ASK_PERMISSIONS {
        let busy = access.busy;
        let permission_row = |id: &'static str, permission: ComputerUsePermission, granted: Option<bool>, line: &'static str, enable: &'static str, stop: gpui::FocusHandle| {
            let mut row = Row::setting(id, &t).status(granted == Some(true), &t).line(line, &t);
            match granted {
                Some(true) => row = row.action(em("Done", t.success_soft)),
                None => row = row.action(em("Checking…", t.muted)),
                Some(false) => {
                    let opening = busy == Some(permission);
                    row = row.action(Button::new(SharedString::from(format!("enable-{id}")), if opening { "Opening…" } else { enable }).track_focus(&stop).disabled(busy.is_some()).on_click(move |_, _, cx| dispatch(AppCommand::ComputerUseEnable { permission }, cx)));
                }
            }
            row.build(marks, &t)
        };
        let mut section = Section::new("Permissions")
            .description(description(concat!(act_core::app_name!(), " needs both macOS permissions to operate other apps.")))
            .badge(pill(if ready { "Setup complete" } else { "Setup required" }, if ready { PillTone::Ready } else { PillTone::Accent }, &t))
            .row(permission_row("computer-use.accessibility", ComputerUsePermission::Accessibility, permissions.map(|permissions| permissions.accessibility), concat!("Allows ", act_core::app_name!(), " to click, type, and navigate apps."), "Enable Accessibility", marks.stop("computer-use.accessibility", cx)))
            .row(permission_row("computer-use.screen-recording", ComputerUsePermission::ScreenRecording, permissions.map(|permissions| permissions.screen_recording), concat!("Allows ", act_core::app_name!(), " to see app windows. System audio is not recorded."), "Enable Screen Recording", marks.stop("computer-use.screen-recording", cx)))
            .rows(failure);
        if access.restart_required {
            section = section.row(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(20.0))
                    .border_t_1()
                    .border_color(t.info_line)
                    .py(px(15.0))
                    .child(div().text_color(t.info_soft).text_size(px(14.0)).child(concat!("Restart ", act_core::app_name!(), " to finish enabling computer use.")))
                    .child(Button::new("computer-use-restart", concat!("Restart ", act_core::app_name!())).on_click(|_, _, cx| dispatch(AppCommand::ComputerUseRestart, cx))),
            );
        }
        section
    } else {
        Section::new("Platform setup").description(description("Computer use is not available on this platform.")).badge(pill("Unavailable", PillTone::Neutral, &t)).rows(failure)
    };
    let mut body = page("Computer use", Some(description(concat!("Let ", act_core::app_name!(), " see and control other applications when you ask it to."))), None, window, cx).child(availability.build(marks, &t)).child(setup.build(marks, &t));
    if ASK_PERMISSIONS {
        body = body.child(div().mt(px(18.0)).text_color(t.quiet).text_size(px(12.0)).child("Permission checks capture one frame and discard it immediately."));
    }
    body.into_any_element()
}
