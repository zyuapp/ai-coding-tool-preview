//! `.text-sweep`: a band of brighter ink travels across a waiting label's letters (SweepBand.tsx): a
//! copy of the label in ink, seen through a soft window that slides across it.

use std::time::Duration;

use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, relative, App, EdgeFade, ElementId, SharedString, StyleRefinement, Window};

const PERIOD: Duration = Duration::from_millis(1900);
const STEPS: u32 = 38;

/// The label in `--muted`, with the band in `--ink` passing through it. Reduced motion shows the label still.
/// A window not in use shows the label still, as it rests every indicator.
pub fn sweep(id: impl Into<ElementId>, text: impl Into<SharedString>, _cx: &App) -> impl IntoElement {
    Sweep { id: id.into(), text: text.into() }
}

#[derive(IntoElement)]
struct Sweep {
    id: ElementId,
    text: SharedString,
}

impl RenderOnce for Sweep {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, ink) = (theme.tokens.muted, theme.tokens.ink);
        let text = self.text;
        let label = div().relative().text_color(muted).child(text.clone());
        if theme.tokens.reduced_motion || !window.is_window_active() {
            return label;
        }
        let mut fill = StyleRefinement::default();
        fill.size.width = Some(relative(1.).into());
        fill.size.height = Some(relative(1.).into());
        let id = self.id.clone();
        let band = act_ui::components::live::live(self.id, fill, window, cx, move |window, cx| {
            let progress = act_ui::components::spinner::stepped_progress(id.clone(), PERIOD, STEPS, window, cx);
            // `sweep-band` moves the window from -130% to 130% of the label in 38 steps; the copy moves
            // back by as much, so it stays on the letters.
            let shift = -1.3 + 2.6 * progress;
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(shift))
                .w_full()
                .inert(true)
                .edge_fade(EdgeFade::horizontal().start(relative(0.2), relative(0.5)).end(relative(0.2), relative(0.5)))
                .child(div().absolute().top_0().bottom_0().left(relative(-shift)).w_full().overflow_hidden().text_color(ink).child(text.clone()))
                .into_any_element()
        });
        label.child(div().absolute().inset_0().inert(true).child(band))
    }
}
