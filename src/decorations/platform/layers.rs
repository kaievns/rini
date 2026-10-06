//! The layers a tile's decorations are drawn with: the caster behind the picture with its mask, and
//! the hairline in front with its outline. They ride the tile's frame beside its picture, and every
//! value they are set to or animated through comes from `domain`. See "The hairline and the shadow,
//! as drawn" in `src/decorations/docs/README.md`.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_foundation::NSString;
use objc2_quartz_core::{
    CAAnimation, CALayer, CAPropertyAnimation, CAShapeLayer, kCACornerCurveCircular,
    kCACornerCurveContinuous, kCAFillRuleEvenOdd,
};
use rini_geometry::space::{TileRect, TileSize, ToCg, Vector};

use crate::decorations::domain::shape::{self, Curve, SHADOW_REACH, Segment};
use crate::decorations::domain::stack::{HAIRLINE_Z, SHADOW_Z};
use crate::decorations::domain::style::{LIGHT_PT, OUTLINE_ALPHA, OUTLINE_PT, Style};

// What a resize installs on the decorations, by the layer it rides.
const SILHOUETTE_KEY: &str = "rini.tile.shadow.path";
const MASK_MOVE_KEY: &str = "rini.tile.mask.move";
const MASK_SIZE_KEY: &str = "rini.tile.mask.size";
const MASK_PATH_KEY: &str = "rini.tile.mask.path";
const CORNER_KEY: &str = "rini.tile.hairline.corner";
const OUTLINE_KEY: &str = "rini.tile.outline.path";

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
    /// The window's corner radius as the window server reports it, which every piece rounds to;
    /// `None` rounds at the fallback.
    pub radius: Option<f64>,
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
        let outline = CAShapeLayer::layer();
        outline.setAnchorPoint(CGPoint::new(0.0, 0.0));
        outline.setFillColor(None);
        outline.setStrokeColor(Some(&CGColor::new_generic_gray(0.0, OUTLINE_ALPHA)));
        outline.setLineWidth(OUTLINE_PT);
        hairline.addSublayer(&outline);
        container.addSublayer(&hairline);

        DecorationLayers {
            shadow,
            shadow_mask,
            hairline,
            outline,
            radius: None,
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
        self.hairline.setCornerCurve(self.curve_at(size).as_layer());
        self.outline.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        self.outline.setPath(Some(&outline_path(size, corner)));
    }

    /// The corner a tile of `size` rounds to, every piece of it: its window's radius, clamped to the
    /// tile.
    pub fn corner_at(&self, size: TileSize) -> f64 {
        shape::radius(self.radius, size)
    }

    /// The curve a tile of `size` rounds its corner on, every piece of it.
    pub fn curve_at(&self, size: TileSize) -> Curve { shape::curve(size, self.corner_at(size)) }

    /// Paints the hairline's white and the shadow's depth as `style` has them.
    pub fn show(&self, style: Style) {
        self.hairline
            .setBorderColor(Some(&CGColor::new_generic_gray(1.0, style.light_alpha)));
        let shadow = style.shadow;
        self.shadow.setShadowOpacity(shadow.opacity);
        self.shadow.setShadowRadius(shadow.radius);
        self.shadow.setShadowOffset(CGSize::new(0.0, shadow.offset_y));
    }

    pub fn set_scale(&self, scale: f64) {
        self.outline.setContentsScale(scale);
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.shadow.setHidden(hidden);
        self.hairline.setHidden(hidden);
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
    }

    /// Takes away every animation `install_resize` put on the decorations.
    pub fn remove_resize(&self) {
        remove_under(&self.shadow, &[SILHOUETTE_KEY]);
        remove_under(&self.shadow_mask, &[MASK_MOVE_KEY, MASK_SIZE_KEY, MASK_PATH_KEY]);
        remove_under(&self.hairline, &[CORNER_KEY]);
        remove_under(&self.outline, &[OUTLINE_KEY]);
    }
}

/// The layer's own corner curve for `curve`.
pub trait AsLayerCurve {
    fn as_layer(self) -> &'static objc2_quartz_core::CALayerCornerCurve;
}

impl AsLayerCurve for Curve {
    fn as_layer(self) -> &'static objc2_quartz_core::CALayerCornerCurve {
        // SAFETY: Core Animation's constant strings, alive for the process.
        unsafe {
            match self {
                Curve::Circular => kCACornerCurveCircular,
                Curve::Continuous => kCACornerCurveContinuous,
            }
        }
    }
}

/// Adds `rect` rounded at `corner` on `curve` to `path`.
fn add_rounded(path: &CGMutablePath, rect: TileRect, corner: f64, curve: Curve) {
    let null = std::ptr::null();
    // SAFETY: a null transform means each point is added as given, to a path the caller owns.
    unsafe {
        for segment in shape::rounded_rect(rect, corner, curve) {
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

fn rounded_path(rect: TileRect, corner: f64, curve: Curve) -> CFRetained<CGPath> {
    let path = CGMutablePath::new();
    add_rounded(&path, rect, corner, curve);
    CFRetained::from(&*path)
}

/// The outline's path around a window of `size` with corners of `corner`, on the tile's curve.
fn outline_path(size: TileSize, corner: f64) -> CFRetained<CGPath> {
    let (rect, radius) = shape::outline(size, corner);
    rounded_path(rect, radius, shape::curve(size, corner))
}

/// The window's silhouette with corners of `corner`, the shadow's shape, floored as
/// `shape::silhouette` says.
pub fn silhouette_path(size: TileSize, corner: f64) -> CFRetained<CGPath> {
    let (rect, radius) = shape::silhouette(size, corner);
    rounded_path(rect, radius, shape::curve(size, corner))
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
    add_rounded(&path, hole.translate(into_mask), radius, shape::curve(size, corner));
    path
}

#[cfg(test)]
mod tests {
    use objc2_quartz_core::CABasicAnimation;

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
            let style = Style::of(focused);
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

    /// A companion's tile wears neither, and a pooled tile handed to a window wears both again.
    #[test]
    fn a_hidden_tile_hides_its_caster_and_its_hairline() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.set_hidden(true);
        assert!(layers.frame_layers().iter().all(|layer| layer.isHidden()));
        layers.set_hidden(false);
        assert!(layers.frame_layers().iter().all(|layer| !layer.isHidden()));
    }

    #[test]
    fn the_outline_is_drawn_at_the_displays_scale() {
        let layers = DecorationLayers::new(&CALayer::layer());
        layers.set_scale(2.0);
        assert_eq!(layers.outline.contentsScale(), 2.0);
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

        layers.install_resize(&Between(
            TileSize::new(1200.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        assert!(installed(&layers.hairline).is_empty(), "the corner holds");
        assert_eq!(
            installed(&layers.outline),
            pairs([("rini.tile.outline.path", "path")])
        );
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

    /// How many pixels of the silhouette's path sit more than half an alpha step from Core
    /// Animation's own corner on the curve the tile rounds on, the one the hairline and a clipped
    /// picture are drawn with.
    fn off_core_animation(size: TileSize, corner: f64) -> usize {
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg());
        let theirs = CALayer::layer();
        theirs.setFrame(frame);
        theirs.setBackgroundColor(Some(&CGColor::new_generic_gray(1.0, 1.0)));
        theirs.setCornerRadius(corner);
        theirs.setCornerCurve(shape::curve(size, corner).as_layer());
        theirs.setMasksToBounds(true);
        let ours = CAShapeLayer::layer();
        ours.setFrame(frame);
        ours.setFillColor(Some(&CGColor::new_generic_gray(1.0, 1.0)));
        ours.setPath(Some(&silhouette_path(size, corner)));
        let (a, b) = (rendered(&theirs, size), rendered(&ours, size));
        a.iter().zip(&b).filter(|(x, y)| x.abs_diff(**y) > 127).count()
    }

    #[test]
    fn every_path_follows_core_animation_s_corner_on_the_tile_s_curve() {
        for (w, h, corner) in [(860.0, 1079.0, 26.0), (1148.0, 1079.0, 16.0), (80.0, 400.0, 26.0), (60.0, 400.0, 26.0), (12.0, 400.0, 6.0)] {
            let size = TileSize::new(w, h);
            assert_eq!(off_core_animation(size, corner), 0, "{w}x{h} at {corner}");
        }
    }

    #[test]
    fn a_placed_tile_s_hairline_rounds_on_its_curve() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(26.0);
        layers.place(TileSize::new(860.0, 1079.0));
        assert_eq!(&*layers.hairline.cornerCurve(), unsafe { kCACornerCurveContinuous });
        layers.place(TileSize::new(60.0, 400.0));
        assert_eq!(&*layers.hairline.cornerCurve(), unsafe { kCACornerCurveCircular });
    }
}
