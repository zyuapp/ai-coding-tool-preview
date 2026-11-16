//! Colours drawn in the colour space of the display a window is on, as Chromium draws them: every
//! sRGB colour is converted before it is blended, and the layer is tagged with the display's space
//! so the window server converts nothing. Blending in sRGB and converting afterwards, as an
//! untagged layer is shown, makes a translucent fill read lighter or darker than Chromium's on any
//! display whose tone curve is not sRGB's.

use foreign_types::ForeignType;
use core_graphics::{
    base::{
        kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedLast, kCGRenderingIntentDefault,
    },
    color_space::{CGColorSpace, kCGColorSpaceSRGB},
    context::CGContext,
    data_provider::CGDataProvider,
    geometry::{CGPoint, CGRect, CGSize},
    image::CGImage,
};
use std::sync::Arc;

/// Grid points per channel of the conversion table.
pub(crate) const LUT_SIZE: usize = 33;
/// The texture slot the table is bound to in every stage that converts colours.
pub(crate) const LUT_TEXTURE_INDEX: u64 = 7;

const FLOAT_COMPONENTS: u32 = 1 << 8;

pub(crate) struct DisplayColor {
    space: CGColorSpace,
    table: Vec<[f32; 4]>,
    pub(crate) texture: metal::Texture,
}

impl DisplayColor {
    pub(crate) fn srgb(device: &metal::DeviceRef) -> Self {
        let space = CGColorSpace::create_with_name(unsafe { kCGColorSpaceSRGB })
            .expect("sRGB colour space");
        Self::new(device, space)
    }

    pub(crate) fn new(device: &metal::DeviceRef, space: CGColorSpace) -> Self {
        let table = conversion_table(&space);
        let texture = upload(device, &table);
        Self { space, table, texture }
    }

    pub(crate) fn is(&self, space: &CGColorSpace) -> bool {
        self.space.as_ptr() == space.as_ptr()
            || unsafe { core_foundation::base::CFEqual(self.space.as_ptr() as _, space.as_ptr() as _) != 0 }
    }

    pub(crate) fn space(&self) -> &CGColorSpace {
        &self.space
    }

    /// One sRGB colour in the display's space, interpolated from the table as the shaders do.
    pub(crate) fn convert(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = LUT_SIZE - 1;
        let mut base = [0usize; 3];
        let mut frac = [0f32; 3];
        for channel in 0..3 {
            let at = rgb[channel].clamp(0.0, 1.0) * n as f32;
            base[channel] = (at.floor() as usize).min(n - 1);
            frac[channel] = at - base[channel] as f32;
        }
        let mut out = [0f32; 3];
        for corner in 0..8 {
            let step = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let mut weight = 1.0;
            for channel in 0..3 {
                weight *= if step[channel] == 1 { frac[channel] } else { 1.0 - frac[channel] };
            }
            if weight == 0.0 {
                continue;
            }
            let value = self.table[index(base[0] + step[0], base[1] + step[1], base[2] + step[2])];
            for channel in 0..3 {
                out[channel] += value[channel] * weight;
            }
        }
        out
    }
}

fn index(r: usize, g: usize, b: usize) -> usize {
    (b * LUT_SIZE + g) * LUT_SIZE + r
}

/// The grid of sRGB colours, converted to `space` by Core Graphics.
fn conversion_table(space: &CGColorSpace) -> Vec<[f32; 4]> {
    let n = LUT_SIZE;
    let (width, height) = (n * n, n);
    let mut source = Vec::with_capacity(width * height * 16);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                for value in [r, g, b] {
                    source.extend_from_slice(&(value as f32 / (n - 1) as f32).to_le_bytes());
                }
                source.extend_from_slice(&1f32.to_le_bytes());
            }
        }
    }
    let info = kCGBitmapByteOrder32Little | FLOAT_COMPONENTS | kCGImageAlphaPremultipliedLast;
    let srgb = CGColorSpace::create_with_name(unsafe { kCGColorSpaceSRGB }).expect("sRGB colour space");
    let provider = CGDataProvider::from_buffer(Arc::new(source));
    let image = CGImage::new(width, height, 32, 128, width * 16, &srgb, info, &provider, false, kCGRenderingIntentDefault);
    let mut context = CGContext::create_bitmap_context(None, width, height, 32, width * 16, space, info);
    context.set_interpolation_quality(core_graphics::context::CGInterpolationQuality::CGInterpolationQualityNone);
    context.draw_image(CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(width as f64, height as f64)), &image);
    let data = context.data();
    // A bitmap context's rows run from the top; the image's first row is drawn there too.
    data.chunks_exact(16)
        .map(|pixel| {
            let channel = |at: usize| f32::from_le_bytes(pixel[at..at + 4].try_into().unwrap_or([0; 4]));
            [channel(0), channel(4), channel(8), 1.0]
        })
        .collect()
}

fn upload(device: &metal::DeviceRef, table: &[[f32; 4]]) -> metal::Texture {
    let descriptor = metal::TextureDescriptor::new();
    descriptor.set_texture_type(metal::MTLTextureType::D3);
    descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
    descriptor.set_width(LUT_SIZE as u64);
    descriptor.set_height(LUT_SIZE as u64);
    descriptor.set_depth(LUT_SIZE as u64);
    descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);
    let texture = device.new_texture(&descriptor);
    let halves: Vec<u16> = table.iter().flat_map(|pixel| pixel.map(half)).collect();
    texture.replace_region_in_slice(
        metal::MTLRegion {
            origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
            size: metal::MTLSize { width: LUT_SIZE as u64, height: LUT_SIZE as u64, depth: LUT_SIZE as u64 },
        },
        0,
        0,
        halves.as_ptr() as *const _,
        (LUT_SIZE * 8) as u64,
        (LUT_SIZE * LUT_SIZE * 8) as u64,
    );
    texture
}

/// An f32 in [0, 1] (or a little past) as an IEEE half.
fn half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let mantissa = (mantissa | 0x80_0000) >> (1 - exponent);
        return sign | ((mantissa + 0x1000) >> 13) as u16;
    }
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    let rounded = (((exponent as u32) << 10) | (mantissa >> 13)) + ((mantissa >> 12) & 1);
    sign | rounded as u16
}
