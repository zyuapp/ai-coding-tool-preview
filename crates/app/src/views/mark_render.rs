//! Draws a screenshot's marks into its pixels, as the canvas in annotate/marks.ts does, in a hidden
//! WebKit page so the badges' numbers are set in the same face and weight.
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use act_core::domain::conversation::ImageAnnotation;
use futures::channel::oneshot;
use gpui::{App, Global, Window};
use serde::Deserialize;

type Reply = Result<String, String>;

#[derive(Default)]
struct Page {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    view: Option<act_browser::hidden::HiddenPage>,
    ready: bool,
    broken: Option<String>,
    next: u64,
    waiting: Vec<String>,
    asked: HashMap<u64, oneshot::Sender<Reply>>,
}

#[derive(Clone, Default)]
struct Painter(Rc<RefCell<Page>>);

impl Global for Painter {}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    id: u64,
    png: Option<String>,
    error: Option<String>,
}

const SCRIPT: &str = r##"
const MARK_COLOR = "#ff453a";
const BADGE_INK = "#ffffff";
function badgeRadius(width, height) { return Math.max(11, Math.round(Math.max(width, height) * 0.013)); }
function spotsAround(box, radius) {
  const { x, y, width, height } = box;
  return [
    { x: x - radius, y: y - radius }, { x: x + width + radius, y: y - radius },
    { x: x - radius, y: y + height + radius }, { x: x + width + radius, y: y + height + radius },
    { x: x + radius, y: y + radius }, { x: x + width - radius, y: y + radius },
    { x: x + radius, y: y + height - radius }, { x: x + width - radius, y: y + height - radius },
  ];
}
function placeBadges(boxes, width, height) {
  const radius = badgeRadius(width, height);
  const placed = [];
  boxes.forEach((box) => {
    const spots = spotsAround(box, radius).map((spot) => ({
      x: Math.min(Math.max(spot.x, radius), Math.max(width - radius, radius)),
      y: Math.min(Math.max(spot.y, radius), Math.max(height - radius, radius)),
    }));
    const free = spots.find((spot) => placed.every((taken) => Math.hypot(spot.x - taken.x, spot.y - taken.y) >= radius * 2));
    placed.push(free ?? spots[0]);
  });
  return placed;
}
function drawArrow(context, annotation, width, height, stroke) {
  const tailX = annotation.x * width, tailY = annotation.y * height;
  const tipX = (annotation.x + annotation.width) * width, tipY = (annotation.y + annotation.height) * height;
  const angle = Math.atan2(tipY - tailY, tipX - tailX);
  const length = Math.hypot(tipX - tailX, tipY - tailY);
  const head = Math.max(Math.min(length * 0.3, stroke * 9), stroke * 4);
  const baseX = tipX - Math.cos(angle) * head, baseY = tipY - Math.sin(angle) * head;
  const spread = head * 0.5;
  context.lineWidth = stroke;
  context.lineCap = "round";
  context.strokeStyle = MARK_COLOR;
  context.fillStyle = MARK_COLOR;
  context.beginPath();
  context.moveTo(tailX, tailY);
  context.lineTo(baseX, baseY);
  context.stroke();
  context.beginPath();
  context.moveTo(tipX, tipY);
  context.lineTo(baseX - Math.sin(angle) * spread, baseY + Math.cos(angle) * spread);
  context.lineTo(baseX + Math.sin(angle) * spread, baseY - Math.cos(angle) * spread);
  context.closePath();
  context.fill();
}
function drawBadge(context, center, radius, mark) {
  context.beginPath();
  context.arc(center.x, center.y, radius, 0, Math.PI * 2);
  context.fillStyle = MARK_COLOR;
  context.fill();
  context.lineWidth = Math.max(1, radius * 0.14);
  context.strokeStyle = BADGE_INK;
  context.stroke();
  context.fillStyle = BADGE_INK;
  context.fillText(mark, center.x, center.y);
}
function drawAnnotations(context, annotations, width, height, prefix) {
  const stroke = Math.max(2, Math.round(Math.max(width, height) * 0.003));
  const radius = badgeRadius(width, height);
  const boxes = annotations.filter((a) => a.kind === "box").map((a) => ({
    x: a.x * width, y: a.y * height, width: Math.max(a.width * width, stroke), height: Math.max(a.height * height, stroke),
  }));
  const centers = placeBadges(boxes, width, height);
  context.lineJoin = "round";
  context.textBaseline = "middle";
  context.textAlign = "center";
  context.font = `700 ${Math.round(radius * 1.1)}px -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif`;
  let mark = 0;
  annotations.forEach((annotation) => {
    if (annotation.kind === "arrow") { drawArrow(context, annotation, width, height, stroke); return; }
    const box = boxes[mark], center = centers[mark];
    mark += 1;
    context.lineWidth = stroke;
    context.strokeStyle = MARK_COLOR;
    context.strokeRect(box.x + stroke / 2, box.y + stroke / 2, Math.max(box.width - stroke, 1), Math.max(box.height - stroke, 1));
    drawBadge(context, center, radius, `${prefix}${mark}`);
  });
}
window.__mark = (job) => {
  const image = new Image();
  image.onload = () => {
    try {
      const canvas = document.createElement("canvas");
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const context = canvas.getContext("2d");
      context.drawImage(image, 0, 0, canvas.width, canvas.height);
      drawAnnotations(context, job.annotations, canvas.width, canvas.height, job.prefix);
      window.ipc.postMessage(JSON.stringify({ id: job.id, png: canvas.toDataURL("image/png").split(",")[1] }));
    } catch (error) {
      window.ipc.postMessage(JSON.stringify({ id: job.id, error: String(error && error.message || error) }));
    }
  };
  image.onerror = () => window.ipc.postMessage(JSON.stringify({ id: job.id, error: "Could not read the image to annotate." }));
  image.src = job.source;
};
window.ipc.postMessage(JSON.stringify({ ready: true }));
"##;

fn painter(cx: &mut App) -> Rc<RefCell<Page>> {
    if !cx.has_global::<Painter>() {
        cx.set_global(Painter::default());
    }
    cx.global::<Painter>().0.clone()
}

/// Opens the page ahead of the first drawing; it needs a window to live in.
pub fn prepare(window: &mut Window, cx: &mut App) {
    let page = painter(cx);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if page.borrow().view.is_some() || page.borrow().broken.is_some() {
            return;
        }
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<String>();
        let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body><script>{SCRIPT}</script></body></html>");
        let builder = wry::WebViewBuilder::new().with_html(html).with_transparent(true).with_ipc_handler(move |request| {
            let _ = sender.unbounded_send(request.into_body());
        });
        let built = act_browser::hidden::open(builder, 16.0, 16.0, window, cx);
        match built {
            Ok(view) => page.borrow_mut().view = Some(view),
            Err(error) => {
                page.borrow_mut().broken = Some(error.to_string());
                return;
            }
        }
        let page = page.clone();
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(message) = receiver.next().await {
                let Ok(answer) = serde_json::from_str::<Answer>(&message) else { continue };
                let page = page.clone();
                let _ = cx.update(move |_| {
                    let mut page = page.borrow_mut();
                    if answer.ready {
                        page.ready = true;
                        for script in std::mem::take(&mut page.waiting) {
                            if let Some(view) = &page.view {
                                let _ = view.view.evaluate_script(&script);
                            }
                        }
                        return;
                    }
                    if let Some(reply) = page.asked.remove(&answer.id) {
                        let _ = reply.send(match (answer.png, answer.error) {
                            (Some(png), _) => Ok(png),
                            (None, error) => Err(error.unwrap_or_else(|| "The marks could not be drawn.".into())),
                        });
                    }
                });
            }
        })
        .detach();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (window, page);
    }
}

/// The image at `path`, one of the app's own attachments, with `annotations` drawn in, as base64 PNG.
pub fn render(path: &str, annotations: &[ImageAnnotation], prefix: &str, cx: &mut App) -> oneshot::Receiver<Reply> {
    let (reply, answer) = oneshot::channel();
    let page = painter(cx);
    if let Some(broken) = page.borrow().broken.clone() {
        let _ = reply.send(Err(broken));
        return answer;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, annotations, prefix);
        let _ = reply.send(Err("Marks cannot be drawn on this platform yet.".into()));
        answer
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let read = crate::effects::window_desktop::read_attachment(path.to_string(), cx);
        let (annotations, prefix) = (annotations.to_vec(), prefix.to_string());
        cx.spawn(async move |_| {
            let data = match read.await {
                Ok(data) => data,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let mut page = page.borrow_mut();
            page.next += 1;
            let id = page.next;
            page.asked.insert(id, reply);
            let job = serde_json::json!({ "id": id, "source": format!("data:image/png;base64,{data}"), "annotations": annotations, "prefix": prefix });
            let script = format!("window.__mark({job})");
            if !page.ready {
                page.waiting.push(script);
                return;
            }
            let failed = page.view.as_ref().and_then(|view| view.view.evaluate_script(&script).err());
            if let Some(error) = failed {
                if let Some(reply) = page.asked.remove(&id) {
                    let _ = reply.send(Err(error.to_string()));
                }
            }
        })
        .detach();
        answer
    }
}
