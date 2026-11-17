//! Picking another browser's sites before their sign-ins are copied in (BrowserImportSection's
//! SitePicker): a search, the sites with how many cookies each holds, and the import button.

use std::collections::HashSet;
use std::rc::Rc;

use act_core::commands::AppCommand;
use act_core::domain::terminal::{BrowserImportSite, MAX_IMPORT_SITES};
use act_core::state::BrowserImportPicker;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::field::{Checkbox, SearchField};
use act_ui::input::{InputEvent, TextInput};
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, App, Context, Entity, FontWeight, KeyDownEvent, ScrollHandle, Subscription, Window};

/// A long list draws this many rows; searching reaches the rest.
const SHOWN_SITES: usize = 200;

type Dispatch = Rc<dyn Fn(AppCommand, &mut Window, &mut App)>;

pub struct ImportPicker {
    picker: BrowserImportPicker,
    importing: bool,
    search: Entity<TextInput>,
    scroll: ScrollHandle,
    dispatch: Dispatch,
    _search_events: Subscription,
}

fn matching<'a>(sites: &'a [BrowserImportSite], filter: &str) -> Vec<&'a BrowserImportSite> {
    let query = filter.trim().to_lowercase();
    sites.iter().filter(|site| site.domain.contains(&query)).collect()
}

impl ImportPicker {
    /// `dispatch` receives the `browser-import.*` commands the picker makes.
    pub fn new(picker: BrowserImportPicker, importing: bool, dispatch: impl Fn(AppCommand, &mut Window, &mut App) + 'static, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx).placeholder("Find a site…").with_text(&picker.filter));
        let search_events = cx.subscribe_in(&search, window, |this, search, event: &InputEvent, window, cx| {
            if *event == InputEvent::Changed {
                let text = search.read(cx).text().to_string();
                if text != this.picker.filter {
                    this.send(AppCommand::BrowserImportFilter { text }, window, cx);
                }
            }
        });
        search.update(cx, |input, cx| input.focus(window, cx));
        Self { picker, importing, search, scroll: ScrollHandle::new(), dispatch: Rc::new(dispatch), _search_events: search_events }
    }

    pub fn set_state(&mut self, picker: BrowserImportPicker, importing: bool, cx: &mut Context<Self>) {
        if picker.filter != self.search.read(cx).text() {
            let filter = picker.filter.clone();
            self.search.update(cx, |input, cx| input.reset(&filter, cx));
        }
        self.picker = picker;
        self.importing = importing;
        cx.notify();
    }

    fn send(&self, command: AppCommand, window: &mut Window, cx: &mut Context<Self>) {
        let dispatch = self.dispatch.clone();
        window.defer(cx, move |window, cx| dispatch(command, window, cx));
    }

    /// Ticks every matching site the import has room for, or clears them once there is nothing left to tick.
    fn toggle_matching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sites = self.picker.sites.clone().unwrap_or_default();
        let shown = matching(&sites, &self.picker.filter);
        let selected: HashSet<&str> = self.picker.selected.iter().map(String::as_str).collect();
        let ticked = shown.iter().filter(|site| selected.contains(site.domain.as_str())).count();
        let all = !shown.is_empty() && ticked == shown.len();
        let room = MAX_IMPORT_SITES.saturating_sub(self.picker.selected.len());
        let changed: Vec<String> = if !all && room > 0 {
            shown.iter().filter(|site| !selected.contains(site.domain.as_str())).take(room).map(|site| site.domain.clone()).collect()
        } else {
            shown.iter().filter(|site| selected.contains(site.domain.as_str())).map(|site| site.domain.clone()).collect()
        };
        for site in changed {
            self.send(AppCommand::BrowserImportToggle { site }, window, cx);
        }
    }

    /// Escape empties the search first, then closes the picker.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        if self.picker.filter.is_empty() {
            self.send(AppCommand::BrowserImportChoose { source_id: None }, window, cx);
        } else {
            self.send(AppCommand::BrowserImportFilter { text: String::new() }, window, cx);
        }
    }

    fn quiet(text: impl Into<gpui::SharedString>, cx: &App) -> gpui::Div {
        let theme = cx.theme();
        div().text_color(theme.tokens.quiet_strong).text_size(px(theme.metrics.text_meta)).child(text.into())
    }

    fn site_row(&self, site: &BrowserImportSite, checked: bool, disabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let domain = site.domain.clone();
        let toggle = cx.listener(move |this, _: &bool, window, cx| this.send(AppCommand::BrowserImportToggle { site: domain.clone() }, window, cx));
        let unit = if site.cookies == 1 { "cookie" } else { "cookies" };
        let mut row = div()
            .id(gpui::SharedString::from(format!("site-{}", site.domain)))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded(px(6.0))
            .px(px(8.0))
            .py(px(6.0))
            .child(Checkbox::new(gpui::SharedString::from(format!("tick-{}", site.domain)), checked).disabled(disabled).on_change(toggle))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.metrics.text_control))
                    .text_color(if disabled { t.quiet_strong } else { t.ink_dim })
                    .child(site.domain.clone()),
            )
            .child(div().flex_none().text_size(px(theme.metrics.text_meta)).text_color(t.quiet_strong).child(format!("{} {unit}", site.cookies)));
        if !disabled {
            let domain = site.domain.clone();
            row = row
                .cursor_pointer()
                .hover(|style| style.bg(t.control_hover))
                .on_click(cx.listener(move |this, _, window, cx| this.send(AppCommand::BrowserImportToggle { site: domain.clone() }, window, cx)));
        }
        row
    }
}

impl Render for ImportPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let t = theme.tokens.clone();
        let text_control = theme.metrics.text_control;
        let text_meta = theme.metrics.text_meta;
        let count = self.picker.selected.len();
        let mut picker = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .pb(px(19.0))
            .pl(px(35.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.escape(window, cx);
                }
            }));
        match &self.picker.sites {
            None => picker = picker.child(Self::quiet("Reading sites…", cx)),
            Some(sites) if sites.is_empty() => picker = picker.child(Self::quiet("This profile holds no sign-ins.", cx)),
            Some(sites) => {
                let sites = sites.clone();
                let shown = matching(&sites, &self.picker.filter);
                let selected: HashSet<&str> = self.picker.selected.iter().map(String::as_str).collect();
                let ticked = shown.iter().filter(|site| selected.contains(site.domain.as_str())).count();
                let all = !shown.is_empty() && ticked == shown.len();
                let full = count >= MAX_IMPORT_SITES;
                let mut list = div()
                    .id("browser-import-sites")
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .max_h(px(280.0))
                    .border_1()
                    .border_color(t.line)
                    .rounded(px(8.0))
                    .p(px(4.0));
                if shown.len() > 1 {
                    let label = if self.picker.filter.trim().is_empty() { "All sites" } else { "All matching" };
                    list = list.child(
                        div()
                            .id("browser-import-every")
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .mb(px(4.0))
                            .border_b_1()
                            .border_color(t.line)
                            .rounded_t(px(6.0))
                            .px(px(8.0))
                            .py(px(8.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.control_hover))
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_matching(window, cx)))
                            .child(Checkbox::new("browser-import-every-tick", all).on_change(cx.listener(|this, _: &bool, window, cx| this.toggle_matching(window, cx))))
                            .child(div().flex_1().text_size(px(text_control)).text_color(t.ink).font_weight(FontWeight::SEMIBOLD).child(label))
                            .child(div().text_size(px(text_meta)).text_color(t.quiet_strong).child(format!("{ticked} of {}", shown.len()))),
                    );
                }
                for site in shown.iter().take(SHOWN_SITES) {
                    let checked = selected.contains(site.domain.as_str());
                    list = list.child(self.site_row(site, checked, full && !checked, cx));
                }
                if shown.is_empty() {
                    list = list.child(Self::quiet("No site matches.", cx).px(px(8.0)).py(px(6.0)));
                }
                picker = picker.child(SearchField::new("browser-import-search", &self.search)).child(list);
                if shown.len() > SHOWN_SITES {
                    picker = picker.child(Self::quiet(format!("Showing {SHOWN_SITES} of {}. Search to find the rest.", shown.len()), cx));
                }
            }
        }
        let run_label = if self.importing {
            "Importing…".to_string()
        } else if count == 0 {
            "Import".to_string()
        } else {
            format!("Import {count} {}", if count == 1 { "site" } else { "sites" })
        };
        picker.child(
            div()
                .flex()
                .justify_end()
                .items_center()
                .gap(px(8.0))
                .child(Button::new("browser-import-cancel", "Cancel").disabled(self.importing).on_click(cx.listener(|this, _, window, cx| {
                    this.send(AppCommand::BrowserImportChoose { source_id: None }, window, cx)
                })))
                .child(
                    Button::new("browser-import-run", run_label)
                        .variant(ButtonVariant::Primary)
                        .busy(self.importing)
                        .disabled(self.importing || count == 0)
                        .on_click(cx.listener(|this, _, window, cx| this.send(AppCommand::BrowserImportRun, window, cx))),
                ),
        )
    }
}
