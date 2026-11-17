//! Engines (EngineSettings.tsx): each installed engine, its sign-in and update, and reloading the
//! settings the engines read.

use act_core::domain::engine::{engine_label, engine_notice, engine_status_line, engine_status_word, engine_update, EngineAccess, AGENT_ENGINES};
use act_core::state::{AgentSettingsReload, WorkspaceState};
use act_core::AppCommand;
use act_ui::components::button::Button;
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};

use super::{description, glyph, line, page, pill, spinning_button, Marks, PillTone, Row, Section};
use crate::views::common::engine_icon;
use crate::views::root::Root;
use crate::views::timeline_row::copy_button;
use crate::workspace::dispatch;

fn reload_line(status: AgentSettingsReload) -> &'static str {
    match status {
        AgentSettingsReload::Pending => "Reload pending",
        AgentSettingsReload::Reloaded => "Settings reloaded",
        AgentSettingsReload::Failed => "Could not reload settings. Try again.",
        _ => "",
    }
}

pub fn render(state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let checking = state.engine_checking;
    let check = spinning_button("engines-check", if checking { "Checking…" } else { "Check again" }, IconName::RefreshCw, checking).track_focus(&marks.stop("engines.installed", cx)).disabled(checking).on_click(|_, _, cx| dispatch(AppCommand::EngineRead { refresh: Some(true) }, cx));
    let mut installed = Section::setting("engines.installed").first().description(description("Install an engine in your terminal, then check again here.")).action(check);
    for engine in AGENT_ENGINES {
        let readiness = state.engine_readiness(engine);
        let notice = engine_notice(engine, &readiness);
        let update = engine_update(&readiness);
        let ready = notice.is_none() && readiness.access == EngineAccess::Ready;
        let mut row = Row::new(engine_label(engine), &t).lead(glyph(engine_icon(engine), t.muted)).describe(line(engine_status_line(engine, &readiness), &t));
        if let Some(fix) = notice.as_ref().filter(|notice| !notice.updatable).and_then(|notice| notice.fix.clone()) {
            row = row.describe(
                div()
                    .mt(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().px(px(5.0)).py(px(1.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono.clone()).text_size(px(12.0)).child(fix.clone()))
                    .child(copy_button(&format!("engine-fix-{engine:?}"), fix.clone(), "Copy", None, window, cx)),
            );
        }
        let updating = state.engine_updating == Some(engine);
        row = if readiness.access == EngineAccess::SignedOut {
            row.action(Button::new(SharedString::from(format!("sign-in-{engine:?}")), "Sign in").name(format!("Sign in to {}", engine_label(engine))).on_click(move |_, _, cx| dispatch(AppCommand::EngineSignIn { engine }, cx)))
        } else if update.as_ref().is_some_and(|update| update.command.is_some()) {
            row.action(
                (if updating { spinning_button(format!("update-{engine:?}"), "Updating…", IconName::RefreshCw, true) } else { Button::new(SharedString::from(format!("update-{engine:?}")), "Update") })
                    .name(format!("{} {}", if updating { "Updating" } else { "Update" }, engine_label(engine)))
                    .disabled(state.engine_updating.is_some() || checking)
                    .on_click(move |_, _, cx| dispatch(AppCommand::EngineUpdate { engine, computer_id: None }, cx)),
            )
        } else {
            let tone = if ready {
                PillTone::Ready
            } else if notice.as_ref().is_some_and(|notice| notice.blocking) {
                PillTone::Blocked
            } else {
                PillTone::Accent
            };
            row.action(pill(engine_status_word(&readiness), tone, &t))
        };
        installed = installed.row(row.build(marks, &t));
    }
    let reload = state.agent_settings_reload;
    let tone = match reload {
        AgentSettingsReload::Pending => Some(t.info_soft),
        AgentSettingsReload::Reloaded => Some(t.success_soft),
        AgentSettingsReload::Failed => Some(t.danger_soft),
        _ => None,
    };
    let reloading = reload == AgentSettingsReload::Reloading;
    let mut agent = Section::setting("engines.agent-settings")
        .description(description("Apply changes to your engines' settings. Busy agents reload when their work finishes."))
        .action(
            spinning_button("reload-agent-settings", if reloading { "Reloading…" } else { "Reload agent settings" }, IconName::RefreshCw, reloading).track_focus(&marks.stop("engines.agent-settings", cx))
                .disabled(matches!(reload, AgentSettingsReload::Reloading | AgentSettingsReload::Pending))
                .on_click(|_, _, cx| dispatch(AppCommand::EngineReloadSettings, cx)),
        );
    if let Some(color) = tone {
        agent = agent.detail(div().mt(px(6.0)).text_color(color).text_size(px(14.0)).font_weight(FontWeight(560.0)).child(reload_line(reload)));
    }
    page("Engines", Some(description(concat!(act_core::app_name!(), " runs the engines installed on this computer."))), None, window, cx).child(installed.build(marks, &t)).child(agent.build(marks, &t)).into_any_element()
}
