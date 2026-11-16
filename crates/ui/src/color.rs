//! The CSS colour functions the stylesheets derive tokens with, over gamma-encoded sRGB.

use gpui::{Hsla, Rgba};

/// `#rrggbb`.
pub fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// `rgb(r g b / a)` with 0–255 channels.
pub fn rgb_triple(channels: [u8; 3], alpha: f32) -> Rgba {
    Rgba {
        r: channels[0] as f32 / 255.0,
        g: channels[1] as f32 / 255.0,
        b: channels[2] as f32 / 255.0,
        a: alpha.clamp(0.0, 1.0),
    }
}

pub const TRANSPARENT: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

/// `rgb(from color r g b / alpha)`: the colour with its alpha replaced.
pub fn alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha.clamp(0.0, 1.0), ..color }
}

/// `color-mix(in srgb, a weight, b)`, `weight` in 0–1, with premultiplied interpolation as CSS does.
pub fn mix(a: Rgba, weight: f32, b: Rgba) -> Rgba {
    let w = weight.clamp(0.0, 1.0);
    let alpha = a.a * w + b.a * (1.0 - w);
    if alpha <= 0.0 {
        return TRANSPARENT;
    }
    let channel = |x: f32, y: f32| (x * a.a * w + y * b.a * (1.0 - w)) / alpha;
    Rgba {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: alpha,
    }
}

/// Source-over compositing: `top` painted on `bottom`.
pub fn over(top: Rgba, bottom: Rgba) -> Rgba {
    let alpha = top.a + bottom.a * (1.0 - top.a);
    if alpha <= 0.0 {
        return TRANSPARENT;
    }
    let channel = |x: f32, y: f32| (x * top.a + y * bottom.a * (1.0 - top.a)) / alpha;
    Rgba {
        r: channel(top.r, bottom.r),
        g: channel(top.g, bottom.g),
        b: channel(top.b, bottom.b),
        a: alpha,
    }
}

pub fn hsla(color: Rgba) -> Hsla {
    color.into()
}

/// WCAG relative luminance.
pub fn luminance(color: Rgba) -> f32 {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// WCAG contrast ratio between two opaque colours.
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Rgba, b: Rgba) -> bool {
        (a.r - b.r).abs() < 1e-3 && (a.g - b.g).abs() < 1e-3 && (a.b - b.b).abs() < 1e-3 && (a.a - b.a).abs() < 1e-3
    }

    #[test]
    fn mix_weights_the_first_colour() {
        let mixed = mix(hex(0xffffff), 0.25, hex(0x000000));
        assert!(close(mixed, Rgba { r: 0.25, g: 0.25, b: 0.25, a: 1.0 }));
    }

    #[test]
    fn mix_with_transparent_keeps_the_hue_and_thins_it() {
        let ring = hex(0xd97757);
        let mixed = mix(ring, 0.18, TRANSPARENT);
        assert!(close(mixed, alpha(ring, 0.18)));
    }

    #[test]
    fn over_an_opaque_ground_is_opaque() {
        let painted = over(alpha(WHITE, 0.5), hex(0x000000));
        assert!(close(painted, Rgba { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }));
    }
}
