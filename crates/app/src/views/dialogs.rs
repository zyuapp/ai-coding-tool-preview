//! The dialogs the workspace opens, each open only while state holds it: project add and edit,
//! worktree delete and move, fork name, archive (WorkspaceDialogs.tsx and the dialogs it draws).

use std::collections::HashSet;

use act_core::domain::project::folder_name;
use act_core::domain::thread::TITLE_LIMIT;
use act_core::state::WorkspaceState;
use act_core::view::dialogs::{archive_consequence, archive_prompt_view, archive_question, project_editor_view, worktree_move_view, ProjectEditorView, WorktreeMoveView};
use act_core::view::worktrees::{worktree_settings_page, worktree_settings_views, WorktreeSettingsView};
use act_core::commands::ProjectAddKey;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant};
use act_ui::components::dialog::{dialog_paragraph, Dialog};
use act_ui::components::field::{FieldGroup, TextField};
use act_ui::input::{InputEvent, TextInput};
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight, SharedString, Subscription, Window};

use super::common::{icon, rimmed};
use super::root::Root;
use crate::workspace::dispatch;

/// A field a dialog owns, made again when the dialog opens on something else.
struct Field {
    key: String,
    input: Entity<TextInput>,
    _subscription: Subscription,
}

#[derive(Default)]
pub struct DialogState {
    fork: Option<Field>,
    project_name: Option<Field>,
    project_root: Option<Field>,
    project_add: Option<Field>,
    cancel: Option<FocusHandle>,
    confirm: Option<FocusHandle>,
    seen_root: Option<String>,
}

type OnEvent = Box<dyn Fn(&mut Root, &Entity<TextInput>, &InputEvent, &mut Context<Root>)>;

fn field(slot: &mut Option<Field>, key: String, text: &str, placeholder: &str, focus: bool, on_event: OnEvent, window: &mut Window, cx: &mut Context<Root>) -> Entity<TextInput> {
    if let Some(field) = slot.as_ref().filter(|field| field.key == key) {
        return field.input.clone();
    }
    let placeholder = placeholder.to_string();
    let input = cx.new(|cx| TextInput::new(cx).with_text(text).placeholder(placeholder));
    let subscription = cx.subscribe(&input, move |root, input, event: &InputEvent, cx| {
        on_event(root, &input, event, cx);
        cx.notify();
    });
    if focus {
        input.update(cx, |input, cx| {
            input.focus(window);
            input.select_all_text(cx);
        });
    }
    *slot = Some(Field { key, input: input.clone(), _subscription: subscription });
    input
}

fn cancel_focus(root: &mut Root, cx: &mut Context<Root>) -> FocusHandle {
    root.dialogs.cancel.get_or_insert_with(|| cx.focus_handle()).clone()
}

fn close(command: AppCommand) -> impl Fn(&mut Window, &mut gpui::App) + 'static {
    move |_, cx| dispatch(command.clone(), cx)
}

/// `.worktree-delete-warning`: body copy in the danger tone.
fn warning(text: impl Into<SharedString>, t: &Tokens) -> gpui::Div {
    div().text_color(t.danger_soft).text_size(px(14.0)).line_height(gpui::relative(1.55)).child(text.into())
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> Vec<AnyElement> {
    let mut dialogs = Vec::new();
    if state.project_add.is_some() {
        dialogs.push(project_add(root, state, window, cx));
    } else {
        root.dialogs.project_add = None;
        root.dialogs.seen_root = None;
    }
    if state.worktree_settings.confirming.is_some() || state.worktree_settings.confirming_safe {
        let busy: HashSet<String> = state.active_runs.keys().cloned().collect();
        let views = worktree_settings_views(state, &busy);
        let page = worktree_settings_page(state, views.as_deref());
        if let Some(worktree) = page.confirmation {
            dialogs.push(worktree_delete(root, &worktree, cx));
        }
        if let Some(worktrees) = page.safe_confirmation {
            dialogs.push(worktree_bulk_delete(root, &worktrees, cx));
        }
    }
    if let Some(view) = worktree_move_view(state) {
        dialogs.push(worktree_move(root, &view, cx));
    }
    match &state.fork_name {
        Some(fork) => dialogs.push(fork_name(root, fork, window, cx)),
        None => root.dialogs.fork = None,
    }
    if let Some(prompt) = archive_prompt_view(state, None) {
        let cancel = cancel_focus(root, cx);
        dialogs.push(
            Dialog::new(SharedString::from(format!("archive-{}", prompt.task_id)), close(AppCommand::ArchivePrompt { thread_id: None }))
                .title(archive_question(&prompt.title))
                .width(px(440.0))
                .initial_focus(&cancel)
                .child(dialog_paragraph(archive_consequence(prompt.running, false), cx))
                .action(Button::new("archive-cancel", "Cancel").size(ButtonSize::Lg).track_focus(&cancel).on_click(|_, _, cx| dispatch(AppCommand::ArchivePrompt { thread_id: None }, cx)))
                .action(Button::new("archive-confirm", "Archive").variant(ButtonVariant::Danger).size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::ArchiveConfirm, cx)))
                .into_any_element(),
        );
    }
    match project_editor_view(state) {
        Some(editor) => dialogs.push(project_edit(root, editor, window, cx)),
        None => {
            root.dialogs.project_name = None;
            root.dialogs.project_root = None;
        }
    }
    dialogs
}

fn fork_name(root: &mut Root, fork: &act_core::state::ForkName, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let key = format!("{}:{}", fork.task_id, fork.worktree);
    let input = field(
        &mut root.dialogs.fork,
        key,
        &fork.title,
        "",
        true,
        Box::new(|_, input, event, cx| match event {
            InputEvent::Submit => {
                let title = input.read(cx).text().trim().to_string();
                if !title.is_empty() {
                    dispatch(AppCommand::ForkConfirm { title }, cx);
                }
            }
            InputEvent::Changed if input.read(cx).text().chars().count() > TITLE_LIMIT => {
                let cut: String = input.read(cx).text().chars().take(TITLE_LIMIT).collect();
                input.update(cx, |input, cx| input.set_text(&cut, cx));
            }
            _ => {}
        }),
        window,
        cx,
    );
    let title = input.read(cx).text().trim().to_string();
    let confirm = title.clone();
    let focus = input.focus_handle(cx);
    Dialog::new("fork-name", close(AppCommand::ForkName { source_id: None, worktree: None }))
        .title(if fork.worktree { "Fork into a new worktree" } else { "Fork thread" })
        .initial_focus(&focus)
        .child(TextField::new("fork-name-field", &input).label("Name"))
        .action(Button::new("fork-cancel", "Cancel").size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::ForkName { source_id: None, worktree: None }, cx)))
        .action(Button::new("fork-confirm", "Fork").variant(ButtonVariant::Primary).size(ButtonSize::Lg).disabled(title.is_empty()).on_click(move |_, _, cx| dispatch(AppCommand::ForkConfirm { title: confirm.clone() }, cx)))
        .into_any_element()
}

fn save_project(root: &mut Root, cx: &mut Context<Root>) {
    let Some(editor) = project_editor_view(crate::workspace::workspace(cx).read(cx).state.as_ref()) else { return };
    let name = root.dialogs.project_name.as_ref().map(|field| field.input.read(cx).text().trim().to_string()).unwrap_or_default();
    let folder = root.dialogs.project_root.as_ref().map(|field| field.input.read(cx).text().trim().to_string()).unwrap_or_default();
    if folder.is_empty() || editor.saving {
        return;
    }
    if folder == editor.project.root && name == editor.project.name.clone().unwrap_or_default() {
        dispatch(AppCommand::EditProject { project_id: None }, cx);
        return;
    }
    dispatch(AppCommand::ProjectEdit { project_id: editor.project.id.clone(), name: Some((!name.is_empty()).then_some(name)), root: Some(folder) }, cx);
}

fn project_edit(root: &mut Root, editor: ProjectEditorView, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let project = editor.project.clone();
    let submit = || -> OnEvent {
        Box::new(|root, _, event, cx| {
            if *event == InputEvent::Submit {
                save_project(root, cx);
            }
        })
    };
    let name_input = field(&mut root.dialogs.project_name, project.id.clone(), project.name.as_deref().unwrap_or(""), &folder_name(&project.root), true, submit(), window, cx);
    let root_input = field(&mut root.dialogs.project_root, project.id.clone(), &project.root, "", false, submit(), window, cx);
    root_input.update(cx, |input, _| input.set_label("Folder"));
    for input in [&name_input, &root_input] {
        if input.read(cx).is_read_only() != editor.saving {
            input.update(cx, |input, cx| input.set_read_only(editor.saving, cx));
        }
    }
    let folder = root_input.read(cx).text().trim().to_string();
    let hint = (editor.error.is_none() && editor.checkouts > 0 && folder != project.root)
        .then(|| format!("{} where they are: they are checkouts of the folder this project is leaving.", if editor.checkouts == 1 { "1 worktree stays".to_string() } else { format!("{} worktrees stay", editor.checkouts) }));
    let picked = root_input.clone();
    let choose = Button::new("project-edit-choose", "Choose folder…").icon(IconName::FolderOpen).icon_size(14.0).disabled(editor.saving).on_click(cx.listener(move |_, _, _, cx| {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        let input = picked.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.first() {
                    let chosen = path.to_string_lossy().into_owned();
                    let _ = input.update(cx, |input, cx| input.set_text(&chosen, cx));
                }
            }
        })
        .detach();
    }));
    let mut group = FieldGroup::new().label("Folder").error(editor.error.clone().map(SharedString::from));
    if let Some(hint) = hint {
        group = group.hint(hint);
    }
    let group = group.child(div().flex().gap(px(8.0)).child(div().flex_1().min_w_0().child(TextField::new("project-edit-root", &root_input))).child(choose));
    let focus = name_input.focus_handle(cx);
    Dialog::new(SharedString::from(format!("project-edit-{}", project.id)), close(AppCommand::EditProject { project_id: None }))
        .title("Edit project")
        .initial_focus(&focus)
        .child(TextField::new("project-edit-name", &name_input).label("Name"))
        .child(group)
        .action(Button::new("project-edit-cancel", "Cancel").size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::EditProject { project_id: None }, cx)))
        .action(Button::new("project-edit-save", if editor.saving { "Saving…" } else { "Save" }).variant(ButtonVariant::Primary).size(ButtonSize::Lg).disabled(folder.is_empty() || editor.saving).on_click(cx.listener(|root, _, _, cx| save_project(root, cx))))
        .into_any_element()
}

fn project_add(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let Some(add) = state.project_add.clone() else { return div().into_any_element() };
    let input = field(
        &mut root.dialogs.project_add,
        "open".to_string(),
        &add.root,
        "~/path/to/project",
        true,
        Box::new(|_, input, event, cx| match event {
            InputEvent::Changed => {
                let root = input.read(cx).text().to_string();
                if crate::workspace::workspace(cx).read(cx).state.project_add.as_ref().is_some_and(|add| add.root != root) {
                    dispatch(AppCommand::AddProjectPath { root }, cx);
                }
            }
            InputEvent::Submit => dispatch(AppCommand::AddProjectKey { key: ProjectAddKey::Enter }, cx),
            _ => {}
        }),
        window,
        cx,
    );
    input.update(cx, |input, _| input.set_label("Folder"));
    // The field follows the state only where the state moved on its own, such as a picked suggestion,
    // so keystrokes the state has not caught up with are never typed over.
    if root.dialogs.seen_root.as_deref() != Some(add.root.as_str()) {
        root.dialogs.seen_root = Some(add.root.clone());
        if input.read(cx).text() != add.root {
            let text = add.root.clone();
            input.update(cx, |input, cx| {
                input.set_text(&text, cx);
                input.move_to_end(cx);
            });
        }
    }
    let local = add.computer_id == "this";
    let name = state.computers.name.clone();
    let links: Vec<&act_core::domain::platform::ComputerLink> = state.computers.paired.iter().map(|computer| &computer.link).collect();
    let chosen = links.iter().find(|link| link.id == add.computer_id).copied();
    let offline = !local && chosen.is_none_or(|link| link.status != act_core::domain::platform::ComputerStatus::Connected);
    let place = if local {
        if name.is_empty() { "this computer".to_string() } else { name.clone() }
    } else {
        chosen.map(|link| link.name.clone()).unwrap_or_else(|| "unavailable computer".into())
    };
    let heading = match (add.start, local) {
        (true, true) => "Start in a folder".to_string(),
        (true, false) => format!("Start in a folder on {place}"),
        (false, _) => format!("Add project on {place}"),
    };
    let devices = !add.start || !local || links.iter().any(|link| link.status == act_core::domain::platform::ComputerStatus::Connected);
    let error = if offline { Some(chosen.and_then(|link| link.error.clone()).unwrap_or_else(|| "That computer is offline.".to_string())) } else { add.error.clone() };
    if input.read(cx).is_disabled() != (add.saving || offline) {
        input.update(cx, |input, cx| input.set_disabled(add.saving || offline, cx));
    }
    let suggested = !add.suggestions.is_empty();
    let meta_size = cx.theme().metrics.text_meta;
    let mut folder = div()
        .flex()
        .gap(px(8.0))
        .capture_key_down(move |event, _, cx| {
            let key = match event.keystroke.key.as_str() {
                "up" => ProjectAddKey::ArrowUp,
                "down" => ProjectAddKey::ArrowDown,
                "tab" if suggested => ProjectAddKey::Tab,
                _ => return,
            };
            cx.stop_propagation();
            dispatch(AppCommand::AddProjectKey { key }, cx);
        })
        .child(div().flex_1().min_w_0().child(TextField::new("project-add-root", &input)));
    if local {
        folder = folder.child(Button::new("project-add-choose", "Choose folder…").icon(IconName::FolderOpen).icon_size(14.0).disabled(add.saving).on_click(|_, _, cx| dispatch(AppCommand::AddProjectPick, cx)));
    }
    let mut group = FieldGroup::new().label("Folder").error(error.map(SharedString::from)).child(folder);
    if suggested {
        group = group.child(
            div()
                .id("project-add-options")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Directories"))
                .max_h(px(200.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p(px(4.0))
                .border_1()
                .border_color(t.line_mid)
                .rounded(px(8.0))
                .children(add.suggestions.iter().enumerate().map(|(index, root)| {
                    let selected = index as i64 == add.selected;
                    div()
                        .id(("project-add-option", index))
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListItem, root.clone()).selected(selected))
                        .skip_tab_stop()
                        .keep_focus_on_click()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .text_size(px(meta_size))
                        .text_color(if selected { t.ink } else { t.ink_soft })
                        .when(selected, |row| row.bg(t.row_selected))
                        .hover(|row| row.bg(t.wash).text_color(t.ink))
                        .cursor_pointer()
                        .child(root.clone())
                        .on_click(move |_, _, cx| dispatch(AppCommand::AddProjectAccept { index: index as i64 }, cx))
                })),
        );
    }
    let focus = input.focus_handle(cx);
    let mut dialog = Dialog::new("project-add", close(AppCommand::AddProjectClose))
        .title(heading)
        .initial_focus(&focus)
        .on_escape(|_, cx| dispatch(AppCommand::AddProjectKey { key: ProjectAddKey::Escape }, cx));
    if devices {
        let mut options = vec![act_ui::components::select::SelectOption::new("this", if name.is_empty() { "This computer".to_string() } else { format!("This computer ({name})") })];
        for link in links.iter().filter(|link| link.status == act_core::domain::platform::ComputerStatus::Connected || link.id == add.computer_id) {
            options.push(act_ui::components::select::SelectOption::new(link.id.clone(), link.name.clone()).disabled(link.status != act_core::domain::platform::ComputerStatus::Connected));
        }
        if !local && chosen.is_none() {
            options.push(act_ui::components::select::SelectOption::new(add.computer_id.clone(), "Unavailable computer").disabled(true));
        }
        dialog = dialog.child(
            FieldGroup::new().label("Device").child(
                act_ui::components::select::Select::new("project-add-device", options, add.computer_id.clone())
                    .disabled(add.saving)
                    .on_change(|value, _, cx| dispatch(AppCommand::AddProjectDevice { computer_id: value.to_string() }, cx)),
            ),
        );
    }
    dialog
        .child(group)
        .action(Button::new("project-add-cancel", "Cancel").size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::AddProjectClose, cx)))
        .action(
            Button::new("project-add-submit", if add.saving { "Opening…" } else if add.start { "Start" } else { "Add project" })
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Lg)
                .disabled(add.saving || offline || add.root.trim().is_empty())
                .on_click(|_, _, cx| dispatch(AppCommand::AddProjectSubmit, cx)),
        )
        .into_any_element()
}

fn returning(linked: usize, place: &str) -> String {
    if linked == 0 {
        String::new()
    } else {
        format!(" {linked} linked {} to {place}.", if linked == 1 { "thread returns" } else { "threads return" })
    }
}

fn worktree_delete(root: &mut Root, worktree: &WorktreeSettingsView, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let cancel = cancel_focus(root, cx);
    let back = returning(worktree.threads.len(), "the project folder");
    let target = div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .px(px(14.0))
        .py(px(13.0))
        .border_1()
        .border_color(t.line)
        .rounded(px(7.0))
        .bg(t.wash)
        .child(div().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(700.0)).child(worktree.title.clone()))
        .child(div().text_color(t.muted).text_size(px(12.0)).child(worktree.project.clone()))
        .child(div().text_color(t.muted).text_size(px(12.0)).line_height(gpui::relative(1.7)).font_family(mono).child(worktree.root.clone()));
    let mut dialog = Dialog::new(SharedString::from(format!("worktree-delete-{}", worktree.root)), close(AppCommand::WorktreeConfirmDelete { root: None }))
        .title(if worktree.available { "Delete worktree?" } else { "Forget missing folder?" })
        .width(px(500.0))
        .initial_focus(&cancel)
        .child(target);
    if !worktree.available {
        dialog = dialog.child(dialog_paragraph(format!("The folder is already gone. Thread history stays.{back}"), cx));
    } else if worktree.repository.is_some() {
        dialog = dialog.child(dialog_paragraph(format!("The branch and thread history stay, and uncommitted changes are saved as a commit.{back}"), cx)).child(warning("Git-ignored files are deleted with the folder.", &t));
    } else {
        if !back.is_empty() {
            dialog = dialog.child(dialog_paragraph(back.trim().to_string(), cx));
        }
        dialog = dialog.child(warning("The repository is unavailable, so every file in the folder is permanently deleted.", &t));
    }
    if worktree.busy {
        dialog = dialog.child(warning("Wait for the active run to finish.", &t));
    }
    let delete_root = worktree.root.clone();
    let missing_only = !worktree.available;
    dialog
        .action(Button::new("worktree-delete-cancel", "Cancel").size(ButtonSize::Lg).track_focus(&cancel).on_click(|_, _, cx| dispatch(AppCommand::WorktreeConfirmDelete { root: None }, cx)))
        .action(
            Button::new("worktree-delete-confirm", if worktree.deleting { "Deleting…" } else if worktree.available { "Delete worktree" } else { "Forget folder" })
                .variant(ButtonVariant::Danger)
                .size(ButtonSize::Lg)
                .disabled(worktree.busy || worktree.deleting)
                .busy(worktree.deleting)
                .on_click(move |_, _, cx| dispatch(AppCommand::WorktreeDelete { task_id: None, root: Some(delete_root.clone()), worktree_id: None, missing_only: Some(missing_only) }, cx)),
        )
        .into_any_element()
}

fn worktree_bulk_delete(root: &mut Root, worktrees: &[WorktreeSettingsView], cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let cancel = cancel_focus(root, cx);
    let count = worktrees.len();
    let noun = if count == 1 { "worktree" } else { "worktrees" };
    let linked = worktrees.iter().map(|worktree| worktree.threads.len()).sum();
    let back = returning(linked, if count == 1 { "its project folder" } else { "their project folder" });
    let roots: Vec<String> = worktrees.iter().map(|worktree| worktree.root.clone()).collect();
    let list = div()
        .id("worktree-bulk-list")
        .max_h(px(240.0))
        .overflow_y_scroll()
        .py(px(4.0))
        .border_1()
        .border_color(t.line)
        .rounded(px(7.0))
        .bg(t.wash)
        .flex()
        .flex_col()
        .children(worktrees.iter().map(|worktree| {
            let threads = worktree.threads.len();
            let linked = if threads > 0 { format!(" · {threads} {}", if threads == 1 { "thread" } else { "threads" }) } else { String::new() };
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(12.0))
                .px(px(13.0))
                .py(px(6.0))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.ink_dim).text_size(px(12.0)).font_weight(FontWeight(600.0)).child(worktree.title.clone()))
                .child(div().flex_none().text_color(t.muted).text_size(px(12.0)).child(format!("{}{linked}", worktree.project)))
        }));
    Dialog::new("worktree-bulk-delete", close(AppCommand::WorktreeConfirmDeleteSafe { open: false }))
        .title(format!("Delete {count} safe {noun}?"))
        .width(px(500.0))
        .initial_focus(&cancel)
        .child(dialog_paragraph(format!("No uncommitted changes, and every commit is on the default branch. Branches and thread history stay.{back}"), cx))
        .child(list)
        .child(warning("Git-ignored files are deleted with the folders.", &t))
        .action(Button::new("worktree-bulk-cancel", "Cancel").size(ButtonSize::Lg).track_focus(&cancel).on_click(|_, _, cx| dispatch(AppCommand::WorktreeConfirmDeleteSafe { open: false }, cx)))
        .action(Button::new("worktree-bulk-confirm", format!("Delete {count} {noun}")).variant(ButtonVariant::Danger).size(ButtonSize::Lg).on_click(move |_, _, cx| dispatch(AppCommand::WorktreeDeleteSafe { roots: roots.clone() }, cx)))
        .into_any_element()
}

fn worktree_move(root: &mut Root, view: &WorktreeMoveView, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let cancel = cancel_focus(root, cx);
    let explanation = if view.worktree {
        "It moves into a checkout of its own and works there from now on.".to_string()
    } else {
        let held = format!("Its {} uncommitted {} committed first so nothing is lost", view.changes, if view.changes == 1 { "change is" } else { "changes are" });
        if view.others > 0 {
            format!("{held}, then the worktree stays for the {} still in it.", if view.others == 1 { "thread" } else { "threads" })
        } else {
            format!("{held}, then the worktree is removed.")
        }
    };
    let (from, to) = if view.worktree { (("Local", IconName::House), ("Worktree", IconName::FolderSymlink)) } else { (("Worktree", IconName::FolderSymlink), ("Local", IconName::House)) };
    let place = |(label, glyph): (&'static str, IconName), target: bool| {
        let disc = rimmed(div(), &t, 13.0).size(px(42.0)).flex().items_center().justify_center().border_1();
        let disc = if target {
            disc.border_color(super::common::with_alpha(t.accent, 0.55)).bg(act_ui::color::mix(t.accent, 0.16, t.surface)).child(icon(glyph, 20.0, t.accent))
        } else {
            disc.border_color(t.line_mid).bg(t.surface).child(icon(glyph, 20.0, t.quiet))
        };
        div()
            .w(px(96.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(7.0))
            .child(disc)
            .child(div().text_size(px(12.0)).when(target, |label| label.text_color(t.ink).font_weight(FontWeight(600.0))).when(!target, |label| label.text_color(t.quiet)).child(label))
    };
    let media = div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(14.0))
        .pt(px(24.0))
        .px(px(20.0))
        .pb(px(18.0))
        .bg(super::common::with_alpha(t.accent, 0.07))
        .child(place(from, false))
        .child(div().mb(px(22.0)).child(icon(IconName::MoveRight, 22.0, t.quiet)))
        .child(place(to, true));
    let worktree = view.worktree;
    let confirm = root.dialogs.confirm.get_or_insert_with(|| cx.focus_handle()).clone();
    let title = if worktree { "Give this thread a worktree?" } else { "Return this thread to your project checkout?" };
    Dialog::new("worktree-move", close(AppCommand::MoveWorktree { worktree: None }))
        .width(px(438.0))
        .media(media)
        .initial_focus(if worktree { &confirm } else { &cancel })
        .child(div().w_full().text_center().text_color(t.ink).text_size(px(17.0)).font_weight(FontWeight(600.0)).child(title))
        .child(div().w_full().flex().justify_center().child(dialog_paragraph(explanation, cx).max_w(px(33.0 * 8.0)).text_center()))
        .child(
            div()
                .mt(px(6.0))
                .flex()
                .justify_center()
                .gap(px(8.0))
                .child(Button::new("worktree-move-cancel", "Cancel").size(ButtonSize::Lg).track_focus(&cancel).on_click(|_, _, cx| dispatch(AppCommand::MoveWorktree { worktree: None }, cx)))
                .child(Button::new("worktree-move-confirm", if worktree { "Move to worktree" } else { "Return to local" }).variant(ButtonVariant::Primary).size(ButtonSize::Lg).track_focus(&confirm).on_click(move |_, _, cx| dispatch(AppCommand::TaskSetWorktree { task_id: None, worktree }, cx))),
        )
        .into_any_element()
}
