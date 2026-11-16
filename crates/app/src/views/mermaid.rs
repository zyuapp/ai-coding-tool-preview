//! Mermaid diagrams (MermaidBlock.tsx). mermaid.js draws each one in a hidden WebKit page, which
//! measures and paints text as a browser does; the page also paints each drawing into pixels at the
//! scale it is shown at, which is what the transcript and the viewer draw.
// Only macOS and Linux have the WebKit pages these serve.
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use act_ui::theme::ActiveTheme;
use base64::Engine as _;
use gpui::{div, prelude::*, px, AnyElement, App, Global, RenderImage, SharedString, Window};
use serde::Deserialize;

/// A rendered diagram and the size it was drawn at, in CSS pixels.
pub struct Diagram {
    /// The page's name for the drawing, which it paints from.
    id: u64,
    pub width: f32,
    pub height: f32,
    /// Paintings made or asked for, by scale in hundredths.
    rasters: RefCell<HashMap<u32, Option<Arc<RenderImage>>>>,
}

#[derive(Clone)]
pub enum Drawn {
    Diagram(Rc<Diagram>),
    Error(String),
}

const MAX_REMEMBERED: usize = 64;
/// The width a diagram is drawn for when the block it belongs to has no width of its own yet.
const ASSUMED_ROOM: f32 = 720.0;
/// The most pixels one side of a painting may have.
const MAX_SIDE: f32 = 8192.0;
const MERMAID_JS: &str = include_str!("../../assets/mermaid/mermaid.min.js");

/// What a page draws with: the theme is baked into each drawing, so a change throws away what was drawn.
#[derive(Clone, PartialEq)]
struct Look {
    dark: bool,
    font: String,
    size: f32,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    drawn: bool,
    #[serde(default)]
    width: f32,
    #[serde(default)]
    height: f32,
    error: Option<String>,
    /// A painting: the scale it was asked for, and its PNG.
    scale: Option<u32>,
    png: Option<String>,
}

enum Job {
    Draw { id: u64, source: String, room: f32 },
    Paint { id: u64, scale: u32 },
}

#[derive(Default)]
struct Engine {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    page: Option<act_browser::hidden::HiddenPage>,
    ready: bool,
    broken: Option<String>,
    look: Option<Look>,
    generation: u64,
    next_id: u64,
    waiting: VecDeque<Job>,
    asked: HashMap<u64, (String, u64)>,
    drawn: HashMap<String, Drawn>,
    order: VecDeque<String>,
    /// Diagrams with paintings on the way, by their page id.
    painting: HashMap<u64, std::rc::Weak<Diagram>>,
}

#[derive(Clone, Default)]
struct Mermaid(Rc<RefCell<Engine>>);

impl Global for Mermaid {}

fn engine(cx: &mut App) -> Rc<RefCell<Engine>> {
    if !cx.has_global::<Mermaid>() {
        cx.set_global(Mermaid::default());
    }
    cx.global::<Mermaid>().0.clone()
}

impl Engine {
    fn remember(&mut self, source: String, drawn: Drawn) {
        self.order.retain(|known| known != &source);
        self.order.push_back(source.clone());
        self.drawn.insert(source, drawn);
        while self.order.len() > MAX_REMEMBERED {
            if let Some(stale) = self.order.pop_front() {
                self.drawn.remove(&stale);
            }
        }
    }

    fn submit(&mut self, job: Job) {
        if self.ready {
            self.send(job);
        } else {
            self.waiting.push_back(job);
        }
    }

    fn send(&mut self, job: Job) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (Some(look), Some(page)) = (&self.look, &self.page) else { return };
            let script = match &job {
                Job::Draw { id, source, room } => {
                    let call = serde_json::json!({ "id": id, "source": source, "room": room, "dark": look.dark, "font": look.font, "size": look.size.round(), "key": self.generation });
                    format!("window.__draw({call})")
                }
                Job::Paint { id, scale } => format!("window.__paint({id}, {scale})"),
            };
            if let Err(error) = page.view.evaluate_script(&script) {
                if let Job::Draw { id, source, .. } = job {
                    self.asked.remove(&id);
                    self.remember(source, Drawn::Error(error.to_string()));
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        if let Job::Draw { source, .. } = job {
            self.remember(source, Drawn::Error("Diagrams are not available on this platform yet.".into()));
        }
    }
}

/// The page the drawings are made in. Its script answers each job through the IPC channel.
const BRIDGE: &str = r#"
let initialized = null;
const drawings = new Map();
function frameOf(element) {
  const [x, y, width, height] = (element.getAttribute("viewBox") || "").trim().split(/[\s,]+/).map(Number);
  if (![x, y, width, height].every(Number.isFinite) || !(width > 0) || !(height > 0)) return null;
  return { x, y, width, height };
}
function natural(svg) {
  const parsed = new DOMParser().parseFromString(svg, "image/svg+xml").querySelector("svg");
  if (!parsed) return null;
  const element = document.importNode(parsed, true);
  element.removeAttribute("width");
  element.removeAttribute("height");
  element.style.removeProperty("max-width");
  let frame = frameOf(element);
  if (!frame) {
    const host = document.createElement("div");
    host.setAttribute("style", "position: absolute; left: -100000px; top: 0; visibility: hidden");
    host.appendChild(element);
    document.body.appendChild(host);
    try {
      const box = element.getBBox();
      if (box.width > 0 && box.height > 0) frame = { x: box.x - 8, y: box.y - 8, width: box.width + 16, height: box.height + 16 };
    } catch (_) {}
    host.remove();
    if (!frame) return null;
    element.setAttribute("viewBox", `${frame.x} ${frame.y} ${frame.width} ${frame.height}`);
  }
  element.setAttribute("width", String(frame.width));
  element.setAttribute("height", String(frame.height));
  return { markup: new XMLSerializer().serializeToString(element), width: frame.width, height: frame.height };
}
window.__draw = async (job) => {
  try {
    if (initialized !== job.key) {
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: job.dark ? "dark" : "default",
        layout: "dagre",
        htmlLabels: false,
        fontFamily: job.font,
        fontSize: job.size,
        themeVariables: { fontSize: `${job.size}px` },
      });
      document.body.style.fontFamily = job.font;
      document.body.style.fontSize = `${job.size}px`;
      await document.fonts.ready;
      initialized = job.key;
    }
    const scratch = document.createElement("div");
    const width = Math.max(320, Math.round(job.room) || 720);
    scratch.setAttribute("style", `position: absolute; left: -100000px; top: 0; width: ${width}px; visibility: hidden`);
    document.body.appendChild(scratch);
    let svg;
    try {
      svg = (await mermaid.render(`mermaid-${job.key}-${job.id}`, job.source, scratch)).svg;
    } finally {
      scratch.remove();
    }
    const drawn = natural(svg);
    if (!drawn) throw new Error("The diagram has no size.");
    drawings.set(job.id, drawn);
    if (drawings.size > 96) drawings.delete(drawings.keys().next().value);
    window.ipc.postMessage(JSON.stringify({ id: job.id, drawn: true, width: drawn.width, height: drawn.height }));
  } catch (error) {
    window.ipc.postMessage(JSON.stringify({ id: job.id, error: error && error.message ? error.message : String(error) }));
  }
};
// WebKit draws no CSS drop-shadow() on SVG shapes, which Chromium does: each becomes an SVG filter.
function shadowed(markup) {
  const filters = [];
  const out = markup.replace(/drop-shadow\(\s*(-?[\d.]+)(?:px)?\s+(-?[\d.]+)(?:px)?\s+(-?[\d.]+)(?:px)?\s+((?:rgba?|hsla?)\([^)]*\)|#[0-9a-fA-F]+|[a-z]+)\s*\)/g, (_, dx, dy, blur, color) => {
    const id = `aic-shadow-${filters.length}`;
    filters.push(`<filter id="${id}" x="-50%" y="-50%" width="200%" height="200%"><feDropShadow dx="${dx}" dy="${dy}" stdDeviation="${Number(blur) / 2}" flood-color="${color}"/></filter>`);
    return `url(#${id})`;
  });
  if (!filters.length) return markup;
  return out.replace(/<svg\b[^>]*>/, (open) => `${open}<defs>${filters.join("")}</defs>`);
}
window.__paint = (id, scale) => {
  const drawn = drawings.get(id);
  if (!drawn) return window.ipc.postMessage(JSON.stringify({ id, scale, error: "gone" }));
  const load = (markup) => new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = reject;
    image.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(markup);
  });
  const shadows = shadowed(drawn.markup);
  // A filtered image comes back with shifted colours, so it only lays the shadows under the plain one.
  Promise.all([shadows === drawn.markup ? null : load(shadows), load(drawn.markup)]).then(([under, image]) => {
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.ceil(drawn.width * scale / 100));
    canvas.height = Math.max(1, Math.ceil(drawn.height * scale / 100));
    const context = canvas.getContext("2d");
    if (under) context.drawImage(under, 0, 0, canvas.width, canvas.height);
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
    const png = canvas.toDataURL("image/png").split(",")[1];
    window.ipc.postMessage(JSON.stringify({ id, scale, png }));
  }, () => window.ipc.postMessage(JSON.stringify({ id, scale, error: "paint" })));
};
window.ipc.postMessage(JSON.stringify({ ready: true }));
"#;

/// The prose font, as the page names it.
fn page_font(ui_family: &str) -> String {
    if ui_family.starts_with('.') || ui_family.is_empty() {
        "-apple-system, BlinkMacSystemFont, sans-serif".into()
    } else {
        format!("\"{ui_family}\", -apple-system, sans-serif")
    }
}

fn look(cx: &App) -> Look {
    let theme = cx.theme();
    Look { dark: theme.tokens.variant == act_ui::ThemeVariant::Dark, font: page_font(theme.typography.ui_family.as_ref()), size: theme.metrics.text_content }
}

/// A painting's PNG, as the BGRA frame GPUI draws.
fn decode(png: &str) -> Option<Arc<RenderImage>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(png).ok()?;
    let mut frame = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.into_rgba8();
    for pixel in frame.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(frame)])))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_page(engine: &Rc<RefCell<Engine>>, window: &mut Window, cx: &mut App) {
    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<String>();
    let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body><script>{MERMAID_JS}</script><script>{BRIDGE}</script></body></html>");
    let builder = wry::WebViewBuilder::new().with_html(html).with_transparent(true).with_ipc_handler(move |request| {
        let _ = sender.unbounded_send(request.into_body());
    });
    let built = act_browser::hidden::open(builder, 1200.0, 900.0, window, cx);
    match built {
        Ok(page) => engine.borrow_mut().page = Some(page),
        Err(error) => {
            engine.borrow_mut().broken = Some(error.to_string());
            return;
        }
    }
    let engine = engine.clone();
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some(message) = receiver.next().await {
            let Ok(reply) = serde_json::from_str::<Reply>(&message) else { continue };
            if let Some(scale) = reply.scale {
                let painted = match reply.png {
                    Some(png) => cx.background_spawn(async move { decode(&png) }).await,
                    None => None,
                };
                let engine = engine.clone();
                let _ = cx.update(|cx| {
                    let diagram = engine.borrow_mut().painting.get(&reply.id).and_then(std::rc::Weak::upgrade);
                    if let Some(diagram) = diagram {
                        diagram.rasters.borrow_mut().insert(scale, painted);
                    }
                    cx.refresh_windows();
                });
                continue;
            }
            let engine = engine.clone();
            let _ = cx.update(|cx| {
                let mut engine = engine.borrow_mut();
                if reply.ready {
                    engine.ready = true;
                    while let Some(job) = engine.waiting.pop_front() {
                        engine.send(job);
                    }
                    return;
                }
                let Some((source, generation)) = engine.asked.remove(&reply.id) else { return };
                if generation != engine.generation {
                    return;
                }
                let drawn = match reply.error {
                    Some(error) => Drawn::Error(error),
                    None if reply.drawn => Drawn::Diagram(Rc::new(Diagram { id: reply.id, width: reply.width, height: reply.height, rasters: RefCell::default() })),
                    None => Drawn::Error("Mermaid returned nothing.".into()),
                };
                engine.remember(source, drawn);
                drop(engine);
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

/// What `source` has drawn as, asking for it when nothing has been yet. `room` is the block's width.
pub fn drawn(source: &str, room: f32, window: &mut Window, cx: &mut App) -> Option<Drawn> {
    let engine = engine(cx);
    let wanted = look(cx);
    {
        let mut state = engine.borrow_mut();
        if state.look.as_ref() != Some(&wanted) {
            if state.look.is_some() {
                state.generation += 1;
                state.drawn.clear();
                state.order.clear();
                state.asked.clear();
            }
            state.look = Some(wanted);
        }
        if let Some(drawn) = state.drawn.get(source) {
            return Some(drawn.clone());
        }
        if let Some(broken) = &state.broken {
            return Some(Drawn::Error(broken.clone()));
        }
        let generation = state.generation;
        if state.asked.values().any(|(asked, at)| asked == source && *at == generation) {
            return None;
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if engine.borrow().page.is_none() {
        open_page(&engine, window, cx);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = window;
    let mut state = engine.borrow_mut();
    if let Some(broken) = &state.broken {
        return Some(Drawn::Error(broken.clone()));
    }
    state.next_id += 1;
    let id = state.next_id;
    let generation = state.generation;
    state.asked.insert(id, (source.to_string(), generation));
    state.submit(Job::Draw { id, source: source.to_string(), room });
    state.drawn.get(source).cloned()
}

/// Draws the source again, as Retry asks.
pub fn forget(source: &str, cx: &mut App) {
    let engine = engine(cx);
    let mut state = engine.borrow_mut();
    state.drawn.remove(source);
    state.order.retain(|known| known != source);
}

/// The diagram painted for `scale` device pixels per CSS pixel, or the nearest painting made so far
/// while that one is on its way.
pub fn raster(diagram: &Rc<Diagram>, scale: f32, cx: &mut App) -> Option<Arc<RenderImage>> {
    let largest = diagram.width.max(diagram.height).max(1.0);
    let scale = scale.min(MAX_SIDE / largest);
    let key = (scale * 100.0).round().max(25.0) as u32;
    let known = diagram.rasters.borrow().get(&key).cloned();
    if let Some(known) = known {
        if known.is_some() {
            return known;
        }
    } else {
        diagram.rasters.borrow_mut().insert(key, None);
        let engine = engine(cx);
        let mut state = engine.borrow_mut();
        state.painting.insert(diagram.id, Rc::downgrade(diagram));
        state.painting.retain(|_, diagram| diagram.strong_count() > 0);
        state.submit(Job::Paint { id: diagram.id, scale: key });
    }
    diagram.rasters.borrow().iter().filter_map(|(at, image)| image.clone().map(|image| (*at, image))).min_by_key(|(at, _)| at.abs_diff(key)).map(|(_, image)| image)
}

/// The diagram at `scale` times its own size.
pub fn picture(id: impl Into<SharedString>, diagram: &Rc<Diagram>, scale: f32, window: &mut Window, cx: &mut App) -> AnyElement {
    let (width, height) = (diagram.width * scale, diagram.height * scale);
    let painted = raster(diagram, scale * window.scale_factor(), cx);
    let id: SharedString = id.into();
    match painted {
        // Drawn pixel for pixel from a device-pixel corner, so its hairlines stay as sharp as the vector page draws them.
        Some(image) => div()
            .id(id)
            .flex_none()
            .w(px(width))
            .h(px(height))
            .child(
                gpui::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let scale = window.scale_factor();
                        let pixels = image.size(0);
                        let origin = gpui::point(px((f32::from(bounds.origin.x) * scale).round() / scale), px((f32::from(bounds.origin.y) * scale).round() / scale));
                        let size = gpui::size(px(pixels.width.0 as f32 / scale), px(pixels.height.0 as f32 / scale));
                        let _ = window.paint_image(gpui::Bounds { origin, size }, gpui::Corners::default(), image.clone(), 0, false);
                    },
                )
                .size_full(),
            )
            .into_any_element(),
        None => div().id(id).flex_none().w(px(width)).h(px(height)).into_any_element(),
    }
}

/// A diagram in a message: drawn once visible, framed like code, opening at full size on a click.
pub fn block(id: gpui::ElementId, source: SharedString, pending: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let group: SharedString = format!("mermaid-{id}").into();
    let scroll_key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        SharedString::from(format!("mermaid-scroll-{id}-{:x}", hasher.finish()))
    };
    let scroll = window.use_keyed_state(scroll_key, cx, |_, _| gpui::ScrollHandle::new()).read(cx).clone();
    let expand_focus = window.use_keyed_state(SharedString::from(format!("mermaid-expand-{id}")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
    let frame = div()
        .id(gpui::ElementId::NamedChild(Box::new(id.clone()), "frame".into()))
        .min_h(px(76.0))
        .overflow_x_scroll()
        .track_scroll(&scroll)
        .border_1()
        .border_color(t.line)
        .rounded(px(10.0))
        .p(px(15.0))
        .bg(t.sunken);
    let drawn = if pending { None } else { drawn(&source, ASSUMED_ROOM, window, cx) };
    let mut outer = div().relative().group(group.clone());
    match drawn {
        Some(Drawn::Diagram(diagram)) => {
            let opened = (diagram.clone(), source.to_string());
            let picture = picture(gpui::ElementId::NamedChild(Box::new(id.clone()), "svg".into()).to_string(), &diagram, 1.0, window, cx);
            let open = {
                let opened = opened.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| super::viewer::open_diagram(opened.0.clone(), opened.1.clone(), cx)
            };
            outer = outer
                .child(
                    frame
                        .hover(|style| style.border_color(t.line_bright))
                        .cursor(gpui::CursorStyle::PointingHand)
                        .skip_tab_stop()
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
                        .on_click(open.clone())
                        .flex()
                        .flex_col()
                        .items_start()
                        .child(div().flex_none().min_w_full().flex().child(div().mx_auto().flex_none().child(picture)))
                        .child(act_markdown::OverflowGutter::new(px(diagram.width), px(12.0))),
                )
                .child(act_ui::components::scrollbar::Scrollbar::horizontal(gpui::ElementId::NamedChild(Box::new(id.clone()), "bar".into()), &scroll))
                .child(
                    // Drawn clear rather than left out, so the keyboard still reaches it.
                    div().absolute().top(px(6.0)).right(px(38.0)).when(!expand_focus.is_focused(window), |button| button.opacity(0.0).group_hover(group, |style| style.opacity(1.0))).child(
                        act_ui::components::button::IconButton::new(gpui::ElementId::NamedChild(Box::new(id), "expand".into()), act_ui::IconName::Maximize2, "Open the diagram at full size")
                            .track_focus(&expand_focus)
                            .variant(act_ui::components::button::IconButtonVariant::Glass)
                            .icon_size(14.0)
                            .tip("Full size")
                            .on_click(open),
                    ),
                );
        }
        Some(Drawn::Error(error)) => {
            let retry_source = source.to_string();
            outer = outer.child(frame.child(block_error(
                gpui::ElementId::NamedChild(Box::new(id), "error".into()),
                "Diagram could not be rendered",
                &error,
                Some(source.to_string()),
                move |cx| {
                    forget(&retry_source, cx);
                    cx.refresh_windows();
                },
                cx,
            )));
        }
        None => {
            outer = outer.child(frame.child(div().p(px(16.0)).flex().justify_center().text_size(px(cx.theme().metrics.text_meta)).child(super::sweep::sweep(gpui::ElementId::NamedChild(Box::new(id), "loading".into()), "Rendering diagram…", cx).into_any_element())));
        }
    }
    outer.into_any_element()
}

/// A block that failed to draw: what went wrong at a glance, the rest on opening, and another go (BlockError).
pub fn block_error(id: gpui::ElementId, title: &'static str, error: &str, source: Option<String>, retry: impl Fn(&mut App) + 'static, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let open_id = gpui::ElementId::NamedChild(Box::new(id.clone()), "open".into());
    let open = window_state_bool(&open_id, cx);
    let reason = error.lines().next().unwrap_or("").trim().to_string();
    let toggle_id = open_id.clone();
    let mut details = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id(open_id)
                .py(px(6.0))
                .cursor_pointer()
                .text_color(t.danger_soft)
                .child(div().flex().items_center().gap(px(7.0)).child(super::common::icon(act_ui::IconName::CircleAlert, 14.0, t.danger_soft)).child(title))
                .when(!reason.is_empty(), |summary| summary.child(div().mt(px(3.0)).ml(px(20.0)).text_color(t.muted).child(reason.clone())))
                .on_click(move |_, _, cx| toggle_state_bool(&toggle_id, cx)),
        );
    if open {
        details = details.child(div().my(px(8.0)).text_color(t.muted).child(error.to_string()));
        if let Some(source) = source {
            details = details.child(div().border_1().border_color(t.line).rounded(px(8.0)).p(px(11.0)).bg(t.sunken).text_color(t.muted_strong).font_family(mono).text_size(px(m.text_meta)).line_height(gpui::relative(1.55)).child(source));
        }
    }
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .text_size(px(m.text_meta))
        .child(details)
        .child(act_ui::components::button::Button::new(gpui::ElementId::NamedChild(Box::new(id), "retry".into()), "Try again").pill(true).on_click(move |_, _, cx| retry(cx)))
        .into_any_element()
}

thread_local! {
    static OPEN_FOLDS: RefCell<std::collections::HashSet<String>> = RefCell::default();
}

fn window_state_bool(id: &gpui::ElementId, _cx: &App) -> bool {
    OPEN_FOLDS.with(|open| open.borrow().contains(&id.to_string()))
}

fn toggle_state_bool(id: &gpui::ElementId, cx: &mut App) {
    OPEN_FOLDS.with(|open| {
        let mut open = open.borrow_mut();
        let key = id.to_string();
        if !open.remove(&key) {
            open.insert(key);
        }
    });
    cx.refresh_windows();
}
