//! Small pieces many views share: time labels, engine marks, and the glass treatments CSS names.

use act_core::domain::engine::AgentEngine;
use act_ui::style::{Corners, Raised};
use act_ui::theme::Tokens;
use act_ui::{Icon, IconName};
use chrono::{Datelike, Local, TimeZone};
use super::letter_spacing::LetterSpacing;
use gpui::{div, prelude::*, px, Hsla, Rgba};

const DAY_MS: i64 = 86_400_000;

fn local(at: i64) -> chrono::DateTime<Local> {
    Local.timestamp_millis_opt(at).single().unwrap_or_else(Local::now)
}

fn day_start(at: i64) -> i64 {
    let day = local(at).date_naive().and_hms_opt(0, 0, 0).unwrap_or_default();
    Local.from_local_datetime(&day).single().map(|moment| moment.timestamp_millis()).unwrap_or(at)
}

/// "2:41 PM", as `Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" })` writes it.
pub fn clock_time(at: i64) -> String {
    local(at).format("%-I:%M %p").to_string()
}

/// When something last moved, as short as the gap allows (src/domain/recency.ts).
pub fn recency_label(at: i64, now: i64) -> String {
    let days = ((day_start(now) - day_start(at)) as f64 / DAY_MS as f64).round() as i64;
    if days <= 0 {
        return clock_time(at);
    }
    if days == 1 {
        return "Yesterday".into();
    }
    let moment = local(at);
    if days < 7 {
        return moment.format("%a").to_string();
    }
    if moment.year() == local(now).year() {
        moment.format("%b %-d").to_string()
    } else {
        moment.format("%b %-d, %Y").to_string()
    }
}

/// The full moment a tooltip gives a timestamp.
pub fn full_moment(at: i64) -> String {
    local(at).format("%A, %B %-d, %Y at %-I:%M:%S %p").to_string()
}

/// `formatElapsed`: seconds, then minutes and seconds, then hours and minutes.
pub fn format_elapsed(ms: i64) -> String {
    let seconds = ((ms.max(0) as f64) / 1000.0).round() as i64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

pub fn engine_icon(engine: AgentEngine) -> IconName {
    match engine {
        AgentEngine::Claude => IconName::Claude,
        AgentEngine::Codex => IconName::Openai,
        AgentEngine::Opencode => IconName::Opencode,
        AgentEngine::Other(_) => IconName::MessageSquare,
    }
}

pub fn icon(name: IconName, size: f32, color: impl Into<Hsla>) -> Icon {
    Icon::new(name).size(px(size)).color(color)
}

/// An icon in the text colour around it (`currentColor`), so it follows hover and press.
pub fn ink_icon(name: IconName, size: f32) -> Icon {
    Icon::new(name).size(px(size))
}

/// A box turned `degrees` about its centre, easing to each new turn over `ms` milliseconds, as a
/// chevron's CSS `transform: rotate()` with `transition: transform <ms> ease` does.
pub fn turning(id: &'static str, degrees: f32, ms: u64) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .transition(gpui::Transition::ease(ms).only(&[gpui::TransitionProperty::Transform]))
        .transform(gpui::ElementTransform::rotate(degrees))
}

/// `--glass-rim` over a rounded box. The box must be `relative`.
pub fn rimmed<E: Styled + ParentElement>(element: E, tokens: &Tokens, radius: f32) -> E {
    element.relative().rounded(px(radius)).rim(tokens.glass_rim, Corners::Radius(radius))
}

/// One count chip (`.chip[data-variant="count"]`).
pub fn count_chip(text: impl IntoElement, tokens: &Tokens) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(18.0))
        .min_w(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(tokens.chip)
        .text_color(tokens.muted_strong)
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight(600.0))
        .tabular_nums()
        .child(text)
}

pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: color.a * alpha, ..color }
}

/// One line cut short with an ellipsis. Wrapping text re-measures against its width each layout,
/// which `nowrap` text does not, so the line is clamped instead.
pub trait Ellipsis: Styled + Sized {
    fn ellipsis(self) -> Self {
        self.whitespace_normal().overflow_hidden().line_clamp(1).text_ellipsis()
    }
}

impl<T: Styled + Sized> Ellipsis for T {}

/// Keeps a float over the transcript visible to the glass drawn over it, which otherwise sees only
/// the frame there (`Window::hide_from_backdrops`). Laid over the float, which must be positioned.
pub fn seen_by_backdrops() -> impl IntoElement {
    gpui::canvas(|_, _, _| {}, |bounds, _, window, _| window.show_to_backdrops(bounds)).absolute().top_0().left_0().size_full()
}
