//! Moving parts drawn as live views of their own (`AnyView::live`): a step of their motion draws
//! only them, while the view around them replays its last frame.

use std::rc::Rc;

use gpui::{div, prelude::*, AnyElement, AnyView, App, Context, ElementId, Entity, StyleRefinement, Window};

type Build = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

struct Live {
    build: Option<Build>,
}

impl Render for Live {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let build = self.build.clone();
        div().size_full().children(build.map(|build| build(window, cx)))
    }
}

/// `build`'s element in a live view sized by `style`. A motion inside that notifies the view drawing
/// it, as `stepped_progress` does, draws only this view again. `build` must not change size or take
/// input: the view around it is not laid out again.
pub fn live(id: impl Into<ElementId>, style: StyleRefinement, window: &mut Window, cx: &mut App, build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> AnyView {
    let slot = window.with_global_id(id.into(), |global_id, window| {
        window.with_element_state(global_id, |slot: Option<Entity<Live>>, _| {
            let slot = slot.unwrap_or_else(|| cx.new(|_| Live { build: None }));
            (slot.clone(), slot)
        })
    });
    slot.update(cx, |slot, _| slot.build = Some(Rc::new(build)));
    AnyView::from(slot).cached(style).live()
}
