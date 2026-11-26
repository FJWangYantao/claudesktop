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
    sync::Arc,
};

#[allow(non_camel_case_types, unused)]
#[expect(missing_docs)]
pub type PathVertex_ScaledPixels = PathVertex<ScaledPixels>;

#[expect(missing_docs)]
pub type DrawOrder = u32;

/// A boolean stored as a `u32` so that GPU-facing structs contain no
/// compiler-inserted padding bytes, which would be undefined behavior to
/// reinterpret as `&[u8]` when writing instance buffers. Guaranteed to be
/// `0` or `1` by construction; shaders read it as a `u32`/`uint`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct PaddedBool32(u32);

impl From<bool> for PaddedBool32 {
    fn from(value: bool) -> Self {
        PaddedBool32(value as u32)
    }
}

#[derive(Default)]
#[expect(missing_docs)]
pub struct Scene {
    pub(crate) paint_operations: Vec<PaintOperation>,
    primitive_bounds: BoundsTree<ScaledPixels>,
    layer_stack: Vec<DrawOrder>,
    pub shadows: Vec<Shadow>,
    pub quads: Vec<Quad>,
    pub paths: Vec<Path<ScaledPixels>>,
    pub underlines: Vec<Underline>,
    pub monochrome_sprites: Vec<MonochromeSprite>,
    pub subpixel_sprites: Vec<SubpixelSprite>,
    pub polychrome_sprites: Vec<PolychromeSprite>,
    pub surfaces: Vec<PaintSurface>,
    // claudesktop patch: 自定义着色器原语（首页背景 / 输入框棱镜描边）。
    pub prism_effects: Vec<PrismEffect>,
    // claudesktop patch: record a view before applying the viewport clip.
    recording: Option<SceneRecording>,
}

pub(crate) struct RecordedScene {
    ops: Vec<PaintOperation>,
    pub(crate) has_non_unit_transform: bool,
}

struct SceneRecording {
    outer_clip: Bounds<ScaledPixels>,
    scene: RecordedScene,
}

#[expect(missing_docs)]
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
        self.subpixel_sprites.clear();
        self.polychrome_sprites.clear();
        self.surfaces.clear();
        self.prism_effects.clear();
    }

    pub fn len(&self) -> usize {
        self.paint_operations.len()
    }

    pub fn push_layer(&mut self, bounds: Bounds<ScaledPixels>) {
        // claudesktop patch: retain the original layer for translated replay.
        let bounds = if let Some(recording) = self.recording.as_mut() {
            recording.scene.ops.push(PaintOperation::StartLayer(bounds));
            bounds.intersect(&recording.outer_clip)
        } else {
            bounds
        };
        let order = self.primitive_bounds.insert(bounds);
        self.layer_stack.push(order);
        self.paint_operations
            .push(PaintOperation::StartLayer(bounds));
    }

    pub fn pop_layer(&mut self) {
        if let Some(recording) = self.recording.as_mut() {
            recording.scene.ops.push(PaintOperation::EndLayer);
        }
        self.layer_stack.pop();
        self.paint_operations.push(PaintOperation::EndLayer);
    }

    pub fn insert_primitive(&mut self, primitive: impl Into<Primitive>) {
        let mut primitive = primitive.into();
        if let Some(recording) = self.recording.as_mut() {
            if primitive
                .bounds()
                .intersect(&primitive.content_mask().bounds)
                .is_empty()
            {
                return;
            }
            recording.scene.has_non_unit_transform |= primitive.has_non_unit_transform();
            recording
                .scene
                .ops
                .push(PaintOperation::Primitive(primitive.clone()));
            primitive.translate_and_clip(ScaledPixels(0.), &recording.outer_clip);
        }
        let clipped_bounds = primitive
            .bounds()
            .intersect(&primitive.content_mask().bounds);

        if clipped_bounds.is_empty() {
            return;
        }

        let order = self
            .layer_stack
            .last()
            .copied()
            .unwrap_or_else(|| self.primitive_bounds.insert(clipped_bounds));
        match &mut primitive {
            Primitive::Shadow(shadow) => {
                shadow.order = order;
                self.shadows.push(*shadow);
            }
            Primitive::Quad(quad) => {
                quad.order = order;
                self.quads.push(*quad);
            }
            Primitive::Path(path) => {
                path.order = order;
                path.id = PathId(self.paths.len());
                self.paths.push(path.clone());
            }
            Primitive::Underline(underline) => {
                underline.order = order;
                self.underlines.push(*underline);
            }
            Primitive::MonochromeSprite(sprite) => {
                sprite.order = order;
                self.monochrome_sprites.push(*sprite);
            }
            Primitive::SubpixelSprite(sprite) => {
                sprite.order = order;
                self.subpixel_sprites.push(*sprite);
            }
            Primitive::PolychromeSprite(sprite) => {
                sprite.order = order;
                self.polychrome_sprites.push(*sprite);
            }
            Primitive::Surface(surface) => {
                surface.order = order;
                self.surfaces.push(surface.clone());
            }
            Primitive::PrismEffect(effect) => {
                effect.set_order(order);
                self.prism_effects.push(effect.clone());
            }
        }
        self.paint_operations
            .push(PaintOperation::Primitive(primitive));
    }

    pub fn replay(&mut self, range: Range<usize>, prev_scene: &Scene) {
        for operation in &prev_scene.paint_operations[range] {
            match operation {
                PaintOperation::Primitive(primitive) => self.insert_primitive(primitive.clone()),
                PaintOperation::StartLayer(bounds) => self.push_layer(*bounds),
                PaintOperation::EndLayer => self.pop_layer(),
            }
        }
    }

    // claudesktop patch: snapshots are immutable. A replay never becomes the next baseline.
    pub(crate) fn begin_recording(&mut self, outer_clip: Bounds<ScaledPixels>) {
        debug_assert!(self.recording.is_none());
        self.recording = Some(SceneRecording {
            outer_clip,
            scene: RecordedScene {
                ops: Vec::new(),
                has_non_unit_transform: false,
            },
        });
    }

    pub(crate) fn end_recording(&mut self) -> RecordedScene {
        self.recording.take().unwrap().scene
    }

    pub(crate) fn replay_translated(
        &mut self,
        scene: &RecordedScene,
        dy: ScaledPixels,
        outer_clip: Bounds<ScaledPixels>,
    ) {
        debug_assert!(self.recording.is_none());
        debug_assert!(!scene.has_non_unit_transform);
        let mut source_clip = outer_clip;
        source_clip.origin.y -= dy;
        for operation in &scene.ops {
            match operation {
                PaintOperation::Primitive(primitive) => {
                    if primitive.bounds().intersects(&source_clip) {
                        let mut primitive = primitive.clone();
                        primitive.translate_and_clip(dy, &outer_clip);
                        self.insert_primitive(primitive);
                    }
                }
                PaintOperation::StartLayer(bounds) => {
                    let mut bounds = *bounds;
                    bounds.origin.y += dy;
                    self.push_layer(bounds.intersect(&outer_clip));
                }
                PaintOperation::EndLayer => self.pop_layer(),
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
        self.subpixel_sprites
            .sort_by_key(|sprite| (sprite.order, sprite.tile.tile_id));
        self.polychrome_sprites
            .sort_by_key(|sprite| (sprite.order, sprite.tile.tile_id));
        self.surfaces.sort_by_key(|surface| surface.order);
        self.prism_effects.sort_by_key(|effect| effect.order());
    }

    #[cfg_attr(
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(any(feature = "x11", feature = "wayland"))
        ),
        allow(dead_code)
    )]
    pub fn batches(&self) -> impl Iterator<Item = PrimitiveBatch> + '_ {
        BatchIterator {
            shadows_start: 0,
            shadows_iter: self.shadows.iter().peekable(),
            quads_start: 0,
            quads_iter: self.quads.iter().peekable(),
            paths_start: 0,
            paths_iter: self.paths.iter().peekable(),
            underlines_start: 0,
            underlines_iter: self.underlines.iter().peekable(),
            monochrome_sprites_start: 0,
            monochrome_sprites_iter: self.monochrome_sprites.iter().peekable(),
            subpixel_sprites_start: 0,
            subpixel_sprites_iter: self.subpixel_sprites.iter().peekable(),
            polychrome_sprites_start: 0,
            polychrome_sprites_iter: self.polychrome_sprites.iter().peekable(),
            surfaces_start: 0,
            surfaces_iter: self.surfaces.iter().peekable(),
            prism_effects_start: 0,
            prism_effects_iter: self.prism_effects.iter().peekable(),
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
    Shadow,
    #[default]
    Quad,
    Path,
    Underline,
    MonochromeSprite,
    SubpixelSprite,
    PolychromeSprite,
    Surface,
    PrismEffect,
}

pub(crate) enum PaintOperation {
    Primitive(Primitive),
    StartLayer(Bounds<ScaledPixels>),
    EndLayer,
}

#[derive(Clone)]
#[expect(missing_docs)]
pub enum Primitive {
    Shadow(Shadow),
    Quad(Quad),
    Path(Path<ScaledPixels>),
    Underline(Underline),
    MonochromeSprite(MonochromeSprite),
    SubpixelSprite(SubpixelSprite),
    PolychromeSprite(PolychromeSprite),
    Surface(PaintSurface),
    // claudesktop patch: 自定义着色器原语。
    PrismEffect(PrismEffect),
}

#[expect(missing_docs)]
impl Primitive {
    // claudesktop patch: sprite matrices can contain absolute coordinates.
    fn has_non_unit_transform(&self) -> bool {
        match self {
            Self::MonochromeSprite(sprite) => sprite.transformation != TransformationMatrix::unit(),
            Self::SubpixelSprite(sprite) => sprite.transformation != TransformationMatrix::unit(),
            _ => false,
        }
    }

    fn translate_and_clip(&mut self, dy: ScaledPixels, clip: &Bounds<ScaledPixels>) {
        let shift_mask = |mask: &mut ContentMask<ScaledPixels>| {
            mask.bounds.origin.y += dy;
            mask.bounds = mask.bounds.intersect(clip);
        };
        let (bounds, mask) = match self {
            Self::Quad(p) => (&mut p.bounds, &mut p.content_mask),
            Self::Underline(p) => (&mut p.bounds, &mut p.content_mask),
            Self::MonochromeSprite(p) => (&mut p.bounds, &mut p.content_mask),
            Self::SubpixelSprite(p) => (&mut p.bounds, &mut p.content_mask),
            Self::PolychromeSprite(p) => (&mut p.bounds, &mut p.content_mask),
            Self::Surface(p) => (&mut p.bounds, &mut p.content_mask),
            Self::Shadow(p) => {
                p.element_bounds.origin.y += dy;
                (&mut p.bounds, &mut p.content_mask)
            }
            Self::Path(p) => {
                p.start.y += dy;
                p.current.y += dy;
                for vertex in &mut p.vertices {
                    vertex.xy_position.y += dy;
                    shift_mask(&mut vertex.content_mask);
                }
                (&mut p.bounds, &mut p.content_mask)
            }
            Self::PrismEffect(effect) => {
                // 自带 bounds / mask 一并平移，直接返回，不走下面的通用分支。
                effect.shift(dy, clip);
                return;
            }
        };
        bounds.origin.y += dy;
        shift_mask(mask);
    }

    pub fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.bounds,
            Primitive::Quad(quad) => &quad.bounds,
            Primitive::Path(path) => &path.bounds,
            Primitive::Underline(underline) => &underline.bounds,
            Primitive::MonochromeSprite(sprite) => &sprite.bounds,
            Primitive::SubpixelSprite(sprite) => &sprite.bounds,
            Primitive::PolychromeSprite(sprite) => &sprite.bounds,
            Primitive::Surface(surface) => &surface.bounds,
            Primitive::PrismEffect(effect) => effect.bounds(),
        }
    }

    pub fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.content_mask,
            Primitive::Quad(quad) => &quad.content_mask,
            Primitive::Path(path) => &path.content_mask,
            Primitive::Underline(underline) => &underline.content_mask,
            Primitive::MonochromeSprite(sprite) => &sprite.content_mask,
            Primitive::SubpixelSprite(sprite) => &sprite.content_mask,
            Primitive::PolychromeSprite(sprite) => &sprite.content_mask,
            Primitive::Surface(surface) => &surface.content_mask,
            Primitive::PrismEffect(effect) => effect.content_mask(),
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
    shadows_start: usize,
    shadows_iter: Peekable<slice::Iter<'a, Shadow>>,
    quads_start: usize,
    quads_iter: Peekable<slice::Iter<'a, Quad>>,
    paths_start: usize,
    paths_iter: Peekable<slice::Iter<'a, Path<ScaledPixels>>>,
    underlines_start: usize,
    underlines_iter: Peekable<slice::Iter<'a, Underline>>,
    monochrome_sprites_start: usize,
    monochrome_sprites_iter: Peekable<slice::Iter<'a, MonochromeSprite>>,
    subpixel_sprites_start: usize,
    subpixel_sprites_iter: Peekable<slice::Iter<'a, SubpixelSprite>>,
    polychrome_sprites_start: usize,
    polychrome_sprites_iter: Peekable<slice::Iter<'a, PolychromeSprite>>,
    surfaces_start: usize,
    surfaces_iter: Peekable<slice::Iter<'a, PaintSurface>>,
    prism_effects_start: usize,
    prism_effects_iter: Peekable<slice::Iter<'a, PrismEffect>>,
}

impl<'a> Iterator for BatchIterator<'a> {
    type Item = PrimitiveBatch;

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
                self.subpixel_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::SubpixelSprite,
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
                self.prism_effects_iter.peek().map(|effect| effect.order()),
                PrimitiveKind::PrismEffect,
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
                Some(PrimitiveBatch::Shadows(shadows_start..shadows_end))
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
                Some(PrimitiveBatch::Quads(quads_start..quads_end))
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
                Some(PrimitiveBatch::Paths(paths_start..paths_end))
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
                Some(PrimitiveBatch::Underlines(underlines_start..underlines_end))
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
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::SubpixelSprite => {
                let texture_id = self.subpixel_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.subpixel_sprites_start;
                let mut sprites_end = sprites_start + 1;
                self.subpixel_sprites_iter.next();
                while self
                    .subpixel_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.subpixel_sprites_start = sprites_end;
                Some(PrimitiveBatch::SubpixelSprites {
                    texture_id,
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::PolychromeSprite => {
                let texture_id = self.polychrome_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.polychrome_sprites_start;
                let mut sprites_end = sprites_start + 1;
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
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::PrismEffect => {
                let effects_start = self.prism_effects_start;
                let mut effects_end = effects_start + 1;
                self.prism_effects_iter.next();
                while self
                    .prism_effects_iter
                    .next_if(|effect| (effect.order(), batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    effects_end += 1;
                }
                self.prism_effects_start = effects_end;
                Some(PrimitiveBatch::PrismEffects(effects_start..effects_end))
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
                Some(PrimitiveBatch::Surfaces(surfaces_start..surfaces_end))
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
#[allow(missing_docs)]
pub enum PrimitiveBatch {
    Shadows(Range<usize>),
    Quads(Range<usize>),
    Paths(Range<usize>),
    Underlines(Range<usize>),
    MonochromeSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    SubpixelSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    PolychromeSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    Surfaces(Range<usize>),
    /// claudesktop patch: 自定义着色器效果；一个批次里可能混着三种，逐个按类型画。
    PrismEffects(Range<usize>),
}

impl PrimitiveBatch {
    #[expect(missing_docs)]
    pub fn label(&self) -> String {
        match self {
            Self::Shadows(range) => format!("shadows ({})", range.len()),
            Self::Quads(range) => format!("quads ({})", range.len()),
            Self::Paths(range) => format!("paths ({})", range.len()),
            Self::Underlines(range) => format!("underlines ({})", range.len()),
            Self::MonochromeSprites { texture_id, range } => {
                format!(
                    "monochrome sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::SubpixelSprites { texture_id, range } => {
                format!(
                    "subpixel sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::PolychromeSprites { texture_id, range } => {
                format!(
                    "polychrome sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::Surfaces(range) => format!("surfaces ({})", range.len()),
            Self::PrismEffects(range) => format!("prism effects ({})", range.len()),
        }
    }
}

#[derive(Default, Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Quad {
    pub order: DrawOrder,
    pub border_style: BorderStyle,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub background: Background,
    pub border_color: Hsla,
    pub corner_radii: Corners<ScaledPixels>,
    pub border_widths: Edges<ScaledPixels>,
}

impl From<Quad> for Primitive {
    fn from(quad: Quad) -> Self {
        Primitive::Quad(quad)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Underline {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub thickness: ScaledPixels,
    pub wavy: PaddedBool32,
}

impl From<Underline> for Primitive {
    fn from(underline: Underline) -> Self {
        Primitive::Underline(underline)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Shadow {
    pub order: DrawOrder,
    pub blur_radius: ScaledPixels,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub element_bounds: Bounds<ScaledPixels>,
    pub element_corner_radii: Corners<ScaledPixels>,
    /// 0 = drop shadow (rendered outside the element), 1 = inset shadow (rendered inside).
    pub inset: u32,
    pub pad: u32, // align to 8 bytes
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

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct MonochromeSprite {
    pub order: DrawOrder,
    pub pad: u32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub tile: AtlasTile,
    pub transformation: TransformationMatrix,
}

impl From<MonochromeSprite> for Primitive {
    fn from(sprite: MonochromeSprite) -> Self {
        Primitive::MonochromeSprite(sprite)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct SubpixelSprite {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub tile: AtlasTile,
    pub transformation: TransformationMatrix,
}

impl From<SubpixelSprite> for Primitive {
    fn from(sprite: SubpixelSprite) -> Self {
        Primitive::SubpixelSprite(sprite)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct PolychromeSprite {
    pub order: DrawOrder,
    pub pad: u32,
    pub grayscale: PaddedBool32,
    pub opacity: f32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub tile: AtlasTile,
}

impl From<PolychromeSprite> for Primitive {
    fn from(sprite: PolychromeSprite) -> Self {
        Primitive::PolychromeSprite(sprite)
    }
}

#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PaintSurface {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub image_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

impl From<PaintSurface> for Primitive {
    fn from(surface: PaintSurface) -> Self {
        Primitive::Surface(surface)
    }
}

// claudesktop patch: 首页背景 / 输入框棱镜描边用的自定义着色器原语。
//
// 这些结构体直接当 D3D11 的 StructuredBuffer 用，布局必须和
// `vendor/gpui-pre-windows/src/prism_shaders.hlsl` 里的同名 struct 逐字节一致：
// 字段顺序别动；末尾的 `pad` 是把整体撑到 16 字节整数倍用的，StructuredBuffer 的
// 元素步长必须是 16 的倍数，否则从第二个元素起全部错位。
//
// 为什么要走 Scene 原语而不是自己开一层：gpui 的绘制是「记一遍图元、按 order 排好、
// 一次性提交」，自己画会在滚动/裁切/重放时漏掉 clip 和 Y 轴平移；接进 Scene 之后
// 离屏截图（`--features headless-screenshot`）也自动包含这些效果。

/// 输入框棱镜描边：SDF 圆角环 + 跟随指针的径向光谱。
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
#[expect(missing_docs)]
pub struct PrismRim {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    /// 指针位置（窗口物理像素），光谱以它为圆心。
    pub pointer: Point<ScaledPixels>,
    /// 光谱半径（像素），超出这个距离只剩描边本来的颜色。
    pub radius: f32,
    /// 描边宽度（像素）。
    pub stroke_width: f32,
    /// 整体不透明度，0 表示不画（View 那边直接不提交即可）。
    pub alpha: f32,
    /// 0 = 浅色背景，1 = 深色背景（决定光谱的亮度曲线）。
    pub dark: u32,
    /// 调色板：0 = 默认暖白、1 = 暖纸、2 = 钴蓝、3 = 靛紫、4 = 琥珀、5 = 纯灰。
    pub tone_key: u32,
    /// 0 = 只画描边，1 = 描边 + 底部反射。
    pub mode: u32,
    pub pad: [f32; 3],
}

const _: () = assert!(std::mem::size_of::<PrismRim>() % 16 == 0);

impl From<PrismRim> for Primitive {
    fn from(rim: PrismRim) -> Self {
        Primitive::PrismEffect(PrismEffect::Rim(rim))
    }
}

/// 首页地平线极光：fbm 波动 + 透视半调点阵。
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
#[expect(missing_docs)]
pub struct HorizonGlow {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    /// 设备像素比：点阵和噪声按物理像素算，换缩放不会变粗。
    pub dpr: f32,
    /// 秒（单调时钟），驱动极光的横向漂移。
    pub time: f32,
    /// 0 = 浅色，1 = 深色。
    pub dark: f32,
    /// 极光从底部往上伸多远（0~1，相对高度）。
    pub reach: f32,
    /// 整体强度，0 时 shader 直接 discard。
    pub intensity: f32,
    /// 横向拉伸：越小波纹越宽。
    pub stretch: f32,
    /// 存在感（半调点阵的疏密与亮度）。
    pub presence: f32,
    /// 棱镜色散强度。
    pub prism: f32,
    /// 发消息瞬间的脉冲（0~1，会自然衰减）。
    pub surge: f32,
    /// 呼吸速度倍率。
    pub speed: f32,
    /// 谱宽：极光横跨多少色相。
    pub spectrum: f32,
    /// 语义色相（0 = 冷调、1 = 暖调），由配色决定。
    pub semantic: f32,
    pub pad: [f32; 3],
}

const _: () = assert!(std::mem::size_of::<HorizonGlow>() % 16 == 0);

impl From<HorizonGlow> for Primitive {
    fn from(horizon: HorizonGlow) -> Self {
        Primitive::PrismEffect(PrismEffect::Horizon(horizon))
    }
}

/// ASCII 波动字阵的参数，全部按 `[f32; 4]` 分组打包（见文件头说明）。
///
/// 八个分组严格对应 `ascii_ripple.hlsl` 里的同名结构体，顺序不能动。
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct AsciiRippleParams {
    /// 0 = 单元格宽、1 = 单元格高、2 = 图集里一格的宽、3 = 字形数量。
    pub grid: [f32; 4],
    /// 静止态字色（rgb + 不透明度）。
    pub ink: [f32; 4],
    /// 波面字色（rgb + 混合增益）。
    pub wave: [f32; 4],
    /// 指针光晕：圆心 x / y（相对 bounds 左上）、半径像素、强度。
    pub aura: [f32; 4],
    /// 0 = 色散量、1 = 色相偏转、2 = 半调、3 = 对比。
    pub optics: [f32; 4],
    /// 0 = 秒、1 = 深色程度、2 = 整体强度、3 = 云雾相位。
    pub timing: [f32; 4],
    /// 0 = 云雾强度、1 = 光晕抬升、2 = 呼吸、3 = 棱镜。
    pub nebula: [f32; 4],
    /// 字形图集的像素宽高（0 = 宽、1 = 高，其余占位）。
    pub atlas_size: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<AsciiRippleParams>() % 16 == 0);

/// ASCII 波动字阵：CPU 算波场与字形索引，GPU 只负责铺格子、取字形、上色。
#[derive(Clone, Debug)]
#[expect(missing_docs)]
pub struct AsciiRipple {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub params: AsciiRippleParams,
    /// 单元格快照（RGBA8，每格 4 字节）。按 `revision` 判断要不要重传。
    pub cells: Option<Arc<AsciiCellsUpload>>,
    /// 62 个字形拼成的水平图集（R8 alpha）。
    pub atlas: Option<Arc<AsciiAtlasUpload>>,
    pub pad: [f32; 3],
}

impl AsciiRipple {
    /// 传给 GPU 的那一份（去掉 `Arc`，只留 shader 看得见的字段）。
    pub fn uniform(&self) -> AsciiRippleUniform {
        AsciiRippleUniform {
            order: self.order,
            bounds: self.bounds,
            content_mask: self.content_mask,
            params: self.params,
            pad: self.pad,
        }
    }
}

/// [`AsciiRipple`] 的 GPU 布局（步长 16 字节对齐）。
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
#[expect(missing_docs)]
pub struct AsciiRippleUniform {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub params: AsciiRippleParams,
    pub pad: [f32; 3],
}

const _: () = assert!(std::mem::size_of::<AsciiRippleUniform>() % 16 == 0);

impl From<AsciiRipple> for Primitive {
    fn from(ripple: AsciiRipple) -> Self {
        Primitive::PrismEffect(PrismEffect::Ascii(ripple))
    }
}

/// 字阵的单元格数据。`revision` 变了才需要重传显存。
#[derive(Clone, Debug, Default)]
#[expect(missing_docs)]
pub struct AsciiCellsUpload {
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    /// `width * height * 4` 字节，RGBA8。
    pub rgba: Vec<u8>,
}

/// 字形图集。`key` 变了（换字体 / 换缩放比）才需要重传显存。
#[derive(Clone, Debug, Default)]
#[expect(missing_docs)]
pub struct AsciiAtlasUpload {
    pub width: u32,
    pub height: u32,
    pub key: u64,
    /// `width * height` 字节，R8。
    pub alpha_r8: Vec<u8>,
}

/// 三种自定义效果共用一条批次，靠 order 和别的图元交错。
#[derive(Clone, Debug)]
#[expect(missing_docs)]
pub enum PrismEffect {
    Rim(PrismRim),
    Horizon(HorizonGlow),
    Ascii(AsciiRipple),
}

/// [`crate::Window::paint_prism_rim`] 的参数，用户空间单位（`Window` 负责对齐设备像素）。
#[derive(Clone, Copy, Debug)]
#[expect(missing_docs)]
pub struct PrismRimStyle {
    pub corner_radii: Corners<Pixels>,
    /// 指针位置（用户空间），光谱以它为圆心。
    pub pointer: Point<Pixels>,
    /// 光谱半径。
    pub radius: Pixels,
    pub stroke_width: Pixels,
    pub alpha: f32,
    pub dark: bool,
    pub tone_key: u32,
    pub mode: u32,
}

impl Default for PrismRimStyle {
    fn default() -> Self {
        Self {
            corner_radii: Corners::default(),
            pointer: Default::default(),
            radius: Pixels(192.),
            stroke_width: Pixels(1.),
            alpha: 1.,
            dark: false,
            tone_key: 0,
            mode: 0,
        }
    }
}

/// [`crate::Window::paint_horizon_glow`] 的参数，全部无量纲。
#[derive(Clone, Copy, Debug)]
#[expect(missing_docs)]
pub struct HorizonGlowParams {
    /// 秒（单调时钟）。
    pub time: f32,
    pub dark: bool,
    pub reach: f32,
    pub intensity: f32,
    pub stretch: f32,
    pub presence: f32,
    pub prism: f32,
    pub surge: f32,
    pub speed: f32,
    pub spectrum: f32,
    pub semantic: f32,
}

impl Default for HorizonGlowParams {
    fn default() -> Self {
        Self {
            time: 0.,
            dark: false,
            reach: 0.55,
            intensity: 0.5,
            stretch: 1.,
            presence: 0.4,
            prism: 0.3,
            surge: 0.,
            speed: 1.,
            spectrum: 1.,
            semantic: 0.,
        }
    }
}

impl PrismEffect {
    pub(crate) fn order(&self) -> DrawOrder {
        match self {
            Self::Rim(rim) => rim.order,
            Self::Horizon(horizon) => horizon.order,
            Self::Ascii(ripple) => ripple.order,
        }
    }

    fn set_order(&mut self, order: DrawOrder) {
        match self {
            Self::Rim(rim) => rim.order = order,
            Self::Horizon(horizon) => horizon.order = order,
            Self::Ascii(ripple) => ripple.order = order,
        }
    }

    pub(crate) fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Self::Rim(rim) => &rim.bounds,
            Self::Horizon(horizon) => &horizon.bounds,
            Self::Ascii(ripple) => &ripple.bounds,
        }
    }

    pub(crate) fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Self::Rim(rim) => &rim.content_mask,
            Self::Horizon(horizon) => &horizon.content_mask,
            Self::Ascii(ripple) => &ripple.content_mask,
        }
    }

    /// 平移重放（滚动缓存）时整体下移并重新裁切。
    fn shift(&mut self, dy: ScaledPixels, clip: &Bounds<ScaledPixels>) {
        let (bounds, mask) = match self {
            Self::Rim(rim) => (&mut rim.bounds, &mut rim.content_mask),
            Self::Horizon(horizon) => (&mut horizon.bounds, &mut horizon.content_mask),
            Self::Ascii(ripple) => (&mut ripple.bounds, &mut ripple.content_mask),
        };
        bounds.origin.y += dy;
        mask.bounds.origin.y += dy;
        mask.bounds = mask.bounds.intersect(clip);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[expect(missing_docs)]
pub struct PathId(pub usize);

/// A line made up of a series of vertices and control points.
#[derive(Clone, Debug)]
#[expect(missing_docs)]
pub struct Path<P: Clone + Debug + Default + PartialEq> {
    pub id: PathId,
    pub order: DrawOrder,
    pub bounds: Bounds<P>,
    pub content_mask: ContentMask<P>,
    pub vertices: Vec<PathVertex<P>>,
    pub color: Background,
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
    #[expect(missing_docs)]
    pub fn clipped_bounds(&self) -> Bounds<T> {
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
#[expect(missing_docs)]
pub struct PathVertex<P: Clone + Debug + Default + PartialEq> {
    pub xy_position: Point<P>,
    pub st_position: Point<f32>,
    pub content_mask: ContentMask<P>,
}

#[expect(missing_docs)]
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
mod scroll_scene_tests {
    use super::*;
    use crate::px;

    fn rect(left: f32, top: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(left), ScaledPixels(top)),
            size: Size::new(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    #[test]
    fn translated_replay_recovers_primitives_hidden_by_the_first_viewport() {
        let viewport = rect(0., 0., 100., 100.);
        let inner = rect(10., 100., 60., 50.);
        let mut scene = Scene::default();
        scene.begin_recording(viewport);
        scene.push_layer(rect(0., 95., 120., 60.));
        scene.insert_primitive(Quad {
            order: 0,
            border_style: BorderStyle::Solid,
            bounds: rect(0., 120., 120., 15.),
            content_mask: ContentMask { bounds: inner },
            background: Background::default(),
            border_color: Hsla::default(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
        });
        scene.pop_layer();
        let snapshot = scene.end_recording();
        assert!(scene.quads.is_empty());

        for dy in [-60., -40., -60.] {
            scene.clear();
            scene.replay_translated(&snapshot, ScaledPixels(dy), viewport);
            assert_eq!(scene.quads.len(), 1);
            assert_eq!(scene.quads[0].bounds, rect(0., 120. + dy, 120., 15.));
            assert_eq!(
                scene.quads[0].content_mask.bounds,
                rect(10., 100. + dy, 60., 50.).intersect(&viewport)
            );
        }
    }

    #[test]
    fn translated_replay_moves_shadow_centers_and_every_path_coordinate() {
        let viewport = rect(0., 0., 100., 100.);
        let mask = ContentMask {
            bounds: rect(10., 10., 70., 80.),
        };
        let mut scene = Scene::default();
        scene.begin_recording(viewport);
        scene.insert_primitive(Shadow {
            order: 0,
            blur_radius: ScaledPixels(2.),
            bounds: rect(10., 20., 30., 30.),
            content_mask: mask,
            corner_radii: Corners::default(),
            color: Hsla::default(),
            element_bounds: rect(12., 22., 26., 26.),
            element_corner_radii: Corners::default(),
            inset: 1,
            pad: 0,
        });
        let mut path = Path::new(point(px(20.), px(30.)));
        path.line_to(point(px(40.), px(30.)));
        path.line_to(point(px(40.), px(50.)));
        let mut path = path.scale(1.);
        path.content_mask = mask;
        for vertex in &mut path.vertices {
            vertex.content_mask = mask;
        }
        let original = path.clone();
        scene.insert_primitive(path);
        let snapshot = scene.end_recording();

        scene.clear();
        scene.replay_translated(&snapshot, ScaledPixels(30.), viewport);
        assert_eq!(scene.shadows[0].bounds.top(), ScaledPixels(50.));
        assert_eq!(scene.shadows[0].element_bounds.top(), ScaledPixels(52.));
        let path = &scene.paths[0];
        assert_eq!(path.bounds.top(), ScaledPixels(60.));
        assert_eq!(path.start.y, original.start.y + ScaledPixels(30.));
        assert_eq!(path.current.y, original.current.y + ScaledPixels(30.));
        assert_eq!(path.content_mask.bounds, rect(10., 40., 70., 60.));
        for (vertex, original) in path.vertices.iter().zip(&original.vertices) {
            assert_eq!(
                vertex.xy_position.y,
                original.xy_position.y + ScaledPixels(30.)
            );
            assert_eq!(vertex.content_mask.bounds, path.content_mask.bounds);
        }
    }
}
