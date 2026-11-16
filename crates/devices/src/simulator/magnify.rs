//! A trackpad pinch over the screen. GPUI has no pinch event, so AppKit's magnify events are watched
//! for the panel's window, where a browser would have sent a wheel with Control held.

use std::ptr::NonNull;

use block2::RcBlock;
use futures::StreamExt;
use gpui::{point, px, Context, Subscription, Window};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventPhase};

use super::panel::SimulatorPanel;

struct Magnified {
    x: f64,
    y: f64,
    magnification: f64,
    ended: bool,
}

pub(crate) fn watch(window: &mut Window, cx: &mut Context<SimulatorPanel>) -> Option<Subscription> {
    let mtm = MainThreadMarker::new()?;
    let view = crate::appkit::content_view(window)?;
    let number = view.window()?.windowNumber();
    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<Magnified>();
    let watched = view.clone();
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event for the duration of the call.
        let event_ref = unsafe { event.as_ref() };
        if event_ref.windowNumber() == number {
            let location = event_ref.locationInWindow();
            let height = watched.frame().size.height;
            let phase = event_ref.phase();
            let ended = phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled);
            let _ = sender.unbounded_send(Magnified { x: location.x, y: height - location.y, magnification: event_ref.magnification(), ended });
        }
        let _ = mtm;
        event.as_ptr()
    });
    // SAFETY: the block returns the event it was given, which is a valid pointer.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &block) }?;
    let task = cx.spawn_in(window, async move |this, cx| {
        while let Some(event) = receiver.next().await {
            let position = point(px(event.x as f32), px(event.y as f32));
            if this.update_in(cx, |this, window, cx| this.magnify(position, event.magnification, event.ended, window, cx)).is_err() {
                break;
            }
        }
    });
    Some(Subscription::new(move || {
        // SAFETY: the monitor came from `addLocalMonitorForEventsMatchingMask` and is removed once.
        unsafe { NSEvent::removeMonitor(&monitor) };
        drop(block);
        drop(task);
    }))
}
