//! The `/` and `@` menus the prompt opens: the engine's commands and skills with the app's own
//! actions, and the threads a draft can name (ComposerMenus.tsx, use-agent-commands.ts).

use act_core::domain::engine::AgentEngine;
use act_core::reducer::sending::prompt_key;
use act_core::state::WorkspaceState;
use act_core::workspace::handles::{handle_token_at, rank_thread_handles, thread_handle_options, ThreadHandleOption};
use act_core::workspace::location::thread_workspace_id;
use act_core::AppCommand;
use crate::effects::window_desktop::AvailableCommand;
use act_ui::components::chip::Chip;
use act_ui::components::scrollbar::{gutter, Scrollbar};
use act_ui::style::{glass, Corners, GlassKind, PaneFill, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{deferred, div, prelude::*, px, AnyElement, Context, FontWeight, MouseButton, ScrollHandle, SharedString};

use super::common::icon;
use super::root::Root;
use crate::workspace::dispatch;

/// How many threads the `@` menu shows at once; the query is what reaches past them.
const THREAD_MENU_LIMIT: usize = 8;

/// The commands an engine offers in a checkout, read as the checkout or engine changes and again
/// whenever the window comes back.
#[derive(Default)]
pub struct Commands {
    key: Option<(Option<String>, AgentEngine)>,
    pub list: Vec<AvailableCommand>,
    pub loading: bool,
    generation: u64,
    /// Bumped when the list itself changes, which puts the highlight back on the first row.
    version: u64,
    pub stale: bool,
}

#[derive(Default)]
pub struct MenuState {
    pub commands: Commands,
    selected_command: usize,
    /// The prompt and command list the highlight belongs to; either changing puts it back on the first row.
    command_seen: (String, u64),
    selected_thread: usize,
    thread_seen: Option<String>,
    command_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
}

#[derive(Clone, PartialEq)]
enum AppAction {
    Compact,
    Review,
    /// A dock launcher's command: the `/` menu is the dock registry, so a view added there is reachable here too.
    Launch(serde_json::Value),
}

#[derive(Clone)]
struct Entry {
    name: String,
    description: String,
    argument_hint: String,
    aliases: Vec<String>,
    action: Option<AppAction>,
}

/// The `/` command that runs a dock launcher, as dock-registry.tsx names them.
fn launcher_command(id: &str) -> Option<&'static str> {
    Some(match id {
        "agents" => "subagents",
        "diff" => "diff",
        "automation" => "automation",
        "browser" => "browser",
        "simulator" => "simulator",
        "terminal" => "terminal",
        "side-chat" => "side",
        _ => return None,
    })
}

/// The `/word` the caret sits in, as its lowercased query and where its `/` starts. A `/` only
/// starts one after whitespace, which keeps paths and URLs from opening the menu.
pub fn command_token_at(text: &str, caret: usize) -> Option<(String, usize)> {
    let before = &text[..caret.min(text.len())];
    let slash = before.rfind('/')?;
    let query = &before[slash + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    if slash > 0 && !before[..slash].chars().next_back().is_some_and(char::is_whitespace) {
        return None;
    }
    Some((query.to_lowercase(), slash))
}

fn short_description(description: &str) -> String {
    let mut end = description.len();
    for (index, ch) in description.char_indices() {
        if matches!(ch, '.' | '!' | '?') && description[index + ch.len_utf8()..].starts_with(char::is_whitespace) {
            end = index + ch.len_utf8();
            break;
        }
    }
    let mut first = description[..end].to_string();
    if first.ends_with(')') {
        if let Some(open) = first.rfind(" (") {
            first.truncate(open);
        }
    }
    if first.chars().count() > 110 {
        let cut: String = first.chars().take(107).collect();
        format!("{}…", cut.trim_end())
    } else {
        first
    }
}

fn workspace_id(root: &Root, state: &WorkspaceState) -> Option<String> {
    match root.composer.thread(state) {
        Some(thread) => thread_workspace_id(state, Some(thread)),
        None => state.draft_project_id.as_deref().and_then(|id| state.project(id)).and_then(|project| project.workspace_id.clone()),
    }
}

/// Reads the engine's commands when the checkout or engine changed, or the window came back.
pub fn ensure_commands(root: &mut Root, state: &WorkspaceState, cx: &mut Context<Root>) {
    let engine = root.composer.thread(state).map(|thread| thread.engine).unwrap_or(state.draft_engine);
    let key = (workspace_id(root, state), engine);
    let commands = &mut root.composer.menus.commands;
    if commands.key.as_ref() == Some(&key) && !commands.stale {
        return;
    }
    if commands.key.as_ref() != Some(&key) {
        commands.list.clear();
        commands.version += 1;
    }
    commands.stale = false;
    commands.key = Some(key.clone());
    commands.loading = true;
    commands.generation += 1;
    let generation = commands.generation;
    let (id, engine) = key;
    // A checkout's commands are read on the computer holding it.
    let read = crate::effects::window_desktop::commands(id, engine, cx);
    let scope = root.composer.task.clone();
    cx.spawn(async move |root, cx| {
        let result = read.await;
        let _ = root.update(cx, |root, cx| {
            root.in_composer(scope.as_deref(), |root| {
                let commands = &mut root.composer.menus.commands;
                if commands.generation != generation {
                    return;
                }
                commands.loading = false;
                let list = result.unwrap_or_default();
                if list != commands.list {
                    commands.list = list;
                    commands.version += 1;
                }
                cx.notify();
            });
        });
    })
    .detach();
}

enum Open {
    Commands { entries: Vec<Entry>, start: usize, query_len: usize },
    Threads { options: Vec<ThreadHandleOption>, any: bool, start: usize, query_len: usize },
}

fn char_to_byte(text: &str, chars: usize) -> usize {
    text.char_indices().nth(chars).map(|(at, _)| at).unwrap_or(text.len())
}

fn open_menu(root: &Root, state: &WorkspaceState) -> Option<Open> {
    let composer = &root.composer;
    let prompt = composer.text();
    if !composer.focused || composer.dismissed.as_deref() == Some(prompt) {
        return None;
    }
    let caret = composer.caret.min(prompt.len());
    if let Some((query, start)) = command_token_at(prompt, caret) {
        let offered = start == 0 && prompt[1 + query.len()..].trim().is_empty();
        let thread = composer.thread(state).filter(|_| composer.task.is_none());
        let settled = thread.filter(|thread| !state.active_runs.contains_key(&thread.id)).filter(|thread| act_core::workspace::activity::waiting_on(state, Some(&thread.id), &prompt_key(state)).is_none());
        let mut actions = Vec::new();
        if offered {
            if settled.is_some_and(|thread| act_core::reducer::runs::compactable_session(state, thread)) {
                actions.push(Entry { name: "compact".into(), description: "Compact the current chat's context.".into(), argument_hint: String::new(), aliases: Vec::new(), action: Some(AppAction::Compact) });
            }
            if settled.is_some_and(|thread| act_core::reducer::runs::reviewable_session(state, thread)) {
                actions.push(Entry { name: "review".into(), description: "Review changes in the current project.".into(), argument_hint: String::new(), aliases: Vec::new(), action: Some(AppAction::Review) });
            }
            if composer.task.is_none() {
                let launchers = super::dock::launchers(state).into_iter().filter(|launcher| launcher.missing.is_none());
                actions.extend(launchers.filter_map(|launcher| {
                    Some(Entry { name: launcher_command(launcher.id)?.into(), description: launcher.description, argument_hint: String::new(), aliases: Vec::new(), action: Some(AppAction::Launch(launcher.command)) })
                }));
            }
        }
        let taken: Vec<String> = actions.iter().map(|action| action.name.clone()).collect();
        let skills = composer.menus.commands.list.iter().filter(|command| !taken.contains(&command.name)).map(|command| Entry {
            name: command.name.clone(),
            description: command.description.clone(),
            argument_hint: command.argument_hint.clone(),
            aliases: command.aliases.clone().unwrap_or_default(),
            action: None,
        });
        let entries: Vec<Entry> = actions
            .into_iter()
            .chain(skills)
            .filter(|entry| entry.name.to_lowercase().starts_with(&query) || entry.aliases.iter().any(|alias| alias.to_lowercase().starts_with(&query)))
            .collect();
        return Some(Open::Commands { query_len: query.len(), entries, start });
    }
    let chars = prompt[..caret].chars().count();
    let (query, start_chars) = handle_token_at(prompt, chars)?;
    let options = thread_handle_options(state, &composer.key(state));
    let matching = rank_thread_handles(&options, &query, THREAD_MENU_LIMIT);
    let start = char_to_byte(prompt, start_chars);
    Some(Open::Threads { any: !options.is_empty(), options: matching, query_len: query.len(), start })
}

impl MenuState {
    fn command_index(&self, prompt: &str, len: usize) -> usize {
        if self.command_seen.0 != prompt || self.command_seen.1 != self.commands.version || self.selected_command >= len {
            0
        } else {
            self.selected_command
        }
    }

    fn thread_index(&self, query: &str, len: usize) -> usize {
        if self.thread_seen.as_deref() != Some(query) || self.selected_thread >= len {
            0
        } else {
            self.selected_thread
        }
    }
}

impl Root {
    /// Whether the `/` or `@` menu stands open over the transcript.
    pub fn composer_menu_open(&self, cx: &gpui::App) -> bool {
        open_menu(self, &self.state(cx)).is_some()
    }

    pub fn command_menu_open(&self, cx: &gpui::App) -> bool {
        matches!(open_menu(self, &self.state(cx)), Some(Open::Commands { .. }))
    }

    /// Moves the highlight in whichever menu is open. Answers whether a menu took the key.
    pub fn menu_step(&mut self, step: i64, cx: &mut Context<Root>) -> bool {
        let state = self.state(cx);
        let prompt = self.composer.text().to_string();
        match open_menu(self, &state) {
            Some(Open::Commands { entries, .. }) if !entries.is_empty() => {
                let len = entries.len() as i64;
                let current = self.composer.menus.command_index(&prompt, entries.len()) as i64;
                self.composer.menus.selected_command = ((current + step).rem_euclid(len)) as usize;
                self.composer.menus.command_seen = (prompt, self.composer.menus.commands.version);
                self.composer.menus.command_scroll.scroll_to_item(self.composer.menus.selected_command);
                cx.notify();
                true
            }
            Some(Open::Threads { options, start, query_len, .. }) if !options.is_empty() => {
                let query = prompt[start + 1..start + 1 + query_len].to_lowercase();
                let len = options.len() as i64;
                let current = self.composer.menus.thread_index(&query, options.len()) as i64;
                let next = ((current + step).rem_euclid(len)) as usize;
                self.composer.menus.selected_thread = next;
                self.composer.menus.thread_seen = Some(query);
                // The line before the first thread from elsewhere is a child of the list too.
                let ruled = options.iter().position(|option| !option.in_scope).is_some_and(|first| first > 0 && next >= first);
                self.composer.menus.thread_scroll.scroll_to_item(next + usize::from(ruled));
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Takes the highlighted row of whichever menu is open. Answers whether a menu took the key.
    pub fn menu_choose(&mut self, cx: &mut Context<Root>) -> bool {
        let state = self.state(cx);
        let prompt = self.composer.text().to_string();
        match open_menu(self, &state) {
            Some(Open::Commands { entries, start, query_len }) if !entries.is_empty() => {
                let index = self.composer.menus.command_index(&prompt, entries.len());
                self.choose_command(entries[index].clone(), start, query_len, cx);
                true
            }
            Some(Open::Threads { options, start, query_len, .. }) if !options.is_empty() => {
                let query = prompt[start + 1..start + 1 + query_len].to_lowercase();
                let index = self.composer.menus.thread_index(&query, options.len());
                self.choose_thread(&options[index], start, query_len, cx);
                true
            }
            _ => false,
        }
    }

    /// Closes whichever menu is open until the prompt changes again.
    pub fn menu_dismiss(&mut self, cx: &mut Context<Root>) -> bool {
        if open_menu(self, &self.state(cx)).is_none() {
            return false;
        }
        self.composer.dismissed = Some(self.composer.text().to_string());
        cx.notify();
        true
    }

    fn choose_command(&mut self, entry: Entry, start: usize, query_len: usize, cx: &mut Context<Root>) {
        if let Some(action) = entry.action {
            self.composer.dismissed = None;
            self.composer.input.update(cx, |input, cx| input.set_text("", cx));
            let task_id = self.composer.task_id(&self.state(cx));
            match action {
                AppAction::Compact => dispatch(AppCommand::RunCompact { task_id }, cx),
                AppAction::Review => dispatch(AppCommand::ReviewOpen { task_id }, cx),
                AppAction::Launch(command) => super::dock::launch(&command, cx),
            }
            return;
        }
        let inserted = format!("/{}{}", entry.name, if entry.argument_hint.is_empty() { "" } else { " " });
        let end = (start + 1 + query_len).min(self.composer.text().len());
        self.composer.input.update(cx, |input, cx| input.replace_range(start..end, &inserted, cx));
        self.composer.dismissed = Some(self.composer.input.read(cx).text().to_string());
        cx.notify();
    }

    fn choose_thread(&mut self, option: &ThreadHandleOption, start: usize, query_len: usize, cx: &mut Context<Root>) {
        let inserted = format!("@{} ", option.handle);
        let end = (start + 1 + query_len).min(self.composer.text().len());
        self.composer.input.update(cx, |input, cx| input.replace_range(start..end, &inserted, cx));
        self.composer.dismissed = Some(self.composer.input.read(cx).text().to_string());
        cx.notify();
    }
}

fn heading(glyph: IconName, label: &'static str, cx: &Context<Root>) -> gpui::Div {
    let t = cx.theme().tokens.clone();
    div().flex_none().flex().items_center().gap(px(7.0)).pt(px(11.0)).px(px(13.0)).pb(px(9.0)).border_b_1().border_color(t.line).text_color(t.quiet).text_size(px(cx.theme().metrics.text_meta)).font_weight(FontWeight(500.0)).child(icon(glyph, 14.0, t.quiet)).child(label)
}

/// A row of a composer menu: a mark, a title over a line of detail, and a tag.
#[allow(clippy::too_many_arguments)]
fn option_row(id: SharedString, selected: bool, mark: IconName, mark_tone: Mark, title: AnyElement, detail: AnyElement, tag: Option<String>, cx: &Context<Root>) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let (line, fill, color) = match mark_tone {
        Mark::App => (t.accent_line, t.accent_chip, t.accent_soft),
        Mark::Skill => (t.line_mid, t.control, t.muted),
        Mark::Thread => (t.line_mid, t.control, t.quiet_strong),
        Mark::Running => (t.accent_line, t.control, t.accent_soft),
    };
    let mut row = div()
        .id(id)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListItem, "").selected(selected))
        .relative()
        .w_full()
        .flex()
        .items_center()
        .gap(px(10.0))
        .min_h(px(56.0))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(10.0))
        .cursor_pointer()
        .child(div().relative().flex_none().size(px(30.0)).mr(px(4.0)).flex().items_center().justify_center().rounded(px(9.0)).border_1().border_color(line).bg(fill).rim(t.glass_rim, Corners::Radius(9.0)).child(icon(mark, 16.0, color)))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.0)).child(title).child(detail));
    if selected {
        row = row.pane(t.glass_pick).rim(t.glass_rim, Corners::Radius(10.0));
    }
    if let Some(tag) = tag {
        row = row.child(Chip::tag(tag));
    }
    row
}

/// A menu's list, shrinking to the room under the card's cap, with the scrollbar laid over it.
fn scrolling(list: gpui::Stateful<gpui::Div>, bar: Scrollbar) -> gpui::Div {
    div().relative().flex().flex_col().flex_shrink().min_h_0().child(list.flex_shrink().min_h_0()).child(bar)
}

enum Mark {
    App,
    Skill,
    Thread,
    Running,
}

/// The menu standing over the composer, rising from its top edge.
pub fn render(root: &mut Root, state: &WorkspaceState, window: &gpui::Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    if state.review_picker.is_some() && root.composer.task.is_none() {
        return None;
    }
    let open = open_menu(root, state)?;
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let mono = theme.typography.mono_family.clone();
    let viewport_room = px(390.0f32.min(f32::from(window.viewport_size().height) - 250.0));
    let prompt = root.composer.text().to_string();
    let pane = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0);
    let room = |tag: Option<&str>| pane - 12.0 - 20.0 - 34.0 - 20.0 - tag.map(|tag| super::text_fit::line_width(tag, m.text_badge, window) + 22.0).unwrap_or(0.0);
    let card = glass(div(), t, GlassKind::Popover, Some(15.0), None).id("composer-menu").map(act_ui::style::glass_in).flex().flex_col().max_h(viewport_room).overflow_hidden();
    let card = match open {
        Open::Commands { entries, .. } => {
            let selected = root.composer.menus.command_index(&prompt, entries.len());
            let scroll = root.composer.menus.command_scroll.clone();
            let mut list = div().id("slash-command-list").flex().flex_col().p(px(6.0)).pr(px(6.0) + gutter(&scroll)).overflow_y_scroll().track_scroll(&scroll);
            for (index, entry) in entries.iter().enumerate() {
                let description = short_description(&entry.description);
                let full = entry.description.trim().to_string();
                let app = entry.action.is_some();
                let fitted = super::text_fit::fit_line(&description, room(Some(if app { act_core::identity::APP_NAME } else { "Skill" })), m.text_meta, window);
                let name = format!("/{}", entry.name);
                let name_width = name.chars().count() as f32 * m.text_control * 0.62;
                let room_left = room(Some(if app { act_core::identity::APP_NAME } else { "Skill" }));
                let mut title = div().flex().min_w_0().gap(px(6.0)).whitespace_nowrap().overflow_hidden().font_family(mono.clone()).text_size(px(m.text_control)).line_height(gpui::relative(1.25)).child(div().flex_none().font_weight(FontWeight(550.0)).text_color(if index == selected { t.ink } else { t.ink_soft }).child(name));
                if !entry.argument_hint.is_empty() {
                    let hint_room = ((room_left - name_width - 6.0) / (m.text_control * 0.62)).floor().max(0.0) as usize;
                    let hint: String = if entry.argument_hint.chars().count() > hint_room { entry.argument_hint.chars().take(hint_room.saturating_sub(1)).collect::<String>() + "…" } else { entry.argument_hint.clone() };
                    title = title.child(div().flex_none().font_weight(FontWeight(450.0)).text_color(t.quiet_strong).child(hint));
                }
                let detail = div().whitespace_nowrap().overflow_hidden().text_size(px(m.text_meta)).line_height(gpui::relative(1.3)).text_color(if index == selected { t.ink_dim } else { t.quiet_strong }).child(fitted);
                let chosen = entry.clone();
                let (start, query_len) = command_token_at(&prompt, root.composer.caret).map(|(query, start)| (start, query.len())).unwrap_or((0, 0));
                let mut row = option_row(SharedString::from(format!("slash-command-{index}")), index == selected, if app { IconName::Command } else { IconName::Sparkles }, if app { Mark::App } else { Mark::Skill }, title.into_any_element(), detail.into_any_element(), Some(if app { "AI Coding Tool".into() } else { "Skill".into() }), cx)
                    .on_hover(root.composer_listener(cx, move |root, hovered: &bool, _, cx| {
                        if *hovered {
                            let prompt = root.composer.text().to_string();
                            root.composer.menus.selected_command = index;
                            root.composer.menus.command_seen = (prompt, root.composer.menus.commands.version);
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        root.composer_listener(cx, move |root, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            root.choose_command(chosen.clone(), start, query_len, cx);
                        }),
                    );
                if description != full {
                    row = row.tip(full);
                }
                list = list.child(row);
            }
            if entries.is_empty() {
                list = list.child(div().py(px(18.0)).px(px(12.0)).text_size(px(m.text_control)).text_color(t.quiet).flex().justify_center().child("No matching commands"));
            }
            let mut card = card.accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Slash commands")).child(heading(IconName::Command, "Commands", cx)).child(scrolling(list, Scrollbar::vertical("slash-command-scrollbar", &scroll)));
            if root.composer.menus.commands.loading {
                card = card.child(div().flex_none().pt(px(8.0)).px(px(13.0)).pb(px(10.0)).border_t_1().border_color(t.line).text_color(t.quiet).text_size(px(m.text_meta)).child("Loading installed skills…"));
            }
            card
        }
        Open::Threads { options, any, start, query_len } => {
            let query = prompt[start + 1..start + 1 + query_len].to_lowercase();
            let selected = root.composer.menus.thread_index(&query, options.len());
            let first_elsewhere = options.iter().position(|option| !option.in_scope);
            let scroll = root.composer.menus.thread_scroll.clone();
            let mut list = div().id("thread-mention-list").flex().flex_col().p(px(6.0)).pr(px(6.0) + gutter(&scroll)).overflow_y_scroll().track_scroll(&scroll);
            for (index, option) in options.iter().enumerate() {
                let tag_text = (!option.in_scope).then(|| option.project.clone().unwrap_or_else(|| "No project".into()));
                let fitted = super::text_fit::fit_line(&option.title, room(tag_text.as_deref()), m.text_control, window);
                let title = div().whitespace_nowrap().overflow_hidden().text_size(px(m.text_control)).font_weight(FontWeight(550.0)).text_color(if index == selected { t.ink } else { t.ink_soft }).child(fitted);
                let mut detail = div().flex().min_w_0().text_size(px(m.text_meta)).line_height(gpui::relative(1.3)).child(div().flex_none().whitespace_nowrap().font_family(mono.clone()).text_color(if index == selected { t.ink_dim } else { t.quiet_strong }).child(format!("@{}", option.handle)));
                if option.running {
                    detail = detail.child(div().flex_none().text_color(t.accent_text).font_weight(FontWeight(550.0)).child(" · Running"));
                }
                let chosen = option.clone();
                let tag = (!option.in_scope).then(|| option.project.clone().unwrap_or_else(|| "No project".into()));
                let row = option_row(SharedString::from(format!("thread-mention-{index}")), index == selected, IconName::MessagesSquare, if option.running { Mark::Running } else { Mark::Thread }, title.into_any_element(), detail.into_any_element(), tag, cx)
                    .on_hover(root.composer_listener(cx, {
                        let query = query.clone();
                        move |root, hovered: &bool, _, cx| {
                            if *hovered {
                                root.composer.menus.selected_thread = index;
                                root.composer.menus.thread_seen = Some(query.clone());
                                cx.notify();
                            }
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        root.composer_listener(cx, move |root, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            root.choose_thread(&chosen, start, query_len, cx);
                        }),
                    );
                if Some(index) == first_elsewhere && index > 0 {
                    list = list.child(div().mt(px(6.0)).mb(px(6.0)).h(px(1.0)).bg(t.line));
                }
                list = list.child(row);
            }
            if options.is_empty() {
                list = list.child(div().py(px(18.0)).px(px(12.0)).text_size(px(m.text_control)).text_color(t.quiet).flex().justify_center().child(if any { "No thread matches" } else { "No other threads yet" }));
            }
            card.accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Threads")).child(heading(IconName::MessagesSquare, "Threads", cx)).child(scrolling(list, Scrollbar::vertical("thread-mention-scrollbar", &scroll)))
        }
    };
    let card = card.on_mouse_down(MouseButton::Left, |_, window, cx| {
        window.prevent_default();
        cx.stop_propagation();
    });
    Some(deferred(div().absolute().left_0().right_0().bottom(gpui::relative(1.0)).pb(px(9.0)).child(card)).with_priority(2).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_opens_only_after_whitespace() {
        assert_eq!(command_token_at("/rev", 4), Some(("rev".into(), 0)));
        assert_eq!(command_token_at("fix it /Com", 11), Some(("com".into(), 7)));
        assert_eq!(command_token_at("see src/main", 12), None);
        assert_eq!(command_token_at("/review now", 11), None);
        assert_eq!(command_token_at("a /b", 2), None);
    }

    #[test]
    fn descriptions_keep_their_first_sentence() {
        assert_eq!(short_description("Compact the chat. Then more."), "Compact the chat.");
        assert_eq!(short_description("Review changes (beta)"), "Review changes");
    }
}
