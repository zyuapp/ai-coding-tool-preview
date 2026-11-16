//! The window's elements as an AccessKit tree, served to AT-SPI (Orca and other assistive clients).

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, NodeId, Rect, TreeUpdate,
};

use crate::platform::access_tree::{reads_only, tree};
use crate::{
    AccessNode, Bounds, ForegroundExecutor, Modifiers, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, point, px,
};

#[derive(Default)]
struct Snapshot {
    title: String,
    nodes: Vec<AccessNode>,
    scale: f32,
}

/// Delivers input to the window, on the main thread. False once the window is gone.
pub(crate) type Deliver = Box<dyn Fn(PlatformInput) -> bool>;

/// Where the real pointer is over the window, in window pixels; None when it is outside.
pub(crate) type Pointer = Box<dyn Fn() -> Option<Point<Pixels>>>;

/// A window's accessibility: its title, and once AT-SPI is asked for, the adapter serving it.
#[derive(Default)]
pub(crate) struct WindowAccessibility {
    title: String,
    adapter: Option<(accesskit_unix::Adapter, Arc<Mutex<Snapshot>>)>,
    /// Whether an assistive client is reading the tree.
    active: Arc<AtomicBool>,
}

struct Activation(Arc<Mutex<Snapshot>>, Arc<AtomicBool>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.1.store(true, Ordering::Relaxed);
        let snapshot = self.0.lock().ok()?;
        Some(tree(&snapshot.title, &snapshot.nodes))
    }
}

struct Deactivation(Arc<AtomicBool>);

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

struct Actions {
    snapshot: Arc<Mutex<Snapshot>>,
    presses: flume::Sender<Point<Pixels>>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        let Ok(snapshot) = self.snapshot.lock() else {
            return;
        };
        let Some(node) = snapshot
            .nodes
            .iter()
            .find(|node| NodeId(node.id) == request.target_node)
        else {
            return;
        };
        if !crate::platform::access_tree::presses(request.action, node.info.role) {
            return;
        }
        if node.info.disabled || reads_only(node.info.role) {
            return;
        }
        let bounds = node.bounds;
        let scale = snapshot.scale.max(1.0);
        let at = point(
            px((bounds.origin.x.0 + bounds.size.width.0 / 2.) / scale),
            px((bounds.origin.y.0 + bounds.size.height.0 / 2.) / scale),
        );
        let _ = self.presses.send(at);
    }
}

impl WindowAccessibility {
    /// Whether an assistive client has read the tree and still is.
    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    /// Reports this frame's elements. The adapter starts on the first report; an assistive client
    /// then reads the tree from it, and a press it asks for is a click at the element's centre.
    pub(crate) fn update(
        &mut self,
        nodes: &[AccessNode],
        scale: f32,
        window: Bounds<Pixels>,
        focused: bool,
        executor: &ForegroundExecutor,
        deliver: impl FnOnce() -> (Deliver, Pointer),
    ) {
        let active = self.active.clone();
        let (adapter, snapshot) = self.adapter.get_or_insert_with(|| {
            let snapshot: Arc<Mutex<Snapshot>> = Arc::default();
            let (presses, pressed) = flume::unbounded::<Point<Pixels>>();
            let (deliver, pointer) = deliver();
            executor
                .spawn(async move {
                    while let Ok(position) = pressed.recv_async().await {
                        if !press(&deliver, &pointer, position) {
                            break;
                        }
                    }
                })
                .detach();
            let adapter = accesskit_unix::Adapter::new(
                Activation(snapshot.clone(), active.clone()),
                Actions {
                    snapshot: snapshot.clone(),
                    presses,
                },
                Deactivation(active),
            );
            (adapter, snapshot)
        });
        if let Ok(mut current) = snapshot.lock() {
            current.title = self.title.clone();
            current.nodes = nodes.to_vec();
            current.scale = scale;
        }
        let scale = f64::from(scale);
        let frame = Rect {
            x0: f64::from(window.origin.x) * scale,
            y0: f64::from(window.origin.y) * scale,
            x1: f64::from(window.origin.x + window.size.width) * scale,
            y1: f64::from(window.origin.y + window.size.height) * scale,
        };
        adapter.set_root_window_bounds(frame, frame);
        adapter.update_window_focus_state(focused);
        let title = self.title.clone();
        adapter.update_if_active(|| tree(&title, nodes));
    }
}

/// Clicks at `position`, then hands hover back to where the real pointer is.
fn press(deliver: &Deliver, pointer: &Pointer, position: Point<Pixels>) -> bool {
    let modifiers = Modifiers::default();
    let restore = match pointer() {
        Some(position) => PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }),
        None => PlatformInput::MouseExited(MouseExitEvent { position: point(px(-1.), px(-1.)), pressed_button: None, modifiers }),
    };
    deliver(PlatformInput::MouseMove(MouseMoveEvent {
        position,
        pressed_button: None,
        modifiers,
    })) && deliver(PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count: 1,
        first_mouse: false,
    })) && deliver(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count: 1,
    })) && deliver(restore)
}
