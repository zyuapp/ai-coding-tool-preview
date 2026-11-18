//! Toasts at the window's corner (Toasts.tsx).

use act_core::domain::toast::ToastTone as CoreTone;
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::toast::{ToastCard, ToastTone, Toasts};
use gpui::{div, prelude::*, AnyElement};

use crate::workspace::dispatch;

pub fn render(state: &WorkspaceState) -> AnyElement {
    let visible = act_core::reducer::toasts::visible_toasts(state);
    if visible.is_empty() {
        return div().into_any_element();
    }
    let cards = visible
        .iter()
        .map(|toast| ToastCard {
            id: toast.id as u64,
            tone: match toast.tone {
                CoreTone::Update => ToastTone::Update,
                CoreTone::Progress => ToastTone::Progress,
                CoreTone::Success => ToastTone::Success,
                CoreTone::Error => ToastTone::Error,
            },
            title: toast.title.clone().into(),
            message: toast.message.clone().map(Into::into),
            action: toast.action.as_ref().map(|action| action.label.clone().into()),
        })
        .collect();
    let actions: Vec<(u64, AppCommand)> = visible
        .iter()
        .filter_map(|toast| {
            let command = serde_json::to_value(&toast.action.as_ref()?.command).ok()?;
            Some((toast.id as u64, AppCommand::from_json(&command).ok()?))
        })
        .collect();
    Toasts::new(cards)
        .on_dismiss(|id, _, cx| dispatch(AppCommand::DismissToast { id: *id as i64 }, cx))
        .on_action(move |id, _, cx| {
            if let Some((_, command)) = actions.iter().find(|(toast, _)| toast == id) {
                dispatch(command.clone(), cx);
            }
        })
        .on_hold(|held, _, cx| dispatch(AppCommand::HoldToasts { held: *held }, cx))
        .into_any_element()
}
