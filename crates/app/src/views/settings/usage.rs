//! Usage (UsageSettings.tsx): each provider's plan limits, on this computer or a paired one.

use act_core::domain::engine::{engine_label, AgentEngine, AGENT_ENGINES};
use act_core::domain::platform::{bar_share, format_share, plan_label, ComputerStatus, PlanUsage, PlanUsageState, UsageWindow};
use act_core::state::WorkspaceState;
use act_core::view::settings::computer_usage;
use act_core::AppCommand;
use act_ui::components::button::ButtonVariant;
use act_ui::components::button::Button;
use act_ui::components::select::{Select, SelectOption};
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::IconName;
use chrono::{DateTime, Local, Utc};
use gpui::{div, prelude::*, px, relative, AnyElement, Context, FontWeight, SharedString, Window};

use super::{description, empty, error, page, spinning_button, Marks, Section};
use crate::views::common::rimmed;
use crate::views::root::Root;
use crate::workspace::dispatch;

const HIGH_SHARE: f64 = 90.0;

fn clock(at: &DateTime<Local>) -> String {
    at.format("%-I:%M%P").to_string()
}

/// A window resetting today reads as a time; one further out carries the day it lands on.
pub(super) fn format_reset(resets_at: Option<&str>, zone: &str) -> Option<String> {
    let at = DateTime::parse_from_rfc3339(resets_at?).ok()?.with_timezone(&Utc).with_timezone(&Local);
    let same_day = at.date_naive() == Local::now().date_naive();
    Some(if same_day { format!("Resets {} ({zone})", clock(&at)) } else { format!("Resets {} at {} ({zone})", at.format("%b %-d"), clock(&at)) })
}

fn usage_window(window: &UsageWindow, zone: &str, (strut, lift): (f32, f32), t: &Tokens) -> AnyElement {
    let share = bar_share(window.utilization);
    let high = window.utilization.unwrap_or(0.0) >= HIGH_SHARE;
    let track = div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Meter, window.label.clone()).numeric(share).value(format_share(window.utilization)))
        .relative()
        .flex_1()
        .h(px(12.0))
        .overflow_hidden()
        .rounded(px(4.0))
        .bg(t.meter_track)
        .border_1()
        .border_color(t.line_strong)
        .child(div().h_full().rounded(px(4.0)).w(relative((share / 100.0) as f32)).bg(if high { t.accent } else { t.info }));
    div()
        .flex()
        .flex_col()
        .child(
            // An inline label on the page text's line, as `.usage-window strong` sits.
            div().min_h(px(strut)).flex().flex_col().justify_end().pb(px(lift)).child(div().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(570.0)).child(window.label.clone())),
        )
        .child(
            div().mt(px(11.0)).flex().items_center().gap(px(14.0)).child(track).child(
                div()
                    .inert(true)
                    .flex_none()
                    .min_w(px(84.0))
                    .text_size(px(14.0))
                    .text_color(if high { t.accent_soft } else { t.ink_faint })
                    .when(high, |share| share.font_weight(FontWeight(650.0)))
                    .child(format_share(window.utilization)),
            ),
        )
        .when_some(format_reset(window.resets_at.as_deref(), zone), |line, reset| line.child(div().mt(px(9.0)).text_color(t.quiet_strong).text_size(px(12.0)).child(reset)))
        .into_any_element()
}

fn provider(engine: AgentEngine, usage: Option<&PlanUsage>, reading: bool, retrying: Option<bool>, first: bool, zone: &str, line: (f32, f32), t: &Tokens) -> AnyElement {
    let plan = match usage {
        Some(PlanUsage::Available { subscription, .. }) => plan_label(subscription.as_deref()),
        _ => None,
    };
    let heading = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .pb(px(16.0))
        .child(div().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(600.0)).child(engine_label(engine)))
        .when_some(plan, |heading, plan| heading.child(rimmed(div(), t, 999.0).px(px(9.0)).py(px(5.0)).bg(t.info_surface_soft).text_color(t.info_soft).text_size(px(12.0)).font_weight(FontWeight(600.0)).child(plan)));
    let quiet = |element: AnyElement| div().pb(px(20.0)).child(element).into_any_element();
    let body: AnyElement = match usage {
        None => quiet(empty(if reading { "Reading usage…" } else { "Not read yet." }, t)),
        Some(PlanUsage::NotApplicable) => quiet(empty("No plan limits apply to this sign-in.", t)),
        Some(PlanUsage::Unavailable { message }) => {
            let mut line = error(message.clone(), t);
            if let Some(retrying) = retrying {
                line = line.child(
                    div().ml(px(8.0)).child(
                        Button::new(SharedString::from(format!("usage-retry-{engine:?}")), if retrying { "Reading…" } else { "Try again" })
                            .variant(ButtonVariant::Link)
                            .disabled(retrying)
                            .on_click(|_, _, cx| dispatch(AppCommand::UsageRead { everywhere: Some(true) }, cx)),
                    ),
                );
            }
            div().pb(px(20.0)).child(line).into_any_element()
        }
        Some(PlanUsage::Available { windows, .. }) => {
            let zone = zone.to_string();
            div().flex().flex_col().gap(px(26.0)).border_t_1().border_color(t.line).py(px(22.0)).children(windows.iter().map(|window| usage_window(window, &zone, line, t))).into_any_element()
        }
    };
    div().pt(px(20.0)).when(!first, |section| section.mt(px(20.0)).border_t_1().border_color(t.line)).child(heading).child(body).into_any_element()
}

/// `asked` marks this computer's own limits, which opening the page always reads.
fn providers(usage: &PlanUsageState, asked: bool, zone: &str, line: (f32, f32), t: &Tokens) -> AnyElement {
    div()
        .border_t_1()
        .border_color(t.line)
        .children(AGENT_ENGINES.iter().enumerate().map(|(index, engine)| {
            let reading = usage.reading.contains(engine);
            provider(*engine, usage.reports.get(engine), asked || reading, asked.then_some(reading), index == 0, zone, line, t)
        }))
        .into_any_element()
}

pub fn render(state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let zone = crate::effects::window_desktop::time_zone();
    let computers = computer_usage(&state.computers.paired);
    let computer = state.usage_computer.as_ref().and_then(|selected| computers.iter().find(|candidate| &candidate.id == selected));
    let reading = match computer {
        Some(computer) => computer.usage.is_some_and(|usage| !usage.reading.is_empty()),
        None => !state.plan_usage.reading.is_empty(),
    };
    let mut actions = div().flex().items_center().gap(px(10.0));
    if !computers.is_empty() {
        let name = &state.computers.name;
        let mut options = vec![SelectOption::new("", if name.is_empty() { "This computer".to_string() } else { format!("This computer · {name}") })];
        options.extend(computers.iter().map(|paired| SelectOption::new(paired.id.clone(), format!("{}{}", paired.name, if paired.status == ComputerStatus::Connected { "" } else { " · Offline" }))));
        let value = computer.map(|computer| computer.id.clone()).unwrap_or_default();
        actions = actions.child(Select::new("usage-computer", options, value).on_change(|value, _, cx| {
            let computer_id = (!value.is_empty()).then(|| value.to_string());
            dispatch(AppCommand::SetUsageComputer { computer_id }, cx);
        }));
    }
    actions = actions.child(spinning_button("usage-refresh", if reading { "Reading…" } else { "Refresh" }, IconName::RefreshCw, reading).track_focus(&marks.stop("usage.plan-limits", cx)).disabled(reading).on_click(|_, _, cx| dispatch(AppCommand::UsageRead { everywhere: Some(true) }, cx)));
    let body = match computer {
        Some(computer) if computer.status == ComputerStatus::Connected && computer.supported && computer.usage.is_some() => providers(computer.usage.expect("checked"), false, &zone, marks.title_line, &t),
        Some(computer) => {
            let message = if computer.status == ComputerStatus::Connecting {
                "Reconnecting…"
            } else if computer.status != ComputerStatus::Connected {
                "Offline"
            } else if !computer.supported {
                "Update AI Coding Tool on that computer to see its limits."
            } else {
                "Reading usage…"
            };
            div().border_t_1().border_color(t.line).py(px(20.0)).child(div().text_color(t.quiet_strong).text_size(px(14.0)).child(message)).into_any_element()
        }
        None => providers(&state.plan_usage, true, &zone, marks.title_line, &t),
    };
    let limits = Section::setting("usage.plan-limits").first().description(description("Usage and reset times reported by each provider.")).action(actions).row(body);
    page("Usage", Some(description("Plan limits across your signed-in accounts.")), None, window, cx).child(limits.build(marks, &t)).into_any_element()
}
