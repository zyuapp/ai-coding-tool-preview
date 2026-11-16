//! What rides the next send, drawn in the composer above the prompt: annotations, pasted text and
//! files as pills, and images as thumbnails (AttachedPill.tsx, AnnotationRow.tsx, PasteRow.tsx,
//! FileRow.tsx, ComposerAttachments.tsx).

use super::letter_spacing::LetterSpacing;
use act_core::domain::conversation::{Annotation, AnnotationAnchor, AttachedFile, PastedText, StagedImage};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{IconButton, IconButtonSize, IconButtonVariant};
use act_ui::style::{glass, Corners, GlassKind, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::style::Backdrop;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{deferred, div, img, prelude::*, px, AnyElement, App, ClickEvent, Context, FontWeight, ObjectFit, SharedString, Window};

use super::common::ink_icon;
use super::root::Root;
use crate::workspace::dispatch;

/// A count of lines for anything with a shape, characters for one long run of text.
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
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One pill: its face, then a remove button while `remove` is set.
fn pill(id: SharedString, tinted: bool, face: gpui::Stateful<gpui::Div>, remove: Option<(String, Handler)>, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let t = &theme.tokens;
    let info = t.tone(act_ui::theme::Tone::Info);
    let border = if tinted { info.line } else { t.line_mid };
    let hover_border = if tinted { info.tone } else { t.line_bright };
    let mut pill = div()
        .id(id.clone())
        .relative()
        .flex()
        .min_w_0()
        .min_h(px(28.0))
        .rounded(px(9.0))
        .border_1()
        .border_color(border)
        .bg(if tinted { info.surface } else { t.control })
        .text_color(if tinted { info.soft } else { t.ink_soft })
        .text_size(px(theme.metrics.text_meta))
        .line_height(gpui::relative(1.4))
        .rim(t.glass_rim, Corners::Radius(9.0))
        .transition(gpui::Transition::ease(120))
        .hover(move |style: gpui::StyleRefinement| {
            let style = style.border_color(hover_border);
            if tinted { style.bg(info.chip) } else { style }
        })
        .child(face.rounded_l(px(8.0)).when(remove.is_none(), |face| face.rounded_r(px(8.0))));
    if let Some((label, handler)) = remove {
        let line = if tinted { info.line } else { t.line_soft };
        pill = pill.child(
            div()
                .id(SharedString::from(format!("{id}-remove")))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, label))
                .flex_none()
                .w(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_color(line)
                .rounded_r(px(8.0))
                .cursor_pointer()
                .text_color(t.muted)
                .hover(|style| style.bg(t.surface_active).text_color(t.ink_soft))
                .child(ink_icon(IconName::X, 12.0))
                .tip("Remove")
                .on_click(handler),
        );
    }
    pill
}

/// The face of a pill: an icon, a name and an optional detail.
fn face(id: SharedString, glyph: Option<IconName>, name: AnyElement, detail: Option<String>, clickable: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let mut face = div().id(id).flex().items_center().gap(px(6.0)).min_w_0().max_w(px(260.0)).px(px(9.0));
    if let Some(glyph) = glyph {
        face = face.child(ink_icon(glyph, 12.0));
    }
    face = face.child(name);
    if let Some(detail) = detail {
        face = face.child(div().flex_none().text_color(t.muted).child(detail));
    }
    if clickable {
        face = face.cursor_pointer().hover(|style| style.bg(t.surface_hover).text_color(t.ink));
    }
    face
}

fn row(id: &'static str) -> gpui::Div {
    let _ = id;
    div().flex().flex_wrap().gap(px(6.0)).pt(px(12.0)).px(px(14.0))
}

/// Annotations as numbered pills; hovering one raises its card.
pub fn annotation_row(scope: &str, annotations: &[Annotation], removable: bool, root: &Root, window: &Window, cx: &mut Context<Root>) -> Option<gpui::Div> {
    if annotations.is_empty() {
        return None;
    }
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let info = t.tone(act_ui::theme::Tone::Info);
    let mut pills = div().flex().flex_wrap().gap(px(6.0));
    for (index, annotation) in annotations.iter().enumerate() {
        let note = annotation.note.trim().to_string();
        let number = div().flex_none().min_w(px(16.0)).h(px(16.0)).px(px(4.0)).rounded_full().bg(t.info_badge).text_color(t.badge_ink).text_size(px(theme.metrics.text_badge)).font_weight(FontWeight(700.0)).tabular_nums().flex().items_center().justify_center().child((index + 1).to_string());
        let label = if note.is_empty() { div().whitespace_nowrap().text_color(info.soft).child(super::text_fit::fit_line(&format!("\u{201c}{}", annotation.quote), 214.0, theme.metrics.text_meta, window)) } else { div().whitespace_nowrap().font_weight(FontWeight(500.0)).child(super::text_fit::fit_line(&note, 214.0, theme.metrics.text_meta, window)) };
        let revealable = removable && matches!(annotation.anchor, Some(AnnotationAnchor::Message { .. }));
        let id: SharedString = format!("{scope}-annotation-{}", annotation.id).into();
        let mut face_el = div().id(SharedString::from(format!("{id}-face"))).accessible(gpui::AccessInfo::new(if revealable { gpui::AccessRole::Button } else { gpui::AccessRole::Group }, format!("{} {}", if revealable { "Show annotation" } else { "Annotation" }, index + 1))).flex().items_center().gap(px(6.0)).min_w_0().max_w(px(260.0)).px(px(9.0)).child(number).child(label);
        if revealable {
            let annotation_id = annotation.id.clone();
            let task = root.composer.task.clone();
            face_el = face_el.cursor_pointer().on_click(move |_, _, cx| dispatch(AppCommand::AnnotationReveal { task_id: task.clone(), annotation_id: annotation_id.clone() }, cx));
        }
        let hover_key = format!("{scope}:{}", annotation.id);
        let hovered = root.composer.annotation_hover.as_deref() == Some(hover_key.as_str());
        let remove = removable.then(|| {
            let annotation_id = annotation.id.clone();
            let task = root.composer.task.clone();
            (format!("Remove annotation {}", index + 1), Box::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| dispatch(AppCommand::AnnotationRemove { task_id: task.clone(), annotation_id: annotation_id.clone() }, cx)) as Handler)
        });
        let mut pill_el = pill(id, true, face_el, remove, cx).on_hover(root.composer_listener(cx, move |root, over: &bool, _, cx| {
            if *over {
                root.composer.annotation_hover = Some(hover_key.clone());
            } else if root.composer.annotation_hover.as_deref() == Some(hover_key.as_str()) {
                root.composer.annotation_hover = None;
            }
            cx.notify();
        }));
        if hovered {
            let card = super::attached_rows::annotation_card(annotation, index, window, cx);
            pill_el = pill_el.child(deferred(div().absolute().left_0().bottom(gpui::relative(1.0)).pb(px(8.0)).child(card)).with_priority(3));
        }
        pills = pills.child(pill_el);
    }
    Some(pills)
}

/// Pasted blocks as pills; each opens to read in full.
pub fn paste_row(pastes: &[PastedText], removable: bool, root: &Root, cx: &mut Context<Root>) -> Option<gpui::Div> {
    if pastes.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let mut pills = div().flex().flex_wrap().gap(px(6.0));
    for (index, paste) in pastes.iter().enumerate() {
        let id: SharedString = format!("paste-{}", paste.id).into();
        let open_id = paste.id.clone();
        let name = div().whitespace_nowrap().font_weight(FontWeight(500.0)).text_color(t.ink_soft).child(format!("Pasted text #{}", index + 1)).into_any_element();
        let face_el = face(SharedString::from(format!("{id}-open")), Some(IconName::ClipboardPaste), name, Some(paste_summary(&paste.text)), true, cx)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Read pasted text {}", index + 1)))
            .tip("View")
            .on_click(root.composer_listener(cx, move |root, _, _, cx| {
                root.composer.paste_viewing = Some(open_id.clone());
                cx.notify();
            }));
        let remove = removable.then(|| {
            let paste_id = paste.id.clone();
            let task = root.composer.task.clone();
            (format!("Remove pasted text {}", index + 1), Box::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| dispatch(AppCommand::PasteRemove { task_id: task.clone(), paste_id: paste_id.clone() }, cx)) as Handler)
        });
        pills = pills.child(pill(id, false, face_el, remove, cx));
    }
    Some(pills)
}

/// Attached files as pills; a file opens in its editor, a folder only names itself.
pub fn file_row(files: &[AttachedFile], removable: bool, task: Option<String>, window: &Window, cx: &mut Context<Root>) -> Option<gpui::Div> {
    if files.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let mut pills = div().flex().flex_wrap().gap(px(6.0));
    for file in files {
        let id: SharedString = format!("file-{}", file.id).into();
        let name = div().whitespace_nowrap().font_weight(FontWeight(500.0)).text_color(t.ink_soft).child(super::text_fit::fit_line(&file.name, if file.folder { 170.0 } else { 220.0 }, cx.theme().metrics.text_meta, window)).into_any_element();
        let face_el = if file.folder {
            face(SharedString::from(format!("{id}-face")), Some(IconName::Folder), name, Some("Folder".into()), false, cx).tip(file.path.clone())
        } else {
            let path = file.path.clone();
            let task = task.clone();
            face(SharedString::from(format!("{id}-face")), Some(IconName::File), name, None, true, cx).accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {}", file.name))).tip(file.path.clone()).on_click(move |_, _, cx| dispatch(AppCommand::FileOpen { task_id: task.clone(), path: path.clone(), line: None }, cx))
        };
        let remove = removable.then(|| {
            let file_id = file.id.clone();
            let task = task.clone();
            (format!("Remove {}", file.name), Box::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| dispatch(AppCommand::FileDetach { task_id: task.clone(), file_id: file_id.clone() }, cx)) as Handler)
        });
        pills = pills.child(pill(id, false, face_el, remove, cx));
    }
    Some(pills)
}

/// The images riding the next send, as thumbnails that open the annotator, with a remove button each.
fn image_strip(root: &Root, images: &[StagedImage], task: Option<String>, cx: &mut Context<Root>) -> Option<gpui::Div> {
    if images.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mut strip = div().flex().flex_wrap().gap(px(8.0)).pt(px(12.0)).px(px(14.0));
    let total = images.len();
    for (index, image) in images.iter().enumerate() {
        let path = image.path.clone();
        let image_id = image.id.clone();
        let opened = image.id.clone();
        let group: SharedString = format!("attachment-{}", image.id).into();
        let marks = root.composer.marks.get(&image.id);
        let count = marks.map(|marks| marks.annotations.len()).unwrap_or(0);
        let picture = match marks.and_then(|marks| marks.preview.clone()) {
            Some(preview) => img(preview).size_full().object_fit(ObjectFit::Cover).rounded(px(9.0)).into_any_element(),
            None => img(std::path::PathBuf::from(&image.path)).size_full().object_fit(ObjectFit::Cover).rounded(px(9.0)).into_any_element(),
        };
        let mut open = div()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(t.line_mid)
            .bg(t.control)
            .cursor_pointer()
            .hover(|style| style.border_color(t.line_bright))
            .child(picture)
            .child(
                div()
                    .absolute()
                    .right(px(4.0))
                    .bottom(px(4.0))
                    .size(px(20.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.surface_raised)
                    .text_color(t.ink_dim)
                    .group_hover(group.clone(), |style| style.bg(t.accent).text_color(t.accent_ink))
                    .child(gpui::svg().path(IconName::Pencil.path()).size(px(14.0)).text_color(t.ink_dim).group_hover(group.clone(), |style| style.text_color(t.accent_ink))),
            );
        if count > 0 {
            open = open.child(
                div()
                    .absolute()
                    .left(px(4.0))
                    .bottom(px(4.0))
                    .min_w(px(17.0))
                    .rounded_full()
                    .px(px(5.0))
                    .py(px(1.0))
                    .bg(t.info_badge)
                    .text_color(t.badge_ink)
                    .text_size(px(m.text_badge))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(15.0))
                    .flex()
                    .justify_center()
                    .child(count.to_string()),
            );
        }
        let open = open
            .tip(format!("Annotate image {}{}", index + 1, if count == 0 { String::new() } else { format!(", {count} mark{}", if count == 1 { "" } else { "s" }) }))
            .on_click(cx.listener(move |root, _, window, cx| root.open_annotator(opened.clone(), path.clone(), act_core::reducer::run_queue::mark_prefix(index, total), window, cx)));
        let remove_group: SharedString = format!("attachment-remove-{}", image.id).into();
        let remove = div()
            .id(SharedString::from(format!("attachment-remove-{}", image.id)))
            .group(remove_group.clone())
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Remove image {}", index + 1)))
            .absolute()
            .top(px(-8.0))
            .right(px(-8.0))
            .size(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(t.ink_faint)
            .hover(|style| style.text_color(t.ink))
            // Its fill stops at the padding box, inside a 2px clear border; the ring is inset there too.
            .child(
                div()
                    .id(SharedString::from(format!("attachment-remove-fill-{}", image.id)))
                    .absolute()
                    .inset(px(2.0))
                    .rounded_full()
                    .bg(t.surface_active)
                    .border_1()
                    .border_color(t.line_strong)
                    .group_hover(remove_group.clone(), |style| style.bg(t.control_hover)),
            )
            .child(ink_icon(IconName::X, 12.0))
            .tip("Remove")
            .on_click({
                let task = task.clone();
                move |_, _, cx| dispatch(AppCommand::ImageRemove { task_id: task.clone(), image_id: image_id.clone() }, cx)
            });
        strip = strip.child(div().relative().size(px(62.0)).child(open).child(remove));
    }
    Some(strip)
}

/// Everything drafted into the composer above its prompt.
pub fn drafts(root: &mut Root, state: &WorkspaceState, window: &Window, cx: &mut Context<Root>) -> Vec<AnyElement> {
    let key = root.composer.key(state);
    let task = root.composer.task.clone();
    let mut out = Vec::new();
    let annotations = state.annotations.get(&key).cloned().unwrap_or_default();
    if let Some(annotations) = annotation_row("draft", &annotations, true, root, window, cx) {
        out.push(row("annotation-row").child(annotations).into_any_element());
    }
    let pastes = state.pastes.get(&key).cloned().unwrap_or_default();
    if let Some(row_el) = paste_row(&pastes, true, root, cx) {
        out.push(row("paste-row").child(row_el).into_any_element());
    }
    if let Some(viewing) = root.composer.paste_viewing.clone() {
        match pastes.iter().position(|paste| paste.id == viewing) {
            Some(index) => out.push(paste_viewer(&pastes[index], index, root, window, cx)),
            None => root.composer.paste_viewing = None,
        }
    }
    let files = state.files.get(&key).cloned().unwrap_or_default();
    if let Some(row_el) = file_row(&files, true, task.clone(), window, cx) {
        out.push(row("file-row").child(row_el).into_any_element());
    }
    // Marks go with their image: once it has left every draft, so have they.
    root.composer.marks.retain(|id, _| state.images.values().any(|images| images.iter().any(|image| &image.id == id)));
    let images = state.images.get(&key).cloned().unwrap_or_default();
    if let Some(strip) = image_strip(root, &images, task, cx) {
        out.push(strip.into_any_element());
    }
    out
}

/// A pasted block read in full over the window, with a copy button.
fn paste_viewer(paste: &PastedText, index: usize, root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let mono = theme.typography.mono_family.clone();
    let text = paste.text.clone();
    let close = root.composer_listener(cx, |root, _: &ClickEvent, _, cx| {
        root.composer.paste_viewing = None;
        cx.notify();
    });
    let body = glass(div(), t, GlassKind::Float, Some(10.0), None)
        .id("paste-full")
        .max_w(px(880.0))
        .max_h(gpui::relative(1.0))
        .overflow_y_scroll()
        .pt(px(16.0))
        .px(px(18.0))
        .pb(px(16.0))
        .font_family(mono)
        .text_size(px(theme.metrics.text_meta))
        .line_height(gpui::relative(1.55))
        .text_color(t.ink_soft)
        .child(paste.text.clone())
        .on_click(|_, _, cx| cx.stop_propagation());
    let label = format!("pasted text #{}", index + 1);
    let viewer = div()
        .id("paste-viewer")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .pt(px(56.0))
        .px(px(32.0))
        .pb(px(32.0))
        .bg(t.veil_deep)
        .veil_backdrop(&t)
        .on_click(close)
        .child(body)
        .child(
            div()
                .absolute()
                .top(px(58.0))
                .right(px(58.0))
                .child(IconButton::new("paste-copy", IconName::Copy, "Copy pasted text").variant(IconButtonVariant::Soft).size(IconButtonSize::Md).tip("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone())))),
        )
        .child(div().absolute().top(px(58.0)).right(px(20.0)).child(IconButton::new("paste-close", IconName::X, format!("Close {label}")).variant(IconButtonVariant::Soft).size(IconButtonSize::Md).tip(super::chrome::key_tip("Close", "Escape")).on_click(root.composer_listener(cx, |root, _, _, cx| {
            root.composer.paste_viewing = None;
            cx.notify();
        }))));
    let viewport = window.viewport_size();
    deferred(gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(div().w(viewport.width).h(viewport.height).child(viewer))).with_priority(5).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::paste_summary;

    #[test]
    fn pastes_name_their_size() {
        assert_eq!(paste_summary("a\nb\nc"), "3 lines");
        assert_eq!(paste_summary(&"x".repeat(1200)), "1,200 characters");
    }
}
