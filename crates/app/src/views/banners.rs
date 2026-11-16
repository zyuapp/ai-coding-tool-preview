//! The banners under the header, each on a row of its own: the error the last action ran into, and
//! threads a newer build wrote (App.tsx ActionErrorBanner, HiddenThreadsNotice).

use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::tooltip::Tippable;
use gpui::{div, prelude::*, px, AnyElement, App, Window};

use super::common::Ellipsis;
use crate::workspace::dispatch;

pub fn render(state: &WorkspaceState, _window: &mut Window, _cx: &mut App) -> Option<AnyElement> {
    let mut banners = Vec::new();
    if let Some(message) = state.storage_error.clone().or_else(|| state.action_error.clone()) {
        let storage = state.storage_error.is_some();
        let mut notice = Notice::new("action-error", div().child(message)).tone(NoticeTone::Danger).floating(true).dismiss_label("Dismiss error");
        if !storage {
            if let Some(section) = state.action_error_page {
                notice = notice.action(Button::new("action-error-settings", "Open settings").size(ButtonSize::Sm).on_click(move |_, _, cx| dispatch(AppCommand::SetSettingsOpen { open: true, section: Some(section), setting_id: None }, cx)));
            }
            notice = notice.on_dismiss(|_, _, cx| dispatch(AppCommand::DismissActionError, cx));
        }
        banners.push(div().w_full().child(notice).into_any_element());
    }
    if state.storage_error.is_none() && !state.zen && state.hidden_threads > 0 {
        let hidden = state.hidden_threads;
        let text = format!("{hidden} {} a newer version", if hidden == 1 { "thread needs" } else { "threads need" });
        let notice = Notice::new("hidden-threads", div().id("hidden-threads-text").ellipsis().child(text.clone()).tip(text))
            .tone(NoticeTone::Info)
            .floating(true)
            .action(Button::new("hidden-threads-update", "Update").size(ButtonSize::Sm).on_click(|_, _, cx| dispatch(AppCommand::AppCheckForUpdates { quiet: false }, cx)))
            .on_dismiss(|_, _, cx| dispatch(AppCommand::DismissHiddenTasks, cx));
        banners.push(div().max_w_full().child(notice).into_any_element());
    }
    if banners.is_empty() {
        return None;
    }
    Some(div().absolute().top(px(78.0)).left(px(22.0)).right(px(22.0)).flex().flex_col().items_start().gap(px(8.0)).children(banners).into_any_element())
}
