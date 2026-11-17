//! The composer's meters: the plan its engine spends, the context window the thread fills, their
//! hover cards, and the warning over the composer once a window runs low (PlanUsageMeter.tsx,
//! ContextUsageMeter.tsx, UsageCard.tsx).

use super::letter_spacing::LetterSpacing;
use act_core::domain::engine::{engine_label, model_label, AgentEngine};
use act_core::domain::platform::{bar_share, plan_label, session_and_week, tightest_window, usage_warning_key, window_span, windows_for, PlanUsage, UsageWindow, NEAR_LIMIT_SHARE};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::meter::{Meter, MeterSize};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::{ActiveTheme, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{deferred, div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};

use super::common::icon;
use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

/// Past this share the context window is close to compacting, so the meter takes the accent.
const HIGH_PERCENT: i64 = 80;

fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

/// 3.1M, 22K, as `toLocaleString("en-US", { notation: "compact" })` writes them: one decimal
/// below 10 of a unit, whole units above, rounding half up into the next unit (999.5K is 1M).
pub fn compact_tokens(tokens: i64) -> String {
    const UNITS: [(i64, &str); 4] = [(1_000, "K"), (1_000_000, "M"), (1_000_000_000, "B"), (1_000_000_000_000, "T")];
    let tokens = tokens.max(0);
    let Some(mut index) = UNITS.iter().rposition(|(unit, _)| tokens >= *unit) else {
        return tokens.to_string();
    };
    loop {
        let (unit, suffix) = UNITS[index];
        let tenths = (i128::from(tokens) * 10 + i128::from(unit) / 2) / i128::from(unit);
        if tenths < 100 {
            return if tenths % 10 == 0 { format!("{}{suffix}", tenths / 10) } else { format!("{}.{}{suffix}", tenths / 10, tenths % 10) };
        }
        let whole = (i128::from(tokens) + i128::from(unit) / 2) / i128::from(unit);
        if whole < 1000 || index + 1 == UNITS.len() {
            return format!("{whole}{suffix}");
        }
        index += 1;
    }
}

/// "Resets 3:40pm" today, "Resets Nov 14 at 3:40pm" further out.
pub fn format_reset(resets_at: Option<&str>) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(resets_at?).ok()?.with_timezone(&chrono::Local);
    let clock = at.format("%-I:%M%P").to_string();
    if at.date_naive() == chrono::Local::now().date_naive() {
        Some(format!("Resets {clock}"))
    } else {
        Some(format!("Resets {} at {clock}", at.format("%b %-d")))
    }
}

/// One bar of a meter's hover card.
fn card_row(label: String, value: String, share: f64, high: bool, detail: Option<String>, cx: &Context<Root>) -> gpui::Div {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(7.0))
        .child(div().flex().justify_between().gap(px(12.0)).text_size(px(m.text_meta)).child(div().text_color(t.ink_dim).font_weight(FontWeight(500.0)).child(label)).child(div().text_color(if high { t.accent_text } else { t.ink_faint }).child(value)))
        .child(Meter::new((share / 100.0) as f32).size(MeterSize::Groove).tone(if high { Tone::Accent } else { Tone::Info }))
        .when_some(detail, |row, detail| row.child(div().text_size(px(m.text_meta)).text_color(t.quiet_strong).child(detail)))
}

/// A meter's hover card, rising over the composer from the meter's right edge.
fn usage_card(title: String, aside: Option<String>, rows: Vec<gpui::Div>, window: &Window, cx: &Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    // The title and its aside share a baseline.
    let [title_top, aside_top] = super::model_library::baseline_group([(m.text_control, Some(m.text_control * 1.3)), (m.text_meta, Some(m.text_meta * 1.3))], window).0;
    let card = glass(div(), &t, GlassKind::Popover, None, None)
        .id("usage-card")
        .enter(gpui::Transition::ease(120), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(4.0))))
        .w(px(288.0))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .pt(px(14.0))
        .px(px(16.0))
        .pb(px(16.0))
        .text_color(t.ink)
        .text_size(px(m.text_control))
        .line_height(gpui::relative(1.3))
        .child(div().flex().items_start().justify_between().gap(px(12.0)).child(div().mt(px(title_top)).font_weight(FontWeight(600.0)).child(title)).when_some(aside, |heading, aside| heading.child(div().mt(px(aside_top)).text_color(t.quiet_strong).text_size(px(m.text_meta)).child(aside))))
        .children(rows);
    deferred(div().absolute().right(px(-8.0)).bottom(px(42.0)).child(card)).with_priority(2).into_any_element()
}

/// The plan at a glance: the session and the fullest week the model spends, or the fullest window
/// alone for a plan that names neither. Hover lists every window it spends; a click opens Usage.
pub fn plan_meter(root: &mut Root, state: &WorkspaceState, engine: AgentEngine, model: &str, width: f32, window: &Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    if width <= 470.0 {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let usage = state.plan_usage.reports.get(&engine)?;
    let PlanUsage::Available { subscription, .. } = usage else { return None };
    let catalog = state.catalog();
    let windows = windows_for(Some(usage), &model_label(&catalog, Some(engine), model));
    let (session, week) = session_and_week(&windows);
    let mut rows: Vec<UsageWindow> = if session.is_some() || week.is_some() { [session, week].into_iter().flatten().collect() } else { tightest_window(&windows).into_iter().collect() };
    if rows.is_empty() {
        return None;
    }
    let spoken = rows.iter().map(|row| format!("{} {}% used", row.label, js_round(bar_share(row.utilization)))).collect::<Vec<_>>().join(", ");
    if width <= 600.0 {
        rows.truncate(1);
    }
    let focus = root.composer.plan_focus.clone();
    let hovered = root.composer.plan_card;
    let shown = hovered || (focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx));
    let mut parts = div().flex().items_baseline().gap(px(if rows.len() > 1 { 12.0 } else { 8.0 })).text_size(px(m.text_meta)).tabular_nums().whitespace_nowrap();
    for row in &rows {
        let share = js_round(bar_share(row.utilization));
        let near = share as f64 >= NEAR_LIMIT_SHARE;
        let mut part = div().flex().items_baseline();
        if let Some(span) = window_span(row) {
            part = part.child(div().mr(px(5.0)).text_color(t.muted).child(span));
        }
        let color = if near { t.accent_text } else if hovered { t.ink } else { t.ink_faint };
        let mut value = div().text_color(color).font_weight(FontWeight(560.0)).child(format!("{share}%"));
        if hovered {
            value = value.underline();
        }
        parts = parts.child(part.child(value));
    }
    let label = engine_label(engine).to_string();
    let card_rows = windows
        .iter()
        .map(|window| {
            let share = js_round(bar_share(window.utilization));
            card_row(window.label.clone(), if window.utilization.is_none() { "Not reported".into() } else { format!("{share}%") }, share as f64, share as f64 >= NEAR_LIMIT_SHARE, format_reset(window.resets_at.as_deref()), cx)
        })
        .collect();
    let meter = div()
        .id("plan-usage")
        .track_focus(&focus)
        .focus_ring(true)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("{} plan: {spoken}", engine_label(engine))))
        .flex()
        .items_center()
        .h(px(28.0))
        .mr(px(-4.0))
        .px(px(8.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .text_color(t.muted)
        .when(hovered, |meter| meter.bg(t.wash))
        .when(width > 720.0, |meter| meter.child(div().mr(px(6.0)).child(icon(IconName::Gauge, 14.0, if hovered { t.ink_dim } else { t.muted }))))
        .child(parts)
        .on_hover(cx.listener(|root, hovered: &bool, _, cx| {
            root.composer.plan_card = *hovered;
            cx.notify();
        }))
        .on_click(|_, _, cx| send(serde_json::json!({ "type": "view.set-settings-open", "open": true, "section": "usage" }), cx));
    let mut wrap = div().relative().flex().child(meter);
    if shown {
        wrap = wrap.child(usage_card(label, plan_label(subscription.as_deref()), card_rows, window, cx));
    }
    Some(wrap.into_any_element())
}

/// The ring and its share, with a card giving the counts. Once the window runs high, Compact stands
/// beside the ring where compacting is on offer.
pub fn context_meter(root: &mut Root, state: &WorkspaceState, width: f32, window: &Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let thread = root.composer.thread(state)?;
    let usage = thread.context_usage.clone()?;
    if usage.limit <= 0 {
        return None;
    }
    let percent = js_round(usage.tokens as f64 / usage.limit as f64 * 100.0).min(100);
    let high = percent >= HIGH_PERCENT;
    let focus = root.composer.context_focus.clone();
    let hovered = root.composer.context_card || focus.is_focused(window);
    let ring = super::ring::Ring { progress: percent as f32 / 100.0, size: 18.0, thickness: 2.9, track: t.line_input, fill: if high { t.accent_text } else { t.ink_dim } };
    let gauge = div()
        .id("context-usage")
        .track_focus(&focus)
        .focus_ring(true)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Meter, "Context window").numeric(percent as f64).value(format!("{percent}% used")))
        .flex()
        .items_center()
        .gap(px(6.0))
        .h(px(28.0))
        .px(px(4.0))
        .rounded(px(8.0))
        .when(hovered, |gauge| gauge.bg(t.wash))
        .child(ring)
        .when(width > 470.0, |gauge| gauge.child(div().text_size(px(m.text_meta)).font_weight(FontWeight(600.0)).tabular_nums().child(format!("{percent}%"))))
        .on_hover(root.composer_listener(cx, |root, hovered: &bool, _, cx| {
            root.composer.context_card = *hovered;
            cx.notify();
        }));
    let mut holder = div().relative().flex().child(gauge);
    if hovered {
        let row = card_row(format!("{percent}% used"), format!("{}% left", 100 - percent), percent as f64, high, Some(format!("{} / {} tokens used", compact_tokens(usage.tokens), compact_tokens(usage.limit))), cx);
        holder = holder.child(usage_card("Context window".into(), None, vec![row], window, cx));
    }
    let settled = !state.active_runs.contains_key(&thread.id) && act_core::workspace::activity::waiting_on(state, Some(&thread.id), &act_core::reducer::sending::prompt_key(state)).is_none();
    let compact = high && settled && root.composer.task.is_none() && act_core::reducer::runs::compactable_session(state, thread);
    let task_id = thread.id.clone();
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .mx(px(-4.0))
            .text_color(if high { t.accent_text } else { t.muted })
            .child(holder)
            .when(compact, |meter| meter.child(Button::new("context-compact", "Compact").variant(ButtonVariant::Soft).on_click(move |_, _, cx| dispatch(AppCommand::RunCompact { task_id: Some(task_id.clone()) }, cx))))
            .into_any_element(),
    )
}

/// A line over the composer once a window the next message spends runs low, until the user puts it away.
pub fn plan_warning(state: &WorkspaceState, engine: AgentEngine, model: &str, width: f32, cx: &mut Context<Root>) -> Option<AnyElement> {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    if state.current_thread().is_some_and(|thread| thread.limit_pause.is_some()) {
        return None;
    }
    let usage = state.plan_usage.reports.get(&engine)?;
    let catalog = state.catalog();
    let tightest = tightest_window(&windows_for(Some(usage), &model_label(&catalog, Some(engine), model)))?;
    if tightest.utilization.unwrap_or(0.0) < NEAR_LIMIT_SHARE {
        return None;
    }
    let warning = usage_warning_key(engine.as_str(), &tightest);
    if state.plan_usage.dismissed.contains(&warning) {
        return None;
    }
    let share = js_round(bar_share(tightest.utilization));
    let reset = format_reset(tightest.resets_at.as_deref()).filter(|_| width > 470.0);
    let summary = div()
        .id("plan-usage-warning")
        .flex()
        .flex_shrink()
        .items_center()
        .gap(px(10.0))
        .min_w_0()
        .min_h(px(28.0))
        .ml(px(-6.0))
        .px(px(6.0))
        .rounded(px(7.0))
        .cursor_pointer()
        .group("plan-usage-warning")
        .hover(|style| style.bg(t.wash))
        .child(div().flex_none().w(px(56.0)).child(Meter::new(share as f32 / 100.0).tone(Tone::Accent)))
        .child(div().flex_none().font_weight(FontWeight(560.0)).child(if share >= 100 { "Limit reached".to_string() } else { format!("{share}% used") }))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.ink_dim).group_hover("plan-usage-warning", |style| style.text_color(t.ink).underline()).child(format!("{} · {}", engine_label(engine), tightest.label)))
        .tip("Open usage settings")
        .on_click(|_, _, cx| send(serde_json::json!({ "type": "view.set-settings-open", "open": true, "section": "usage" }), cx));
    let text = div().flex().items_center().gap(px(10.0)).min_w_0().child(summary).when_some(reset, |text, reset| text.child(div().flex_1().whitespace_nowrap().text_color(t.quiet_strong).text_size(px(m.text_meta)).child(reset)));
    let id: SharedString = format!("plan-warning-{warning}").into();
    Some(
        div()
            .w_full()
            .max_w(px(860.0))
            .mb(px(8.0))
            .child(Notice::new(id, text).floating(true).tone(NoticeTone::Accent).dismiss_label("Hide usage warning until reset").dismiss_tip("Hide until reset").on_dismiss(move |_, _, cx| dispatch(AppCommand::UsageDismissWarning { warning: warning.clone() }, cx)))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::compact_tokens;

    #[test]
    fn token_counts_read_compact() {
        assert_eq!(compact_tokens(950), "950");
        assert_eq!(compact_tokens(1_000), "1K");
        assert_eq!(compact_tokens(240_000), "240K");
        assert_eq!(compact_tokens(3_140_000), "3.1M");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_tokens(1_050), "1.1K");
        assert_eq!(compact_tokens(9_950), "10K");
        assert_eq!(compact_tokens(21_749), "22K");
        assert_eq!(compact_tokens(999_500), "1M");
    }
}
