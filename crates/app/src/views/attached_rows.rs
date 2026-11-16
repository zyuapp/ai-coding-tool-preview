//! What rides a sent message, as pills: annotations, pasted text and files (AttachedPill.tsx,
//! AnnotationRow.tsx, PasteRow.tsx, FileRow.tsx).

use act_core::domain::conversation::{Annotation, AttachedFile, PastedText};
use act_core::AppCommand;
use act_ui::style::{glass, Corners, GlassKind, Raised};
use act_ui::theme::{ActiveTheme, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, App, Div, EdgeFade, FontWeight, SharedString, Stateful, TextRun, Window};

use super::common::{icon, ink_icon, Ellipsis};
use super::letter_spacing::LetterSpacing;
use crate::workspace::dispatch;

/// One pill's frame; `tone` tints the kind that has one.
fn pill(id: SharedString, tone: Option<Tone>, cx: &App) -> Stateful<Div> {
    let t = cx.theme().tokens.clone();
    let colors = tone.map(|tone| t.tone(tone));
    let line = colors.as_ref().map(|colors| colors.line).unwrap_or(t.line_mid);
    let hover_line = colors.as_ref().map(|colors| colors.tone).unwrap_or(t.line_bright);
    let hover_fill = colors.as_ref().map(|colors| colors.chip);
    div()
        .id(id)
        .relative()
        .flex()
        .min_w_0()
        .min_h(px(28.0))
        .border_1()
        .border_color(line)
        .rounded(px(9.0))
        .bg(colors.as_ref().map(|colors| colors.surface).unwrap_or(t.control))
        .text_color(colors.as_ref().map(|colors| colors.soft).unwrap_or(t.ink_soft))
        .text_size(px(cx.theme().metrics.text_meta))
        .line_height(gpui::relative(1.4))
        .rim(t.glass_rim, Corners::Radius(9.0))
        .transition(gpui::Transition::ease(120))
        .hover(move |style: gpui::StyleRefinement| {
            let style = style.border_color(hover_line);
            match hover_fill {
                Some(fill) => style.bg(fill),
                None => style,
            }
        })
}

fn face(cx: &App) -> Div {
    let _ = cx;
    div().flex().items_center().gap(px(6.0)).min_w_0().max_w(px(260.0)).px(px(9.0)).rounded(px(8.0))
}

fn row(children: Vec<AnyElement>) -> Div {
    div().w_full().flex().flex_wrap().justify_end().gap(px(6.0)).children(children)
}

/// Sent annotations as numbered pills, each raising a card with its quote and note on hover.
pub fn annotation_row(message_id: &str, annotations: &[Annotation], window: &Window, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let pills = annotations
        .iter()
        .enumerate()
        .map(|(index, annotation)| {
            let note = annotation.note.trim().to_string();
            let number = div()
                .flex_none()
                .min_w(px(16.0))
                .h(px(16.0))
                .px(px(4.0))
                .rounded_full()
                .bg(t.info_badge)
                .text_color(t.badge_ink)
                .text_size(px(m.text_badge))
                .font_weight(FontWeight(700.0))
                .tabular_nums()
                .flex()
                .items_center()
                .justify_center()
                .child(format!("{}", index + 1));
            let label = if note.is_empty() {
                div().max_w(px(190.0)).ellipsis().text_color(t.muted).child(format!("“{}", annotation.quote))
            } else {
                div().max_w(px(190.0)).ellipsis().text_color(t.ink_dim).child(note.clone())
            };
            let group: SharedString = format!("annotation-{message_id}-{index}").into();
            let card = annotation_card(annotation, index, window, cx);
            pill(group.clone(), Some(Tone::Info), cx)
                .group(group.clone())
                .child(face(cx).child(number).child(label))
                .child(
                    div()
                        .id("card")
                        .absolute()
                        .bottom(gpui::relative(1.0))
                        .right_0()
                        .pb(px(8.0))
                        .transition(gpui::Transition::ease_out(110).only(&[gpui::TransitionProperty::Opacity, gpui::TransitionProperty::Transform]))
                        .invisible()
                        .opacity(0.0)
                        .transform(gpui::ElementTransform::translate(px(0.0), px(3.0)))
                        .group_hover(group, |style| style.visible().opacity(1.0).transform(gpui::ElementTransform::default()))
                        .child(card),
                )
                .into_any_element()
        })
        .collect();
    row(pills).into_any_element()
}

/// The card's width, and what its quote and note wrap in (`.annotation-card`, its body padding,
/// the quote's bar inset, the note's pencil and gap).
const CARD_WIDTH: f32 = 320.0;
const QUOTE_WIDTH: f32 = CARD_WIDTH - 24.0 - 10.0;
const NOTE_WIDTH: f32 = CARD_WIDTH - 24.0 - 12.0 - 7.0;

/// Whether `text` at `size` runs past `lines` lines in `width` (`useClamped`).
fn clamps(text: &str, size: f32, width: f32, lines: usize, window: &Window) -> bool {
    let run = TextRun { len: text.len(), font: window.text_style().font(), color: gpui::Hsla::default(), background_color: None, underline: None, strikethrough: None };
    window
        .text_system()
        .shape_text(SharedString::from(text.to_string()), px(size), &[run], Some(px(width)), None)
        .is_ok_and(|shaped| shaped.iter().map(|line| line.wrap_boundaries().len() + 1).sum::<usize>() > lines)
}

/// Which annotation, what was quoted, what the user wrote. Clamped text fades out over its last line.
pub(crate) fn annotation_card(annotation: &Annotation, index: usize, window: &Window, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let note = annotation.note.trim().to_string();
    let head = div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .px(px(11.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(t.line_soft)
        .text_color(t.quiet)
        .text_size(px(m.text_badge))
        .child(
            div()
                .flex_none()
                .min_w(px(16.0))
                .h(px(16.0))
                .px(px(4.0))
                .rounded_full()
                .bg(t.info_badge)
                .text_color(t.badge_ink)
                .font_weight(FontWeight(700.0))
                .flex()
                .items_center()
                .justify_center()
                .child(format!("{}", index + 1)),
        )
        .child(div().letter_spacing(0.07).child("ANNOTATION"));
    let quote_line = m.text_meta * 1.55;
    let quote_clamped = clamps(&annotation.quote, m.text_meta, QUOTE_WIDTH, 4, window);
    let quote = div()
        .relative()
        .pl(px(10.0))
        .text_color(t.muted)
        .text_size(px(m.text_meta))
        .line_height(gpui::relative(1.55))
        .line_clamp(4)
        .text_ellipsis()
        .overflow_hidden()
        .when(quote_clamped, |quote| quote.edge_fade(EdgeFade::vertical().end(px(0.0), px(quote_line))))
        .child(div().absolute().top(px(2.0)).bottom(px(2.0)).left_0().w(px(2.0)).rounded(px(2.0)).bg(t.info_line))
        .child(annotation.quote.clone());
    let written = if note.is_empty() {
        div().text_color(t.quiet_soft).text_size(px(m.text_meta)).italic().child("No note taken.")
    } else {
        let note_line = m.text_control * 1.45;
        let note_clamped = clamps(&note, m.text_control, NOTE_WIDTH, 3, window);
        div()
            .flex()
            .gap(px(7.0))
            .text_color(t.ink)
            .text_size(px(m.text_control))
            .line_height(gpui::relative(1.45))
            .child(div().mt(px(3.0)).flex_none().child(icon(IconName::Pencil, 12.0, t.info_soft)))
            .child(
                div()
                    .line_clamp(3)
                    .text_ellipsis()
                    .overflow_hidden()
                    .when(note_clamped, |text| text.edge_fade(EdgeFade::vertical().end(px(0.0), px(note_line))))
                    .child(note),
            )
    };
    glass(div(), &t, GlassKind::Float, Some(11.0), None)
        .w(px(CARD_WIDTH))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(head)
        .child(div().flex().flex_col().gap(px(9.0)).px(px(12.0)).pt(px(11.0)).pb(px(12.0)).child(quote).child(written))
        .into_any_element()
}

/// What a pill says it holds: lines for anything with a shape, characters for one long run of text.
pub fn paste_summary(text: &str) -> String {
    let lines = text.matches('\n').count() + 1;
    if lines > 1 {
        format!("{} lines", thousands(lines))
    } else {
        format!("{} characters", thousands(text.encode_utf16().count()))
    }
}

fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Pasted blocks as pills, each opening its full text over the window.
pub fn paste_row(message_id: &str, pastes: &[PastedText], cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let pills = pastes
        .iter()
        .enumerate()
        .map(|(index, paste)| {
            let label = format!("Pasted text #{}", index + 1);
            let text = paste.text.clone();
            let title = label.clone();
            pill(format!("paste-{message_id}-{}", paste.id).into(), None, cx)
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Read pasted text {}", index + 1)))
                .cursor_pointer()
                .child(
                    face(cx)
                        .hover(|style| style.bg(t.surface_hover).text_color(t.ink))
                        .child(ink_icon(IconName::ClipboardPaste, 12.0))
                        .child(div().font_weight(FontWeight(500.0)).ellipsis().child(label))
                        .child(div().text_color(t.muted).child(paste_summary(&paste.text))),
                )
                .on_click(move |_, _, cx| super::viewer::open_paste(text.clone(), title.clone(), cx))
                .tip("View")
                .into_any_element()
        })
        .collect();
    row(pills).into_any_element()
}

/// Attached files as pills; a file opens where files open, a folder only names itself.
pub fn file_row(message_id: &str, task_id: &str, files: &[AttachedFile], cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let pills = files
        .iter()
        .map(|file| {
            let mut face = face(cx);
            if file.folder {
                face = face.child(ink_icon(IconName::Folder, 12.0)).child(div().font_weight(FontWeight(500.0)).ellipsis().child(file.name.clone())).child(div().text_color(t.muted).child("Folder"));
            } else {
                face = face.hover(|style| style.bg(t.surface_hover).text_color(t.ink)).child(ink_icon(IconName::File, 12.0)).child(div().font_weight(FontWeight(500.0)).ellipsis().child(file.name.clone()));
            }
            let mut element = pill(format!("file-{message_id}-{}", file.id).into(), None, cx).child(face).tip(file.path.clone());
            if !file.folder {
                let path = file.path.clone();
                let task_id = task_id.to_string();
                element = element.accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {}", file.name))).cursor_pointer().on_click(move |_, _, cx| dispatch(AppCommand::FileOpen { task_id: Some(task_id.clone()), path: path.clone(), line: None }, cx));
            }
            element.into_any_element()
        })
        .collect();
    row(pills).into_any_element()
}
