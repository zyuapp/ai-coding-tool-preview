//! Dictation: one speech host for the window, whose model status the workspace hears from the
//! start and whose effects the reducer asks for (dictation-effects.ts).

use std::sync::{Arc, OnceLock};

use act_core::effects::EffectWork;
use act_devices::dictation::host::{DictationHost, DictationHostOptions, Listening};

use super::{Inbound, Sink};

struct Dictation {
    host: DictationHost,
    sink: Sink,
    _model: Listening,
}

static DICTATION: OnceLock<Dictation> = OnceLock::new();

fn dispatcher(sink: &Sink) -> act_devices::simulator::effects::Dispatch {
    let sink = sink.clone();
    Arc::new(move |input| sink.send(Inbound::Input(input)))
}

/// The speech host, started on first ask with the workspace told where the model stands.
pub fn host(sink: &Sink) -> &'static DictationHost {
    let dictation = DICTATION.get_or_init(|| {
        let _guard = act_host::runtime().enter();
        let host = DictationHost::new(DictationHostOptions::desktop());
        let model = act_devices::dictation::effects::subscribe(&host, dispatcher(sink));
        Dictation { host, sink: sink.clone(), _model: model }
    });
    &dictation.host
}

/// Performs a `dictation.*` effect; answers false for any other.
pub fn perform(sink: &Sink, work: &EffectWork) -> bool {
    let host = host(sink);
    let sink = DICTATION.get().map(|dictation| dictation.sink.clone()).unwrap_or_else(|| sink.clone());
    act_devices::dictation::effects::perform(host, work, dispatcher(&sink))
}

/// Stops the helper as the app quits.
pub fn stop() {
    if let Some(dictation) = DICTATION.get() {
        dictation.host.stop_helper();
    }
}
