//! Browser (BrowserSettings.tsx, BrowserImportSection.tsx): whether the agent may drive the browser
//! panel, sign-ins copied from another browser, and the session the panel keeps.

use act_core::domain::terminal::MAX_IMPORT_SITES;
use act_core::reducer::devices::source_name;
use act_core::state::{BrowserImportPicker, WorkspaceState};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::{Checkbox, SearchField, Switch};
use act_ui::input::InputEvent;
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, KeyDownEvent, SharedString, Window};

use super::{description, empty, error, page, spinning_button, status, Held, Marks, Row, Section};
use super::Clip;
use crate::views::root::Root;
use crate::workspace::dispatch;

/// A long list draws this many rows; searching reaches the rest.
const SHOWN_SITES: usize = 200;

#[derive(Default)]
pub struct BrowserState {
    /// The site search, made for the picker of one source.
    filter: Option<(String, Held)>,
}

fn quiet(text: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().text_color(t.quiet_strong).text_size(px(12.0)).child(text.into()).into_any_element()
}

fn site_picker(root: &mut Root, picker: &BrowserImportPicker, importing: bool, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let query = picker.filter.trim().to_lowercase();
    let sites = picker.sites.clone().unwrap_or_default();
    let matching: Vec<_> = sites.iter().filter(|site| site.domain.contains(&query)).collect();
    let selected: std::collections::HashSet<&str> = picker.selected.iter().map(String::as_str).collect();
    let count = picker.selected.len();
    let ticked = matching.iter().filter(|site| selected.contains(site.domain.as_str())).count();
    let all = !matching.is_empty() && ticked == matching.len();
    let room = MAX_IMPORT_SITES.saturating_sub(count);
    let full = room == 0;

    let mut body = div().flex().flex_col().gap(px(10.0)).pb(px(19.0)).pl(px(35.0));
    match &picker.sites {
        None => body = body.child(quiet("Reading sites…", &t)),
        Some(list) if list.is_empty() => body = body.child(quiet("This profile holds no sign-ins.", &t)),
        Some(_) => {
            let key = picker.source_id.clone();
            if root.settings.browser.filter.as_ref().is_none_or(|(source, _)| *source != key) {
                let held = Held::new(&picker.filter, "Find a site…", |_, input, event, cx| {
                    if *event == InputEvent::Changed {
                        dispatch(AppCommand::BrowserImportFilter { text: input.read(cx).text().to_string() }, cx);
                    }
                }, cx);
                root.pending_focus = Some(held.input.clone());
                root.settings.browser.filter = Some((key, held));
            }
            let input = root.settings.browser.filter.as_ref().map(|(_, held)| held.input.clone()).expect("made above");
            if input.read(cx).text() != picker.filter {
                let text = picker.filter.clone();
                input.update(cx, |input, cx| input.reset(&text, cx));
            }
            body = body.child(SearchField::new("browser-import-filter", &input));
            let row = |id: SharedString| div().id(id).flex().items_center().gap(px(10.0)).rounded(px(6.0)).px(px(8.0)).py(px(6.0));
            let mut list = div().id("browser-import-sites").accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "Sites")).max_h(px(280.0)).overflow_y_scroll().border_1().border_color(t.line).rounded(px(8.0)).p(px(4.0)).flex().flex_col();
            if matching.len() > 1 {
                let toggle: Vec<String> = if !all && room > 0 {
                    matching.iter().filter(|site| !selected.contains(site.domain.as_str())).take(room).map(|site| site.domain.clone()).collect()
                } else {
                    matching.iter().filter(|site| selected.contains(site.domain.as_str())).map(|site| site.domain.clone()).collect()
                };
                let flip = toggle.clone();
                let every = format!("{} {ticked} of {}", if query.is_empty() { "All sites" } else { "All matching" }, matching.len());
                list = list.child(
                    row("browser-import-every".into())
                        // A label around its checkbox: the box takes the keyboard and reads the row.
                        .skip_tab_stop()
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
                        .mb(px(4.0))
                        .border_b_1()
                        .border_color(t.line)
                        .py(px(8.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.control_hover))
                        .on_click(move |_, _, cx| {
                            for site in &flip {
                                dispatch(AppCommand::BrowserImportToggle { site: site.clone() }, cx);
                            }
                        })
                        .child(Checkbox::new("browser-import-every-box", all).label(every).mixed(ticked > 0 && !all).accent(true).on_change(move |_, _, cx| {
                            for site in &toggle {
                                dispatch(AppCommand::BrowserImportToggle { site: site.clone() }, cx);
                            }
                        }))
                        .child(div().flex_1().min_w_0().text_color(t.ink).font_weight(FontWeight(560.0)).text_size(px(14.0)).child(if query.is_empty() { "All sites" } else { "All matching" }))
                        .child(div().text_color(t.quiet_strong).text_size(px(12.0)).child(format!("{ticked} of {}", matching.len()))),
                );
            }
            for site in matching.iter().take(SHOWN_SITES) {
                let on = selected.contains(site.domain.as_str());
                let disabled = full && !on;
                let domain = site.domain.clone();
                let toggle = domain.clone();
                let label = format!("{} {} {}", site.domain, site.cookies, if site.cookies == 1 { "cookie" } else { "cookies" });
                list = list.child(
                    row(SharedString::from(format!("browser-import-site-{}", site.domain)))
                        .skip_tab_stop()
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
                        .when(!disabled, |row| row.cursor_pointer().hover(|style| style.bg(t.control_hover)).on_click(move |_, _, cx| dispatch(AppCommand::BrowserImportToggle { site: toggle.clone() }, cx)))
                        .child(Checkbox::new(SharedString::from(format!("browser-import-box-{}", site.domain)), on).label(label).accent(true).disabled(disabled).on_change(move |_, _, cx| dispatch(AppCommand::BrowserImportToggle { site: domain.clone() }, cx)))
                        .child(div().flex_1().min_w_0().clip().text_size(px(14.0)).text_color(if disabled { t.quiet_strong } else { t.ink_dim }).child(site.domain.clone()))
                        .child(div().text_color(t.quiet_strong).text_size(px(12.0)).child(format!("{} {}", site.cookies, if site.cookies == 1 { "cookie" } else { "cookies" }))),
                );
            }
            if matching.is_empty() {
                list = list.child(div().px(px(8.0)).py(px(6.0)).child(quiet("No site matches.", &t)));
            }
            body = body.child(list);
            if matching.len() > SHOWN_SITES {
                body = body.child(quiet(format!("Showing {SHOWN_SITES} of {}. Search to find the rest.", matching.len()), &t));
            }
        }
    }
    let run_label = if importing {
        "Importing…".to_string()
    } else if count == 0 {
        "Import".to_string()
    } else {
        format!("Import {count} {}", if count == 1 { "site" } else { "sites" })
    };
    body = body.child(
        div()
            .flex()
            .justify_end()
            .items_center()
            .gap(px(8.0))
            .child(Button::new("browser-import-cancel", "Cancel").disabled(importing).on_click(|_, _, cx| dispatch(AppCommand::BrowserImportChoose { source_id: None }, cx)))
            .child(Button::new("browser-import-run", run_label).variant(ButtonVariant::Primary).disabled(importing || count == 0).on_click(|_, _, cx| dispatch(AppCommand::BrowserImportRun, cx))),
    );
    let filter = picker.filter.clone();
    div()
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if event.keystroke.key != "escape" || importing {
                return;
            }
            cx.stop_propagation();
            if filter.is_empty() {
                dispatch(AppCommand::BrowserImportChoose { source_id: None }, cx);
            } else {
                dispatch(AppCommand::BrowserImportFilter { text: String::new() }, cx);
            }
        })
        .child(body)
        .into_any_element()
}

fn import_section(root: &mut Root, state: &WorkspaceState, marks: &Marks, cx: &mut Context<Root>) -> Section {
    let t = cx.theme().tokens.clone();
    let import = &state.browser_import;
    let mut section = Section::setting("browser.import").description(description("Copy sign-ins from another browser for the sites you pick. Passwords are never copied."));
    if let Some(notice) = &import.notice {
        section = section.row(div().border_t_1().border_color(t.line).py(px(13.0)).text_color(t.success_soft).text_size(px(14.0)).child(notice.clone()));
    }
    if let Some(message) = &import.error {
        section = section.row(error(message.clone(), &t));
    }
    match &import.sources {
        None => section = section.row(empty("Looking for browsers…", &t)),
        Some(sources) if sources.is_empty() => section = section.row(empty("No other browsers found on this computer.", &t)),
        Some(sources) => {
            let mut picking_any = false;
            for source in sources {
                let picking = import.picker.as_ref().is_some_and(|picker| picker.source_id == source.id);
                picking_any |= picking;
                let last = import.last.as_ref().filter(|last| last.source_id == source.id);
                let shown = sources.iter().filter(|item| item.browser == source.browser).count() > 1;
                let mut row = Row::new(if shown { source_name(Some(source), sources) } else { source.browser.clone() }, &t).lead(status(last.is_some(), true, None, &t));
                if let Some(last) = last {
                    let what = if last.sites.len() == 1 { last.sites[0].clone() } else { format!("{} sites", last.sites.len()) };
                    row = row.line(format!("Last imported {what}."), &t);
                }
                if !picking {
                    if last.is_some() {
                        row = row.action(spinning_button(format!("browser-import-again-{}", source.id), if import.importing { "Importing…" } else { "Import again" }, IconName::RotateCw, import.importing).disabled(import.importing).on_click(|_, _, cx| dispatch(AppCommand::BrowserImportAgain, cx)));
                    }
                    let id = source.id.clone();
                    row = row.action(Button::new(SharedString::from(format!("browser-import-choose-{}", source.id)), "Choose sites").disabled(import.importing).on_click(move |_, _, cx| dispatch(AppCommand::BrowserImportChoose { source_id: Some(id.clone()) }, cx)));
                }
                section = section.row(row.build(marks, &t));
                if picking {
                    let picker = import.picker.clone().expect("checked above");
                    section = section.row(site_picker(root, &picker, import.importing, cx));
                }
            }
            if !picking_any {
                root.settings.browser.filter = None;
            }
        }
    }
    section
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let enabled = state.browser_tools;
    let availability = Section::new("Availability").first().row(
        Row::setting("browser.availability", &t)
            .line("The agent can open and read pages in the browser panel. Off leaves the panel to you alone.", &t)
            .on_title_click(move |_, cx| dispatch(AppCommand::SetBrowserTools { enabled: !enabled }, cx))
            .action(Switch::new("browser-tools-switch", enabled, "Browser use").track_focus(&marks.stop("browser.availability", cx)).on_change(|value: &bool, _, cx| dispatch(AppCommand::SetBrowserTools { enabled: *value }, cx)))
            .build(marks, &t),
    );
    let import = import_section(root, state, marks, cx);
    let origins = &state.browser_origins;
    let armed = marks.armed("browser-clear");
    let allowed = format!("{} {}", origins.len(), if origins.len() == 1 { "site allowed" } else { "sites allowed" });
    let mut session = Section::setting("browser.session")
        .description(description("Signing out clears every cookie, cache, and stored login, and takes back the sites the agent may open on its own."))
        .action(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .when(!armed.get(), |action| action.child(div().text_color(t.quiet_strong).text_size(px(12.0)).whitespace_nowrap().child(allowed)))
                .child(ConfirmAction::new("browser-clear", "Clear browser data", "Sign out of everything", |_, cx| dispatch(AppCommand::BrowserClearData, cx)).variant(ButtonVariant::Danger).armed_into(&armed).gap(px(10.0))),
        );
    if origins.is_empty() {
        session = session.row(empty("The agent has to ask before it opens any site.", &t));
    }
    for origin in origins {
        session = session.row(Row::titled(div().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(570.0)).child(origin.clone()).into_any_element()).build(marks, &t));
    }
    page("Browser", Some(description(concat!("The browser panel keeps one session for the whole app, so a site you sign into stays signed in everywhere ", act_core::app_name!(), " works."))), None, window, cx)
        .child(availability.build(marks, &t))
        .child(import.build(marks, &t))
        .child(session.build(marks, &t))
        .into_any_element()
}
