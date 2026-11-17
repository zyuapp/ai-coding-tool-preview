//! A new thread: the question it asks, where it will start, the composer, and recent chats
//! (TimelineEmptyState.tsx, ThreadStartOptions.tsx, DraftRecents.tsx).

use act_core::domain::engine::engine_label;
use act_core::domain::project::project_name;
use act_core::state::WorkspaceState;
use act_core::workspace::order::recent_in;
use act_core::AppCommand;
use act_ui::components::list::ListHeading;
use act_ui::overlay::bounds_probe;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, relative, Context, FontWeight, SharedString, Window};

use super::letter_spacing::LetterSpacing;
use super::common::recency_label;
use super::composer;
use super::root::Root;
use crate::workspace::dispatch;

fn grow(mut element: gpui::Div, factor: f32) -> gpui::Div {
    element.style().flex_grow = Some(factor);
    element.style().flex_shrink = Some(1.0);
    element.style().flex_basis = Some(relative(0.0).into());
    element
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let inset = m.page_inset(f32::from(window.viewport_size().width));
    let side = super::session_panel::reserved(state);
    let engine = state.draft_engine;
    let start = super::thread_start::render(root, state, window, cx);
    let panel = composer::panel(root, state, Some(start), window, cx);
    let probe = root.composer.probe.clone();
    let project = state.draft_project_id.as_ref().and_then(|id| state.project(id));
    let recent = recent_in(&state.threads, project.map(|project| project.id.as_str()), 4);
    let now = act_core::util::now();
    let room = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0) - 8.0 - 16.0;
    let mut recents = div()
        .id("draft-recents")
        .when(!t.reduced_motion, |recents| recents.enter(gpui::Transition::cubic(360, 0.32, 0.72, 0.0, 1.0), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(6.0)))))
        .w_full()
        .max_w(px(860.0))
        .flex()
        .flex_col()
        .pt(px(6.0))
        .px(px(4.0));
    if !recent.is_empty() {
        let heading = project.map(|project| format!("Recent in {}", project_name(project))).unwrap_or_else(|| "Recent chats".into());
        recents = recents.accessible(gpui::AccessInfo::new(gpui::AccessRole::Navigation, heading.clone())).child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, "")).child(ListHeading::new("draft-recents-heading", heading)));
        for thread in recent {
            let id = thread.id.clone();
            recents = recents.child(
                div()
                    .id(SharedString::from(format!("recent-{}", thread.id)))
                    .min_h(px(36.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.surface_hover))
                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_size(px(m.text_control)).text_color(t.ink_dim).child(super::text_fit::fit_line(&thread.title, room - 120.0, m.text_control, window)))
                    .child(div().text_color(t.quiet).text_size(px(12.0)).tabular_nums().child(recency_label(thread.ranked_at, now)))
                    .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx)),
            );
        }
    }
    let column = |child: gpui::AnyElement| div().w_full().pl(px(inset)).pr(px(inset + side)).flex().justify_center().child(child);
    let question = if state.restored { format!("What should {} work on?", engine_label(engine)) } else { "Loading threads…".into() };
    // After the session card, as the composer and recents follow it in the page.
    div()
        .tab_order(1)
        .size_full()
        .flex()
        .flex_col()
        // The tracks share only free room, as CSS grid fr tracks do, so the padding sits inside.
        .child(grow(div(), 1.0).min_h_0().flex().flex_col().justify_end().child(div().flex().flex_col().items_center().pb(px(10.0)).pl(px(inset)).pr(px(inset + side)).child(div().text_color(t.ink).text_size(px(m.text_hero)).font_weight(FontWeight(600.0)).line_height(relative(1.2)).letter_spacing(-0.025).child(question))))
        .child(column(div().relative().w_full().flex().justify_center().pt(px(12.0)).pb(px(20.0)).child(panel).child(bounds_probe(&probe)).into_any_element()))
        .child(grow(div(), 1.5).min_h_0().overflow_hidden().child(column(recents.into_any_element())))
}
