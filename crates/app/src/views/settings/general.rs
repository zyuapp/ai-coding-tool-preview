//! General (GeneralSettings.tsx): the terminal command, dictation, notifications, Claude's
//! experiments, and the app's own updates and licences.

use act_core::domain::platform::{AppUpdate, CliInstallState, CliStatus, DictationModel, CLI_COMMAND};
use act_core::domain::shortcuts::{display_shortcut, shortcut_settings};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::Switch;
use act_ui::components::kbd::{Kbd, KbdSize};
use act_ui::components::spinner::Spinning;
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Window};

use super::{description, em, error, flow, page, with_code, Inline, Marks, Row, Section};
use crate::views::root::Root;
use crate::workspace::dispatch;

const CLI_INSTALL_PATH: &str = concat!("/usr/local/bin/", act_core::cli_command!());

/// Which terminal-command change is in flight, so only its button reads as working.
#[derive(Default)]
pub struct GeneralState {
    cli_step: Option<bool>,
}

fn cli_description(status: Option<&CliStatus>) -> String {
    let Some(status) = status else { return "Looking for the command…".into() };
    match status.state {
        CliInstallState::Installed if status.on_path == Some(false) => format!("Installed at {}. Add its folder to PATH to run {CLI_COMMAND} by name.", status.path),
        CliInstallState::Installed => format!("Installed at {}.", status.path),
        CliInstallState::Conflict => format!("Something else already answers to {CLI_COMMAND} at {}.", status.path),
        CliInstallState::Unsupported => "The command can only be installed on macOS or Linux.".into(),
        CliInstallState::Missing if status.path == CLI_INSTALL_PATH => format!("Goes in {}, which asks for your password once.", status.path),
        CliInstallState::Missing => format!("Goes in {}, inside your user account.", status.path),
    }
}

fn computer_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "this Mac"
    } else {
        "this computer"
    }
}

fn dictation_size() -> (&'static str, bool) {
    if cfg!(target_os = "macos") {
        ("about 450 MB", true)
    } else {
        ("about 660 MB", false)
    }
}

fn dictation_description(state: &WorkspaceState) -> String {
    let dictation = &state.dictation;
    let (size, shared) = dictation_size();
    let name = computer_name();
    match dictation.model {
        DictationModel::Ready => format!("Parakeet, on {name}.{}", if shared { " Shared with other apps that use it." } else { "" }),
        DictationModel::Downloading if dictation.progress.is_none() => format!("Getting Parakeet ready for {name}."),
        DictationModel::Downloading => format!("Downloading Parakeet, {size}."),
        DictationModel::Failed => dictation.message.clone().unwrap_or_else(|| "The download stopped.".into()),
        _ => format!("Parakeet, {size}. Downloaded once{}.", if shared { " and shared with other apps that use it" } else { "" }),
    }
}

fn update_description(update: &AppUpdate, current: Option<&str>) -> Option<String> {
    match update {
        AppUpdate::Available { version } => Some(format!("Version {version} is available.")),
        AppUpdate::Downloading { version, .. } => Some(format!("Downloading version {version}.")),
        AppUpdate::Ready { version } => Some(format!("Version {version} is downloaded. Restart to install it.")),
        AppUpdate::Idle => current.map(|current| format!("Version {current}.")),
    }
}

fn spinner_em(id: &'static str, text: String, cx: &gpui::App) -> AnyElement {
    let t = &cx.theme().tokens;
    em(div().flex().items_center().gap(px(6.0)).child(Spinning::new(id, IconName::LoaderCircle).size(px(12.0))).child(text), t.muted)
}

fn switch(id: &'static str, on: bool, label: &'static str, focus: &gpui::FocusHandle, command: fn(bool) -> AppCommand) -> AnyElement {
    Switch::new(id, on, label).track_focus(focus).on_change(move |value: &bool, _, cx| dispatch(command(*value), cx)).into_any_element()
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let cli = &state.cli;
    if !cli.busy {
        root.settings.general.cli_step = None;
    }
    let cli_stop = marks.stop("general.cli", cx);
    let installing = cli.busy && root.settings.general.cli_step != Some(false);
    let removing = cli.busy && root.settings.general.cli_step == Some(false);
    let set_cli = |installed: bool| {
        cx.listener(move |root: &mut Root, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Root>| {
            root.settings.general.cli_step = Some(installed);
            dispatch(if installed { AppCommand::CliInstall } else { AppCommand::CliUninstall }, cx);
        })
    };
    let confirm_cli = |installed: bool| {
        let listener = cx.entity().downgrade();
        move |_: &mut Window, cx: &mut gpui::App| {
            let _ = listener.update(cx, |root, _| root.settings.general.cli_step = Some(installed));
            dispatch(if installed { AppCommand::CliInstall } else { AppCommand::CliUninstall }, cx);
        }
    };
    let status = cli.status.as_ref();
    let mut cli_row = Row::setting("general.cli", &t).status(status.is_some_and(|status| status.state == CliInstallState::Installed), &t).line(cli_description(status), &t);
    match status {
        None if cli.error.is_none() => cli_row = cli_row.action(spinner_em("cli-checking", "Checking…".into(), cx)),
        None => cli_row = cli_row.action(Button::new("cli-check", "Check again").track_focus(&cli_stop).on_click(|_, _, cx| dispatch(AppCommand::CliRead, cx))),
        Some(status) => match status.state {
            CliInstallState::Installed => {
                if status.current == Some(false) {
                    cli_row = cli_row.action(Button::new("cli-update", if installing { "Updating…" } else { "Update" }).track_focus(&cli_stop).disabled(cli.busy).on_click(set_cli(true)));
                }
                cli_row = cli_row.action(ConfirmAction::new("cli-uninstall", if removing { "Removing…" } else { "Uninstall" }, format!("Uninstall {CLI_COMMAND}"), confirm_cli(false)).disabled(cli.busy));
            }
            CliInstallState::Missing => cli_row = cli_row.action(Button::new("cli-install", if cli.busy { "Installing…" } else { "Install" }).track_focus(&cli_stop).disabled(cli.busy).on_click(set_cli(true))),
            CliInstallState::Conflict => cli_row = cli_row.action(ConfirmAction::new("cli-replace", if cli.busy { "Installing…" } else { "Replace it" }, format!("Replace {}", status.path), confirm_cli(true)).disabled(cli.busy)),
            CliInstallState::Unsupported => {}
        },
    }
    let mut terminal = Section::new("Terminal command")
        .first()
        .description(with_code(&[("Run", false), (CLI_COMMAND, true), ("in a folder to open it here as a project, or", false), (&format!("{CLI_COMMAND} ~/code/app"), true), ("to open another one.", false)], cx))
        .row(cli_row.build(marks, &t));
    if let Some(message) = &cli.error {
        terminal = terminal.row(error(message.clone(), &t));
    }

    let mut sections = vec![terminal.build(marks, &t)];

    let dictation = &state.dictation;
    if dictation.model != DictationModel::Unsupported {
        let binding = shortcut_settings(&state.shortcuts).into_iter().find(|setting| setting.action.id == "dictation.hold").and_then(|setting| setting.binding).map(|binding| display_shortcut(&binding, cfg!(target_os = "macos")));
        let mut parts = vec![Inline::Words("Hold the microphone".into())];
        if let Some(binding) = binding {
            parts.push(Inline::Words(" or ".into()));
            parts.push(Inline::Element(Kbd::new([binding]).size(KbdSize::Inline).into_any_element()));
        }
        parts.push(Inline::Words(format!(" and let go to add the words to your message, or tap it to talk hands-free. What you say stays on {}.", computer_name())));
        let words = flow(parts);
        let mut row = Row::setting("general.dictation", &t).status(dictation.model == DictationModel::Ready, &t).line(dictation_description(state), &t);
        match dictation.model {
            DictationModel::Missing | DictationModel::Failed => {
                row = row.action(Button::new("dictation-download", if dictation.model == DictationModel::Failed { "Try again" } else { "Download" }).track_focus(&marks.stop("general.dictation", cx)).on_click(|_, _, cx| dispatch(AppCommand::DictationDownload, cx)))
            }
            DictationModel::Downloading => {
                let progress = dictation.progress.map(|progress| format!("{}%", (progress * 100.0).round())).unwrap_or_else(|| "Preparing…".into());
                row = row.action(spinner_em("dictation-progress", progress, cx)).action(Button::new("dictation-cancel", "Cancel").on_click(|_, _, cx| dispatch(AppCommand::DictationCancelDownload, cx)));
            }
            _ => {}
        }
        sections.push(Section::new("Dictation").description(words).row(row.build(marks, &t)).build(marks, &t));
    }

    let protection = state.memory_protection;
    sections.push(
        Section::new("Memory protection")
            .row(
                Row::setting("general.memory-protection", &t)
                    .line("When this computer runs low on memory, stop the thread using the most.", &t)
                    .on_title_click(move |_, cx| dispatch(AppCommand::SetMemoryProtection { enabled: !protection }, cx))
                    .action(switch("memory-protection-switch", protection, "Memory protection", &marks.stop("general.memory-protection", cx), |enabled| AppCommand::SetMemoryProtection { enabled }))
                    .build(marks, &t),
            )
            .build(marks, &t),
    );
    let notifications = state.notifications;
    sections.push(
        Section::new("Notifications")
            .row(
                Row::setting("general.notifications", &t)
                    .line("When a run finishes, fails, or needs you. Your phone hears it once you step away from this computer.", &t)
                    .on_title_click(move |_, cx| dispatch(AppCommand::SetNotifications { enabled: !notifications }, cx))
                    .action(switch("notifications-switch", notifications, "Notifications", &marks.stop("general.notifications", cx), |enabled| AppCommand::SetNotifications { enabled }))
                    .build(marks, &t),
            )
            .build(marks, &t),
    );
    let chrome = state.chrome_browser;
    sections.push(
        Section::new("Claude")
            .description(description("Experimental settings that only Claude threads read. They can change or disappear."))
            .row(
                Row::setting("general.chrome-browser", &t)
                    .line("Claude drives the Chrome you already have open, instead of the browser panel, when you ask for your own browser. Needs the Claude in Chrome extension, and Chrome running.", &t)
                    .on_title_click(move |_, cx| dispatch(AppCommand::SetChromeBrowser { enabled: !chrome }, cx))
                    .action(switch("chrome-switch", chrome, "Claude in Chrome", &marks.stop("general.chrome-browser", cx), |enabled| AppCommand::SetChromeBrowser { enabled }))
                    .build(marks, &t),
            )
            .build(marks, &t),
    );

    let current = state.app_build.as_ref().map(|build| build.version.clone()).unwrap_or_else(|| crate::startup::APP_VERSION.to_string());
    let mut updates = Row::setting("general.updates", &t);
    if let Some(line) = update_description(&state.app_update, Some(&current)) {
        updates = updates.line(line, &t);
    }
    let update_stop = marks.stop("general.updates", cx);
    updates = match &state.app_update {
        AppUpdate::Idle => updates.action(Button::new("check-updates", "Check for updates").track_focus(&update_stop).on_click(|_, _, cx| dispatch(AppCommand::AppCheckForUpdates { quiet: false }, cx))),
        AppUpdate::Available { .. } => updates.action(Button::new("download-update", "Download").track_focus(&update_stop).on_click(|_, _, cx| dispatch(AppCommand::AppDownloadUpdate, cx))),
        AppUpdate::Downloading { percent, .. } => updates.action(spinner_em("update-progress", format!("{percent}%"), cx)),
        AppUpdate::Ready { .. } => updates.action(Button::new("install-update", "Restart to update").track_focus(&update_stop).on_click(|_, _, cx| dispatch(AppCommand::AppInstallUpdate, cx))),
    };
    sections.push(
        Section::new(act_core::identity::APP_NAME)
            .row(updates.build(marks, &t))
            .row(Row::setting("general.licenses", &t).action(Button::new("licenses", "View licenses").variant(ButtonVariant::Secondary).track_focus(&marks.stop("general.licenses", cx)).on_click(|_, _, cx| dispatch(AppCommand::AppOpenSourceLicenses, cx))).build(marks, &t))
            .build(marks, &t),
    );
    page("General", Some(description(concat!("How ", act_core::app_name!(), " answers from outside its own window."))), None, window, cx).children(sections).into_any_element()
}
