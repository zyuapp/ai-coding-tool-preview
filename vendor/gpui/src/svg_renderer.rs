use crate::{AssetSource, DevicePixels, IsZero, Result, SharedString, Size};
use resvg::tiny_skia::Pixmap;
use std::{
    hash::Hash,
    sync::{Arc, LazyLock},
};

/// When rendering SVGs, we render them at twice the size to get a higher-quality result.
pub const SMOOTH_SVG_SCALE_FACTOR: f32 = 2.;

#[derive(Clone, PartialEq, Hash, Eq)]
pub(crate) struct RenderSvgParams {
    pub(crate) path: SharedString,
    pub(crate) size: Size<DevicePixels>,
}

#[derive(Clone)]
pub struct SvgRenderer {
    asset_source: Arc<dyn AssetSource>,
    usvg_options: Arc<usvg::Options<'static>>,
    hairline_options: Arc<usvg::Options<'static>>,
}

pub enum SvgSize {
    Size(Size<DevicePixels>),
    ScaleFactor(f32),
}

impl SvgRenderer {
    pub fn new(asset_source: Arc<dyn AssetSource>) -> Self {
        Self {
            asset_source,
            usvg_options: Arc::new(usvg_options(None)),
            // Skia draws hairlines without caps.
            hairline_options: Arc::new(usvg_options(Some("* { stroke-linecap: butt }".into()))),
        }
    }

    pub(crate) fn render(
        &self,
        params: &RenderSvgParams,
    ) -> Result<Option<(Size<DevicePixels>, Vec<u8>)>> {
        anyhow::ensure!(!params.size.is_zero(), "can't render at a zero size");

        // Load the tree.
        let Some(bytes) = self.asset_source.load(&params.path)? else {
            return Ok(None);
        };

        let tree = usvg::Tree::from_data(&bytes, &self.usvg_options)?;
        let smooth_scale = params.size.width.0 as f32 / tree.size().width();
        // Strokes at most a device pixel wide are drawn as modulated hairlines, as Skia does, which
        // only happens at the device scale; the result is upsampled to the smooth size.
        let device_scale = smooth_scale / SMOOTH_SVG_SCALE_FACTOR;
        let hairline = max_stroke_width(tree.root()).is_some_and(|width| width * device_scale <= 1.0);
        let pixmap = if hairline {
            Self::render_tree(&usvg::Tree::from_data(&bytes, &self.hairline_options)?, device_scale)?
        } else {
            Self::render_tree(&tree, smooth_scale)?
        };

        // Convert the pixmap's pixels into an alpha mask.
        let alpha = pixmap.pixels().iter().map(|p| p.alpha()).collect::<Vec<_>>();
        if !hairline {
            let size = Size::new(DevicePixels(pixmap.width() as i32), DevicePixels(pixmap.height() as i32));
            return Ok(Some((size, alpha)));
        }
        let factor = SMOOTH_SVG_SCALE_FACTOR as usize;
        let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
        let mut alpha_mask = Vec::with_capacity(width * height * factor * factor);
        for row in alpha.chunks_exact(width) {
            let wide = row.iter().flat_map(|&a| std::iter::repeat_n(a, factor)).collect::<Vec<_>>();
            for _ in 0..factor {
                alpha_mask.extend_from_slice(&wide);
            }
        }
        let size = Size::new(DevicePixels((width * factor) as i32), DevicePixels((height * factor) as i32));
        Ok(Some((size, alpha_mask)))
    }

    fn render_tree(tree: &usvg::Tree, scale: f32) -> Result<Pixmap, usvg::Error> {
        let svg_size = tree.size();
        let mut pixmap = resvg::tiny_skia::Pixmap::new((svg_size.width() * scale) as u32, (svg_size.height() * scale) as u32)
            .ok_or(usvg::Error::InvalidSize)?;
        resvg::render(tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        Ok(pixmap)
    }

    pub fn render_pixmap(&self, bytes: &[u8], size: SvgSize) -> Result<Pixmap, usvg::Error> {
        let tree = usvg::Tree::from_data(bytes, &self.usvg_options)?;
        let scale = match size {
            SvgSize::Size(size) => size.width.0 as f32 / tree.size().width(),
            SvgSize::ScaleFactor(scale) => scale,
        };
        Self::render_tree(&tree, scale)
    }
}

fn usvg_options(style_sheet: Option<String>) -> usvg::Options<'static> {
    static FONT_DB: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    let default_font_resolver = usvg::FontResolver::default_font_selector();
    let font_resolver = Box::new(move |font: &usvg::Font, db: &mut Arc<usvg::fontdb::Database>| {
        if db.is_empty() {
            *db = FONT_DB.clone();
        }
        default_font_resolver(font, db)
    });
    usvg::Options {
        font_resolver: usvg::FontResolver {
            select_font: font_resolver,
            select_fallback: usvg::FontResolver::default_fallback_selector(),
        },
        style_sheet,
        ..Default::default()
    }
}

/// The widest stroke in the tree, in the root's units, or `None` when nothing is stroked.
fn max_stroke_width(group: &usvg::Group) -> Option<f32> {
    group.children().iter().fold(None, |widest: Option<f32>, node| {
        let width = match node {
            usvg::Node::Group(group) => max_stroke_width(group),
            usvg::Node::Path(path) => path.stroke().map(|stroke| {
                let ts = path.abs_transform();
                stroke.width().get() * (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt()
            }),
            usvg::Node::Text(text) => max_stroke_width(text.flattened()),
            usvg::Node::Image(_) => None,
        };
        match (widest, width) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    struct Chevron;

    impl AssetSource for Chevron {
        fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
            Ok(Some(Cow::Borrowed(br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round"><path d="m9 18 6-6-6-6"/></svg>"#)))
        }

        fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
            Ok(Vec::new())
        }
    }

    fn peak(device_size: i32) -> u8 {
        let size = (device_size as f32 * SMOOTH_SVG_SCALE_FACTOR) as i32;
        let params = RenderSvgParams { path: "chevron.svg".into(), size: Size::new(DevicePixels(size), DevicePixels(size)) };
        let (_, mask) = SvgRenderer::new(Arc::new(Chevron)).render(&params).unwrap().unwrap();
        mask.into_iter().max().unwrap()
    }

    #[test]
    fn device_pixel_strokes_are_hairlines() {
        // A 45° hairline splits each step's coverage between two pixels, with no bright corner.
        assert!(peak(12) <= 140, "{}", peak(12));
        assert!(peak(24) > 200, "{}", peak(24));
    }
}
