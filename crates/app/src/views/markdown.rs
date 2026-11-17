//! Messages drawn as Markdown, styled from the theme, with their links wired to the workspace
//! (MarkdownMessage.tsx, StreamingText.tsx).

use std::cell::Cell;

use act_core::AppCommand;
use act_markdown::{LinkAction, LinkCapabilities, LinkEvent, LinkTarget, MarkdownEvent, MarkdownStyle, MarkdownView};
use act_ui::theme::{ActiveTheme, Theme};
use act_ui::tooltip::Tippable;
use gpui::{px, App, ElementId, Entity, Hsla, InteractiveElement, IntoElement, Styled, Window};

use crate::effects::window_desktop;
use crate::workspace::dispatch;

/// Where a message is drawn, which sets its size, leading and ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Answer,
    WorkNote,
    /// What the user wrote, shown as written.
    User,
    /// A tool call's input, shown as written in the code face.
    Input,
}

fn hsla(color: gpui::Rgba) -> Hsla {
    color.into()
}

pub fn style(theme: &Theme, variant: Variant) -> MarkdownStyle {
    let t = &theme.tokens;
    let mut style = if t.variant == act_ui::ThemeVariant::Dark { MarkdownStyle::dark() } else { MarkdownStyle::light() };
    style.ui_font = theme.typography.ui_family.clone();
    style.mono_font = theme.typography.mono_family.clone();
    style.font_size = px(theme.metrics.text_content);
    style.line_height = match variant {
        Variant::User => 1.5,
        _ => 1.7,
    };
    style.text = hsla(match variant {
        Variant::Answer | Variant::User => t.ink_soft,
        Variant::WorkNote | Variant::Input => t.muted_strong,
    });
    if variant == Variant::Input {
        style.ui_font = theme.typography.mono_family.clone();
        style.font_size = px(theme.metrics.text_meta);
        style.line_height = 1.55;
    }
    style.ink = hsla(t.ink);
    style.muted_strong = hsla(t.muted_strong);
    style.quiet = hsla(t.quiet);
    style.accent_text = hsla(t.accent_text);
    style.accent_soft = hsla(t.accent_soft);
    style.link = hsla(t.link);
    style.link_hover = hsla(t.link_hover);
    style.link_line = hsla(t.link_line);
    style.code_ink = hsla(t.code_ink);
    style.inline_code_background = hsla(act_ui::color::mix(t.code_surface, 0.7, t.canvas));
    style.surface = hsla(t.surface);
    style.line = hsla(t.line);
    style.line_soft = hsla(t.line_soft);
    style.line_strong = hsla(t.line_strong);
    style.selection = hsla(t.text_selection);
    style.file_glyph = Some(act_ui::IconName::FileText.path().into());
    style.thread_glyph = Some(act_ui::IconName::MessageSquareText.path().into());
    style
}

fn capabilities() -> LinkCapabilities {
    LinkCapabilities { select_thread: true, open_file: true, open_url_in_app: true, open_image: true, open_commit: true }
}

/// The checkout and message a reply's linked images are read through (MessageArtifactScope).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageScope {
    pub root: String,
    pub message_id: String,
}

/// A message's view, kept for as long as the row drawing it is, and brought up to `text`. With a
/// scope, links to local images open in the viewer rather than as files.
pub fn message_view(id: impl Into<ElementId>, text: &str, streaming: bool, variant: Variant, task_id: Option<String>, images: Option<ImageScope>, window: &mut Window, cx: &mut App) -> Entity<MarkdownView> {
    let fresh = Cell::new(false);
    let theme = cx.theme().clone();
    let view = window.use_keyed_state(id, cx, |window, cx| {
        fresh.set(true);
        let mut view = MarkdownView::new(window, cx);
        view.set_plain(matches!(variant, Variant::User | Variant::Input), cx);
        view.copy_on_action::<act_ui::input::Copy>();
        view.set_style(style(&theme, variant), cx);
        view.set_link_capabilities(capabilities(), cx);
        view.set_has_message_id(images.is_some(), cx);
        view.set_fence_renderer(fence);
        view.set_copy_button(super::timeline_row::block_copy_button);
        view.set_link_tip(|id, tip, place, _, _| gpui::div().id(id).size_full().tip_placed(tip, place).into_any_element());
        view.set_scrollbar_renderer(|id, handle, _, _| act_ui::components::scrollbar::Scrollbar::horizontal(id, handle).into_any_element());
        view
    });
    // Inline code naming a commit opens it once the thread's checkout is known to hold it.
    let checkout = task_id.as_deref().and_then(|id| {
        let state = crate::workspace::workspace(cx).read(cx).state.clone();
        // A paired computer's thread is looked up, and its checkout asked, on that computer.
        let holder = std::iter::once(&*state).chain(state.computers.paired.iter().filter_map(|computer| computer.state.as_deref())).find(|held| held.thread(id).is_some())?;
        act_core::workspace::location::thread_workspace_id(holder, holder.thread(id))
    });
    if let Some(checkout) = checkout {
        super::known_commits::install(cx);
        let weak = view.downgrade();
        view.update(cx, |view, _| view.set_known_commit(move |hash| super::known_commits::known(&checkout, hash, &weak)));
    }
    if fresh.get() {
        let task = task_id.clone();
        let handle = window.window_handle();
        cx.subscribe(&view, move |_, event: &MarkdownEvent, cx| {
            if let MarkdownEvent::Link(link) = event {
                if let (LinkTarget::Image(path), LinkAction::Click { .. }, Some(scope)) = (&link.target, &link.action, &images) {
                    return open_message_image(path, scope, task.as_deref(), cx);
                }
                let (link, task) = (link.clone(), task.clone());
                let _ = handle.update(cx, |_, window, cx| follow_link(&link, task, window, cx));
            }
        })
        .detach();
    }
    view.update(cx, |view, cx| {
        let wanted = style(&theme, variant);
        if view.style().text != wanted.text || view.style().font_size != wanted.font_size || view.style().ui_font != wanted.ui_font {
            view.set_style(wanted, cx);
        }
        if view.text() != text || view.is_streaming() != streaming {
            view.set_text(text.to_string(), streaming, cx);
        }
    });
    view
}

/// A click follows the link; ⌘ takes a web link to the dock's browser. A web or file link's context
/// menu offers the same, and copying it (WebLink and FileLink in MarkdownMessage.tsx).
pub fn follow_link(link: &LinkEvent, task_id: Option<String>, window: &mut Window, cx: &mut App) {
    let modifiers = match link.action {
        LinkAction::Click { modifiers } => modifiers,
        LinkAction::ContextMenu { position } => return link_menu(&link.target, task_id, position, window, cx),
    };
    match &link.target {
        LinkTarget::Thread(id) => dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx),
        LinkTarget::File(file) => dispatch(AppCommand::FileOpen { task_id, path: file.file.clone(), line: file.line.map(|line| line as i64) }, cx),
        LinkTarget::Web(url) if modifiers.secondary() => open_url_in_app(url, cx),
        LinkTarget::Web(url) | LinkTarget::Other(url) => dispatch(AppCommand::LinkOpen { url: url.clone() }, cx),
        LinkTarget::Image(source) => dispatch(AppCommand::ImageOpen { source: source.clone(), gallery: None }, cx),
        LinkTarget::Commit(commit) => dispatch(AppCommand::DiffOpenCommit { commit: commit.clone(), task_id }, cx),
    }
}

/// The images a message links, shown in order, each opening the viewer over all of them.
fn message_gallery(text: &str, scope: &ImageScope, task_id: Option<&str>) -> Vec<String> {
    linked_images(&scope.message_id, text).iter().map(|image| act_host::image_download::message_image_url(&image.path, &scope.root, &scope.message_id, false, task_id)).collect()
}

fn open_message_image(path: &str, scope: &ImageScope, task_id: Option<&str>, cx: &mut App) {
    let source = act_host::image_download::message_image_url(path, &scope.root, &scope.message_id, false, task_id);
    let text = MESSAGE_IMAGES.with(|cache| cache.borrow().get(&scope.message_id).map(|(text, _)| text.clone()));
    let gallery = text.map(|text| message_gallery(&text, scope, task_id)).filter(|gallery| gallery.len() > 1);
    dispatch(AppCommand::ImageOpen { source, gallery }, cx);
}

thread_local! {
    /// Each message's linked images by id, with the text they were read from.
    static MESSAGE_IMAGES: std::cell::RefCell<std::collections::HashMap<String, (String, std::rc::Rc<Vec<act_markdown::images::MessageImage>>)>> = Default::default();
    /// Previews asked for, and those that could not be made.
    static PREVIEWS: std::cell::RefCell<(std::collections::HashSet<std::path::PathBuf>, std::collections::HashSet<std::path::PathBuf>)> = Default::default();
    /// Each message's preview strip scroll, which its edge fades follow.
    static STRIPS: std::cell::RefCell<std::collections::HashMap<String, gpui::ScrollHandle>> = Default::default();
}

fn linked_images(message_id: &str, text: &str) -> std::rc::Rc<Vec<act_markdown::images::MessageImage>> {
    MESSAGE_IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((known, images)) = cache.get(message_id) {
            if known == text {
                return images.clone();
            }
        }
        let images = std::rc::Rc::new(act_markdown::images::message_images(text));
        if cache.len() > 2_000 {
            cache.clear();
        }
        cache.insert(message_id.to_string(), (text.to_string(), images.clone()));
        images
    })
}

/// Previews of the local images a settled message links, under it (MessageImagePreview).
pub fn image_previews(text: &str, scope: &ImageScope, task_id: Option<&str>, cx: &mut App) -> Option<gpui::AnyElement> {
    use act_ui::style::Raised;
    use gpui::{div, img, InteractiveElement, ObjectFit, ParentElement, StatefulInteractiveElement, Styled, StyledImage};
    let images = linked_images(&scope.message_id, text);
    if images.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let gallery = message_gallery(text, scope, task_id);
    let strip = STRIPS.with(|strips| strips.borrow_mut().entry(scope.message_id.clone()).or_default().clone());
    // `preview-fade`: a strip wider than the answer fades at whichever edge still has previews past
    // it, each fade growing to 28px over the first and last 4% of the scroll.
    let max_x = f32::from(strip.max_offset().width);
    let fade = (max_x > 1.0).then(|| {
        let progress = (-f32::from(strip.offset().x) / max_x).clamp(0.0, 1.0);
        gpui::EdgeFade::horizontal().start(px(0.0), px(28.0 * (progress / 0.04).min(1.0))).end(px(0.0), px(28.0 * ((1.0 - progress) / 0.04).min(1.0)))
    });
    let mut row = div().id(gpui::SharedString::from(format!("message-images-{}", scope.message_id))).flex().gap(px(12.0)).max_w_full().overflow_x_scroll().track_scroll(&strip).pt(px(2.0)).pb(px(8.0)).mt(px(16.0));
    row.style().restrict_scroll_to_axis = Some(true);
    if let Some(fade) = fade {
        row = row.edge_fade(fade);
    }
    for (index, image) in images.iter().enumerate() {
        let Some(window_desktop::MessageImage { original, shown }) = window_desktop::message_image(&image.path, &scope.root, &scope.message_id, cx) else { continue };
        let failed = PREVIEWS.with(|previews| previews.borrow().1.contains(&original));
        if shown.is_none() && !failed && PREVIEWS.with(|previews| previews.borrow_mut().0.insert(original.clone())) {
            let key = original.clone();
            let copied = window_desktop::preserve_message_image(image.path.clone(), scope.root.clone(), scope.message_id.clone(), cx);
            cx.spawn(async move |cx| {
                if copied.await.is_err() {
                    PREVIEWS.with(|previews| previews.borrow_mut().1.insert(key));
                }
                let _ = cx.update(|cx| cx.refresh_windows());
            })
            .detach();
        }
        let id = gpui::SharedString::from(format!("message-image-{}-{index}", scope.message_id));
        let preview = if failed {
            let path = image.path.clone();
            let task = task_id.map(String::from);
            let size = cx.theme().metrics.text_content * 0.85;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_h(px(36.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(t.line_strong)
                .text_color(t.ink_dim)
                .text_size(px(size))
                .line_height(gpui::relative(1.7))
                .relative()
                .rim(t.glass_rim, act_ui::style::Corners::Radius(8.0))
                .cursor_pointer()
                .hover(|style| style.bg(t.surface_hover).text_color(t.ink))
                .child(act_ui::Icon::new(act_ui::IconName::FileText).size(px(size * 0.85)))
                .child(format!("{} · Preview unavailable", image.label))
                .tip(image.path.clone())
                .on_click(move |_, _, cx| dispatch(AppCommand::FileOpen { task_id: task.clone(), path: path.clone(), line: None }, cx))
                .into_any_element()
        } else {
            let (source, gallery) = (gallery[index].clone(), gallery.clone());
            let mut button = div()
                .id(id)
                .flex_none()
                .w(px(320.0))
                .h(px(180.0))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(t.line)
                .bg(t.sunken)
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|style| style.border_color(t.line_strong))
                .accessible_name(format!("Enlarge {}", image.label))
                .on_click(move |_, _, cx| dispatch(AppCommand::ImageOpen { source: source.clone(), gallery: (gallery.len() > 1).then(|| gallery.clone()) }, cx));
            if let Some(shown) = shown {
                button = button.child(img(shown).max_w_full().max_h_full().object_fit(ObjectFit::Contain));
            }
            button.into_any_element()
        };
        row = row.child(preview);
    }
    Some(row.into_any_element())
}

fn open_url_in_app(url: &str, cx: &mut App) {
    super::sidebar::send(serde_json::json!({ "type": "browser.open", "url": url, "newTab": true }), cx);
}

fn link_menu(target: &LinkTarget, task_id: Option<String>, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut App) {
    use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
    let entries: Vec<MenuEntry> = match target {
        LinkTarget::Web(url) => {
            let (browser, app, copy) = (url.clone(), url.clone(), url.clone());
            vec![
                MenuItem::new("Open in browser").on_select(move |_, cx| dispatch(AppCommand::LinkOpen { url: browser.clone() }, cx)).into(),
                MenuItem::new(concat!("Open in ", act_core::app_name!())).shortcut(if cfg!(target_os = "macos") { "⌘Click" } else { "Ctrl+Click" }).on_select(move |_, cx| open_url_in_app(&app, cx)).into(),
                MenuEntry::Separator,
                MenuItem::new("Copy link").on_select(move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))).into(),
            ]
        }
        LinkTarget::File(file) => {
            let (path, line, copy) = (file.file.clone(), file.line.map(|line| line as i64), file.file.clone());
            vec![
                MenuItem::new("Open file").on_select(move |_, cx| dispatch(AppCommand::FileOpen { task_id: task_id.clone(), path: path.clone(), line }, cx)).into(),
                MenuEntry::Separator,
                MenuItem::new("Copy path").on_select(move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))).into(),
            ]
        }
        _ => return,
    };
    let Some(root) = window.root::<super::root::Root>().flatten() else { return };
    let menu = Menu::build(entries, act_ui::overlay::Placement::At { position }, window, cx, |menu| menu.top_level());
    root.update(cx, |root, cx| root.menus.show("message-link", menu, cx));
}

/// Fenced blocks the app draws itself: Mermaid diagrams and visuals.
fn fence(request: act_markdown::FenceRequest, window: &mut Window, cx: &mut App) -> Option<(gpui::AnyElement, gpui::SharedString)> {
    match request.lang.as_ref() {
        "mermaid" => Some((super::mermaid::block(request.id, request.source, request.pending, window, cx), "Copy the diagram".into())),
        "visual" => Some((super::visual::block(request.id, request.source, request.pending, false, window, cx), "Copy the visual's HTML".into())),
        _ => None,
    }
}
