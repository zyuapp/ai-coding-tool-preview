//! Shortcuts (ShortcutSettings.tsx): every bindable action by group, each changed by pressing the
//! keystroke it should take.

use std::collections::HashMap;
use std::time::Duration;

use act_core::domain::settings_catalog::shortcut_setting_id;
use act_core::domain::shortcuts::{display_shortcut, shortcut_keys, shortcut_settings, ShortcutSurface};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::Button;
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::kbd::Kbd;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, Animation, AnimationExt, AnyElement, Context, FocusHandle, FontWeight, SharedString, Window};

use super::{em, page, Marks, Row, Section};
use crate::views::root::Root;
use crate::workspace::dispatch;

/// Each row's Cancel stop, so the keyboard follows the button that swaps in for the pressed one.
#[derive(Default)]
pub struct ShortcutsState {
    cancels: HashMap<String, FocusHandle>,
    seen_capture: Option<String>,
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let held = &mut root.settings.shortcuts;
    if held.seen_capture != state.capturing_shortcut {
        let left = std::mem::replace(&mut held.seen_capture, state.capturing_shortcut.clone());
        match &state.capturing_shortcut {
            Some(id) => {
                let cancel = held.cancels.entry(id.clone()).or_insert_with(|| cx.focus_handle()).clone();
                window.focus(&cancel, cx);
            }
            None => {
                if let Some(id) = left {
                    let cancel = held.cancels.get(&id).cloned();
                    let adrift = match window.focused(cx) {
                        None => true,
                        Some(focused) => Some(&focused) == cancel.as_ref() || focused == root.focus,
                    };
                    if adrift {
                        window.focus(&marks.stop(&shortcut_setting_id(&id), cx), cx);
                    }
                }
            }
        }
    }
    let mut cancel_stop = |id: &str, cx: &mut Context<Root>| root.settings.shortcuts.cancels.entry(id.to_string()).or_insert_with(|| cx.focus_handle()).clone();
    let mac = cfg!(target_os = "macos");
    let settings = shortcut_settings(&state.shortcuts);
    let changed = settings.iter().filter(|setting| setting.changed).count();
    let reset = ConfirmAction::new("shortcuts-reset", "Restore defaults", "Restore all", |_, cx| dispatch(AppCommand::ResetShortcuts, cx)).disabled(changed == 0).gap(px(10.0));
    let mut groups: Vec<(&str, Vec<AnyElement>)> = Vec::new();
    let mut above_capturing = false;
    let mut last_group = "";
    for setting in settings {
        let action = setting.action;
        let capturing = state.capturing_shortcut.as_deref() == Some(action.id);
        let unavailable = state.desktop_shortcut_unavailable.as_ref().filter(|refusal| action.surface == ShortcutSurface::Desktop && setting.binding.as_deref() == Some(refusal.binding.as_str()));
        let under = above_capturing && last_group == action.group;
        above_capturing = capturing;
        last_group = action.group;
        let cancel = cancel_stop(action.id, cx);
        let change = marks.stop(&shortcut_setting_id(action.id), cx);
        let mut row = Row::new(action.label, &t).mark(shortcut_setting_id(action.id)).line(action.description, &t).action_column(274.0, 6.0).capturing(capturing).under_capture(under);
        if let Some(refusal) = unavailable {
            row = row.describe(div().mt(px(5.0)).text_color(t.accent_soft).text_size(px(12.0)).line_height(gpui::relative(1.45)).child(refusal.message.clone()));
        }
        let id = action.id;
        if capturing {
            let accent = t.accent;
            let dot = div().size(px(7.0)).rounded_full().bg(accent).with_animation(SharedString::from(format!("capture-{id}")), Animation::new(Duration::from_millis(1400)).repeat(), |dot, delta| {
                let fade = 1.0 - (delta * std::f32::consts::PI).sin() * 0.65;
                dot.opacity(fade)
            });
            row = row
                .action(div().mr(px(12.0)).child(em(div().flex().items_center().gap(px(6.0)).text_color(t.accent_soft).font_weight(FontWeight(560.0)).child(dot).child("Press a keystroke…"), t.accent_soft)))
                .action(Button::new(SharedString::from(format!("cancel-{id}")), "Cancel").name(format!("Cancel changing {}", action.label)).track_focus(&cancel).tip("Cancel (Escape)").on_click(|_, _, cx| dispatch(AppCommand::CaptureShortcut { action: None }, cx)));
        } else {
            row = match &setting.binding {
                Some(binding) => row.action(div().mr(px(12.0)).child(Kbd::new(shortcut_keys(binding, mac)))),
                None => row.action(div().mr(px(12.0)).child(em("Not set", t.muted))),
            };
            if setting.changed {
                let default = (!action.default_binding.is_empty()).then(|| action.default_binding.to_string());
                let mut reset = Button::new(SharedString::from(format!("reset-{id}")), "Reset").name(format!("Reset {}", action.label));
                if let Some(default) = &default {
                    reset = reset.tip(format!("Reset to {}", display_shortcut(default, mac)));
                }
                row = row.action(reset.on_click(move |_, _, cx| dispatch(AppCommand::SetShortcut { action: id.to_string(), binding: default.clone() }, cx)));
            }
            row = row
                .action(Button::new(SharedString::from(format!("change-{id}")), "Change").name(format!("Change {}", action.label)).track_focus(&change).on_click(move |_, _, cx| dispatch(AppCommand::CaptureShortcut { action: Some(id.to_string()) }, cx)))
                .action(Button::new(SharedString::from(format!("clear-{id}")), "Clear").name(format!("Clear {}", action.label)).disabled(setting.binding.is_none()).on_click(move |_, _, cx| dispatch(AppCommand::SetShortcut { action: id.to_string(), binding: None }, cx)));
        }
        let built = row.build(marks, &t);
        match groups.iter_mut().find(|(group, _)| *group == action.group) {
            Some((_, rows)) => rows.push(built),
            None => groups.push((action.group, vec![built])),
        }
    }
    let mut body = page("Shortcuts", None, Some(reset.into_any_element()), window, cx);
    for (index, (name, rows)) in groups.into_iter().enumerate() {
        let mut section = Section::new(name).rows(rows);
        if index == 0 {
            section = section.first();
        }
        body = body.child(section.build(marks, &t));
    }
    body.into_any_element()
}
