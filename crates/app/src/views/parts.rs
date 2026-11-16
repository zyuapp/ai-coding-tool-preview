//! Parts of the window drawn as views of their own. Each is cached, so a frame that only changes
//! one, such as a scroll of the transcript or the sidebar or a caret blink in the composer, reuses
//! the others' last layout and paint.

use gpui::{div, prelude::*, px, AnyView, Context, Entity, StyleRefinement, Subscription, WeakEntity, Window};

use super::root::Root;
use super::{composer, conversation, find_bar, sidebar, topbar};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sidebar,
    Topbar,
    Composer,
    Transcript,
    /// The find bar over the page, and over the dock.
    Find,
    DockFind,
}

pub struct Part {
    root: WeakEntity<Root>,
    kind: Kind,
    _root_changed: Subscription,
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let element = self.root.upgrade().map(|root| {
            root.update(cx, |root, cx| {
                // The sidebar lists every computer's threads; the rest draw the one on screen.
                let state = if kind == Kind::Sidebar { root.state(cx) } else { root.shown(cx) };
                match kind {
                    Kind::Sidebar => sidebar::render(root, &state, window, cx).into_any_element(),
                    Kind::Topbar => topbar::render(root, &state, window, cx),
                    Kind::Composer => composer::render(root, &state, window, cx).into_any_element(),
                    Kind::Transcript => {
                        super::visual::transcript_drawn(cx);
                        conversation::render(root, &state, window, cx).into_any_element()
                    }
                    Kind::Find | Kind::DockFind => find_bar::render(root, &state, kind == Kind::Find, window, cx).unwrap_or_else(|| div().into_any_element()),
                }
            })
        });
        div().relative().size_full().children(element)
    }
}

pub struct Parts {
    images: Entity<super::image_memory::ImageMemory>,
    sidebar: Entity<Part>,
    topbar: Entity<Part>,
    composer: Entity<Part>,
    transcript: Entity<Part>,
    find: Entity<Part>,
    dock_find: Entity<Part>,
}

impl Parts {
    pub fn new(cx: &mut Context<Root>) -> Parts {
        let images = super::image_memory::ImageMemory::new(cx);
        let root = cx.entity();
        let mut part = |kind: Kind| {
            let root = root.clone();
            cx.new(|cx| Part {
                root: root.downgrade(),
                kind,
                // Whatever changes the root draws every part again.
                _root_changed: cx.observe(&root, |_, _, cx| cx.notify()),
            })
        };
        let parts = Parts {
            images,
            sidebar: part(Kind::Sidebar),
            topbar: part(Kind::Topbar),
            composer: part(Kind::Composer),
            transcript: part(Kind::Transcript),
            find: part(Kind::Find),
            dock_find: part(Kind::DockFind),
        };
        super::visual::set_transcript_view(parts.transcript.entity_id(), cx);
        parts
    }

    fn cached(part: &Entity<Part>) -> AnyView {
        let mut style = StyleRefinement::default();
        style.size.width = Some(gpui::relative(1.).into());
        style.size.height = Some(gpui::relative(1.).into());
        AnyView::from(part.clone()).cached(style)
    }

    /// The cache every image in the window is loaded through.
    pub fn images(&self) -> Entity<super::image_memory::ImageMemory> {
        self.images.clone()
    }

    /// The sidebar, filling the box `sidebar::slot` gives it.
    pub fn sidebar(&self) -> AnyView {
        Self::cached(&self.sidebar)
    }

    /// The topbar, filling a box as tall as the chrome's head.
    pub fn topbar(&self) -> AnyView {
        Self::cached(&self.topbar)
    }

    /// The composer, over the whole page and resting at its foot.
    pub fn composer(&self) -> AnyView {
        Self::cached(&self.composer)
    }

    /// Draws the transcript again without drawing the cached parts again.
    pub fn redraw_transcript(&self, cx: &mut gpui::App) {
        self.transcript.update(cx, |_, cx| cx.notify());
    }

    /// Draws again what shows streamed text: the transcript and the find bars counting its matches.
    pub fn redraw_streamed(&self, cx: &mut gpui::App) {
        for part in [&self.transcript, &self.find, &self.dock_find] {
            part.update(cx, |_, cx| cx.notify());
        }
    }

    pub fn transcript(&self) -> AnyView {
        Self::cached(&self.transcript)
    }

    /// The find bar, hanging from the top of a box as wide as what it searches; empty while find
    /// is closed or open elsewhere. `dock` picks the dock's bar over the page's.
    pub fn find(&self, dock: bool) -> AnyView {
        let mut slot = div().absolute().top_0().left_0().right_0().h(px(0.0));
        let style = slot.style().clone();
        AnyView::from(if dock { self.dock_find.clone() } else { self.find.clone() }).cached(style)
    }
}
