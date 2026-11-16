//! Annotating an answer: selected text grows a popover that adds it to the chat, the note is written
//! where the text was, and each drafted annotation is lit and numbered where it sits
//! (AnnotateLayer.tsx, use-annotations.ts).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_core::domain::conversation::{Annotation, AnnotationAnchor};
use act_core::AppCommand;
use act_markdown::MarkdownView;
use act_ui::components::button::{Button, ButtonVariant, IconButton, IconButtonVariant};
use act_ui::input::{InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Pixels, Point, SharedString, Subscription, Window};

use super::root::Root;
use super::timeline_row::RowModel;
use crate::workspace::dispatch;

/// As long as a revealed marker's flash runs.
const FLASH: Duration = Duration::from_millis(1800);

/// Selected answer text, held where it was selected so the popover can sit on it.
#[derive(Clone, PartialEq)]
struct Selected {
    message_id: String,
    quote: String,
    start: usize,
    end: usize,
    at: Point<Pixels>,
}

/// The note being written at a highlight: for a new annotation, or an existing one.
struct Noting {
    annotation_id: Option<String>,
    quote: String,
    anchor: (String, usize, usize),
    at: Point<Pixels>,
    opened: Instant,
    input: Entity<TextInput>,
    _subscription: Subscription,
}

#[derive(Default)]
pub struct AnnotateState {
    /// Markdown views drawn this frame, by message id.
    pub drawn: Rc<RefCell<HashMap<String, Entity<MarkdownView>>>>,
    shown: HashMap<String, Entity<MarkdownView>>,
    noting: Option<Noting>,
    /// A selection put away without a choice, which stays away until the selection changes.
    dismissed: Option<Selected>,
    answered: Option<i64>,
    flashing: Option<(String, Instant)>,
    area: Option<BoundsCell>,
}

impl AnnotateState {
    /// A message's Markdown view as last drawn.
    pub fn shown_view(&self, message_id: &str) -> Option<Entity<MarkdownView>> {
        self.shown.get(message_id).cloned()
    }

    /// The views of the frame just drawn, which this frame places the layer against.
    pub fn next_frame(&mut self) {
        self.shown = std::mem::take(&mut *self.drawn.borrow_mut());
    }
}

fn selected(state: &AnnotateState, cx: &gpui::App) -> Option<(Selected, Entity<MarkdownView>)> {
    // A highlight lives in one message, so a selection across two answers offers none.
    if state.shown.values().filter(|view| view.read(cx).has_selection()).count() > 1 {
        return None;
    }
    for (message_id, view) in &state.shown {
        let read = view.read(cx);
        let Some((start, end)) = read.selection_range() else { continue };
        let quote = read.selected_text().map(|text| text.trim().to_string()).unwrap_or_default();
        if quote.is_empty() {
            continue;
        }
        let bounds = read.range_text_bounds(start, end)?;
        let at = Point { x: bounds.origin.x + bounds.size.width / 2.0, y: bounds.origin.y };
        return Some((Selected { message_id: message_id.clone(), quote, start: read.rendered_offset(start), end: read.rendered_offset(end), at }, view.clone()));
    }
    None
}

fn open_note(root: &mut Root, annotation_id: Option<String>, quote: String, anchor: (String, usize, usize), note: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Root>) {
    let input = cx.new(|cx| TextInput::new(cx).with_text(note).label("Annotation note").placeholder("Add a note"));
    let subscription = cx.subscribe(&input, |root, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Submit) {
            commit(root, cx);
        }
    });
    input.update(cx, |input, cx| {
        input.select_all_text(cx);
        input.focus(window);
    });
    root.conversation.annotate.noting = Some(Noting { annotation_id, quote, anchor, at, opened: Instant::now(), input, _subscription: subscription });
    cx.notify();
}

/// Keeps what was written: a new note adds the annotation, an edited one rewrites its note.
fn commit(root: &mut Root, cx: &mut Context<Root>) {
    let Some(noting) = root.conversation.annotate.noting.take() else { return };
    let note = noting.input.read(cx).text().to_string();
    match noting.annotation_id {
        Some(annotation_id) => dispatch(AppCommand::AnnotationNote { task_id: None, annotation_id, note }, cx),
        None => {
            let (message_id, start, end) = noting.anchor;
            dispatch(AppCommand::AnnotationAdd { task_id: None, quote: noting.quote, note: Some(note), anchor: Some(AnnotationAnchor::Message { message_id, start: start as i64, end: end as i64 }) }, cx);
        }
    }
    cx.notify();
}

/// Anything that puts the editor away keeps what was typed; only an empty note is dropped.
fn dismiss(root: &mut Root, cx: &mut Context<Root>) {
    let typed = root.conversation.annotate.noting.as_ref().is_some_and(|noting| !noting.input.read(cx).text().trim().is_empty());
    if typed {
        commit(root, cx);
    } else {
        root.conversation.annotate.noting = None;
        cx.notify();
    }
}

/// The popover over a selection, the note editor, and the numbered markers.
pub fn layer(root: &mut Root, task_id: &str, annotations: &[Annotation], model: &Rc<RowModel>, window: &mut Window, cx: &mut Context<Root>) -> Vec<AnyElement> {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let _ = (task_id, model);
    let area = root.conversation.annotate.area.get_or_insert_with(bounds_cell).clone();
    let origin = area.get().map(|bounds| bounds.origin).unwrap_or_default();
    let mut layers: Vec<AnyElement> = vec![div().absolute().inset_0().child(bounds_probe(&area)).into_any_element()];
    let entity = cx.entity().downgrade();

    // Markers at the end of each drafted annotation drawn on screen.
    let flashing = root.conversation.annotate.flashing.clone().filter(|(_, at)| at.elapsed() < FLASH);
    for (index, annotation) in annotations.iter().enumerate() {
        let Some(AnnotationAnchor::Message { message_id, start, end }) = &annotation.anchor else { continue };
        let Some(view) = root.conversation.annotate.shown.get(message_id) else { continue };
        let read = view.read(cx);
        let (Some(from), Some(to)) = (read.position_at((*start).max(0) as usize), read.position_at((*end).max(0) as usize)) else { continue };
        let Some(bounds) = read.range_bounds(from, to) else { continue };
        let x = bounds.origin.x + bounds.size.width - origin.x;
        let y = bounds.origin.y - origin.y;
        // `annotation-marker-flash`: a ring swelling 10px out of the marker as it fades, twice.
        let ring = flashing.as_ref().filter(|(id, _)| id == &annotation.id).map(|(_, at)| {
            let period = FLASH.as_secs_f32() / 2.0;
            let progress = (at.elapsed().as_secs_f32() % period) / period;
            act_ui::theme::tokens::cubic_bezier([0.0, 0.0, 0.58, 1.0], progress)
        });
        let flash = ring.is_some() && t.reduced_motion;
        let (id, quote, note, anchor) = (annotation.id.clone(), annotation.quote.clone(), annotation.note.clone(), (message_id.clone(), (*start).max(0) as usize, (*end).max(0) as usize));
        let at = Point { x: bounds.origin.x + bounds.size.width, y: bounds.origin.y };
        let weak = entity.clone();
        layers.push(
            div()
                .id(SharedString::from(format!("annotation-marker-{}", annotation.id)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Edit annotation {}", index + 1)))
                .absolute()
                .left(x + px(2.0))
                .top(y - px(20.0))
                .min_w(px(20.0))
                .h(px(20.0))
                .px(px(4.0))
                .rounded_full()
                .border_1()
                .border_color(if flash { t.info } else { t.info_badge })
                .bg(t.info_badge)
                .when_some(ring.filter(|_| !t.reduced_motion), |marker, eased| {
                    window.request_animation_frame();
                    marker.shadow(vec![gpui::BoxShadow {
                        color: act_ui::color::alpha(t.info, 1.0 - eased).into(),
                        offset: gpui::point(px(0.0), px(0.0)),
                        blur_radius: px(0.0),
                        spread_radius: px(10.0 * eased),
                    }])
                })
                .text_color(t.badge_ink)
                .text_size(px(m.text_badge))
                .font_weight(FontWeight(650.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.border_color(t.info_line).bg(act_ui::color::mix(t.info_badge, 0.86, t.ink)))
                .child(format!("{}", index + 1))
                .tip("Edit annotation")
                .on_click(move |_, window, cx| {
                    if let Some(root) = weak.upgrade() {
                        root.update(cx, |root, cx| open_note(root, Some(id.clone()), quote.clone(), anchor.clone(), &note, at, window, cx));
                    }
                })
                .into_any_element(),
        );
    }

    // A reveal asked from the composer brings the passage into view and flashes its marker.
    if let Some(reveal) = crate::workspace::workspace(cx).read(cx).state.annotation_reveal.clone() {
        if root.conversation.annotate.answered.is_none() {
            root.conversation.annotate.answered = Some(reveal.count);
        } else if root.conversation.annotate.answered != Some(reveal.count) {
            root.conversation.annotate.answered = Some(reveal.count);
            root.conversation.annotate.flashing = Some((reveal.annotation_id.clone(), Instant::now()));
            root.conversation.reveal(&reveal.message_id, window);
            cx.spawn(async move |root, cx| {
                cx.background_executor().timer(FLASH).await;
                let _ = root.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    // The note editor, or the popover over a fresh selection.
    if root.conversation.scrolled_since_annotating() {
        dismiss(root, cx);
        root.conversation.annotate.dismissed = selected(&root.conversation.annotate, cx).map(|(selection, _)| selection);
    }
    if let Some(noting) = &root.conversation.annotate.noting {
        let editing = noting.annotation_id.clone();
        let weak = entity.clone();
        let save = entity.clone();
        let remove = entity.clone();
        let mut editor = glass(div(), &t, GlassKind::Capsule, None, None)
            .id("annotate-editor")
            .when(!t.reduced_motion, |menu| menu.enter(gpui::Transition::ease_out(110), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(5.0)))))
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, if editing.is_some() { "Edit annotation" } else { "New annotation" }))
            .occlude()
            .flex()
            .items_center()
            .gap(px(2.0))
            .pl(px(14.0))
            .pr(px(5.0))
            .py(px(4.0))
            .capture_key_down(move |event, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    if let Some(root) = weak.upgrade() {
                        root.update(cx, |root, cx| {
                            root.conversation.annotate.noting = None;
                            cx.notify();
                        });
                    }
                }
            })
            .on_mouse_down_out({
                let weak = entity.clone();
                move |_, _, cx| {
                    if let Some(root) = weak.upgrade() {
                        root.update(cx, |root, cx| {
                            // The press that opened the editor is still settling.
                            if root.conversation.annotate.noting.as_ref().is_some_and(|noting| noting.opened.elapsed() > Duration::from_millis(300)) {
                                dismiss(root, cx);
                            }
                        });
                    }
                }
            })
            .child(div().w(px(270.0)).text_color(t.ink).text_size(px(m.text_control)).child(noting.input.clone()))
            .child(Button::new("annotate-save", "").name("Save annotation").icon(IconName::Check).icon_size(14.0).variant(ButtonVariant::Primary).pill(true).tip("Save (↩)").on_click(move |_, _, cx| {
                if let Some(root) = save.upgrade() {
                    root.update(cx, commit);
                }
            }));
        if let Some(annotation_id) = editing {
            editor = editor.child(div().ml(px(5.0)).pl(px(4.0)).border_l_1().border_color(t.line).child(IconButton::new("annotate-remove", IconName::X, "Remove annotation").variant(IconButtonVariant::Danger).round(true).icon_size(14.0).tip("Remove").on_click(move |_, _, cx| {
                dispatch(AppCommand::AnnotationRemove { task_id: None, annotation_id: annotation_id.clone() }, cx);
                if let Some(root) = remove.upgrade() {
                    root.update(cx, |root, cx| {
                        root.conversation.annotate.noting = None;
                        cx.notify();
                    });
                }
            })));
        }
        let at = noting.at - origin;
        layers.push(div().absolute().left(at.x - px(170.0)).top(at.y - px(48.0)).child(editor).into_any_element());
        return layers;
    }
    // As the page reads the selection on pointerup, a drag still selecting offers nothing yet.
    if root.conversation.selecting() {
        return layers;
    }
    let Some((selection, view)) = selected(&root.conversation.annotate, cx) else {
        root.conversation.annotate.dismissed = None;
        return layers;
    };
    if root.conversation.annotate.dismissed.as_ref() == Some(&selection) {
        return layers;
    }
    let side = root.composer.task.is_none() && state_thread_exists(root, cx);
    let note_root = entity.clone();
    let note_choice = selection.clone();
    let note_view = view.clone();
    let mut popover = glass(div(), &t, GlassKind::Capsule, None, None)
        .id("annotate-popover")
        .when(!t.reduced_motion, |menu| menu.enter(gpui::Transition::ease_out(110), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(5.0)))))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "Annotate selection"))
        .occlude()
        .flex()
        .gap(px(2.0))
        .p(px(3.0))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            Button::new("annotate-add", "Add to chat").keep_focus().icon(IconName::MessageSquareQuote).icon_size(14.0).variant(ButtonVariant::Ghost).pill(true).tip("Add to chat (↩)").on_click(move |_, window, cx| {
                let chosen = note_choice.clone();
                note_view.update(cx, |view, cx| view.clear_selection(cx));
                if let Some(root) = note_root.upgrade() {
                    root.update(cx, |root, cx| open_note(root, None, chosen.quote.clone(), (chosen.message_id.clone(), chosen.start, chosen.end), "", chosen.at, window, cx));
                }
            }),
        );
    if side {
        let side_root = entity.clone();
        let quote = selection.quote.clone();
        let side_view = view.clone();
        popover = popover.child(
            Button::new("annotate-side", "Add to side chat").keep_focus().icon(IconName::GitFork).icon_size(14.0).variant(ButtonVariant::Ghost).pill(true).tip(super::chrome::key_tip("Add to side chat", "Mod+Enter")).on_click(move |_, _, cx| {
                side_view.update(cx, |view, cx| view.clear_selection(cx));
                let quote = quote.clone();
                if let Some(root) = side_root.upgrade() {
                    root.update(cx, |root, cx| root.annotate_to_side_chat(quote, cx));
                }
            }),
        );
    }
    let at = selection.at - origin;
    layers.push(div().absolute().left(at.x - px(400.0)).w(px(800.0)).flex().justify_center().top(at.y - px(44.0)).child(popover).into_any_element());
    layers
}

fn state_thread_exists(root: &Root, cx: &gpui::App) -> bool {
    root.state(cx).current_thread().is_some()
}

impl Root {
    /// Hands a selection to the thread's first side chat as a bare reference, opening one when it has none.
    pub fn annotate_to_side_chat(&mut self, quote: String, cx: &mut Context<Root>) {
        let state = self.state(cx);
        let Some(thread) = state.current_thread() else { return };
        let existing = act_core::state::dock_side_chats(&state, &thread.id).first().map(|chat| chat.id.clone());
        match existing {
            Some(chat) => {
                dispatch(AppCommand::SelectDockTab { tab: chat.clone() }, cx);
                dispatch(AppCommand::SetDockOpen { open: true }, cx);
                dispatch(AppCommand::AnnotationAdd { task_id: Some(chat), quote, note: None, anchor: None }, cx);
            }
            None => {
                let chat_id = uuid::Uuid::new_v4().to_string();
                dispatch(AppCommand::SideChatOpen { chat_id: chat_id.clone() }, cx);
                dispatch(AppCommand::AnnotationAdd { task_id: Some(chat_id), quote, note: None, anchor: None }, cx);
            }
        }
        cx.notify();
    }
}

/// Mod+Enter hands the selected text to a side chat, as the popover's second button does.
pub fn enter_side(root: &mut Root, cx: &mut Context<Root>) -> bool {
    if root.conversation.annotate.noting.is_some() || root.composer.task.is_some() {
        return false;
    }
    let Some((selection, view)) = selected(&root.conversation.annotate, cx) else { return false };
    view.update(cx, |view, cx| view.clear_selection(cx));
    root.annotate_to_side_chat(selection.quote, cx);
    true
}

/// Enter adds the selected text to the chat, as the popover's button does.
pub fn enter(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) -> bool {
    if root.conversation.annotate.noting.is_some() {
        return false;
    }
    let Some((selection, view)) = selected(&root.conversation.annotate, cx) else { return false };
    view.update(cx, |view, cx| view.clear_selection(cx));
    open_note(root, None, selection.quote, (selection.message_id, selection.start, selection.end), "", selection.at, window, cx);
    true
}
