//! The type scale and chrome metrics of tokens.css. Only the reading and terminal sizes follow the
//! user; every chrome size stays where it was designed, so a size choice never moves a row.

use crate::typography::Typography;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub text_meta: f32,
    pub text_menu: f32,
    pub text_control: f32,
    /// The conversation's reading size, which the user sets.
    pub text_content: f32,
    pub text_heading: f32,
    pub text_display: f32,
    pub text_hero: f32,
    pub text_badge: f32,
    pub text_tiny: f32,
    /// The terminal's size, which the user sets.
    pub terminal_text: f32,
    pub row_slot: f32,
    pub hit_target: f32,
    pub row_inset: f32,
    /// The height every pane's top strip takes, so their lower edges line up across the window.
    pub chrome_head: f32,
    /// The frame showing around the page.
    pub frame_gap: f32,
    pub page_radius: f32,
    /// The narrowest the conversation goes with the right panel beside it.
    pub page_min: f32,
    pub window_strip: f32,
    /// Room left for the traffic lights where the platform insets them over the content.
    pub window_controls_inset: f32,
    pub settings_titlebar_inset: f32,
}

impl Metrics {
    pub fn new(typography: &Typography, inset_window_controls: bool) -> Self {
        Self {
            text_meta: 12.0,
            text_menu: 13.0,
            text_control: 14.0,
            text_content: typography.reading_size as f32,
            text_heading: 17.0,
            text_display: 21.0,
            text_hero: 25.0,
            text_badge: 12.0,
            text_tiny: 12.0,
            terminal_text: typography.terminal_size as f32,
            row_slot: 22.0,
            hit_target: 24.0,
            row_inset: 8.0,
            chrome_head: 72.0,
            frame_gap: 8.0,
            page_radius: 11.0,
            page_min: 480.0,
            window_strip: 40.0,
            window_controls_inset: if inset_window_controls { 84.0 } else { 0.0 },
            settings_titlebar_inset: if inset_window_controls { 48.0 } else { 0.0 },
        }
    }

    /// `--page-inset`: the room either side of the conversation for a window this wide.
    pub fn page_inset(&self, viewport_width: f32) -> f32 {
        (0.04 * viewport_width - self.frame_gap).clamp(20.0, 64.0)
    }
}
