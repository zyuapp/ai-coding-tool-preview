//! The main composer's dictation: the microphone held to talk or tapped for hands-free, the level
//! strip that stands in the bar while it listens, and the keystroke that dictates
//! (ComposerDictation.tsx, keyboard-host.ts holdToTalk).

use std::rc::Rc;
use std::time::Instant;

use act_core::domain::platform::DictationPhase;
use act_core::domain::shortcuts::{display_shortcut, resolve_shortcuts, shortcut_for, ShortcutSurface};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_devices::dictation::levels::DictationLevels;
use act_devices::dictation::ui::{ComposerDictation, DictationStrip, MicButton};
use gpui::{prelude::*, AnyElement, Context, Entity, KeyUpEvent, ModifiersChangedEvent, Subscription};

use super::root::Root;
use crate::workspace::{dispatch, workspace};

/// How long a press may last and still count as a tap, which keeps listening hands-free.
const TAP_MS: u128 = 300;

pub struct Dictation {
    levels: Entity<DictationLevels>,
    mic: Entity<MicButton>,
    /// The key holding to talk and when it went down, or None when its press stopped hands-free dictation.
    held: Option<(String, Instant)>,
    /// A hold that was dropped as the window lost the keyboard, so it is dropped once.
    left: bool,
    _keys: Subscription,
}

fn binding(state: &WorkspaceState) -> Option<String> {
    resolve_shortcuts(&state.shortcuts).into_iter().find(|binding| binding.action == "dictation.hold").map(|binding| binding.binding)
}

impl Dictation {
    pub fn new(cx: &mut Context<Root>) -> Option<Dictation> {
        let host = crate::effects::window_desktop::dictation(cx);
        let levels = cx.new(|cx| DictationLevels::new(host, cx));
        let mic = cx.new(|cx| MicButton::new(levels.clone(), Rc::new(|talk, _, cx| dispatch(talk.command(), cx)), cx));
        let keys = cx.intercept_keystrokes(|event, _, cx| {
            let Some(stroke) = crate::keys::keystroke(&event.keystroke) else { return };
            let state = workspace(cx).read(cx).state.clone();
            let bindings = resolve_shortcuts(&state.shortcuts);
            if shortcut_for(&bindings, &stroke, ShortcutSurface::Any).is_none_or(|binding| binding.action != "dictation.hold") {
                return;
            }
            cx.stop_propagation();
            let key = event.keystroke.key.clone();
            let session = state.dictation.session.clone();
            let stopping = session.as_ref().is_some_and(|session| session.hands_free && session.phase != DictationPhase::Transcribing);
            let root = cx.windows().first().and_then(|window| window.downcast::<Root>());
            if let Some(root) = root {
                let _ = root.update(cx, |root, _, _| {
                    if let Some(dictation) = &mut root.composer.dictation {
                        if dictation.held.as_ref().is_some_and(|(held, _)| *held == key) {
                            return;
                        }
                        dictation.held = (!stopping).then(|| (key.clone(), Instant::now()));
                    }
                });
            }
            dispatch(if stopping { AppCommand::DictationStop { send: None } } else { AppCommand::DictationStart }, cx);
        });
        Some(Dictation { levels, mic, held: None, left: false, _keys: keys })
    }

    pub fn sync(&self, state: &WorkspaceState, cx: &mut Context<Root>) {
        let view = act_core::view::workspace::dictation_view(state);
        let disabled = !state.restored && state.current_thread().is_none();
        let dictation = ComposerDictation { model: state.dictation.model, phase: view.phase, hands_free: view.hands_free, binding: binding(state).map(|binding| display_shortcut(&binding, cfg!(target_os = "macos"))) };
        self.mic.update(cx, |mic, cx| mic.set(dictation, disabled, cx));
    }

    /// A hold whose key goes up in another app never hears its release, so leaving the window drops it.
    pub fn window_left(&mut self, state: &WorkspaceState, cx: &mut Context<Root>) {
        let holding = state.dictation.session.as_ref().is_some_and(|session| !session.hands_free);
        if state.focused || !holding {
            self.left = false;
            return;
        }
        if !self.left {
            self.left = true;
            self.held = None;
            cx.defer(|cx| dispatch(AppCommand::DictationCancel, cx));
        }
    }

    pub fn away(&self, cx: &gpui::App) -> bool {
        self.mic.read(cx).away()
    }
}

impl Root {
    /// The dictation key going up ends the hold: a tap keeps listening hands-free, a hold adds the words.
    fn release_talk(&mut self, cx: &mut Context<Root>) {
        let Some(dictation) = &mut self.composer.dictation else { return };
        let Some((_, at)) = dictation.held.take() else { return };
        let tapped = at.elapsed().as_millis() < TAP_MS;
        dispatch(if tapped { AppCommand::DictationHandsFree } else { AppCommand::DictationStop { send: None } }, cx);
    }

    pub fn dictation_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Root>) {
        if self.composer.dictation.as_ref().and_then(|dictation| dictation.held.as_ref()).is_some_and(|(key, _)| *key == event.keystroke.key) {
            self.release_talk(cx);
        }
    }

    pub fn dictation_modifiers(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Root>) {
        let held = self.composer.dictation.as_ref().is_some_and(|dictation| dictation.held.is_some());
        if !held {
            return;
        }
        let state = self.state(cx);
        let Some(binding) = binding(&state) else { return };
        let wanted = act_core::domain::shortcuts::parse_shortcut(&binding);
        let mac = cfg!(target_os = "macos");
        let modifiers = event.modifiers;
        let still = wanted.is_some_and(|stroke| (!stroke.r#mod || if mac { modifiers.platform } else { modifiers.control }) && (!stroke.ctrl || modifiers.control) && (!stroke.alt || modifiers.alt) && (!stroke.shift || modifiers.shift));
        if !still {
            self.release_talk(cx);
        }
    }
}

pub fn mic(root: &mut Root, _state: &WorkspaceState, _disabled: bool, _cx: &mut Context<Root>) -> Option<AnyElement> {
    root.composer.dictation.as_ref().map(|dictation| dictation.mic.clone().into_any_element())
}

pub fn strip(root: &mut Root, phase: DictationPhase, hands_free: bool, width: f32, cx: &mut Context<Root>) -> AnyElement {
    let Some(dictation) = &root.composer.dictation else { return gpui::div().into_any_element() };
    let away = dictation.mic.read(cx).away();
    DictationStrip::new(phase, away, hands_free, dictation.levels.clone()).roomy(width > 520.0).into_any_element()
}
