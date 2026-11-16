//! The cards in the window's corner: raised, replaced, held, and taken down.

use crate::commands::AppCommand;
use crate::domain::toast::{Toast, MAX_REMEMBERED_TOASTS, MAX_TOASTS, TOAST_LIFETIME_MS};
use crate::effects::{Effect, EffectWork};
use crate::reducer::Step;
use crate::state::WorkspaceState;

fn leaving(held: bool, toast: &Toast, now: i64) -> Option<i64> {
    if toast.persistent || held {
        None
    } else {
        Some(now + TOAST_LIFETIME_MS)
    }
}

/// Raises a toast; one about the same subject takes the place of the last. `toast.id` is assigned here.
pub fn with_toast(state: &mut WorkspaceState, mut toast: Toast, now: i64) -> Vec<Effect> {
    if let Some(subject) = &toast.subject {
        if let Some(index) = state.toasts.iter().position(|item| item.subject.as_ref() == Some(subject)) {
            toast.id = state.toasts[index].id;
            let at = leaving(state.toasts_held, &toast, now);
            let id = toast.id;
            state.toasts[index] = toast;
            return vec![EffectWork::ScheduleToastDismissal { id, at }.into()];
        }
    }
    let id = state.toast_sequence + 1;
    toast.id = id;
    let at = leaving(state.toasts_held, &toast, now);
    state.toasts.push(toast);
    if state.toasts.len() > MAX_TOASTS {
        let excess = state.toasts.len() - MAX_TOASTS;
        state.toasts.drain(..excess);
    }
    state.toast_sequence = id;
    at.map(|at| vec![EffectWork::ScheduleToastDismissal { id, at: Some(at) }.into()]).unwrap_or_default()
}

/// Every toast's time given back, or held, as the pointer comes and goes.
pub fn rescheduled_toasts(state: &WorkspaceState, now: i64) -> Vec<Effect> {
    state.toasts.iter().filter(|toast| !toast.persistent).map(|toast| EffectWork::ScheduleToastDismissal { id: toast.id, at: leaving(state.toasts_held, toast, now) }.into()).collect()
}

/// In zen mode only errors are shown.
pub fn visible_toasts(state: &WorkspaceState) -> Vec<&Toast> {
    state.toasts.iter().filter(|toast| !state.zen || toast.tone == crate::domain::toast::ToastTone::Error).collect()
}

pub fn reduce_toasts(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::HoldToasts { held } => {
            if *held == state.toasts_held {
                return Step::none();
            }
            state.toasts_held = *held;
            Step::effects(rescheduled_toasts(state, crate::util::now()))
        }
        AppCommand::DismissToast { id } => {
            let Some(index) = state.toasts.iter().position(|toast| toast.id == *id) else { return Step::none() };
            let toast = state.toasts.remove(index);
            let Some(remember) = toast.remember.filter(|key| !state.dismissed_toasts.contains(key)) else { return Step::none() };
            state.dismissed_toasts.push(remember);
            if state.dismissed_toasts.len() > MAX_REMEMBERED_TOASTS {
                let excess = state.dismissed_toasts.len() - MAX_REMEMBERED_TOASTS;
                state.dismissed_toasts.drain(..excess);
            }
            Step::persisting()
        }
        _ => Step::none(),
    }
}
