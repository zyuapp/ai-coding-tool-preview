//! What a panel or list shows when it has nothing to show (EmptyState.tsx).

use gpui::{div, prelude::*, px, AnyElement, App, FontWeight, SharedString, Window};

use crate::icon::{Icon, IconName};
use crate::style::{Corners, PaneFill, Raised};
use crate::theme::{ActiveTheme, Pane};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmptySize {
    /// Fills the panel and centres a little above the middle.
    #[default]
    Panel,
    /// One quiet line where the list's rows would be.
    Inline,
}

#[derive(IntoElement)]
pub struct EmptyState {
    size: EmptySize,
    icon: Option<IconName>,
    title: Option<SharedString>,
    detail: Option<SharedString>,
    action: Option<AnyElement>,
    danger: bool,
    spinning: bool,
}

impl EmptyState {
    pub fn new() -> Self {
        Self { size: EmptySize::Panel, icon: None, title: None, detail: None, action: None, danger: false, spinning: false }
    }

    pub fn inline() -> Self {
        Self { size: EmptySize::Inline, ..Self::new() }
    }

    /// Drawn in a glass orb above a panel's title.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// Turns the orb red, for a panel that is empty because something failed.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Turns the orb's icon while something is on its way (`SpinningIcon`).
    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }
}

impl Default for EmptyState {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for EmptyState {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        if self.size == EmptySize::Inline {
            let text = match (self.title, self.detail) {
                (Some(title), Some(detail)) => format!("{title} {detail}"),
                (Some(title), None) => title.to_string(),
                (None, Some(detail)) => detail.to_string(),
                (None, None) => String::new(),
            };
            return div()
                .mt(px(7.0))
                .flex()
                .flex_wrap()
                .items_center()
                .text_color(t.quiet_soft)
                .text_size(px(m.text_meta))
                .line_height(gpui::relative(1.5))
                .child(text)
                .children(self.action.map(|action| div().ml(px(6.0)).child(action)))
                .into_any_element();
        }
        let bottom = window.viewport_size().height * 0.1;
        let has_icon = self.icon.is_some();
        let mut panel = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .pt(px(24.0))
            .px(px(24.0))
            .pb(bottom)
            .text_center();
        if let Some(icon) = self.icon {
            let (fill, color) = if self.danger { (t.danger_surface, t.danger) } else { (t.info_surface, t.info) };
            panel = panel.child(
                div()
                    .relative()
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .pane(Pane::sheened(fill))
                    .rim(t.glass_rim, Corners::Full)
                    .text_color(color)
                    // Every panel's empty state draws its glyph at 18 in the 28 orb.
                    .child(if self.spinning {
                        super::spinner::Spinning::new("empty-state-spinning", icon).size(px(18.0)).into_any_element()
                    } else {
                        Icon::new(icon).size(px(18.0)).into_any_element()
                    }),
            );
        }
        if let Some(title) = self.title {
            panel = panel.child(
                div()
                    .mt(px(if has_icon { 15.0 } else { 0.0 }))
                    .mb(px(7.0))
                    .text_color(t.ink_dim)
                    .text_size(px(m.text_heading))
                    .font_weight(FontWeight(560.0))
                    .child(title),
            );
        }
        if let Some(detail) = self.detail {
            panel = panel.child(
                div()
                    .max_w(px(300.0))
                    .text_color(t.quiet_strong)
                    .text_size(px(m.text_control))
                    .line_height(gpui::relative(1.5))
                    .child(detail),
            );
        }
        if let Some(action) = self.action {
            panel = panel.child(div().mt(px(14.0)).child(action));
        }
        panel.into_any_element()
    }
}
