//! The window's elements as an AccessKit tree, served to VoiceOver from the view.

use std::{cell::RefCell, ffi::c_void, rc::Rc};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, NodeId, TreeUpdate,
};
use cocoa::{base::id, foundation::NSPoint};

use crate::platform::access_tree::reads_only;
use crate::{AccessNode, Pixels, Point, point, px};

#[derive(Default)]
struct Snapshot {
    title: String,
    nodes: Vec<AccessNode>,
    scale: f32,
}

/// Presses the element at a point, in window points.
pub(crate) type Press = Box<dyn Fn(Point<Pixels>)>;

pub(crate) struct WindowAccessibility {
    adapter: accesskit_macos::Adapter,
    snapshot: Rc<RefCell<Snapshot>>,
    active: bool,
}

struct Activation(Rc<RefCell<Snapshot>>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(tree(&self.0.borrow()))
    }
}

struct Actions {
    snapshot: Rc<RefCell<Snapshot>>,
    press: Press,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        let snapshot = self.snapshot.borrow();
        let Some(node) = snapshot.nodes.iter().find(|node| NodeId(node.id) == request.target_node) else {
            return;
        };
        if !crate::platform::access_tree::presses(request.action, node.info.role) {
            return;
        }
        let bounds = node.bounds;
        let scale = snapshot.scale.max(1.0);
        let at = point(
            px((bounds.origin.x.0 + bounds.size.width.0 / 2.) / scale),
            px((bounds.origin.y.0 + bounds.size.height.0 / 2.) / scale),
        );
        drop(snapshot);
        (self.press)(at);
    }
}

impl WindowAccessibility {
    /// # Safety
    /// `view` must be a valid `NSView` that outlives this.
    pub(crate) unsafe fn new(view: id, press: Press) -> Self {
        let snapshot: Rc<RefCell<Snapshot>> = Rc::default();
        let actions = Actions { snapshot: snapshot.clone(), press };
        let adapter = unsafe { accesskit_macos::Adapter::new(view as *mut c_void, false, actions) };
        Self { adapter, snapshot, active: false }
    }

    /// Returns the events to raise once the accessibility state is no longer borrowed.
    pub(crate) fn update(
        &mut self,
        title: String,
        nodes: &[AccessNode],
        scale: f32,
    ) -> Option<accesskit_macos::QueuedEvents> {
        {
            let mut snapshot = self.snapshot.borrow_mut();
            snapshot.title = title;
            snapshot.nodes = nodes.to_vec();
            snapshot.scale = scale;
        }
        let snapshot = self.snapshot.clone();
        self.adapter.update_if_active(|| tree(&snapshot.borrow()))
    }

    /// Returns the events to raise once the accessibility state is no longer borrowed.
    pub(crate) fn set_focused(&mut self, focused: bool) -> Option<accesskit_macos::QueuedEvents> {
        self.adapter.update_view_focus_state(focused)
    }

    /// Whether an assistive client has read the tree.
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    pub(crate) fn children(&mut self) -> id {
        self.active = true;
        let mut activation = Activation(self.snapshot.clone());
        self.adapter.view_children(&mut activation) as id
    }

    pub(crate) fn focus(&mut self) -> id {
        self.active = true;
        let mut activation = Activation(self.snapshot.clone());
        self.adapter.focus(&mut activation) as id
    }

    /// Whether an element that acts sits under `point`, in device pixels from the view's top left.
    /// Text is left to the window, so a click on a link inside it stays a click.
    pub(crate) fn covers(&self, x: f32, y: f32) -> bool {
        self.snapshot.borrow().nodes.iter().any(|node| {
            let bounds = node.bounds;
            !reads_only(node.info.role)
                && !node.info.disabled
                && x >= bounds.origin.x.0
                && y >= bounds.origin.y.0
                && x < bounds.origin.x.0 + bounds.size.width.0
                && y < bounds.origin.y.0 + bounds.size.height.0
        })
    }

    pub(crate) fn hit_test(&mut self, point: NSPoint) -> id {
        let mut activation = Activation(self.snapshot.clone());
        // Both are a pair of f64 in the same order.
        let point = unsafe { std::mem::transmute::<NSPoint, _>(point) };
        self.adapter.hit_test(point, &mut activation) as id
    }
}

fn tree(snapshot: &Snapshot) -> TreeUpdate {
    crate::platform::access_tree::tree(&snapshot.title, &snapshot.nodes)
}
