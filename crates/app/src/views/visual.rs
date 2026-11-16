//! Interactive visuals (VisualBlock.tsx, domain/visual-frame.ts): a `visual` fence is HTML, CSS and
//! script drawn live in its reply. Each runs in a WebKit page of its own, served from a scheme that
//! reaches nothing of the app's and nothing on the network, laid over the row it belongs to and
//! clipped to what of that row is in view. The page reports the height its content needs.
// Only macOS has the WebKit pages these run in.
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code, unused_imports))]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_ui::components::button::{IconButton, IconButtonVariant};
use act_ui::theme::{ActiveTheme, Theme};
use act_ui::IconName;
use gpui::{canvas, div, prelude::*, px, AnyElement, App, Bounds, ElementId, Global, Pixels, SharedString, Window};
use serde::Deserialize;

pub const VISUAL_SCHEME: &str = "aicodingtool-visual";
const FRAME_URL: &str = "aicodingtool-visual://frame/";
/// A visual taller than this scrolls inside its page, so a layout that grows with its page cannot run away.
const MAX_HEIGHT: f32 = 2000.0;
/// How long a visual may take to report its size before it is taken to have failed.
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REMEMBERED: usize = 64;
/// Pages kept alive off screen, so scrolling back to a visual finds it as it was left.
const MAX_PAGES: usize = 12;
/// How long a page off screen is kept.
const IDLE_PAGE: Duration = Duration::from_secs(120);
/// Room the row's copy and full-size buttons take at its top right, which the page leaves open.
const BUTTONS: (f32, f32) = (70.0, 40.0);

const CSP: &str = "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; style-src 'unsafe-inline'; img-src data: blob:; media-src data: blob:; font-src data:; form-action 'none'; base-uri 'none'";

/// Runs before the visual. It takes the markup once, applies the theme whenever it is sent one, and
/// reports the height the content needs, so the page never scrolls a visual that fits.
const BRIDGE: &str = r#"(() => {
  const post = (message) => window.ipc.postMessage(JSON.stringify(message));
  let rendered = false;
  let reported = -1;
  let queued = false;
  let failed = false;
  const measure = () => {
    queued = false;
    const body = document.body;
    const height = Math.ceil(Math.max(body.scrollHeight, body.getBoundingClientRect().height));
    document.documentElement.style.overflowY = height > 2000 ? "auto" : "hidden";
    if (height === reported) return;
    reported = height;
    post({ type: "size", height });
  };
  const schedule = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(measure);
    setTimeout(() => queued && measure(), 100);
  };
  const style = (scheme, vars) => {
    const root = document.documentElement;
    if (vars && typeof vars === "object") {
      for (const [name, value] of Object.entries(vars)) {
        if (/^--[a-z0-9-]+$/.test(name) && typeof value === "string") root.style.setProperty(name, value);
      }
    }
    if (scheme === "light" || scheme === "dark") root.style.colorScheme = scheme;
  };
  const report = (message) => {
    if (failed) return;
    failed = true;
    post({ type: "error", message: String(message) });
  };
  addEventListener("error", (event) => {
    if (/^ResizeObserver loop/.test(event.message)) return;
    report(event.message || "Script error");
  });
  addEventListener("unhandledrejection", (event) => report(event.reason && event.reason.message || event.reason));
  const host = (data) => {
    if (!data || typeof data !== "object") return;
    if (data.type === "theme") {
      style(data.scheme, data.vars);
      dispatchEvent(new Event("visualthemechange"));
      schedule();
    }
    if (data.type !== "render" || rendered || typeof data.html !== "string") return;
    rendered = true;
    style(data.scheme, data.vars);
    const template = document.createElement("template");
    template.innerHTML = data.html;
    for (const inert of template.content.querySelectorAll("script")) {
      const live = document.createElement("script");
      for (const attribute of inert.attributes) live.setAttribute(attribute.name, attribute.value);
      live.textContent = inert.textContent;
      inert.replaceWith(live);
    }
    const body = document.body;
    new ResizeObserver(schedule).observe(body);
    new MutationObserver(schedule).observe(body, { subtree: true, childList: true, attributes: true, characterData: true });
    addEventListener("load", schedule, true);
    body.append(template.content);
    schedule();
  };
  Object.defineProperty(window, "__visualHost", { value: host });
  addEventListener("DOMContentLoaded", () => post({ type: "ready" }));
})();"#;

const BASE_STYLE: &str = r#"
*, *::before, *::after { box-sizing: border-box; }
html { background: transparent; overflow: hidden; scrollbar-width: thin; scrollbar-color: var(--color-border-strong) transparent; }
body {
  position: relative; display: flow-root; margin: 0;
  color: var(--color-text); font: var(--font-size, 14px)/1.5 var(--font-sans, system-ui, sans-serif);
  accent-color: var(--color-accent); -webkit-font-smoothing: antialiased;
}
button, input, select, textarea { font: inherit; color: inherit; }
"#;

fn frame_document() -> String {
    // An X window cannot show the transcript through the page, so there the page wears its colour.
    let style = if cfg!(target_os = "linux") { BASE_STYLE.replacen("background: transparent", "background: var(--color-bg)", 1) } else { BASE_STYLE.to_string() };
    format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><style>{style}</style><script>{BRIDGE}</script></head><body></body></html>")
}

/// The colour a visual's page is drawn on where it cannot be see-through.
#[cfg(target_os = "linux")]
fn backdrop(theme: &Theme) -> gtk::gdk::RGBA {
    let canvas = theme.tokens.canvas;
    gtk::gdk::RGBA::new(f64::from(canvas.r), f64::from(canvas.g), f64::from(canvas.b), 1.0)
}

/// The custom properties a visual is styled with, which the visualize skill documents by these names.
fn theme_message(theme: &Theme) -> serde_json::Value {
    let t = &theme.tokens;
    let p = &theme.palette;
    let rgba = |color: gpui::Rgba| format!("rgba({}, {}, {}, {})", (color.r * 255.0).round(), (color.g * 255.0).round(), (color.b * 255.0).round(), color.a);
    let hex = |value: u32| format!("#{:06x}", value & 0xFF_FFFF);
    let family = theme.typography.ui_family.to_string();
    let sans = if family.starts_with('.') || family.is_empty() { "-apple-system, BlinkMacSystemFont, system-ui, sans-serif".to_string() } else { format!("\"{family}\", -apple-system, system-ui, sans-serif") };
    let mono = format!("\"{}\", ui-monospace, monospace", theme.typography.mono_family);
    let vars = serde_json::json!({
        "--color-bg": rgba(t.canvas),
        "--color-surface": rgba(t.sunken),
        "--color-surface-raised": rgba(t.surface),
        "--color-surface-hover": rgba(t.surface_hover),
        "--color-text": rgba(t.ink),
        "--color-text-secondary": rgba(t.muted),
        "--color-text-tertiary": rgba(t.quiet),
        "--color-border": rgba(t.line),
        "--color-border-strong": rgba(t.line_strong),
        "--color-accent": rgba(t.accent),
        "--color-on-accent": rgba(t.accent_ink),
        "--color-success": rgba(t.success),
        "--color-warning": hex(p.ansi_yellow),
        "--color-danger": rgba(t.danger),
        "--color-info": rgba(t.info),
        "--series-1": hex(p.ansi_blue),
        "--series-2": hex(p.ansi_green),
        "--series-3": hex(p.ansi_yellow),
        "--series-4": hex(p.ansi_magenta),
        "--series-5": hex(p.ansi_cyan),
        "--series-6": hex(p.ansi_red),
        "--font-sans": sans,
        "--font-mono": mono,
        "--font-size": format!("{}px", theme.metrics.text_content),
        "--radius": "10px",
    });
    serde_json::json!({ "scheme": if t.variant == act_ui::ThemeVariant::Dark { "dark" } else { "light" }, "vars": vars })
}

#[derive(Deserialize)]
struct Reply {
    #[serde(rename = "type")]
    kind: String,
    height: Option<f64>,
    message: Option<String>,
}

/// One live page and where it stands.
struct Page {
    #[cfg(target_os = "macos")]
    clip: objc2::rc::Retained<super::visual_native::ClipView>,
    #[cfg(target_os = "linux")]
    clip: super::visual_x11::ClipView,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    page: wry::WebView,
    /// Takes the keyboard for a page the app paints, once it is clicked.
    #[cfg(target_os = "linux")]
    focus: gpui::FocusHandle,
    source: Rc<str>,
    full: bool,
    rendered: bool,
    height: Option<f32>,
    error: Option<String>,
    created: Instant,
    /// The draw this page was last placed in.
    seen: u64,
    /// Placed by the main transcript, whose view a draw may reuse without placing its pages again.
    in_transcript: bool,
    seen_at: Instant,
    theme: serde_json::Value,
}

impl Page {
    fn drop_native(&self) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        self.clip.detach();
    }

    fn send(&self, message: &serde_json::Value) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let _ = self.page.evaluate_script(&format!("window.__visualHost && window.__visualHost({message})"));
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = message;
    }
}

#[derive(Default)]
struct Visuals {
    pages: HashMap<u64, Page>,
    next: u64,
    /// The draw now under way, and how many times each source has been placed in it.
    generation: u64,
    placed: HashMap<(Rc<str>, bool), usize>,
    sweep_asked: bool,
    /// The main transcript's view, and the last draw that drew it rather than reusing it.
    transcript_view: Option<gpui::EntityId>,
    transcript_drawn: u64,
    /// Pages by (source, full, occurrence in draw order).
    slots: HashMap<(Rc<str>, bool, usize), u64>,
    heights: VecDeque<(Rc<str>, f32)>,
    /// Failures by source, which outlive a page dropped off screen until Try again.
    errors: HashMap<Rc<str>, String>,
    covered: bool,
    /// What lies over the transcript's foot, which pages stop short of.
    occluders: Vec<Bounds<Pixels>>,
    /// Controls floating over the transcript, which pages leave a hole for.
    holes: Vec<Bounds<Pixels>>,
    theme: Option<serde_json::Value>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    replies: Option<futures::channel::mpsc::UnboundedSender<(u64, String)>>,
}

#[derive(Clone, Default)]
struct VisualsGlobal(Rc<RefCell<Visuals>>);

impl Global for VisualsGlobal {}

fn visuals(cx: &mut App) -> Rc<RefCell<Visuals>> {
    if !cx.has_global::<VisualsGlobal>() {
        cx.set_global(VisualsGlobal::default());
    }
    cx.global::<VisualsGlobal>().0.clone()
}

impl Visuals {
    fn remembered(&self, source: &str) -> Option<f32> {
        self.heights.iter().rev().find(|(known, _)| &**known == source).map(|(_, height)| *height)
    }

    fn remember(&mut self, source: Rc<str>, height: f32) {
        self.heights.retain(|(known, _)| *known != source);
        self.heights.push_back((source, height));
        while self.heights.len() > MAX_REMEMBERED {
            self.heights.pop_front();
        }
    }

    fn page_for(&self, source: &str, full: bool) -> Option<&Page> {
        self.pages.values().filter(|page| &*page.source == source && page.full == full).max_by_key(|page| page.seen)
    }

    fn close(&mut self, id: u64) {
        if let Some(page) = self.pages.remove(&id) {
            page.drop_native();
        }
        self.slots.retain(|_, slot| *slot != id);
    }
}

/// Starts a draw: pages not placed in it are taken off the window once it is done.
pub fn begin_frame(covered: bool, occluders: Vec<Bounds<Pixels>>, holes: Vec<Bounds<Pixels>>, window: &mut Window, cx: &mut App) {
    let visuals = visuals(cx);
    let mut state = visuals.borrow_mut();
    state.generation += 1;
    state.placed.clear();
    state.covered = covered;
    state.occluders = occluders;
    state.holes = holes;
    let theme = theme_message(cx.theme());
    if state.theme.as_ref() != Some(&theme) {
        #[cfg(target_os = "linux")]
        let backdrop = backdrop(cx.theme());
        for page in state.pages.values_mut() {
            #[cfg(target_os = "linux")]
            webkit2gtk::WebViewExt::set_background_color(&wry::WebViewExtUnix::webview(&page.page), &backdrop);
            if page.rendered && page.theme != theme {
                let mut message = theme.clone();
                message["type"] = "theme".into();
                page.send(&message);
                page.theme = theme.clone();
            }
        }
        state.theme = Some(theme);
    }
    if !state.pages.is_empty() && !state.sweep_asked {
        state.sweep_asked = true;
        let visuals = visuals.clone();
        window.on_next_frame(move |_, _| sweep(&visuals));
    }
}

/// Names the view the main transcript draws in.
pub fn set_transcript_view(view: gpui::EntityId, cx: &mut App) {
    visuals(cx).borrow_mut().transcript_view = Some(view);
}

/// Called as the main transcript draws, rather than reusing its last draw.
pub fn transcript_drawn(cx: &mut App) {
    let visuals = visuals(cx);
    let mut state = visuals.borrow_mut();
    state.transcript_drawn = state.generation;
}

fn sweep(visuals: &Rc<RefCell<Visuals>>) {
    let mut state = visuals.borrow_mut();
    state.sweep_asked = false;
    let generation = state.generation;
    // A reused transcript draw keeps its pages where they were.
    let reused = state.transcript_drawn != generation;
    for page in state.pages.values_mut() {
        if page.seen != generation && page.in_transcript && reused {
            page.seen = generation;
        }
    }
    for page in state.pages.values() {
        if page.seen != generation {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            page.clip.hide();
        }
    }
    let now = Instant::now();
    let mut idle: Vec<(u64, Instant)> = state.pages.iter().filter(|(_, page)| page.seen != generation).map(|(id, page)| (*id, page.seen_at)).collect();
    idle.sort_by_key(|(_, at)| *at);
    let excess = state.pages.len().saturating_sub(MAX_PAGES);
    let stale: Vec<u64> = idle.iter().enumerate().filter(|(index, (_, at))| *index < excess || now.duration_since(*at) > IDLE_PAGE).map(|(_, (id, _))| *id).collect();
    for id in stale {
        state.close(id);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_page(state: &mut Visuals, source: Rc<str>, full: bool, window: &mut Window, cx: &mut App) -> Option<u64> {
    use std::borrow::Cow;
    #[cfg(target_os = "macos")]
    let parent = super::visual_native::content_view(window)?;
    if state.replies.is_none() {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<(u64, String)>();
        state.replies = Some(sender);
        let visuals = visuals(cx);
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some((id, message)) = receiver.next().await {
                let visuals = visuals.clone();
                let _ = cx.update(|cx| {
                    if receive(&visuals, id, &message) {
                        cx.refresh_windows();
                    }
                });
            }
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    let clip = super::visual_native::ClipView::attach(&parent);
    #[cfg(target_os = "linux")]
    let Some(clip) = super::visual_x11::ClipView::attach(window, cx) else {
        state.errors.insert(source, "Visuals need GTK, which did not start.".into());
        return None;
    };
    state.next += 1;
    let id = state.next;
    let sender = state.replies.clone()?;
    #[cfg(target_os = "macos")]
    let handle = super::visual_native::ParentHandle(std::ptr::NonNull::new(objc2::rc::Retained::as_ptr(&clip) as *mut std::ffi::c_void)?);
    let builder = wry::WebViewBuilder::new()
        .with_custom_protocol(VISUAL_SCHEME.into(), |_, request| {
            let url = request.uri().to_string();
            if url.split(['?', '#']).next() != Some(FRAME_URL) {
                return wry::http::Response::builder().status(404).body(Cow::Borrowed(&b"Not found"[..])).unwrap();
            }
            wry::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .header("Content-Security-Policy", CSP)
                .header("X-Content-Type-Options", "nosniff")
                .body(Cow::Owned(frame_document().into_bytes()))
                .unwrap()
        })
        .with_url(FRAME_URL)
        .with_navigation_handler(|url| url.starts_with(FRAME_URL))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_transparent(true)
        .with_focused(false)
        .with_incognito(true)
        .with_accept_first_mouse(true)
        .with_bounds(wry::Rect { position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(), size: wry::dpi::LogicalSize::new(1.0, 1.0).into() })
        .with_ipc_handler(move |request| {
            let _ = sender.unbounded_send((id, request.into_body()));
        });
    #[cfg(target_os = "macos")]
    let built = builder.build_as_child(&handle);
    // An X window cannot show what is under it, so the page is drawn on the transcript's own colour.
    #[cfg(target_os = "linux")]
    let builder = {
        let backdrop = backdrop(cx.theme());
        let byte = |value: f64| (value * 255.0).round() as u8;
        builder.with_transparent(false).with_background_color((byte(backdrop.red()), byte(backdrop.green()), byte(backdrop.blue()), 255))
    };
    #[cfg(target_os = "linux")]
    let built = wry::WebViewBuilderExtUnix::build_gtk(builder, clip.container());
    let page = match built {
        Ok(page) => page,
        Err(error) => {
            clip.detach();
            state.errors.insert(source, error.to_string());
            return None;
        }
    };
    #[cfg(target_os = "linux")]
    clip.connect_page(&wry::WebViewExtUnix::webview(&page));
    let _ = page.zoom(gpui::page_zoom() as f64);
    let theme = state.theme.clone().unwrap_or_else(|| theme_message(cx.theme()));
    state.pages.insert(
        id,
        Page {
            clip,
            page,
            #[cfg(target_os = "linux")]
            focus: cx.focus_handle(),
            source,
            full,
            rendered: false,
            height: None,
            error: None,
            created: Instant::now(),
            seen: 0,
            in_transcript: false,
            seen_at: Instant::now(),
            theme,
        },
    );
    // A visual that has not reported its size by the deadline has failed.
    cx.spawn(async move |cx| {
        cx.background_executor().timer(TIMEOUT + Duration::from_millis(50)).await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
    Some(id)
}

/// Takes in what a page said, which is untrusted: the visual's own script can post anything the bridge can.
fn receive(visuals: &Rc<RefCell<Visuals>>, id: u64, message: &str) -> bool {
    let Ok(reply) = serde_json::from_str::<Reply>(message) else { return false };
    let mut state = visuals.borrow_mut();
    let Some(page) = state.pages.get_mut(&id) else { return false };
    match reply.kind.as_str() {
        "ready" if !page.rendered => {
            page.rendered = true;
            let mut message = page.theme.clone();
            message["type"] = "render".into();
            message["html"] = (*page.source).into();
            page.send(&message);
            false
        }
        "size" => {
            let Some(height) = reply.height.filter(|height| height.is_finite()) else { return false };
            let height = (height.ceil() as f32).clamp(0.0, MAX_HEIGHT);
            page.height = Some(height);
            let (source, full) = (page.source.clone(), page.full);
            if !full {
                state.remember(source, height);
            }
            true
        }
        "error" => {
            let Some(text) = reply.message else { return false };
            if page.error.is_none() {
                page.error = Some(text.chars().take(500).collect());
                let source = page.source.clone();
                state.errors.entry(source).or_insert_with(|| text.chars().take(500).collect());
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Puts the page for this occurrence of `source` over `bounds`, clipped to the mask in force and
/// stopping short of what covers the transcript's foot.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn place(source: &Rc<str>, full: bool, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let visuals = visuals(cx);
    let mut state = visuals.borrow_mut();
    let in_transcript = state.transcript_view.is_some_and(|view| window.is_rendering_view(view));
    let occurrence = {
        let count = state.placed.entry((source.clone(), full)).or_insert(0);
        *count += 1;
        *count - 1
    };
    let key = (source.clone(), full, occurrence);
    let id = match state.slots.get(&key).copied().filter(|id| state.pages.get(id).is_some_and(|page| page.in_transcript == in_transcript)) {
        Some(id) => id,
        None => {
            // A page drawn for this source and left unplaced is reused before a new one is made.
            let generation = state.generation;
            let spare = state.pages.iter().filter(|(_, page)| page.source == *source && page.full == full && page.in_transcript == in_transcript && page.seen != generation).map(|(id, _)| *id).find(|id| !state.slots.iter().any(|(slot, held)| held == id && slot != &key && state.placed.get(&(slot.0.clone(), slot.1)).is_some_and(|count| slot.2 < *count)));
            let id = match spare {
                Some(id) => id,
                None => match open_page(&mut state, source.clone(), full, window, cx) {
                    Some(id) => id,
                    None => return,
                },
            };
            state.slots.insert(key, id);
            id
        }
    };
    let generation = state.generation;
    let covered = state.covered && !full;
    let mut mask = window.content_mask().bounds;
    if !full {
        // Whatever floats over the view cuts it short from the nearer end.
        for occluder in &state.occluders {
            let across = occluder.origin.x < bounds.origin.x + bounds.size.width && bounds.origin.x < occluder.origin.x + occluder.size.width;
            let (top, bottom) = (occluder.origin.y, occluder.origin.y + occluder.size.height);
            let (mask_top, mask_bottom) = (mask.origin.y, mask.origin.y + mask.size.height);
            if !across || bottom <= mask_top || top >= mask_bottom {
                continue;
            }
            if top + (bottom - top) / 2.0 > mask_top + (mask_bottom - mask_top) / 2.0 {
                mask.size.height = (top - mask_top).max(px(0.0));
            } else {
                mask.origin.y = bottom.min(mask_bottom);
                mask.size.height = (mask_bottom - mask.origin.y).max(px(0.0));
            }
        }
    }
    let hovered = bounds.contains(&window.mouse_position());
    let floating = state.holes.clone();
    let Some(page) = state.pages.get_mut(&id) else { return };
    page.seen = generation;
    page.seen_at = Instant::now();
    page.in_transcript = in_transcript;
    let visible = bounds.intersect(&mask);
    let ready = page.height.is_some() && page.error.is_none();
    if covered || page.error.is_some() || visible.size.width <= px(0.5) || visible.size.height <= px(0.5) {
        page.clip.hide();
        return;
    }
    let zoom = gpui::page_zoom() as f64;
    let points = |bounds: Bounds<Pixels>| (f64::from(bounds.origin.x) * zoom, f64::from(bounds.origin.y) * zoom, f64::from(bounds.size.width) * zoom, f64::from(bounds.size.height) * zoom);
    let page_points = points(bounds);
    let mut holes: Vec<(f64, f64, f64, f64)> = Vec::new();
    if hovered && !full {
        holes.push((page_points.2 - BUTTONS.0 as f64 * zoom, 0.0, BUTTONS.0 as f64 * zoom, BUTTONS.1 as f64 * zoom));
    }
    if !full {
        holes.extend(floating.iter().filter_map(|hole| {
            let hole = points(*hole);
            let overlaps = hole.0 < page_points.0 + page_points.2 && page_points.0 < hole.0 + hole.2 && hole.1 < page_points.1 + page_points.3 && page_points.1 < hole.1 + hole.3;
            overlaps.then(|| (hole.0 - page_points.0, hole.1 - page_points.1, hole.2, hole.3))
        }));
    }
    // A page that has not measured yet still lays out and runs its frames, but is not seen.
    page.clip.set_scrolls(page.height.is_some_and(|height| height >= MAX_HEIGHT));
    #[cfg(target_os = "macos")]
    page.clip.place(&wry::WebViewExtMacOS::webview(&page.page), points(visible), page_points, &holes, ready);
    #[cfg(target_os = "linux")]
    {
        page.clip.place(f64::from(window.scale_factor()), points(visible), page_points, &holes, ready);
        if ready {
            page.clip.paint(bounds, visible, &page.focus, window);
        }
    }
}

/// Where a visual stands, for drawing its block.
struct Status {
    height: Option<f32>,
    error: Option<String>,
    drawing: bool,
}

fn status(source: &str, full: bool, cx: &mut App) -> Status {
    let visuals = visuals(cx);
    let mut state = visuals.borrow_mut();
    let timed_out = state.page_for(source, full).filter(|page| page.height.is_none() && page.error.is_none() && page.created.elapsed() >= TIMEOUT).map(|page| page.source.clone());
    if let Some(source) = timed_out {
        state.errors.entry(source).or_insert_with(|| "The visual did not finish drawing.".into());
    }
    let page = state.page_for(source, full);
    let height = page.and_then(|page| page.height).or_else(|| if full { None } else { state.remembered(source) });
    let error = state.errors.get(source).cloned().or_else(|| page.and_then(|page| page.error.clone()));
    let drawing = page.is_none_or(|page| page.height.is_none()) && error.is_none();
    Status { height, error, drawing }
}

/// Throws away every page of `source` and what it failed with, so it draws again from nothing.
fn retry(source: &str, cx: &mut App) {
    let visuals = visuals(cx);
    let mut state = visuals.borrow_mut();
    let ids: Vec<u64> = state.pages.iter().filter(|(_, page)| &*page.source == source).map(|(id, _)| *id).collect();
    for id in ids {
        state.close(id);
    }
    state.errors.remove(source);
    state.heights.retain(|(known, _)| &**known != source);
    drop(state);
    cx.refresh_windows();
}

/// The open visual's source, held above the transcript that may drop the row it came from.
#[derive(Default)]
struct OpenVisual(Option<String>);

impl Global for OpenVisual {}

pub fn open(source: String, cx: &mut App) {
    cx.set_global(OpenVisual(Some(source)));
    cx.refresh_windows();
}

pub fn opened(cx: &App) -> Option<String> {
    cx.try_global::<OpenVisual>().and_then(|open| open.0.clone())
}

pub fn close(cx: &mut App) {
    cx.set_global(OpenVisual(None));
    cx.refresh_windows();
}

fn loading(id: ElementId, height: f32, label: bool, cx: &mut App) -> gpui::Div {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    use act_ui::style::Raised;
    let mut slot = div()
        .relative()
        .h(px(height))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .bg(t.sunken)
        .text_color(t.muted)
        .text_size(px(m.text_meta))
        .rim(t.glass_rim, act_ui::style::Corners::Radius(10.0));
    if label {
        slot = slot.child(super::sweep::sweep(id, "Drawing visual…", cx));
    }
    slot
}

/// A visual in a message. `pending` marks a fence still being written; `full` is the copy drawn in
/// the viewer, whose height is its own.
pub fn block(id: ElementId, source: SharedString, pending: bool, full: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let named = |name: &'static str| ElementId::NamedChild(Box::new(id.clone()), name.into());
    if pending {
        let height = visuals(cx).borrow().remembered(&source).unwrap_or(120.0);
        return loading(named("loading"), height, true, cx).into_any_element();
    }
    let source: Rc<str> = Rc::from(source.as_ref());
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        visuals(cx).borrow_mut().errors.entry(source.clone()).or_insert_with(|| "Visuals are not available on this platform yet.".into());
    }
    let Status { height, error, drawing } = status(&source, full, cx);
    let shown = height.unwrap_or(160.0);
    // A page that failed before it measured takes no room.
    let frame_height = if error.is_some() && height.is_none() { 0.0 } else { shown };
    let placed = source.clone();
    let mut frame = div().relative().w_full().h(px(frame_height)).child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                place(&placed, full, bounds, window, cx);
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                let _ = (bounds, window, cx, &placed, full);
            },
        )
        .absolute()
        .inset_0(),
    );
    if drawing {
        frame = frame.child(div().absolute().top_0().left_0().right_0().child(loading(named("drawing"), shown, true, cx)));
    }
    // A page the app paints hears the keys its row receives once clicked; the app keeps its shortcuts.
    #[cfg(target_os = "linux")]
    let painted_focus = visuals(cx).borrow().page_for(&source, full).filter(|page| page.clip.painted()).map(|page| page.focus.clone());
    #[cfg(target_os = "linux")]
    if let Some(focus) = painted_focus {
        let key = |down: bool| {
            let (source, focus) = (source.clone(), focus.clone());
            move |keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut App| {
                let modifiers = &keystroke.modifiers;
                if !focus.is_focused(window) || modifiers.control || modifiers.alt || modifiers.platform || keystroke.key == "escape" {
                    return false;
                }
                if let Some(page) = visuals(cx).borrow().page_for(&source, full) {
                    page.clip.key(down, keystroke);
                }
                true
            }
        };
        let (pressed, released) = (key(true), key(false));
        frame = frame
            .track_focus(&focus)
            .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                if pressed(&event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            })
            .on_key_up(move |event: &gpui::KeyUpEvent, window, cx| {
                if released(&event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            });
    }
    let mut outer = div().id(named("visual")).relative().child(frame);
    if let Some(error) = error {
        let retry_source = source.to_string();
        outer = outer.child(super::mermaid::block_error(named("error"), "The visual hit an error", &error, None, move |cx| retry(&retry_source, cx), cx));
    } else if !full {
        let group: SharedString = format!("visual-{id}").into();
        let opened = source.to_string();
        let expand_focus = window.use_keyed_state(SharedString::from(format!("visual-expand-{id}")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        // Drawn clear rather than left out, so the keyboard still reaches it.
        outer = outer.group(group.clone()).child(
            div().absolute().top(px(6.0)).right(px(38.0)).when(!expand_focus.is_focused(window), |button| button.opacity(0.0).group_hover(group, |style| style.opacity(1.0))).child(
                IconButton::new(named("expand"), IconName::Maximize2, "Open the visual at full size")
                    .track_focus(&expand_focus)
                    .variant(IconButtonVariant::Glass)
                    .icon_size(14.0)
                    .tip("Full size")
                    .on_click(move |_, _, cx| open(opened.clone(), cx)),
            ),
        );
    }
    outer.into_any_element()
}
