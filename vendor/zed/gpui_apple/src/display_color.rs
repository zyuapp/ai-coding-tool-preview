//! Colours drawn in the colour space of the display a window is on, as Chromium draws them: every
//! sRGB colour is converted before it is blended, and the layer is tagged with the display's space
//! so the window server converts nothing. Blending in sRGB and converting afterwards, as an
//! untagged layer is shown, makes a translucent fill read lighter or darker than Chromium's on any
//! display whose tone curve is not sRGB's.

use anyhow::{Context as _, Result};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLTexture, MTLTextureDescriptor,
    MTLTextureType, MTLTextureUsage,
};
use std::{ffi::c_void, ptr::NonNull};

/// Grid points per channel of the conversion table.
pub(crate) const LUT_SIZE: usize = 33;
/// The texture slot the table is bound to in every stage that converts colours.
pub(crate) const LUT_TEXTURE_INDEX: usize = 7;

/// 32-bit little-endian float components with premultiplied alpha last.
const FLOAT_RGBA_INFO: u32 = (2 << 12) | (1 << 8) | 1;

pub(crate) struct DisplayColor {
    space: CFRetained<CGColorSpace>,
    table: Vec<[f32; 4]>,
    pub(crate) texture: Retained<ProtocolObject<dyn MTLTexture>>,
}

impl DisplayColor {
    pub(crate) fn srgb(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self> {
        Self::new(device, srgb_space()?)
    }

    pub(crate) fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        space: CFRetained<CGColorSpace>,
    ) -> Result<Self> {
        let table = conversion_table(&space).unwrap_or_else(|| {
            log::error!("failed to build the display colour table; drawing unconverted");
            grid()
        });
        let texture = upload(device, &table)?;
        Ok(Self {
            space,
            table,
            texture,
        })
    }

    /// Colours drawn unconverted and tagged as `space`.
    pub(crate) fn unconverted(
        device: &ProtocolObject<dyn MTLDevice>,
        space: CFRetained<CGColorSpace>,
    ) -> Result<Self> {
        let table = grid();
        let texture = upload(device, &table)?;
        Ok(Self {
            space,
            table,
            texture,
        })
    }

    pub(crate) fn is(&self, space: &CGColorSpace) -> bool {
        *self.space == *space
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
                weight *= if step[channel] == 1 {
                    frac[channel]
                } else {
                    1.0 - frac[channel]
                };
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

fn srgb_space() -> Result<CFRetained<CGColorSpace>> {
    // SAFETY: The constant is extern, from Core Graphics.
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .context("creating sRGB colour space")
}

fn index(r: usize, g: usize, b: usize) -> usize {
    (b * LUT_SIZE + g) * LUT_SIZE + r
}

/// The grid of sRGB colours, one per pixel of a row-major image `LUT_SIZE²` wide and `LUT_SIZE`
/// high.
fn grid() -> Vec<[f32; 4]> {
    let n = LUT_SIZE;
    let mut grid = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let at = |value: usize| value as f32 / (n - 1) as f32;
                grid.push([at(r), at(g), at(b), 1.0]);
            }
        }
    }
    grid
}

/// The grid of sRGB colours, converted to `space` by Core Graphics.
fn conversion_table(space: &CGColorSpace) -> Option<Vec<[f32; 4]>> {
    let n = LUT_SIZE;
    let (width, height) = (n * n, n);
    let bytes_per_row = width * 16;
    let srgb = srgb_space().ok()?;
    let mut source = grid();
    let mut converted = vec![[0f32; 4]; width * height];
    // SAFETY: Both buffers hold `height` rows of `bytes_per_row` bytes and outlive the contexts,
    // which are dropped before this returns.
    unsafe {
        let source_context = CGBitmapContextCreate(
            source.as_mut_ptr() as *mut c_void,
            width,
            height,
            32,
            bytes_per_row,
            Some(&srgb),
            FLOAT_RGBA_INFO,
        )?;
        let image = CGBitmapContextCreateImage(Some(&source_context))?;
        let context = CGBitmapContextCreate(
            converted.as_mut_ptr() as *mut c_void,
            width,
            height,
            32,
            bytes_per_row,
            Some(space),
            FLOAT_RGBA_INFO,
        )?;
        CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::None);
        CGContext::draw_image(
            Some(&context),
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(width as f64, height as f64),
            ),
            Some(&image),
        );
    }
    // A bitmap context's rows run from the top; the image's first row is drawn there too.
    Some(
        converted
            .into_iter()
            .map(|[r, g, b, _]| [r, g, b, 1.0])
            .collect(),
    )
}

fn upload(
    device: &ProtocolObject<dyn MTLDevice>,
    table: &[[f32; 4]],
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    let descriptor = MTLTextureDescriptor::new();
    descriptor.setTextureType(MTLTextureType::Type3D);
    descriptor.setPixelFormat(MTLPixelFormat::RGBA16Float);
    // SAFETY: The dimensions are positive and this descriptor creates a 3D texture.
    unsafe {
        descriptor.setWidth(LUT_SIZE);
        descriptor.setHeight(LUT_SIZE);
        descriptor.setDepth(LUT_SIZE);
    }
    descriptor.setUsage(MTLTextureUsage::ShaderRead);
    let texture = device
        .newTextureWithDescriptor(&descriptor)
        .context("allocating display colour table")?;
    let halves: Vec<u16> = table.iter().flat_map(|pixel| pixel.map(half)).collect();
    // SAFETY: `halves` holds LUT_SIZE³ RGBA16Float texels laid out with these strides.
    unsafe {
        texture.replaceRegion_mipmapLevel_slice_withBytes_bytesPerRow_bytesPerImage(
            MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: LUT_SIZE,
                    height: LUT_SIZE,
                    depth: LUT_SIZE,
                },
            },
            0,
            0,
            NonNull::from(halves.as_slice()).cast(),
            LUT_SIZE * 8,
            LUT_SIZE * LUT_SIZE * 8,
        );
    }
    Ok(texture)
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

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_graphics::kCGColorSpaceDisplayP3;

    #[test]
    fn converts_srgb_into_the_display_space() -> Result<()> {
        let device = objc2_metal::MTLCreateSystemDefaultDevice().context("no Metal device")?;
        let srgb = DisplayColor::srgb(&device)?;
        for rgb in [[0.2, 0.5, 0.9], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]] {
            let converted = srgb.convert(rgb);
            for channel in 0..3 {
                assert!(
                    (converted[channel] - rgb[channel]).abs() < 0.002,
                    "{rgb:?} -> {converted:?}"
                );
            }
        }

        let p3 = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceDisplayP3 }))
            .context("no Display P3")?;
        let p3 = DisplayColor::new(&device, p3)?;
        let red = p3.convert([1.0, 0.0, 0.0]);
        let expected = [0.9175, 0.2003, 0.1386];
        for channel in 0..3 {
            assert!((red[channel] - expected[channel]).abs() < 0.01, "{red:?}");
        }
        Ok(())
    }
}
