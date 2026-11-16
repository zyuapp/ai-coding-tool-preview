//! A column of filters or places, one of them chosen (NavList.tsx).

use std::rc::Rc;

use gpui::{div, prelude::*, px, App, ClickEvent, CursorStyle, ElementId, FontWeight, SharedString, Window};

use crate::icon::{Icon, IconName};
use crate::style::{Corners, Raised};
use crate::theme::ActiveTheme;

pub fn nav_list() -> gpui::Div {
    div().flex().flex_col()
}

#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    label: SharedString,
    selected: bool,
    icon: Option<IconName>,
    count: Option<SharedString>,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl NavItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        Self { id: id.into(), label: label.into(), selected, icon: None, count: None, on_click: None }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// How many the item holds, quiet against the trailing edge.
    pub fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NavItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let mut item = div()
            .id(self.id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, self.label.clone()).toggled(self.selected))
            .relative()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(9.0))
            .rounded(px(7.0))
            .px(px(9.0))
            .py(px(8.0))
            .text_size(px(m.text_meta))
            .cursor(CursorStyle::PointingHand);
        item = if self.selected {
            item.bg(t.surface_active).rim(t.glass_rim, Corners::Radius(7.0)).text_color(t.ink).font_weight(FontWeight(560.0))
        } else {
            let wash = t.wash;
            item.text_color(t.muted).hover(move |style| style.bg(wash))
        };
        if let Some(icon) = self.icon {
            item = item.child(Icon::new(icon).size(px(14.0)));
        }
        item = item.child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(self.label));
        if let Some(count) = self.count {
            item = item.child(
                crate::style::tabular_nums(div().flex_none().text_color(t.quiet).text_size(px(m.text_badge)).font_weight(FontWeight::NORMAL)).child(count),
            );
        }
        if let Some(handler) = self.on_click {
            item = item.on_click(move |event, window, cx| handler(event, window, cx));
        }
        item
    }
}

/// A rule between groups of items.
#[derive(IntoElement)]
pub struct NavSeparator;

impl RenderOnce for NavSeparator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div().flex_none().mx(px(8.0)).my(px(10.0)).h(px(1.0)).bg(cx.theme().tokens.line_soft)
    }
}
