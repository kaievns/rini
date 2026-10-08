//! The layers a tile's decorations are drawn with: the caster behind the picture with its mask, and
//! the hairline in front with its outline, the focus ring and the border. They ride the tile's frame
//! beside its picture, and every value they are set to or animated through comes from `domain`. See
//! "The hairline and the shadow, as drawn" and "The focus ring and the border, as drawn" in
//! `src/decorations/docs/README.md`.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_foundation::NSString;
use objc2_quartz_core::{
    CAAnimation, CALayer, CAPropertyAnimation, CAShapeLayer, kCACornerCurveContinuous,
    kCAFillRuleEvenOdd,
};
use rini_geometry::space::{TileRect, TileSize, ToCg, Vector};

use crate::decorations::domain::paint::Colour;
use crate::decorations::domain::shape::{self, SHADOW_REACH, Segment};
use crate::decorations::domain::stack::{HAIRLINE_Z, SHADOW_Z};
use crate::decorations::domain::style::{Decoration, LIGHT_PT, OUTLINE_ALPHA, OUTLINE_PT, Style};

// What a resize installs on the decorations, by the layer it rides.
const SILHOUETTE_KEY: &str = "rini.tile.shadow.path";
const MASK_MOVE_KEY: &str = "rini.tile.mask.move";
const MASK_SIZE_KEY: &str = "rini.tile.mask.size";
const MASK_PATH_KEY: &str = "rini.tile.mask.path";
const CORNER_KEY: &str = "rini.tile.hairline.corner";
const OUTLINE_KEY: &str = "rini.tile.outline.path";
const BAND_SIZE_KEY: &str = "rini.tile.band.size";
const BAND_CORNER_KEY: &str = "rini.tile.band.corner";

pub fn add_under(layer: &CALayer, animation: &CAAnimation, key: &str) {
    layer.addAnimation_forKey(animation, Some(&NSString::from_str(key)));
}

pub fn remove_under(layer: &CALayer, keys: &[&str]) {
    for key in keys {
        layer.removeAnimationForKey(&NSString::from_str(key));
    }
}

/// A tile's resize as Core Animation plays it: an animation of `key_path` through each value
/// `value` maps from the sizes the tile passes through, on the tile's own timing. The overlay fills
/// it from the tile's track.
pub trait Resize {
    fn point(
        &self,
        key_path: &str,
        value: impl Fn(TileSize) -> CGPoint,
    ) -> Retained<CAPropertyAnimation>;
    fn rect(
        &self,
        key_path: &str,
        value: impl Fn(TileSize) -> CGRect,
    ) -> Retained<CAPropertyAnimation>;
    fn number(
        &self,
        key_path: &str,
        value: impl Fn(TileSize) -> f64,
    ) -> Retained<CAPropertyAnimation>;
    fn path(
        &self,
        key_path: &str,
        value: impl Fn(TileSize) -> CFRetained<CGPath>,
    ) -> Retained<CAPropertyAnimation>;
    /// Whether `f` of the sizes the resize passes through differs between any two of them.
    fn changes(&self, f: impl Fn(TileSize) -> f64) -> bool;
}

/// What one tile wears. See "A layer shadow covers the whole layer" in
/// `src/animation/docs/capture-overlay-research.md`.
pub struct DecorationLayers {
    /// The caster: no contents, only the shadow, masked to what lies outside the window.
    pub shadow: Retained<CALayer>,
    /// Persistent, so a resize can animate its path; a swapped-in mask cannot be animated.
    pub shadow_mask: Retained<CAShapeLayer>,
    /// The hairline's white line, a border at the tile's frame.
    pub hairline: Retained<CALayer>,
    /// The hairline's dark outline, a stroke just outside the white line, as the hairline's child.
    pub outline: Retained<CAShapeLayer>,
    /// The ring and the border, bands outside the window, as the hairline's children after the
    /// outline, so they are drawn in front of the picture, the hairline and the outline.
    pub bands: Bands,
    /// The window's corner radius as the window server reports it, which every piece rounds to;
    /// `None` rounds at the fallback.
    pub radius: Option<f64>,
    /// What the bands were last worn for, whose rule's corner they round to ahead of `radius`.
    worn: Decoration,
    /// The size and corner the caster's silhouette and its mask were last shaped for.
    built: Cell<Option<(TileSize, f64)>>,
}

impl DecorationLayers {
    /// The caster with its even-odd mask, and the hairline holding the outline, under `container`.
    /// No `masksToBounds` on the caster, which would clip its own shadow, or on the hairline, which
    /// would clip the outline.
    pub fn new(container: &CALayer) -> DecorationLayers {
        let shadow = CALayer::layer();
        shadow.setAnchorPoint(CGPoint::new(0.0, 0.0));
        // The default shadow colour (opaque black) is what a window casts; style is applied at install.
        let shadow_mask = CAShapeLayer::layer();
        shadow_mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
        // SAFETY: Core Animation's own fill-rule constant, and a mask layer we just created and own.
        unsafe {
            shadow_mask.setFillRule(kCAFillRuleEvenOdd);
            shadow.setMask(Some(&shadow_mask));
        }
        container.addSublayer(&shadow);

        let hairline = CALayer::layer();
        hairline.setAnchorPoint(CGPoint::new(0.0, 0.0));
        hairline.setBorderWidth(LIGHT_PT);
        round_continuous(&hairline);
        let outline = CAShapeLayer::layer();
        outline.setAnchorPoint(CGPoint::new(0.0, 0.0));
        outline.setFillColor(None);
        outline.setStrokeColor(Some(&CGColor::new_generic_gray(0.0, OUTLINE_ALPHA)));
        outline.setLineWidth(OUTLINE_PT);
        hairline.addSublayer(&outline);
        let bands = Bands::new(&hairline);
        container.addSublayer(&hairline);

        DecorationLayers {
            shadow,
            shadow_mask,
            hairline,
            outline,
            bands,
            radius: None,
            worn: Decoration::default(),
            built: Cell::new(None),
        }
    }

    /// The layers that ride the tile's frame: the caster and the hairline.
    pub fn frame_layers(&self) -> [&CALayer; 2] {
        [&self.shadow, &self.hairline]
    }

    /// Takes every animation off every decoration layer.
    pub fn remove_animations(&self) {
        for layer in [
            &*self.shadow,
            &self.shadow_mask,
            &self.hairline,
            &self.outline,
            &self.bands.ring.layer,
            &self.bands.border.layer,
        ] {
            layer.removeAllAnimations();
        }
    }

    /// Draws the decorations of a tile at depth `z`: the caster `SHADOW_Z` behind its picture, the
    /// hairline `HAIRLINE_Z` in front of it.
    pub fn stack(&self, z: f64) {
        self.shadow.setZPosition(z + SHADOW_Z);
        self.hairline.setZPosition(z + HAIRLINE_Z);
    }

    /// Shapes the decorations for a tile of `size`, laid at its frame, all at one corner: the
    /// shadow and its mask only when the size or the corner differs from the ones they were shaped
    /// for, since a pooled tile can be dressed for a window of another radius at the same size; the
    /// hairline every time.
    pub fn place(&self, size: TileSize) {
        let corner = self.corner_at(size);
        if self.built.get() != Some((size, corner)) {
            self.shadow.setShadowPath(Some(&silhouette_path(size, corner)));
            self.shadow_mask.setFrame(shape::shadow_mask_frame(size).to_cg());
            self.shadow_mask.setPath(Some(&shadow_mask_path(size, corner)));
            self.built.set(Some((size, corner)));
        }
        self.hairline.setCornerRadius(corner);
        self.outline.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        self.outline.setPath(Some(&outline_path(size, corner)));
        self.bands.place(size, self.band_corner_at(size));
    }

    /// The corner a tile of `size` rounds to, every piece of it but the bands: its window's radius,
    /// clamped to the tile.
    pub fn corner_at(&self, size: TileSize) -> f64 {
        shape::radius(self.radius, size)
    }

    /// The corner the bands round to on a tile of `size`: a window rule's, else its window's.
    fn band_corner_at(&self, size: TileSize) -> f64 {
        shape::radius(self.worn.corner(|| self.radius), size)
    }

    /// Paints the hairline's white, the shadow's depth, the ring and the border as `style` has them.
    pub fn show(&self, style: Style) {
        self.hairline
            .setBorderColor(Some(&CGColor::new_generic_gray(1.0, style.light_alpha)));
        let shadow = style.shadow;
        self.shadow.setShadowOpacity(shadow.opacity);
        self.shadow.setShadowRadius(shadow.radius);
        self.shadow.setShadowOffset(CGSize::new(0.0, shadow.offset_y));
        self.bands.paint(&style);
    }

    /// Draws the outline at the display's `scale`, and the bands of `decoration` in whole device
    /// pixels of it, at its rule's corner; laid at the next `place`.
    pub fn wear(&mut self, decoration: &Decoration, scale: f64) {
        self.outline.setContentsScale(scale);
        self.worn = *decoration;
        self.bands.wear(decoration, scale);
    }

    /// Carries the decorations through `resize`, each piece at the corner of every size it passes
    /// through: the caster's silhouette, its mask's frame and path, the hairline's corner while it
    /// changes, and the outline's path. The frame layers' bounds ride the tile's own resize.
    pub fn install_resize(&self, resize: &impl Resize) {
        let origin = CGPoint::new(0.0, 0.0);
        let corner = |size: TileSize| self.corner_at(size);
        let silhouette = resize.path("shadowPath", |size| silhouette_path(size, corner(size)));
        add_under(&self.shadow, &silhouette, SILHOUETTE_KEY);
        let mask = |size: TileSize| shape::shadow_mask_frame(size).to_cg();
        let mask_position = resize.point("position", |size| mask(size).origin);
        add_under(&self.shadow_mask, &mask_position, MASK_MOVE_KEY);
        let mask_bounds = resize.rect("bounds", |size| CGRect::new(origin, mask(size).size));
        add_under(&self.shadow_mask, &mask_bounds, MASK_SIZE_KEY);
        let mask_path = resize.path("path", |size| {
            CFRetained::from(&*shadow_mask_path(size, corner(size)))
        });
        add_under(&self.shadow_mask, &mask_path, MASK_PATH_KEY);
        if resize.changes(corner) {
            add_under(
                &self.hairline,
                &resize.number("cornerRadius", corner),
                CORNER_KEY,
            );
        } else {
            remove_under(&self.hairline, &[CORNER_KEY]);
        }
        let outline = resize.path("path", |size| outline_path(size, corner(size)));
        add_under(&self.outline, &outline, OUTLINE_KEY);
        self.bands.install_resize(resize, |size: TileSize| self.band_corner_at(size));
    }

    /// Takes away every animation `install_resize` put on the decorations.
    pub fn remove_resize(&self) {
        remove_under(&self.shadow, &[SILHOUETTE_KEY]);
        remove_under(&self.shadow_mask, &[MASK_MOVE_KEY, MASK_SIZE_KEY, MASK_PATH_KEY]);
        remove_under(&self.hairline, &[CORNER_KEY]);
        remove_under(&self.outline, &[OUTLINE_KEY]);
        self.bands.remove_resize();
    }
}

/// The bands a window wears outside it, laid alike on a tile and at rest: the ring, and the border
/// in front of it, each a `Band` under one parent, in that order.
pub struct Bands {
    pub ring: Band,
    pub border: Band,
}

impl Bands {
    pub fn new(parent: &CALayer) -> Bands { Bands { ring: Band::new(parent), border: Band::new(parent) } }

    /// The widths `decoration`'s bands are laid at on a display of `scale` (`shape::widths`).
    pub fn wear(&self, decoration: &Decoration, scale: f64) {
        let widths = shape::widths(decoration, scale);
        self.ring.set_width(widths.ring);
        self.border.set_width(widths.border);
    }

    pub fn place(&self, size: TileSize, corner: f64) {
        self.ring.place(size, corner);
        self.border.place(size, corner);
    }

    /// Shows each band in the colour `style` gives it, or hides it.
    pub fn paint(&self, style: &Style) {
        self.ring.paint(style.ring);
        self.border.paint(style.border);
    }

    fn install_resize(&self, resize: &impl Resize, corner: impl Fn(TileSize) -> f64 + Copy) {
        self.ring.install_resize(resize, corner);
        self.border.install_resize(resize, corner);
    }

    fn remove_resize(&self) {
        self.ring.remove_resize();
        self.border.remove_resize();
    }
}

/// A band drawn outside a window, as the ring and the border are: a `CALayer` border on the
/// continuous curve, its inner contour the window's own corner (`shape::band`), hanging from the
/// window's top-left in its parent's points. On a tile and at rest alike, so the two draw the same
/// pixels.
pub struct Band {
    pub layer: Retained<CALayer>,
    /// In points, whole device pixels of the display it is drawn on; 0 draws nothing.
    width: Cell<f64>,
}

impl Band {
    /// A band under `parent`, drawn nowhere until it is given a width and a colour.
    pub fn new(parent: &CALayer) -> Band {
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        round_continuous(&layer);
        layer.setHidden(true);
        parent.addSublayer(&layer);
        Band { layer, width: Cell::new(0.0) }
    }

    pub fn set_width(&self, width: f64) {
        self.width.set(width);
        self.layer.setBorderWidth(width);
    }

    /// Lays the band around a window of `size` at `corner`, the window at its parent's origin.
    pub fn place(&self, size: TileSize, corner: f64) {
        let (rect, outer) = shape::band(size, corner, self.width.get());
        self.layer.setFrame(rect.to_cg());
        self.layer.setCornerRadius(outer);
    }

    /// Shows the band in `colour`, or hides it with none.
    pub fn paint(&self, colour: Option<Colour>) {
        let shown = colour.filter(|_| self.width.get() > 0.0);
        if let Some(Colour { r, g, b, a }) = shown {
            self.layer.setBorderColor(Some(&CGColor::new_srgb(r.into(), g.into(), b.into(), a.into())));
        }
        self.layer.setHidden(shown.is_none());
    }

    /// Carries the band through `resize`: its bounds at every size, and its corner while it changes.
    /// Its position, the window's corner less its width, holds.
    fn install_resize(&self, resize: &impl Resize, corner: impl Fn(TileSize) -> f64) {
        let width = self.width.get();
        let band = |size: TileSize| shape::band(size, corner(size), width);
        let bounds = resize.rect("bounds", |size| CGRect::new(CGPoint::new(0.0, 0.0), band(size).0.size.to_cg()));
        add_under(&self.layer, &bounds, BAND_SIZE_KEY);
        let outer = |size: TileSize| band(size).1;
        if resize.changes(outer) {
            add_under(&self.layer, &resize.number("cornerRadius", outer), BAND_CORNER_KEY);
        } else {
            remove_under(&self.layer, &[BAND_CORNER_KEY]);
        }
    }

    fn remove_resize(&self) { remove_under(&self.layer, &[BAND_SIZE_KEY, BAND_CORNER_KEY]); }
}

/// Rounds `layer`'s corner on the window's own curve, continuous, the one every path here follows.
pub fn round_continuous(layer: &CALayer) {
    // SAFETY: Core Animation's constant string, alive for the process.
    layer.setCornerCurve(unsafe { kCACornerCurveContinuous });
}

/// Adds `rect` rounded at `corner` to `path`.
fn add_rounded(path: &CGMutablePath, rect: TileRect, corner: f64) {
    let null = std::ptr::null();
    // SAFETY: a null transform means each point is added as given, to a path the caller owns.
    unsafe {
        for segment in shape::rounded_rect(rect, corner) {
            match segment {
                Segment::Move(to) => CGMutablePath::move_to_point(Some(path), null, to.x, to.y),
                Segment::Line(to) => CGMutablePath::add_line_to_point(Some(path), null, to.x, to.y),
                Segment::Cubic(a, b, to) => CGMutablePath::add_curve_to_point(
                    Some(path),
                    null,
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    to.x,
                    to.y,
                ),
                Segment::Close => CGMutablePath::close_subpath(Some(path)),
            }
        }
    }
}

fn rounded_path(rect: TileRect, corner: f64) -> CFRetained<CGPath> {
    let path = CGMutablePath::new();
    add_rounded(&path, rect, corner);
    CFRetained::from(&*path)
}

/// The outline's path around a window of `size` with corners of `corner`.
fn outline_path(size: TileSize, corner: f64) -> CFRetained<CGPath> {
    let (rect, radius) = shape::outline(size, corner);
    rounded_path(rect, radius)
}

/// The window's silhouette with corners of `corner`, the shadow's shape, as `shape::silhouette`
/// has it.
pub fn silhouette_path(size: TileSize, corner: f64) -> CFRetained<CGPath> {
    let (rect, radius) = shape::silhouette(size, corner);
    rounded_path(rect, radius)
}

/// The shadow mask's path, the whole mask minus the window's shape with corners of `corner`, wound
/// even-odd so the inside is the hole. Always one rect plus one rounded rect of the same segments,
/// so two mask paths interpolate.
pub fn shadow_mask_path(size: TileSize, corner: f64) -> CFRetained<CGMutablePath> {
    let (whole, hole, radius) = shape::shadow_mask(size, corner);
    // The mask layer's own points start at the mask's corner, one reach out from the tile's.
    let into_mask = Vector::new(SHADOW_REACH, SHADOW_REACH);
    let path = CGMutablePath::new();
    // SAFETY: a null transform means the rect is added as given, to a path we just created.
    unsafe {
        CGMutablePath::add_rect(Some(&path), std::ptr::null(), whole.translate(into_mask).to_cg());
    }
    add_rounded(&path, hole.translate(into_mask), radius);
    path
}

#[cfg(test)]
mod tests {
    use objc2_quartz_core::{CABasicAnimation, kCACornerCurveCircular};

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn parent(layer: &CALayer) -> Option<*const CALayer> {
        layer.superlayer().map(|parent| Retained::as_ptr(&parent))
    }

    /// A resize from one size to another: every animation is handed over bare, under its key path,
    /// and a value changes when it differs at the two sizes.
    struct Between(TileSize, TileSize);

    fn bare(key_path: &str) -> Retained<CAPropertyAnimation> {
        Retained::into_super(CABasicAnimation::animationWithKeyPath(Some(
            &NSString::from_str(key_path),
        )))
    }

    impl Resize for Between {
        fn point(
            &self,
            key_path: &str,
            _: impl Fn(TileSize) -> CGPoint,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }
        fn rect(
            &self,
            key_path: &str,
            _: impl Fn(TileSize) -> CGRect,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }
        fn number(
            &self,
            key_path: &str,
            _: impl Fn(TileSize) -> f64,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }
        fn path(
            &self,
            key_path: &str,
            _: impl Fn(TileSize) -> CFRetained<CGPath>,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }
        fn changes(&self, f: impl Fn(TileSize) -> f64) -> bool {
            f(self.0) != f(self.1)
        }
    }

    /// Each animation on `layer` as its key and the key path it animates, sorted by key.
    fn installed(layer: &CALayer) -> Vec<(String, String)> {
        let mut keys: Vec<(String, String)> = layer
            .animationKeys()
            .map(|keys| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|key| {
                // SAFETY: a read of the layer's own animation.
                let animation = unsafe { layer.animationForKey(&NSString::from_str(&key)) }
                    .expect("an animation under the key")
                    .downcast::<CAPropertyAnimation>()
                    .expect("a property animation");
                let path = animation.keyPath().map(|path| path.to_string()).unwrap_or_default();
                (key, path)
            })
            .collect();
        keys.sort();
        keys
    }

    fn pairs<const N: usize>(pairs: [(&str, &str); N]) -> Vec<(String, String)> {
        pairs.iter().map(|(key, path)| (key.to_string(), path.to_string())).collect()
    }

    #[test]
    fn new_layers_hang_the_outline_under_the_hairline_and_mask_the_shadow() {
        let container = CALayer::layer();
        let layers = DecorationLayers::new(&container);
        let container = Some(&*container as *const CALayer);
        assert_eq!(parent(&layers.shadow), container);
        assert_eq!(parent(&layers.hairline), container);
        assert_eq!(
            parent(&layers.outline),
            Some(&*layers.hairline as *const CALayer)
        );
        let mask = layers.shadow.mask().expect("a mask");
        assert!(std::ptr::eq(&*mask, &**layers.shadow_mask as &CALayer));
        // SAFETY: Core Animation's own fill-rule constant.
        assert_eq!(*layers.shadow_mask.fillRule(), *unsafe { kCAFillRuleEvenOdd });
        assert!(!layers.shadow.masksToBounds());
        assert!(!layers.hairline.masksToBounds());
    }

    #[test]
    fn new_layers_hang_from_their_corner_and_stroke_the_outline_dark_and_unfilled() {
        let layers = DecorationLayers::new(&CALayer::layer());
        let corner = CGPoint::new(0.0, 0.0);
        assert_eq!(layers.shadow.anchorPoint(), corner);
        assert_eq!(layers.shadow_mask.anchorPoint(), corner);
        assert_eq!(layers.hairline.anchorPoint(), corner);
        assert_eq!(layers.outline.anchorPoint(), corner);
        assert_eq!(layers.hairline.borderWidth(), LIGHT_PT);
        assert!(layers.outline.fillColor().is_none());
        assert!(CGColor::equal_to_color(
            layers.outline.strokeColor().as_deref(),
            Some(&CGColor::new_generic_gray(0.0, OUTLINE_ALPHA))
        ));
        assert_eq!(layers.outline.lineWidth(), OUTLINE_PT);
    }

    #[test]
    fn a_placed_hairline_rounds_to_its_windows_radius_and_clamps() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(layers.hairline.borderWidth(), LIGHT_PT);
        assert_eq!(layers.hairline.cornerRadius(), 16.0);
        assert_eq!(
            CGPath::bounding_box(layers.outline.path().as_deref()),
            rect(-0.25, -0.25, 859.5, 1081.5)
        );
        layers.place(TileSize::new(12.0, 400.0));
        assert_eq!(
            layers.hairline.cornerRadius(),
            6.0,
            "clamped to half the shorter side"
        );
    }

    /// The mask's path is in its own points, from the mask's corner one reach out from the tile's.
    #[test]
    fn a_resized_tile_reshapes_its_shadow_and_a_moved_one_keeps_it() {
        let layers = DecorationLayers::new(&CALayer::layer());
        let shaped = |layers: &DecorationLayers| {
            (
                CGPath::bounding_box(layers.shadow.shadowPath().as_deref()),
                layers.shadow_mask.frame(),
                CGPath::bounding_box(layers.shadow_mask.path().as_deref()),
            )
        };
        let placed = |size: TileSize| {
            let mask = shape::shadow_mask_frame(size).to_cg();
            (
                CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()),
                mask,
                CGRect::new(CGPoint::new(0.0, 0.0), mask.size),
            )
        };
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(shaped(&layers), placed(TileSize::new(859.0, 1081.0)));
        let resized = TileSize::new(400.0, 1081.0);
        layers.place(resized);
        assert_eq!(shaped(&layers), placed(resized));
        let silhouette = layers.shadow.shadowPath().expect("a silhouette");
        let mask = layers.shadow_mask.path().expect("a mask path");
        layers.place(resized);
        assert!(same(layers.shadow.shadowPath(), &silhouette), "moved, not reshaped");
        assert!(same(layers.shadow_mask.path(), &mask), "moved, not reshaped");
    }

    /// Whether `path` is the very path object `was`, not merely an equal one.
    fn same(path: Option<Retained<CGPath>>, was: &CGPath) -> bool {
        path.is_some_and(|path| std::ptr::eq(&*path, was))
    }

    /// Whether the caster's silhouette and its mask's path are the ones of `size` at `corner`,
    /// compared whole, since their bounds cannot see a radius.
    fn shaped_at(layers: &DecorationLayers, size: TileSize, corner: f64) -> bool {
        CGPath::equal_to_path(
            layers.shadow.shadowPath().as_deref(),
            Some(&silhouette_path(size, corner)),
        ) && CGPath::equal_to_path(
            layers.shadow_mask.path().as_deref(),
            Some(&shadow_mask_path(size, corner)),
        )
    }

    #[test]
    fn the_shadow_and_its_mask_round_to_the_tiles_corner_as_the_hairline_does() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(26.0);
        let size = TileSize::new(859.0, 1081.0);
        layers.place(size);
        assert!(shaped_at(&layers, size, 26.0));
        assert!(!shaped_at(&layers, size, 16.0), "another corner differs at the same bounds");
        assert_eq!(layers.hairline.cornerRadius(), 26.0);
    }

    #[test]
    fn a_pooled_tile_dressed_for_a_window_of_another_radius_rebuilds_its_shadow_at_the_same_size() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        let size = TileSize::new(859.0, 1081.0);
        layers.radius = Some(16.0);
        layers.place(size);
        assert!(shaped_at(&layers, size, 16.0));
        layers.radius = Some(26.0);
        layers.place(size);
        assert!(shaped_at(&layers, size, 26.0));
        let silhouette = layers.shadow.shadowPath().expect("a silhouette");
        layers.place(size);
        assert!(same(layers.shadow.shadowPath(), &silhouette), "not rebuilt");
    }

    #[test]
    fn the_hairlines_white_and_the_shadows_depth_follow_focus() {
        let layers = DecorationLayers::new(&CALayer::layer());
        let alpha =
            |layers: &DecorationLayers| CGColor::alpha(layers.hairline.borderColor().as_deref());
        for focused in [true, false] {
            let style = Style::of(focused, false, &Decoration::default());
            layers.show(style);
            assert_eq!(alpha(&layers), style.light_alpha);
            assert_eq!(layers.shadow.shadowOpacity(), style.shadow.opacity);
            assert_eq!(layers.shadow.shadowRadius(), style.shadow.radius);
            assert_eq!(
                layers.shadow.shadowOffset(),
                CGSize::new(0.0, style.shadow.offset_y)
            );
        }
    }

    #[test]
    fn the_outline_is_drawn_at_the_displays_scale() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.wear(&Decoration::default(), 2.0);
        assert_eq!(layers.outline.contentsScale(), 2.0);
    }

    const RING: crate::decorations::domain::style::Band = crate::decorations::domain::style::Band {
        width: 4.0,
        active: Colour { r: 0.5, g: 0.78, b: 1.0, a: 1.0 },
        inactive: Colour { r: 0.31, g: 0.31, b: 0.31, a: 1.0 },
    };

    const BORDER: crate::decorations::domain::style::Band = crate::decorations::domain::style::Band {
        width: 4.0,
        active: Colour { r: 1.0, g: 0.78, b: 0.5, a: 1.0 },
        inactive: Colour { r: 0.31, g: 0.31, b: 0.31, a: 1.0 },
    };

    fn ringed() -> Decoration { Decoration { ring: Some(RING), ..Decoration::default() } }

    fn srgb(c: Colour) -> objc2_core_foundation::CFRetained<CGColor> {
        CGColor::new_srgb(c.r.into(), c.g.into(), c.b.into(), c.a.into())
    }

    fn painted(layer: &CALayer, c: Colour) -> bool {
        !layer.isHidden() && CGColor::equal_to_color(layer.borderColor().as_deref(), Some(&srgb(c)))
    }

    /// The hairline's children, first drawn first.
    fn children(layers: &DecorationLayers) -> Vec<*const CALayer> {
        // SAFETY: a read of the layer's own sublayers, which nothing else mutates.
        let sublayers = unsafe { layers.hairline.sublayers() }.expect("the outline and the bands");
        sublayers.iter().map(|layer| Retained::as_ptr(&layer)).collect()
    }

    #[test]
    fn the_focus_ring_is_in_front_of_the_hairline_and_the_outline() {
        let layers = DecorationLayers::new(&CALayer::layer());
        assert_eq!(parent(&layers.bands.ring.layer), Some(&*layers.hairline as *const CALayer));
        let outline: &CALayer = &layers.outline;
        assert_eq!(children(&layers)[..2], [outline as *const CALayer, &*layers.bands.ring.layer as *const CALayer], "drawn after the outline");
        layers.stack(0.0);
        assert!(layers.hairline.zPosition() > 0.0, "and with the hairline, in front of the picture");
    }

    #[test]
    fn the_border_is_drawn_in_front_of_the_ring() {
        let layers = DecorationLayers::new(&CALayer::layer());
        assert_eq!(parent(&layers.bands.border.layer), Some(&*layers.hairline as *const CALayer));
        let order = children(&layers);
        assert_eq!(order.len(), 3);
        assert_eq!(order[2], &*layers.bands.border.layer as *const CALayer, "after the outline and the ring");
    }

    #[test]
    fn the_border_is_worn_in_the_paint_focus_gives_it_with_the_ring_outside_it() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        let both = Decoration { border: Some(BORDER), ..ringed() };
        layers.wear(&both, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        let (ring, border) = (&layers.bands.ring.layer, &layers.bands.border.layer);
        assert_eq!(border.frame(), rect(-4.0, -4.0, 867.0, 1089.0));
        assert_eq!((border.borderWidth(), border.cornerRadius()), (4.0, 20.0));
        assert_eq!(ring.frame(), rect(-8.0, -8.0, 875.0, 1097.0), "out past the border");
        assert_eq!((ring.borderWidth(), ring.cornerRadius()), (8.0, 24.0), "and in under it to the window");
        layers.show(Style::of(true, true, &both));
        assert!(painted(border, BORDER.active) && painted(ring, RING.active));
        layers.show(Style::of(false, false, &both));
        assert!(painted(border, BORDER.inactive), "worn without the ring");
        assert!(ring.isHidden());
    }

    #[test]
    fn the_ring_is_shown_only_in_the_paint_its_style_has() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.wear(&ringed(), 2.0);
        layers.show(Style::of(true, true, &ringed()));
        let ring = layers.bands.ring.layer.clone();
        assert!(painted(&ring, RING.active));
        assert!(layers.bands.border.layer.isHidden(), "borders off");
        layers.show(Style::of(false, true, &ringed()));
        assert!(painted(&ring, RING.inactive));
        layers.show(Style::of(true, false, &ringed()));
        assert!(ring.isHidden(), "not its workspace's active window");
        layers.wear(&Decoration::default(), 2.0);
        layers.show(Style::of(true, true, &Decoration::default()));
        assert!(ring.isHidden(), "rings off");
    }

    #[test]
    fn every_band_rounds_on_the_continuous_curve_at_its_outset() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        layers.wear(&Decoration { ring: Some(crate::decorations::domain::style::Band { width: 1.3, ..RING }), ..Decoration::default() }, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        let ring = &layers.bands.ring.layer;
        assert_eq!(ring.borderWidth(), 1.5, "whole device pixels");
        assert_eq!(ring.frame(), rect(-1.5, -1.5, 862.0, 1084.0));
        assert_eq!(ring.cornerRadius(), 17.5);
        // SAFETY: Core Animation's constant string.
        assert_eq!(&*ring.cornerCurve(), unsafe { kCACornerCurveContinuous });
        layers.place(TileSize::new(12.0, 400.0));
        assert_eq!(ring.cornerRadius(), 7.5, "the window's clamped corner, out by the width");
    }

    /// A window rule's corner is the bands' alone: the hairline, its outline and the shadow are what
    /// macOS draws at rest, at the window's own corner, and keep it.
    #[test]
    fn a_rule_radius_rounds_the_bands_and_leaves_the_hairline_at_the_windows_corner() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        let border = crate::decorations::domain::style::Band { width: 4.0, ..RING };
        layers.wear(&Decoration { ring: Some(RING), border: Some(border), radius: Some(8.0) }, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(layers.hairline.cornerRadius(), 16.0, "the window server's");
        assert_eq!(layers.bands.border.layer.cornerRadius(), 12.0, "the rule's, out by the border");
        layers.wear(&Decoration { ring: Some(RING), border: Some(border), radius: None }, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(layers.bands.border.layer.cornerRadius(), 20.0, "a pooled tile worn with no rule's: the server's");
    }

    #[test]
    fn taking_every_animation_away_clears_every_decoration_layer() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.install_resize(&Between(
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        for layer in layers.frame_layers() {
            add_under(layer, &bare("position"), "rini.tile.move");
        }
        layers.remove_animations();
        for layer in [
            &*layers.shadow,
            &layers.shadow_mask,
            &layers.hairline,
            &layers.outline,
            &layers.bands.ring.layer,
            &layers.bands.border.layer,
        ] {
            assert!(installed(layer).is_empty());
        }
    }

    /// An entrance grows from no width, so its corner changes; a resize between two wide sizes keeps
    /// the corner, and takes away the one an earlier resize animated.
    #[test]
    fn a_resize_installs_each_decoration_under_its_own_key() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.install_resize(&Between(
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        assert_eq!(
            installed(&layers.shadow),
            pairs([("rini.tile.shadow.path", "shadowPath")])
        );
        assert_eq!(
            installed(&layers.shadow_mask),
            pairs([
                ("rini.tile.mask.move", "position"),
                ("rini.tile.mask.path", "path"),
                ("rini.tile.mask.size", "bounds"),
            ])
        );
        assert_eq!(
            installed(&layers.hairline),
            pairs([("rini.tile.hairline.corner", "cornerRadius")])
        );
        assert_eq!(
            installed(&layers.outline),
            pairs([("rini.tile.outline.path", "path")])
        );
        for band in [&layers.bands.ring.layer, &layers.bands.border.layer] {
            assert_eq!(
                installed(band),
                pairs([("rini.tile.band.corner", "cornerRadius"), ("rini.tile.band.size", "bounds")])
            );
        }

        layers.install_resize(&Between(
            TileSize::new(1200.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        assert!(installed(&layers.hairline).is_empty(), "the corner holds");
        assert_eq!(
            installed(&layers.outline),
            pairs([("rini.tile.outline.path", "path")])
        );
        for band in [&layers.bands.ring.layer, &layers.bands.border.layer] {
            assert_eq!(installed(band), pairs([("rini.tile.band.size", "bounds")]));
        }
    }

    /// Only the resize's own keys go: what the overlay installs on the frame layers stays.
    #[test]
    fn taking_a_resize_away_leaves_no_decoration_animation() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.install_resize(&Between(
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        for layer in layers.frame_layers() {
            add_under(layer, &bare("position"), "rini.tile.move");
        }
        layers.remove_resize();
        for layer in layers.frame_layers() {
            assert_eq!(installed(layer), pairs([("rini.tile.move", "position")]));
        }
        assert!(installed(&layers.shadow_mask).is_empty());
        assert!(installed(&layers.outline).is_empty());
        assert!(installed(&layers.bands.ring.layer).is_empty());
        assert!(installed(&layers.bands.border.layer).is_empty());
    }

    #[test]
    fn the_hairline_is_stacked_in_front_and_the_caster_behind() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.stack(-2.0);
        assert_eq!(layers.shadow.zPosition(), -2.0 + SHADOW_Z);
        assert_eq!(layers.hairline.zPosition(), -2.0 + HAIRLINE_Z);
    }

    /// Alpha of `layer` rendered at 2x into a bitmap of `size`, top row first.
    fn rendered(layer: &CALayer, size: TileSize) -> Vec<u8> {
        use objc2_core_graphics::{
            CGBitmapContextCreate, CGBitmapContextGetData, CGColorSpace, CGContext, CGImageAlphaInfo,
        };
        let (w, h) = ((size.width * 2.0) as usize, (size.height * 2.0) as usize);
        let space = CGColorSpace::new_device_rgb().unwrap();
        // SAFETY: a fresh context that owns its bytes.
        let ctx = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                w,
                h,
                8,
                w * 4,
                Some(&space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        CGContext::translate_ctm(Some(&ctx), 0.0, h as f64);
        CGContext::scale_ctm(Some(&ctx), 2.0, -2.0);
        layer.setGeometryFlipped(true);
        layer.renderInContext(&ctx);
        let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
        // SAFETY: the context owns `w * h * 4` bytes and outlives this copy.
        unsafe { std::slice::from_raw_parts(data, w * h * 4) }.chunks(4).map(|p| p[3]).collect()
    }

    /// Core Animation's own corner on a tile of `size`: a white layer clipped at `corner` on `curve`.
    fn clipped(size: TileSize, corner: f64, curve: &objc2_quartz_core::CALayerCornerCurve) -> Vec<u8> {
        let layer = CALayer::layer();
        layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        layer.setBackgroundColor(Some(&CGColor::new_generic_gray(1.0, 1.0)));
        layer.setCornerRadius(corner);
        layer.setCornerCurve(curve);
        layer.setMasksToBounds(true);
        rendered(&layer, size)
    }

    /// `path` filled white over a tile of `size`, from `origin` in the tile's points, by `rule`.
    fn filled(size: TileSize, path: &CGPath, origin: CGPoint, rule: Option<&NSString>) -> Vec<u8> {
        let tile = CALayer::layer();
        tile.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        let shape = CAShapeLayer::layer();
        shape.setFrame(CGRect::new(origin, size.to_cg()));
        shape.setFillColor(Some(&CGColor::new_generic_gray(1.0, 1.0)));
        if let Some(rule) = rule {
            shape.setFillRule(rule);
        }
        shape.setPath(Some(path));
        tile.addSublayer(&shape);
        rendered(&tile, size)
    }

    /// The most any pixel of `a` sits from `b`, in alpha levels.
    fn farthest(a: &[u8], b: &[u8]) -> u8 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
    }

    /// Core Animation's clip and Core Graphics' fill of one circular corner sit up to 64 alpha
    /// levels apart at an edge pixel; the silhouette and the mask's hole sit no farther than that
    /// from Core Animation's continuous corner, the one the hairline and a clipped picture are drawn
    /// with, down to a tile far narrower than the corner's reach.
    #[test]
    fn every_path_follows_core_animation_s_corner_at_every_size() {
        let at = CGPoint::new(0.0, 0.0);
        let wide = TileSize::new(860.0, 1079.0);
        // SAFETY: a null transform means the path is taken as given.
        let circle = unsafe { CGPath::with_rounded_rect(CGRect::new(at, wide.to_cg()), 26.0, 26.0, std::ptr::null()) };
        // SAFETY: Core Animation's constant strings.
        let floor = farthest(&clipped(wide, 26.0, unsafe { kCACornerCurveCircular }), &filled(wide, &circle, at, None));
        let into_tile = CGPoint::new(-SHADOW_REACH, -SHADOW_REACH);
        for (w, h, corner) in [
            (860.0, 1079.0, 26.0),
            (1148.0, 1079.0, 16.0),
            (80.0, 400.0, 26.0),
            (60.0, 400.0, 26.0),
            (52.0, 400.0, 26.0),
            (32.0, 400.0, 16.0),
            (12.0, 400.0, 6.0),
            (1.0, 400.0, 0.5),
        ] {
            let size = TileSize::new(w, h);
            // SAFETY: Core Animation's constant string.
            let theirs = clipped(size, corner, unsafe { kCACornerCurveContinuous });
            let silhouette = filled(size, &silhouette_path(size, corner), at, None);
            // SAFETY: Core Animation's own fill-rule constant.
            let mask = filled(size, &shadow_mask_path(size, corner), into_tile, Some(unsafe { kCAFillRuleEvenOdd }));
            let hole: Vec<u8> = mask.iter().map(|alpha| 255 - alpha).collect();
            assert!(farthest(&theirs, &silhouette) <= floor, "silhouette {w}x{h} at {corner}: {} past {floor}", farthest(&theirs, &silhouette));
            assert!(farthest(&theirs, &hole) <= floor, "hole {w}x{h} at {corner}: {} past {floor}", farthest(&theirs, &hole));
        }
    }

    #[test]
    fn the_hairline_rounds_on_the_continuous_curve_at_every_size() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(26.0);
        for size in [TileSize::new(860.0, 1079.0), TileSize::new(52.0, 400.0), TileSize::new(0.5, 400.0)] {
            layers.place(size);
            // SAFETY: Core Animation's constant string.
            assert_eq!(&*layers.hairline.cornerCurve(), unsafe { kCACornerCurveContinuous }, "{size:?}");
        }
    }
}
