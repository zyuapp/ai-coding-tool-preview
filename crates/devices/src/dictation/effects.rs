//! The reducer's dictation effects, each answer carried back under the hold it belongs to
//! (dictation-effects.ts).

use act_core::domain::platform::DictationModel;
use act_core::effects::EffectWork;
use act_core::events::WorkspaceEvent;

use super::host::{DictationHost, Listening};
use crate::simulator::effects::Dispatch;

/// Performs a `dictation.*` effect. Returns false for any other effect, which it leaves alone.
pub fn perform(host: &DictationHost, work: &EffectWork, dispatch: Dispatch) -> bool {
    let host = host.clone();
    let runtime = act_host::runtime();
    match work.clone() {
        EffectWork::DictationStart { id } => {
            runtime.spawn(async move {
                let event = match host.start(&id).await {
                    Ok(()) => WorkspaceEvent::DictationStarted { id },
                    Err(message) => WorkspaceEvent::DictationFailed { id, message },
                };
                dispatch(event.into());
            });
        }
        EffectWork::DictationStop { id } => {
            runtime.spawn(async move {
                let event = match host.stop(&id).await {
                    Ok(text) => WorkspaceEvent::DictationTranscribed { id, text },
                    Err(message) => WorkspaceEvent::DictationFailed { id, message },
                };
                dispatch(event.into());
            });
        }
        EffectWork::DictationCancel { id } => {
            runtime.spawn(async move { host.cancel(&id).await });
        }
        EffectWork::DictationCancelDownload => {
            runtime.spawn(async move {
                if let Err(message) = host.cancel_download().await {
                    dispatch(WorkspaceEvent::ActionFailed { message, page: None }.into());
                }
            });
        }
        EffectWork::DictationDownload => {
            runtime.spawn(async move {
                if let Err(message) = host.download().await {
                    dispatch(WorkspaceEvent::DictationModel { model: DictationModel::Failed, message: Some(message), progress: None }.into());
                }
            });
        }
        _ => return false,
    }
    true
}

/// Tells the workspace where the speech model stands, now and whenever it moves, until dropped.
pub fn subscribe(host: &DictationHost, dispatch: Dispatch) -> Listening {
    host.on_model(move |status| {
        dispatch(WorkspaceEvent::DictationModel { model: status.model, message: status.message.clone().filter(|message| !message.is_empty()), progress: status.progress }.into());
    })
}
