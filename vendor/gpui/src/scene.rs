// todo("windows"): remove
#![cfg_attr(windows, allow(dead_code))]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AtlasTextureId, AtlasTile, Background, Bounds, ContentMask, Corners, Edges, Hsla, Pixels,
    Point, Radians, ScaledPixels, Size, bounds_tree::BoundsTree, point,
};
use std::{
    fmt::Debug,
    iter::Peekable,
    ops::{Add, Range, Sub},
    slice,
};

#[allow(non_camel_case_types, unused)]
pub(crate) type PathVertex_ScaledPixels = PathVertex<ScaledPixels>;

pub(crate) type DrawOrder = u32;

#[derive(Default)]
pub(crate) struct Scene {
    pub(crate) paint_operations: Vec<PaintOperation>,
    primitive_bounds: BoundsTree<ScaledPixels>,
    layer_stack: Vec<DrawOrder>,
    pub(crate) shadows: Vec<Shadow>,
    pub(crate) quads: Vec<Quad>,
    pub(crate) paths: Vec<Path<ScaledPixels>>,
    pub(crate) underlines: Vec<Underline>,
    pub(crate) monochrome_sprites: Vec<MonochromeSprite>,
    pub(crate) polychrome_sprites: Vec<PolychromeSprite>,
    pub(crate) surfaces: Vec<PaintSurface>,
    pub(crate) backdrop_blurs: Vec<BackdropBlur>,
    /// A region backdrop filters read as a flat color instead of what was drawn there.
    pub(crate) backdrop_hidden: Option<(Bounds<ScaledPixels>, crate::Rgba)>,
    /// Parts of the hidden region drawn by something else, which backdrop filters still see.
    pub(crate) backdrop_holes: Vec<Bounds<ScaledPixels>>,
    /// A region whose content a backdrop filter replaces with its filtered copy rather than
    /// drawing that copy over it.
    pub(crate) backdrop_replaced: Option<Bounds<ScaledPixels>>,
    /// Regions inside the replaced one whose backdrop filters draw over what is there after all.
    pub(crate) backdrop_drawn_over: Vec<Bounds<ScaledPixels>>,
    /// A translucent fill laid over the hidden region, which backdrop filters drawn after it and
    /// over it read the region through.
    pub(crate) backdrop_tint: Option<(DrawOrder, Bounds<ScaledPixels>, crate::Rgba)>,
}

impl Scene {
    pub fn clear(&mut self) {
        self.paint_operations.clear();
        self.primitive_bounds.clear();
        self.layer_stack.clear();
        self.paths.clear();
        self.shadows.clear();
        self.quads.clear();
        self.underlines.clear();
        self.monochrome_sprites.clear();
        self.polychrome_sprites.clear();
        self.surfaces.clear();
        self.backdrop_blurs.clear();
        self.backdrop_hidden = None;
        self.backdrop_holes.clear();
        self.backdrop_replaced = None;
        self.backdrop_drawn_over.clear();
        self.backdrop_tint = None;
    }

    pub fn len(&self) -> usize {
        self.paint_operations.len()
    }

    pub fn push_layer(&mut self, bounds: Bounds<ScaledPixels>) {
        let order = self.primitive_bounds.insert(bounds);
        self.layer_stack.push(order);
        self.paint_operations
            .push(PaintOperation::StartLayer(bounds));
    }

    pub fn pop_layer(&mut self) {
        self.layer_stack.pop();
        self.paint_operations.push(PaintOperation::EndLayer);
    }

    pub fn insert_primitive(&mut self, primitive: impl Into<Primitive>) {
        let mut primitive = primitive.into();
        // A shadow paints 3 sigma of blur and a pixel of antialiasing beyond its bounds
        // (shaders.metal), so whatever is drawn later over that must order above it.
        let painted_bounds = match &primitive {
            Primitive::Shadow(shadow) if shadow.inset != 0 => shadow.element_bounds,
            Primitive::Shadow(shadow) => shadow.bounds.dilate(shadow.blur_radius * 3.0 + ScaledPixels(1.0)),
            _ => *primitive.bounds(),
        };
        let clipped_bounds = painted_bounds.intersect(&primitive.content_mask().bounds);

        if clipped_bounds.is_empty() {
            return;
        }

        // A blur samples what was drawn before it, so it always takes an order of its own.
        let order = match &primitive {
            Primitive::BackdropBlur(_) => self.primitive_bounds.insert(clipped_bounds),
            _ => self
                .layer_stack
                .last()
                .copied()
                .unwrap_or_else(|| self.primitive_bounds.insert(clipped_bounds)),
        };
        match &mut primitive {
            Primitive::Shadow(shadow) => {
                shadow.order = order;
                self.shadows.push(shadow.clone());
            }
            Primitive::Quad(quad) => {
                quad.order = order;
                self.quads.push(quad.clone());
            }
            Primitive::Path(path) => {
                path.order = order;
                path.id = PathId(self.paths.len());
                self.paths.push(path.clone());
            }
            Primitive::Underline(underline) => {
                underline.order = order;
                self.underlines.push(underline.clone());
            }
            Primitive::MonochromeSprite(sprite) => {
                sprite.order = order;
                self.monochrome_sprites.push(sprite.clone());
            }
            Primitive::PolychromeSprite(sprite) => {
                sprite.order = order;
                self.polychrome_sprites.push(sprite.clone());
            }
            Primitive::Surface(surface) => {
                surface.order = order;
                self.surfaces.push(surface.clone());
            }
            Primitive::BackdropBlur(blur) => {
                blur.order = order;
                self.backdrop_blurs.push(blur.clone());
            }
        }
        self.paint_operations
            .push(PaintOperation::Primitive(primitive));
    }

    pub(crate) fn hide_from_backdrops(&mut self, bounds: Bounds<ScaledPixels>, color: crate::Rgba) {
        // A second region in a frame widens the one region to cover both.
        let bounds = match self.backdrop_hidden {
            Some((hidden, _)) => hidden.union(&bounds),
            None => bounds,
        };
        self.backdrop_hidden = Some((bounds, color));
        self.paint_operations.push(PaintOperation::HideFromBackdrops(bounds, color));
    }

    pub(crate) fn replace_under_backdrops(&mut self, bounds: Bounds<ScaledPixels>) {
        let bounds = match self.backdrop_replaced {
            Some(replaced) => replaced.union(&bounds),
            None => bounds,
        };
        self.backdrop_replaced = Some(bounds);
        self.paint_operations.push(PaintOperation::ReplaceUnderBackdrops(bounds));
    }

    pub(crate) fn draw_over_backdrops(&mut self, bounds: Bounds<ScaledPixels>) {
        self.backdrop_drawn_over.push(bounds);
        self.paint_operations.push(PaintOperation::DrawOverBackdrops(bounds));
    }

    /// The replaced region a backdrop filter over `bounds` erases before drawing its copy, if any.
    pub(crate) fn replaced_under(&self, bounds: &Bounds<ScaledPixels>) -> Option<Bounds<ScaledPixels>> {
        let inside = |outer: &Bounds<ScaledPixels>| {
            outer.origin.x <= bounds.origin.x
                && outer.origin.y <= bounds.origin.y
                && outer.bottom_right().x >= bounds.bottom_right().x
                && outer.bottom_right().y >= bounds.bottom_right().y
        };
        if self.backdrop_drawn_over.iter().any(inside) {
            return None;
        }
        self.backdrop_replaced.filter(|replaced| replaced.intersects(bounds))
    }

    pub(crate) fn tint_hidden_backdrops(&mut self, bounds: Bounds<ScaledPixels>, color: crate::Rgba) {
        // Blurs inserted later over these bounds take a higher order than this one.
        let order = self.primitive_bounds.insert(bounds);
        self.backdrop_tint = Some((order, bounds, color));
        self.paint_operations.push(PaintOperation::TintHiddenBackdrops(bounds, color));
    }

    /// The color a backdrop filter reads over the hidden region through, if it was drawn over the tint.
    pub(crate) fn hidden_tint_for(&self, order: DrawOrder, bounds: &Bounds<ScaledPixels>) -> Option<crate::Rgba> {
        self.backdrop_tint.filter(|(tint, tinted, _)| order > *tint && tinted.intersects(bounds)).map(|(_, _, color)| color)
    }

    pub(crate) fn show_to_backdrops(&mut self, bounds: Bounds<ScaledPixels>) {
        self.backdrop_holes.push(bounds);
        self.paint_operations.push(PaintOperation::ShowToBackdrops(bounds));
    }

    pub fn replay(&mut self, range: Range<usize>, prev_scene: &Scene) {
        for operation in &prev_scene.paint_operations[range] {
            match operation {
                PaintOperation::Primitive(primitive) => self.insert_primitive(primitive.clone()),
                PaintOperation::StartLayer(bounds) => self.push_layer(*bounds),
                PaintOperation::EndLayer => self.pop_layer(),
                PaintOperation::HideFromBackdrops(bounds, color) => self.hide_from_backdrops(*bounds, *color),
                PaintOperation::ShowToBackdrops(bounds) => self.show_to_backdrops(*bounds),
                PaintOperation::ReplaceUnderBackdrops(bounds) => self.replace_under_backdrops(*bounds),
                PaintOperation::DrawOverBackdrops(bounds) => self.draw_over_backdrops(*bounds),
                PaintOperation::TintHiddenBackdrops(bounds, color) => self.tint_hidden_backdrops(*bounds, *color),
            }
        }
    }

    pub fn finish(&mut self) {
        self.shadows.sort_by_key(|shadow| shadow.order);
        self.quads.sort_by_key(|quad| quad.order);
        self.paths.sort_by_key(|path| path.order);
        self.underlines.sort_by_key(|underline| underline.order);
        self.monochrome_sprites
            .sort_by_key(|sprite| (sprite.order, sprite.tile.tile_id));
        self.polychrome_sprites
            .sort_by_key(|sprite| (sprite.order, sprite.tile.tile_id));
        self.surfaces.sort_by_key(|surface| surface.order);
        self.backdrop_blurs.sort_by_key(|blur| blur.order);
    }

    #[cfg_attr(
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(any(feature = "x11", feature = "wayland"))
        ),
        allow(dead_code)
    )]
    pub(crate) fn batches(&self) -> impl Iterator<Item = PrimitiveBatch<'_>> {
        BatchIterator {
            shadows: &self.shadows,
            shadows_start: 0,
            shadows_iter: self.shadows.iter().peekable(),
            quads: &self.quads,
            quads_start: 0,
            quads_iter: self.quads.iter().peekable(),
            paths: &self.paths,
            paths_start: 0,
            paths_iter: self.paths.iter().peekable(),
            underlines: &self.underlines,
            underlines_start: 0,
            underlines_iter: self.underlines.iter().peekable(),
            monochrome_sprites: &self.monochrome_sprites,
            monochrome_sprites_start: 0,
            monochrome_sprites_iter: self.monochrome_sprites.iter().peekable(),
            polychrome_sprites: &self.polychrome_sprites,
            polychrome_sprites_start: 0,
            polychrome_sprites_iter: self.polychrome_sprites.iter().peekable(),
            surfaces: &self.surfaces,
            surfaces_start: 0,
            surfaces_iter: self.surfaces.iter().peekable(),
            backdrop_blurs: &self.backdrop_blurs,
            backdrop_blurs_start: 0,
            backdrop_blurs_iter: self.backdrop_blurs.iter().peekable(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Default)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
pub(crate) enum PrimitiveKind {
    BackdropBlur,
    Shadow,
    #[default]
    Quad,
    Path,
    Underline,
    MonochromeSprite,
    PolychromeSprite,
    Surface,
}

pub(crate) enum PaintOperation {
    Primitive(Primitive),
    StartLayer(Bounds<ScaledPixels>),
    EndLayer,
    HideFromBackdrops(Bounds<ScaledPixels>, crate::Rgba),
    ShowToBackdrops(Bounds<ScaledPixels>),
    ReplaceUnderBackdrops(Bounds<ScaledPixels>),
    DrawOverBackdrops(Bounds<ScaledPixels>),
    TintHiddenBackdrops(Bounds<ScaledPixels>, crate::Rgba),
}

#[derive(Clone)]
pub(crate) enum Primitive {
    Shadow(Shadow),
    Quad(Quad),
    Path(Path<ScaledPixels>),
    Underline(Underline),
    MonochromeSprite(MonochromeSprite),
    PolychromeSprite(PolychromeSprite),
    Surface(PaintSurface),
    BackdropBlur(BackdropBlur),
}

impl Primitive {
    pub fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.bounds,
            Primitive::Quad(quad) => &quad.bounds,
            Primitive::Path(path) => &path.bounds,
            Primitive::Underline(underline) => &underline.bounds,
            Primitive::MonochromeSprite(sprite) => &sprite.bounds,
            Primitive::PolychromeSprite(sprite) => &sprite.bounds,
            Primitive::Surface(surface) => &surface.bounds,
            Primitive::BackdropBlur(blur) => &blur.bounds,
        }
    }

    pub fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.content_mask,
            Primitive::Quad(quad) => &quad.content_mask,
            Primitive::Path(path) => &path.content_mask,
            Primitive::Underline(underline) => &underline.content_mask,
            Primitive::MonochromeSprite(sprite) => &sprite.content_mask,
            Primitive::PolychromeSprite(sprite) => &sprite.content_mask,
            Primitive::Surface(surface) => &surface.content_mask,
            Primitive::BackdropBlur(blur) => &blur.content_mask,
        }
    }
}

#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
struct BatchIterator<'a> {
    shadows: &'a [Shadow],
    shadows_start: usize,
    shadows_iter: Peekable<slice::Iter<'a, Shadow>>,
    quads: &'a [Quad],
    quads_start: usize,
    quads_iter: Peekable<slice::Iter<'a, Quad>>,
    paths: &'a [Path<ScaledPixels>],
    paths_start: usize,
    paths_iter: Peekable<slice::Iter<'a, Path<ScaledPixels>>>,
    underlines: &'a [Underline],
    underlines_start: usize,
    underlines_iter: Peekable<slice::Iter<'a, Underline>>,
    monochrome_sprites: &'a [MonochromeSprite],
    monochrome_sprites_start: usize,
    monochrome_sprites_iter: Peekable<slice::Iter<'a, MonochromeSprite>>,
    polychrome_sprites: &'a [PolychromeSprite],
    polychrome_sprites_start: usize,
    polychrome_sprites_iter: Peekable<slice::Iter<'a, PolychromeSprite>>,
    surfaces: &'a [PaintSurface],
    surfaces_start: usize,
    surfaces_iter: Peekable<slice::Iter<'a, PaintSurface>>,
    backdrop_blurs: &'a [BackdropBlur],
    backdrop_blurs_start: usize,
    backdrop_blurs_iter: Peekable<slice::Iter<'a, BackdropBlur>>,
}

impl<'a> Iterator for BatchIterator<'a> {
    type Item = PrimitiveBatch<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut orders_and_kinds = [
            (
                self.shadows_iter.peek().map(|s| s.order),
                PrimitiveKind::Shadow,
            ),
            (self.quads_iter.peek().map(|q| q.order), PrimitiveKind::Quad),
            (self.paths_iter.peek().map(|q| q.order), PrimitiveKind::Path),
            (
                self.underlines_iter.peek().map(|u| u.order),
                PrimitiveKind::Underline,
            ),
            (
                self.monochrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::MonochromeSprite,
            ),
            (
                self.polychrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::PolychromeSprite,
            ),
            (
                self.surfaces_iter.peek().map(|s| s.order),
                PrimitiveKind::Surface,
            ),
            (
                self.backdrop_blurs_iter.peek().map(|b| b.order),
                PrimitiveKind::BackdropBlur,
            ),
        ];
        orders_and_kinds.sort_by_key(|(order, kind)| (order.unwrap_or(u32::MAX), *kind));

        let first = orders_and_kinds[0];
        let second = orders_and_kinds[1];
        let (batch_kind, max_order_and_kind) = if first.0.is_some() {
            (first.1, (second.0.unwrap_or(u32::MAX), second.1))
        } else {
            return None;
        };

        match batch_kind {
            PrimitiveKind::Shadow => {
                let shadows_start = self.shadows_start;
                let mut shadows_end = shadows_start + 1;
                self.shadows_iter.next();
                while self
                    .shadows_iter
                    .next_if(|shadow| (shadow.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    shadows_end += 1;
                }
                self.shadows_start = shadows_end;
                Some(PrimitiveBatch::Shadows(
                    &self.shadows[shadows_start..shadows_end],
                ))
            }
            PrimitiveKind::Quad => {
                let quads_start = self.quads_start;
                let mut quads_end = quads_start + 1;
                self.quads_iter.next();
                while self
                    .quads_iter
                    .next_if(|quad| (quad.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    quads_end += 1;
                }
                self.quads_start = quads_end;
                Some(PrimitiveBatch::Quads(&self.quads[quads_start..quads_end]))
            }
            PrimitiveKind::Path => {
                let paths_start = self.paths_start;
                let mut paths_end = paths_start + 1;
                self.paths_iter.next();
                while self
                    .paths_iter
                    .next_if(|path| (path.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    paths_end += 1;
                }
                self.paths_start = paths_end;
                Some(PrimitiveBatch::Paths(&self.paths[paths_start..paths_end]))
            }
            PrimitiveKind::Underline => {
                let underlines_start = self.underlines_start;
                let mut underlines_end = underlines_start + 1;
                self.underlines_iter.next();
                while self
                    .underlines_iter
                    .next_if(|underline| (underline.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    underlines_end += 1;
                }
                self.underlines_start = underlines_end;
                Some(PrimitiveBatch::Underlines(
                    &self.underlines[underlines_start..underlines_end],
                ))
            }
            PrimitiveKind::MonochromeSprite => {
                let texture_id = self.monochrome_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.monochrome_sprites_start;
                let mut sprites_end = sprites_start + 1;
                self.monochrome_sprites_iter.next();
                while self
                    .monochrome_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.monochrome_sprites_start = sprites_end;
                Some(PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    sprites: &self.monochrome_sprites[sprites_start..sprites_end],
                })
            }
            PrimitiveKind::PolychromeSprite => {
                let texture_id = self.polychrome_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.polychrome_sprites_start;
                let mut sprites_end = self.polychrome_sprites_start + 1;
                self.polychrome_sprites_iter.next();
                while self
                    .polychrome_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.polychrome_sprites_start = sprites_end;
                Some(PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    sprites: &self.polychrome_sprites[sprites_start..sprites_end],
                })
            }
            PrimitiveKind::Surface => {
                let surfaces_start = self.surfaces_start;
                let mut surfaces_end = surfaces_start + 1;
                self.surfaces_iter.next();
                while self
                    .surfaces_iter
                    .next_if(|surface| (surface.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    surfaces_end += 1;
                }
                self.surfaces_start = surfaces_end;
                Some(PrimitiveBatch::Surfaces(
                    &self.surfaces[surfaces_start..surfaces_end],
                ))
            }
            PrimitiveKind::BackdropBlur => {
                let blurs_start = self.backdrop_blurs_start;
                let mut blurs_end = blurs_start + 1;
                self.backdrop_blurs_iter.next();
                while self
                    .backdrop_blurs_iter
                    .next_if(|blur| (blur.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    blurs_end += 1;
                }
                self.backdrop_blurs_start = blurs_end;
                Some(PrimitiveBatch::BackdropBlurs(
                    &self.backdrop_blurs[blurs_start..blurs_end],
                ))
            }
        }
    }
}

#[derive(Debug)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
pub(crate) enum PrimitiveBatch<'a> {
    Shadows(&'a [Shadow]),
    Quads(&'a [Quad]),
    Paths(&'a [Path<ScaledPixels>]),
    Underlines(&'a [Underline]),
    MonochromeSprites {
        texture_id: AtlasTextureId,
        sprites: &'a [MonochromeSprite],
    },
    PolychromeSprites {
        texture_id: AtlasTextureId,
        sprites: &'a [PolychromeSprite],
    },
    Surfaces(&'a [PaintSurface]),
    BackdropBlurs(&'a [BackdropBlur]),
}

/// Alpha ramps along each axis, as CSS `mask-image: linear-gradient()`: along y, alpha rises from 0 at
/// `y_clear` to 1 at `y_solid`, and falls from 1 at `y_end_solid` to 0 at `y_end_clear`; along x
/// likewise. Positions are window coordinates; the default fades nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct FadeMask {
    /// Where alpha is 0 before the top ramp.
    pub y_clear: f32,
    /// Where the top ramp reaches 1.
    pub y_solid: f32,
    /// Where the bottom ramp leaves 1.
    pub y_end_solid: f32,
    /// Where alpha is 0 after the bottom ramp.
    pub y_end_clear: f32,
    /// Where alpha is 0 before the left ramp.
    pub x_clear: f32,
    /// Where the left ramp reaches 1.
    pub x_solid: f32,
    /// Where the right ramp leaves 1.
    pub x_end_solid: f32,
    /// Where alpha is 0 after the right ramp.
    pub x_end_clear: f32,
}

const FADE_FAR: f32 = 1.0e9;

impl Default for FadeMask {
    fn default() -> Self {
        Self {
            y_clear: -FADE_FAR,
            y_solid: -FADE_FAR,
            y_end_solid: FADE_FAR,
            y_end_clear: FADE_FAR,
            x_clear: -FADE_FAR,
            x_solid: -FADE_FAR,
            x_end_solid: FADE_FAR,
            x_end_clear: FADE_FAR,
        }
    }
}

impl FadeMask {
    /// Whether this fades anything.
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Both masks at once. Ramps on the same edge keep the one that reaches further in.
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            y_clear: self.y_clear.max(other.y_clear),
            y_solid: self.y_solid.max(other.y_solid),
            y_end_solid: self.y_end_solid.min(other.y_end_solid),
            y_end_clear: self.y_end_clear.min(other.y_end_clear),
            x_clear: self.x_clear.max(other.x_clear),
            x_solid: self.x_solid.max(other.x_solid),
            x_end_solid: self.x_end_solid.min(other.x_end_solid),
            x_end_clear: self.x_end_clear.min(other.x_end_clear),
        }
    }

    /// The mask in device pixels.
    pub fn scale(&self, factor: f32) -> Self {
        if self.is_none() {
            return *self;
        }
        let scale = |value: f32| if value.abs() >= FADE_FAR { value } else { value * factor };
        Self {
            y_clear: scale(self.y_clear),
            y_solid: scale(self.y_solid),
            y_end_solid: scale(self.y_end_solid),
            y_end_clear: scale(self.y_end_clear),
            x_clear: scale(self.x_clear),
            x_solid: scale(self.x_solid),
            x_end_solid: scale(self.x_end_solid),
            x_end_clear: scale(self.x_end_clear),
        }
    }

    /// The alpha the mask leaves at `position`.
    pub fn alpha_at(&self, x: f32, y: f32) -> f32 {
        let rise = |t: f32, clear: f32, solid: f32| ((t - clear) / (solid - clear).max(1.0e-4)).clamp(0.0, 1.0);
        let fall = |t: f32, solid: f32, clear: f32| ((clear - t) / (clear - solid).max(1.0e-4)).clamp(0.0, 1.0);
        rise(y, self.y_clear, self.y_solid)
            * fall(y, self.y_end_solid, self.y_end_clear)
            * rise(x, self.x_clear, self.x_solid)
            * fall(x, self.x_end_solid, self.x_end_clear)
    }
}

/// Blurs what is already drawn under `bounds`, as CSS `backdrop-filter: blur() saturate()
/// brightness()` does, clipped to the rounded bounds and the content mask.
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub(crate) struct BackdropBlur {
    pub order: DrawOrder,
    pub pad: u32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub blur_radius: ScaledPixels,
    pub saturation: f32,
    pub brightness: f32,
    pub opacity: f32,
    pub fade: FadeMask,
}

impl From<BackdropBlur> for Primitive {
    fn from(blur: BackdropBlur) -> Self {
        Primitive::BackdropBlur(blur)
    }
}

#[derive(Default, Debug, Clone)]
#[repr(C)]
pub(crate) struct Quad {
    pub order: DrawOrder,
    pub border_style: BorderStyle,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub background: Background,
    pub border_color: Hsla,
    pub corner_radii: Corners<ScaledPixels>,
    pub border_widths: Edges<ScaledPixels>,
    pub fade: FadeMask,
}

impl From<Quad> for Primitive {
    fn from(quad: Quad) -> Self {
        Primitive::Quad(quad)
    }
}

#[derive(Debug, Clone)]
#[repr(C)]
pub(crate) struct Underline {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub thickness: ScaledPixels,
    pub wavy: u32,
    pub fade: FadeMask,
}

impl From<Underline> for Primitive {
    fn from(underline: Underline) -> Self {
        Primitive::Underline(underline)
    }
}

#[derive(Debug, Clone)]
#[repr(C)]
pub(crate) struct Shadow {
    pub order: DrawOrder,
    pub blur_radius: ScaledPixels,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    /// The box casting the shadow. As in CSS, an outer shadow is not drawn under it.
    pub element_bounds: Bounds<ScaledPixels>,
    pub element_corner_radii: Corners<ScaledPixels>,
    pub fade: FadeMask,
    /// 1 for an inset shadow: drawn only inside the box, where `bounds` does not cover it.
    pub inset: u32,
    pub pad: u32,
}

impl From<Shadow> for Primitive {
    fn from(shadow: Shadow) -> Self {
        Primitive::Shadow(shadow)
    }
}

/// The style of a border.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum BorderStyle {
    /// A solid border.
    #[default]
    Solid = 0,
    /// A dashed border.
    Dashed = 1,
}

/// A data type representing a 2 dimensional transformation that can be applied to an element.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct TransformationMatrix {
    /// 2x2 matrix containing rotation and scale,
    /// stored row-major
    pub rotation_scale: [[f32; 2]; 2],
    /// translation vector
    pub translation: [f32; 2],
}

impl Eq for TransformationMatrix {}

impl TransformationMatrix {
    /// The unit matrix, has no effect.
    pub fn unit() -> Self {
        Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [0.0, 0.0],
        }
    }

    /// Move the origin by a given point
    pub fn translate(mut self, point: Point<ScaledPixels>) -> Self {
        self.compose(Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [point.x.0, point.y.0],
        })
    }

    /// Clockwise rotation in radians around the origin
    pub fn rotate(self, angle: Radians) -> Self {
        self.compose(Self {
            rotation_scale: [
                [angle.0.cos(), -angle.0.sin()],
                [angle.0.sin(), angle.0.cos()],
            ],
            translation: [0.0, 0.0],
        })
    }

    /// Scale around the origin
    pub fn scale(self, size: Size<f32>) -> Self {
        self.compose(Self {
            rotation_scale: [[size.width, 0.0], [0.0, size.height]],
            translation: [0.0, 0.0],
        })
    }

    /// Perform matrix multiplication with another transformation
    /// to produce a new transformation that is the result of
    /// applying both transformations: first, `other`, then `self`.
    #[inline]
    pub fn compose(self, other: TransformationMatrix) -> TransformationMatrix {
        if other == Self::unit() {
            return self;
        }
        // Perform matrix multiplication
        TransformationMatrix {
            rotation_scale: [
                [
                    self.rotation_scale[0][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][0],
                    self.rotation_scale[0][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][1],
                ],
                [
                    self.rotation_scale[1][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][0],
                    self.rotation_scale[1][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][1],
                ],
            ],
            translation: [
                self.translation[0]
                    + self.rotation_scale[0][0] * other.translation[0]
                    + self.rotation_scale[0][1] * other.translation[1],
                self.translation[1]
                    + self.rotation_scale[1][0] * other.translation[0]
                    + self.rotation_scale[1][1] * other.translation[1],
            ],
        }
    }

    /// Apply transformation to a point, mainly useful for debugging
    pub fn apply(&self, point: Point<Pixels>) -> Point<Pixels> {
        let input = [point.x.0, point.y.0];
        let mut output = self.translation;
        for (i, output_cell) in output.iter_mut().enumerate() {
            for (k, input_cell) in input.iter().enumerate() {
                *output_cell += self.rotation_scale[i][k] * *input_cell;
            }
        }
        Point::new(output[0].into(), output[1].into())
    }
}

impl Default for TransformationMatrix {
    fn default() -> Self {
        Self::unit()
    }
}

#[derive(Clone, Debug)]
#[repr(C)]
pub(crate) struct MonochromeSprite {
    pub order: DrawOrder,
    /// 1 for a glyph, 0 for an icon; also aligns to 8 bytes.
    pub pad: u32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub tile: AtlasTile,
    pub transformation: TransformationMatrix,
    pub fade: FadeMask,
}

impl From<MonochromeSprite> for Primitive {
    fn from(sprite: MonochromeSprite) -> Self {
        Primitive::MonochromeSprite(sprite)
    }
}

#[derive(Clone, Debug)]
#[repr(C)]
pub(crate) struct PolychromeSprite {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub grayscale: bool,
    pub opacity: f32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub tile: AtlasTile,
    pub fade: FadeMask,
}

impl From<PolychromeSprite> for Primitive {
    fn from(sprite: PolychromeSprite) -> Self {
        Primitive::PolychromeSprite(sprite)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PaintSurface {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    #[cfg(target_os = "macos")]
    pub corner_radii: Corners<ScaledPixels>,
    #[cfg(target_os = "macos")]
    pub image_buffer: core_video::pixel_buffer::CVPixelBuffer,
}

impl From<PaintSurface> for Primitive {
    fn from(surface: PaintSurface) -> Self {
        Primitive::Surface(surface)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PathId(pub(crate) usize);

/// A line made up of a series of vertices and control points.
#[derive(Clone, Debug)]
pub struct Path<P: Clone + Debug + Default + PartialEq> {
    pub(crate) id: PathId,
    pub(crate) order: DrawOrder,
    pub(crate) bounds: Bounds<P>,
    pub(crate) content_mask: ContentMask<P>,
    pub(crate) vertices: Vec<PathVertex<P>>,
    pub(crate) color: Background,
    pub(crate) fade: FadeMask,
    start: Point<P>,
    current: Point<P>,
    contour_count: usize,
}

impl Path<Pixels> {
    /// Create a new path with the given starting point.
    pub fn new(start: Point<Pixels>) -> Self {
        Self {
            id: PathId(0),
            order: DrawOrder::default(),
            vertices: Vec::new(),
            start,
            current: start,
            bounds: Bounds {
                origin: start,
                size: Default::default(),
            },
            content_mask: Default::default(),
            color: Default::default(),
            fade: FadeMask::default(),
            contour_count: 0,
        }
    }

    /// Scale this path by the given factor.
    pub fn scale(&self, factor: f32) -> Path<ScaledPixels> {
        Path {
            id: self.id,
            order: self.order,
            bounds: self.bounds.scale(factor),
            content_mask: self.content_mask.scale(factor),
            vertices: self
                .vertices
                .iter()
                .map(|vertex| vertex.scale(factor))
                .collect(),
            start: self.start.map(|start| start.scale(factor)),
            current: self.current.scale(factor),
            contour_count: self.contour_count,
            color: self.color,
            fade: self.fade.scale(factor),
        }
    }

    /// Move the start, current point to the given point.
    pub fn move_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        self.start = to;
        self.current = to;
    }

    /// Draw a straight line from the current point to the given point.
    pub fn line_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }
        self.current = to;
    }

    /// Draw a curve from the current point to the given point, using the given control point.
    pub fn curve_to(&mut self, to: Point<Pixels>, ctrl: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }

        self.push_triangle(
            (self.current, ctrl, to),
            (point(0., 0.), point(0.5, 0.), point(1., 1.)),
        );
        self.current = to;
    }

    /// Push a triangle to the Path.
    pub fn push_triangle(
        &mut self,
        xy: (Point<Pixels>, Point<Pixels>, Point<Pixels>),
        st: (Point<f32>, Point<f32>, Point<f32>),
    ) {
        self.bounds = self
            .bounds
            .union(&Bounds {
                origin: xy.0,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.1,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.2,
                size: Default::default(),
            });

        self.vertices.push(PathVertex {
            xy_position: xy.0,
            st_position: st.0,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.1,
            st_position: st.1,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.2,
            st_position: st.2,
            content_mask: Default::default(),
        });
    }
}

impl<T> Path<T>
where
    T: Clone + Debug + Default + PartialEq + PartialOrd + Add<T, Output = T> + Sub<Output = T>,
{
    #[allow(unused)]
    pub(crate) fn clipped_bounds(&self) -> Bounds<T> {
        self.bounds.intersect(&self.content_mask.bounds)
    }
}

impl From<Path<ScaledPixels>> for Primitive {
    fn from(path: Path<ScaledPixels>) -> Self {
        Primitive::Path(path)
    }
}

#[derive(Clone, Debug)]
#[repr(C)]
pub(crate) struct PathVertex<P: Clone + Debug + Default + PartialEq> {
    pub(crate) xy_position: Point<P>,
    pub(crate) st_position: Point<f32>,
    pub(crate) content_mask: ContentMask<P>,
}

impl PathVertex<Pixels> {
    pub fn scale(&self, factor: f32) -> PathVertex<ScaledPixels> {
        PathVertex {
            xy_position: self.xy_position.scale(factor),
            st_position: self.st_position,
            content_mask: self.content_mask.scale(factor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{point, size, solid_background};

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds::new(point(ScaledPixels(x), ScaledPixels(y)), size(ScaledPixels(width), ScaledPixels(height)))
    }

    #[test]
    fn a_quad_drawn_over_a_shadows_blur_orders_above_it() {
        let mut scene = Scene::default();
        let content_mask = ContentMask { bounds: rect(0., 0., 1000., 1000.) };
        let casting = rect(0., 0., 100., 100.);
        scene.insert_primitive(Shadow {
            order: 0,
            blur_radius: ScaledPixels(10.),
            bounds: casting,
            corner_radii: Corners::default(),
            content_mask: content_mask.clone(),
            color: Hsla::default(),
            element_bounds: casting,
            element_corner_radii: Corners::default(),
            fade: FadeMask::default(),
            inset: 0,
            pad: 0,
        });
        scene.insert_primitive(Quad {
            order: 0,
            border_style: BorderStyle::default(),
            bounds: rect(110., 0., 50., 50.),
            content_mask,
            background: solid_background(crate::Rgba { r: 1., g: 1., b: 1., a: 1. }),
            border_color: Hsla::default(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            fade: FadeMask::default(),
        });
        assert!(scene.quads[0].order > scene.shadows[0].order);
    }

    #[test]
    fn a_blur_inside_a_drawn_over_region_keeps_what_is_under_it() {
        let mut scene = Scene::default();
        scene.replace_under_backdrops(rect(0., 0., 500., 800.));
        scene.draw_over_backdrops(rect(0., 100., 500., 40.));
        assert!(scene.replaced_under(&rect(0., 100., 500., 40.)).is_none());
        assert!(scene.replaced_under(&rect(100., 90., 200., 300.)).is_some());
    }

    #[test]
    fn only_blurs_drawn_over_the_tint_read_through_it() {
        let mut scene = Scene::default();
        let blur = |bounds| BackdropBlur { bounds, content_mask: ContentMask { bounds: rect(0., 0., 1000., 1000.) }, ..BackdropBlur::default() };
        scene.insert_primitive(blur(rect(300., 500., 400., 100.)));
        let veil = crate::Rgba { r: 0., g: 0., b: 0., a: 0.3 };
        scene.tint_hidden_backdrops(rect(0., 0., 1000., 1000.), veil);
        scene.insert_primitive(blur(rect(0., 0., 280., 1000.)));
        let under = &scene.backdrop_blurs[0];
        let over = &scene.backdrop_blurs[1];
        assert!(scene.hidden_tint_for(under.order, &under.bounds).is_none());
        assert!(scene.hidden_tint_for(over.order, &over.bounds).is_some());
    }
}
