//! CSS `letter-spacing` and `font-variant-numeric`, which GPUI carries as font features.

use gpui::Styled;

pub trait LetterSpacing: Styled + Sized {
    /// Spaces every letter by `em` of the font size.
    fn letter_spacing(self, em: f32) -> Self {
        act_ui::style::letter_spacing(self, em)
    }

    /// Every digit as wide as the others (`tabular-nums`).
    fn tabular_nums(self) -> Self {
        act_ui::style::tabular_nums(self)
    }
}

impl<E: Styled> LetterSpacing for E {}
