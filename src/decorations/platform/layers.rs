//! The layers a tile's decorations are drawn with: the caster behind the picture with its mask, and
//! the hairline in front with its outline, the focus ring and the border. They ride the tile's frame
//! beside its picture, and every value they are set to or animated through comes from `domain`. See
//! "The hairline and the shadow, as drawn" and "The focus ring and the border, as drawn" in
//! `src/decorations/docs/README.md`.

use std::cell::{Cell, RefCell};
use std::thread::LocalKey;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_foundation::{NSArray, NSNumber, NSString};
use objc2_quartz_core::{
    CAAnimation, CAGradientLayer, CALayer, CAPropertyAnimation, CAShapeLayer,
    kCACornerCurveContinuous, kCAFillRuleEvenOdd,
};
use rini_geometry::space::{FromCg, TileRect, TileSize, ToCg, Vector};

use crate::decorations::domain::paint::{Colour, Gradient, Paint};
use crate::decorations::domain::shape::{self, Fit, Lay, Laying, SHADOW_REACH, Segment, Widths};
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
const BAND_START_KEY: &str = "rini.tile.band.start";
const BAND_END_KEY: &str = "rini.tile.band.end";
const BAND_WIDTH_KEY: &str = "rini.tile.band.width";
const BAND_MOVE_KEY: &str = "rini.tile.band.move";

pub fn add_under(layer: &CALayer, animation: &CAAnimation, key: &str) {
    layer.addAnimation_forKey(animation, Some(&NSString::from_str(key)));
}

pub fn remove_under(layer: &CALayer, keys: &[&str]) {
    for key in keys {
        layer.removeAnimationForKey(&NSString::from_str(key));
    }
}

/// A tile's resize as Core Animation plays it: an animation of `key_path` through each value
/// `value` maps from the fits the tile passes through, on the tile's own timing. The overlay fills
/// it from the tile's track, each size laid as its decorations are (`DecorationLayers::fit_at`).
pub trait Resize {
    fn point(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CGPoint,
    ) -> Retained<CAPropertyAnimation>;
    fn rect(&self, key_path: &str, value: impl Fn(&Fit) -> CGRect)
    -> Retained<CAPropertyAnimation>;
    fn number(&self, key_path: &str, value: impl Fn(&Fit) -> f64) -> Retained<CAPropertyAnimation>;
    fn path(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CFRetained<CGPath>,
    ) -> Retained<CAPropertyAnimation>;
    /// Whether `f` of the fits the resize passes through differs between any two of them.
    fn changes(&self, f: impl Fn(&Fit) -> f64) -> bool;
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
    /// outline, so they are drawn in front of the picture, the hairline and the outline: each band
    /// its window wears, and no layer for one it does not.
    pub bands: Bands,
    /// The window's corner radius as the window server reports it, which every piece rounds to;
    /// `None` rounds at the fallback.
    pub radius: Option<f64>,
    /// What the bands were last worn for, whose rule's corner they round to ahead of `radius`.
    worn: Decoration,
    /// How wide the bands were last worn, in the display's device pixels.
    widths: Widths,
    /// How every piece is laid in place of the window's own (`own`), while one is given.
    laying: Option<Laying>,
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
        container.addSublayer(&hairline);
        let bands = Bands::new(&hairline);

        DecorationLayers {
            shadow,
            shadow_mask,
            hairline,
            outline,
            bands,
            radius: None,
            worn: Decoration::default(),
            widths: Widths { border: 0.0, ring: 0.0 },
            laying: None,
            built: Cell::new(None),
        }
    }

    /// The layers that ride the tile's frame: the caster and the hairline.
    pub fn frame_layers(&self) -> [&CALayer; 2] { [&self.shadow, &self.hairline] }

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
        self.bands.remove_animations();
    }

    /// Draws the decorations of a tile at depth `z`: the caster `SHADOW_Z` behind its picture, the
    /// hairline `HAIRLINE_Z` in front of it.
    pub fn stack(&self, z: f64) {
        self.shadow.setZPosition(z + SHADOW_Z);
        self.hairline.setZPosition(z + HAIRLINE_Z);
    }

    /// Shapes the decorations for a tile of `size`, laid at its frame as `fit_at` lays it: the
    /// shadow and its mask only when the size or the corner differs from the ones they were shaped
    /// for, since a tile can be dressed again at the same size for a radius that changed; the
    /// hairline every time.
    pub fn place(&self, size: TileSize) {
        let fit = self.fit_at(size);
        let corner = fit.corner;
        if self.built.get() != Some((size, corner)) {
            self.shadow.setShadowPath(Some(&silhouette_path(size, corner)));
            self.shadow_mask.setFrame(shape::shadow_mask_frame(size).to_cg());
            self.shadow_mask.setPath(Some(&shadow_mask_path(size, corner)));
            self.built.set(Some((size, corner)));
        }
        self.hairline.setCornerRadius(corner);
        self.outline.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        self.outline.setPath(Some(&outline_path(size, corner)));
        self.bands.place(&fit);
    }

    /// How its window lays it: every piece but the bands at the window's radius, the bands at a
    /// window rule's, else the window's, each on the window's edge at the width it was worn at.
    pub fn own(&self) -> Laying {
        let corner = self.radius.unwrap_or(shape::FALLBACK_RADIUS);
        let band_corner = self.worn.corner(|| self.radius).unwrap_or(shape::FALLBACK_RADIUS);
        Laying::on_edge(corner, band_corner, self.widths)
    }

    /// Laid as `laying` has it from the next `place` or resize on, or as its window lays it with
    /// none.
    pub fn lay(&mut self, laying: Option<Laying>) { self.laying = laying; }

    /// How a tile of `size` is laid: as it was given, else as its window lays it, clamped to
    /// `size`.
    pub fn fit_at(&self, size: TileSize) -> Fit {
        self.laying.unwrap_or_else(|| self.own()).fit(size)
    }

    /// The corner a tile of `size` rounds to, every piece of it but the bands.
    pub fn corner_at(&self, size: TileSize) -> f64 { self.fit_at(size).corner }

    /// Paints the hairline's white, the shadow's depth, the ring and the border as `style` has them.
    pub fn show(&self, style: Style) {
        self.light(style);
        self.bands.paint(&style);
    }

    /// Paints the hairline's white and the shadow's depth as `style` has them, the bands left as
    /// they are.
    pub fn light(&self, style: Style) {
        self.hairline
            .setBorderColor(Some(&CGColor::new_generic_gray(1.0, style.light_alpha)));
        let shadow = style.shadow;
        self.shadow.setShadowOpacity(shadow.opacity);
        self.shadow.setShadowRadius(shadow.radius);
        self.shadow.setShadowOffset(CGSize::new(0.0, shadow.offset_y));
    }

    /// Draws the outline at the display's `scale`, and the bands of `decoration` in whole device
    /// pixels of it, at its rule's corner; laid at the next `place`. Each band's layer is made as a
    /// decoration with it is worn, and taken away as one without it is (`Bands::wear`).
    pub fn wear(&mut self, decoration: &Decoration, scale: f64) {
        self.outline.setContentsScale(scale);
        self.worn = *decoration;
        self.widths = shape::widths(decoration, scale);
        self.bands.wear(decoration, scale);
    }

    /// Carries the decorations through `resize`, each piece as every fit it passes through lays
    /// it: the caster's silhouette, its mask's frame and path, the hairline's corner while it
    /// changes, the outline's path, and the bands. The frame layers' bounds ride the tile's own
    /// resize.
    pub fn install_resize(&self, resize: &impl Resize) {
        let origin = CGPoint::new(0.0, 0.0);
        let silhouette = resize.path("shadowPath", |fit| silhouette_path(fit.size, fit.corner));
        add_under(&self.shadow, &silhouette, SILHOUETTE_KEY);
        let mask = |fit: &Fit| shape::shadow_mask_frame(fit.size).to_cg();
        let mask_position = resize.point("position", |fit| mask(fit).origin);
        add_under(&self.shadow_mask, &mask_position, MASK_MOVE_KEY);
        let mask_bounds = resize.rect("bounds", |fit| CGRect::new(origin, mask(fit).size));
        add_under(&self.shadow_mask, &mask_bounds, MASK_SIZE_KEY);
        let mask_path = resize.path("path", |fit| {
            CFRetained::from(&*shadow_mask_path(fit.size, fit.corner))
        });
        add_under(&self.shadow_mask, &mask_path, MASK_PATH_KEY);
        if resize.changes(|fit| fit.corner) {
            add_under(
                &self.hairline,
                &resize.number("cornerRadius", |fit| fit.corner),
                CORNER_KEY,
            );
        } else {
            remove_under(&self.hairline, &[CORNER_KEY]);
        }
        let outline = resize.path("path", |fit| outline_path(fit.size, fit.corner));
        add_under(&self.outline, &outline, OUTLINE_KEY);
        self.bands.install_resize(resize);
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
/// in front of it, each a `Band` under one parent, in that order. Each is made as a decoration with
/// it is worn and taken off the parent as one without it is, so a window wearing the ring alone
/// carries one band's layer, and one wearing neither carries none.
pub struct Bands {
    parent: Retained<CALayer>,
    pub ring: Option<Band>,
    pub border: Option<Band>,
}

impl Bands {
    /// Under `parent`, wearing no band until one is worn.
    pub fn new(parent: &CALayer) -> Bands {
        Bands {
            parent: parent.retain(),
            ring: None,
            border: None,
        }
    }

    /// The bands `decoration` has, at the widths they are laid at on a display of `scale`
    /// (`shape::widths`): a ring made with the border on is put under it.
    pub fn wear(&mut self, decoration: &Decoration, scale: f64) {
        let widths = shape::widths(decoration, scale);
        match decoration.border {
            Some(_) => self
                .border
                .get_or_insert_with(|| Band::new(&self.parent, None))
                .set_width(widths.border),
            None => take_off(&mut self.border),
        }
        match decoration.ring {
            Some(_) => {
                let border = self.border.as_ref().map(|border| -> &CALayer { &border.layer });
                self.ring
                    .get_or_insert_with(|| Band::new(&self.parent, border))
                    .set_width(widths.ring);
            }
            None => take_off(&mut self.ring),
        }
    }

    fn each(&self) -> impl Iterator<Item = &Band> { self.ring.iter().chain(&self.border) }

    /// Each band laid around a window as `fit` lays it.
    pub fn place(&self, fit: &Fit) {
        if let Some(ring) = &self.ring {
            ring.place(fit.size, fit.ring);
        }
        if let Some(border) = &self.border {
            border.place(fit.size, fit.border);
        }
    }

    /// Shows each band in the paint `style` gives it, or hides it.
    pub fn paint(&self, style: &Style) {
        if let Some(ring) = &self.ring {
            ring.paint(style.ring);
        }
        if let Some(border) = &self.border {
            border.paint(style.border);
        }
    }

    /// Each band carried through `resize`, laid as each fit lays it.
    pub fn install_resize(&self, resize: &impl Resize) {
        if let Some(ring) = &self.ring {
            ring.install_resize(resize, |fit| fit.ring);
        }
        if let Some(border) = &self.border {
            border.install_resize(resize, |fit| fit.border);
        }
    }

    /// Takes every animation `install_resize` put on the bands.
    pub fn remove_resize(&self) {
        for band in self.each() {
            band.remove_resize();
        }
    }

    fn remove_animations(&self) {
        for band in self.each() {
            band.remove_animations();
        }
    }
}

/// Takes `band`'s layer off its parent, and the band with it.
fn take_off(band: &mut Option<Band>) {
    if let Some(band) = band.take() {
        band.layer.removeFromSuperlayer();
    }
}

/// A band drawn outside a window, as the ring and the border are, its inner contour the window's own
/// corner (`shape::band`), hanging from the window's top-left in its parent's points: in a colour, a
/// `CALayer` border on the continuous curve; in a gradient, the same layer's gradient masked to a
/// border of the same width and corner. On a tile and at rest alike, so the two draw the same pixels.
/// See "Gradients" in `src/decorations/docs/README.md`.
pub struct Band {
    pub layer: Retained<CAGradientLayer>,
    /// The band's own shape, black, which the layer is masked to while a gradient fills it.
    mask: Retained<CALayer>,
    /// In points, whole device pixels of the display it is drawn on.
    width: Cell<f64>,
    /// The gradient it is filled with, if one is; the mask is attached exactly while one is.
    gradient: Cell<Option<Gradient>>,
}

impl Band {
    /// A band under `parent`, below `sibling` if one is given and in front of the rest otherwise,
    /// drawn nowhere until it is given a width and a paint.
    fn new(parent: &CALayer, sibling: Option<&CALayer>) -> Band {
        let layer = CAGradientLayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        round_continuous(&layer);
        layer.setHidden(true);
        let mask = CALayer::layer();
        mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
        round_continuous(&mask);
        mask.setBorderColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));
        match sibling {
            Some(sibling) => parent.insertSublayer_below(&layer, Some(sibling)),
            None => parent.addSublayer(&layer),
        }
        Band {
            layer,
            mask,
            width: Cell::new(0.0),
            gradient: Cell::new(None),
        }
    }

    pub fn set_width(&self, width: f64) {
        self.width.set(width);
        self.layer.setBorderWidth(width);
        self.mask.setBorderWidth(width);
    }

    /// Lays the band around a window of `size` as `lay` has it, the window at its parent's origin.
    pub fn place(&self, size: TileSize, lay: Lay) {
        if self.width.get() != lay.width {
            self.set_width(lay.width);
        }
        let (rect, outer) = shape::band_at(size, lay);
        self.layer.setFrame(rect.to_cg());
        self.layer.setCornerRadius(outer);
        self.mask.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), rect.size.to_cg()));
        self.mask.setCornerRadius(outer);
        self.lay_axis(rect.size);
    }

    /// Shows the band in `paint`, or hides it with none. A mask taken off carries no animation, and
    /// one attached mid-resize takes the band's own (`mask_rides_resize`).
    pub fn paint(&self, paint: Option<Paint>) {
        match paint {
            Some(Paint::Solid(colour)) => {
                self.layer.setBorderColor(Some(&solid(colour)));
                // SAFETY: no colours draws no gradient, and no mask leaves the border whole.
                unsafe {
                    self.layer.setColors(None);
                    self.layer.setMask(None);
                }
                if self.gradient.take().is_some() {
                    remove_under(&self.mask, &[BAND_SIZE_KEY, BAND_CORNER_KEY, BAND_WIDTH_KEY]);
                }
            }
            Some(Paint::Gradient(gradient)) => {
                self.layer.setBorderColor(None);
                let (colours, locations) = drawn(&gradient);
                self.layer.setLocations(Some(&locations));
                // SAFETY: an array of `CGColor`s, which is what `colors` takes, and a mask layer the
                // band owns and hangs on no other.
                unsafe {
                    self.layer.setColors(Some(&colours));
                    self.layer.setMask(Some(&self.mask));
                }
                match self.gradient.get() {
                    None => self.mask_rides_resize(),
                    Some(was) if was.angle != gradient.angle => {
                        remove_under(&self.layer, &[BAND_START_KEY, BAND_END_KEY])
                    }
                    Some(_) => {}
                }
                self.gradient.set(Some(gradient));
                self.lay_axis(TileSize::from_cg(self.layer.bounds().size));
            }
            None => {}
        }
        self.layer.setHidden(paint.is_none());
    }

    /// The resize the band's layer plays, put on the mask just attached to it.
    fn mask_rides_resize(&self) {
        for key in [BAND_SIZE_KEY, BAND_CORNER_KEY, BAND_WIDTH_KEY] {
            let key = NSString::from_str(key);
            // SAFETY: a read of the band's own animation.
            if let Some(animation) = unsafe { self.layer.animationForKey(&key) } {
                self.mask.addAnimation_forKey(&animation, Some(&key));
            }
        }
    }

    /// Lays the gradient's axis over a band of `size`, if a gradient fills it.
    fn lay_axis(&self, size: TileSize) {
        if let Some(gradient) = self.gradient.get() {
            let axis = gradient.axis(size);
            self.layer.setStartPoint(axis.start.to_cg());
            self.layer.setEndPoint(axis.end.to_cg());
        }
    }

    /// Carries the band through `resize`, laid as `lay` takes it from each fit: its bounds at every
    /// fit, its corner, its width and its position while each changes, the mask's bounds, corner
    /// and width with them while a gradient has it attached, and a gradient's axis while it moves
    /// with the size. A gradient painted mid-resize at another angle keeps the axis of the size the
    /// band is laid at until the next.
    fn install_resize(&self, resize: &impl Resize, lay: impl Fn(&Fit) -> Lay) {
        let band = |fit: &Fit| shape::band_at(fit.size, lay(fit));
        let bounds = resize.rect("bounds", |fit| {
            CGRect::new(CGPoint::new(0.0, 0.0), band(fit).0.size.to_cg())
        });
        let outer = |fit: &Fit| band(fit).1;
        let rounds = resize.changes(outer).then(|| resize.number("cornerRadius", outer));
        let width = |fit: &Fit| lay(fit).width;
        let widens = resize.changes(width).then(|| resize.number("borderWidth", width));
        let masked = self.gradient.get().is_some().then_some(&*self.mask);
        let layers: [Option<&CALayer>; 2] = [Some(&self.layer), masked];
        for layer in layers.into_iter().flatten() {
            add_under(layer, &bounds, BAND_SIZE_KEY);
            for (animation, key) in [(&rounds, BAND_CORNER_KEY), (&widens, BAND_WIDTH_KEY)] {
                match animation {
                    Some(animation) => add_under(layer, animation, key),
                    None => remove_under(layer, &[key]),
                }
            }
        }
        let (x, y) = (
            |fit: &Fit| band(fit).0.origin.x,
            |fit: &Fit| band(fit).0.origin.y,
        );
        if resize.changes(x) || resize.changes(y) {
            let position = resize.point("position", |fit| band(fit).0.origin.to_cg());
            add_under(&self.layer, &position, BAND_MOVE_KEY);
        } else {
            remove_under(&self.layer, &[BAND_MOVE_KEY]);
        }
        match self.gradient.get().filter(Gradient::follows_size) {
            Some(gradient) => {
                let axis = |fit: &Fit| gradient.axis(band(fit).0.size);
                add_under(
                    &self.layer,
                    &resize.point("startPoint", |fit| axis(fit).start.to_cg()),
                    BAND_START_KEY,
                );
                add_under(
                    &self.layer,
                    &resize.point("endPoint", |fit| axis(fit).end.to_cg()),
                    BAND_END_KEY,
                );
            }
            None => remove_under(&self.layer, &[BAND_START_KEY, BAND_END_KEY]),
        }
    }

    fn remove_resize(&self) {
        remove_under(&self.layer, &[
            BAND_SIZE_KEY,
            BAND_CORNER_KEY,
            BAND_WIDTH_KEY,
            BAND_MOVE_KEY,
            BAND_START_KEY,
            BAND_END_KEY,
        ]);
        remove_under(&self.mask, &[BAND_SIZE_KEY, BAND_CORNER_KEY, BAND_WIDTH_KEY]);
    }

    /// Takes every animation off the band and its mask.
    fn remove_animations(&self) {
        self.layer.removeAllAnimations();
        self.mask.removeAllAnimations();
    }
}

fn srgb(Colour { r, g, b, a }: Colour) -> CFRetained<CGColor> {
    CGColor::new_srgb(r.into(), g.into(), b.into(), a.into())
}

/// How many colours' `CGColor`s, and how many gradients' stops, are kept, so a band painted again,
/// or another band painted alike, is not given them afresh: placing a gradient's stops takes
/// milliseconds.
const KEPT: usize = 16;

type Stops = (Retained<NSArray>, Retained<NSArray<NSNumber>>);

thread_local! {
    static SOLIDS: RefCell<Vec<(Colour, CFRetained<CGColor>)>> = const { RefCell::new(Vec::new()) };
    static DRAWN: RefCell<Vec<(Gradient, Stops)>> = const { RefCell::new(Vec::new()) };
}

/// What `make` gives for `key`, made once while it is among the last `KEPT` asked of `cache`.
fn kept<K: PartialEq + Copy + 'static, V: Clone + 'static>(
    cache: &'static LocalKey<RefCell<Vec<(K, V)>>>,
    key: K,
    make: impl FnOnce(K) -> V,
) -> V {
    cache.with_borrow_mut(|kept| {
        if let Some((_, value)) = kept.iter().find(|(kept, _)| *kept == key) {
            return value.clone();
        }
        let value = make(key);
        if kept.len() == KEPT {
            kept.remove(0);
        }
        kept.push((key, value.clone()));
        value
    })
}

/// `colour` as the `CGColor` a band's border is drawn in.
fn solid(colour: Colour) -> CFRetained<CGColor> { kept(&SOLIDS, colour, srgb) }

/// `gradient`'s stops as Core Animation takes them, its colours and their locations.
fn drawn(gradient: &Gradient) -> Stops {
    kept(&DRAWN, *gradient, |gradient| {
        let stops = gradient.stops();
        let colours: Vec<Retained<AnyObject>> =
            stops.iter().map(|stop| Retained::from(&**srgb(stop.colour))).collect();
        let locations: Vec<Retained<NSNumber>> =
            stops.iter().map(|stop| NSNumber::numberWithDouble(stop.location)).collect();
        (
            NSArray::from_retained_slice(&colours),
            NSArray::from_retained_slice(&locations),
        )
    })
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
    mask_path(size, corner, SHADOW_REACH)
}

/// `shadow_mask_path` reaching `reach` out.
pub fn mask_path(size: TileSize, corner: f64, reach: f64) -> CFRetained<CGMutablePath> {
    let (whole, hole, radius) = shape::mask(size, corner, reach);
    // The mask layer's own points start at the mask's corner, one reach out from the tile's.
    let into_mask = Vector::new(reach, reach);
    let path = CGMutablePath::new();
    // SAFETY: a null transform means the rect is added as given, to a path we just created.
    unsafe {
        CGMutablePath::add_rect(Some(&path), std::ptr::null(), whole.translate(into_mask).to_cg());
    }
    add_rounded(&path, hole.translate(into_mask), radius);
    path
}

/// Core Animation's process-wide lock, held by a test from its first line until it has read back
/// what it installed. A commit on any thread drops the animations of every layer outside a render
/// context, which a test's layers are, and every commit takes this lock. See docs/testing.md.
#[cfg(test)]
pub(crate) struct HeldCoreAnimation(std::marker::PhantomData<*const ()>);

#[cfg(test)]
thread_local! {
    static CORE_ANIMATION_HELD: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn hold_core_animation() -> HeldCoreAnimation {
    // SAFETY: released by the guard's `Drop`, on this thread, which the guard cannot leave.
    unsafe { objc2_quartz_core::CATransaction::lock() };
    CORE_ANIMATION_HELD.set(CORE_ANIMATION_HELD.get() + 1);
    HeldCoreAnimation(std::marker::PhantomData)
}

#[cfg(test)]
impl Drop for HeldCoreAnimation {
    fn drop(&mut self) {
        CORE_ANIMATION_HELD.set(CORE_ANIMATION_HELD.get() - 1);
        // SAFETY: taken by `hold_core_animation` on this thread.
        unsafe { objc2_quartz_core::CATransaction::unlock() };
    }
}

/// Called by every test helper that reads an animation back off a layer.
#[cfg(test)]
pub(crate) fn assert_core_animation_held() {
    assert!(
        CORE_ANIMATION_HELD.get() > 0,
        "a test that reads back an animation holds hold_core_animation() from its first line"
    );
}

#[cfg(test)]
mod tests {
    use objc2_quartz_core::{CABasicAnimation, kCACornerCurveCircular};

    use super::*;
    use crate::decorations::domain::style::{Ring, Urgency};

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn parent(layer: &CALayer) -> Option<*const CALayer> {
        layer.superlayer().map(|parent| Retained::as_ptr(&parent))
    }

    /// A resize from one fit to another: every animation is handed over bare, under its key path,
    /// and a value changes when it differs at the two fits.
    struct Between(Fit, Fit);

    /// A resize between two sizes, each laid as `layers` lays it.
    fn between(layers: &DecorationLayers, from: TileSize, to: TileSize) -> Between {
        Between(layers.fit_at(from), layers.fit_at(to))
    }

    fn bare(key_path: &str) -> Retained<CAPropertyAnimation> {
        Retained::into_super(CABasicAnimation::animationWithKeyPath(Some(
            &NSString::from_str(key_path),
        )))
    }

    impl Resize for Between {
        fn point(
            &self,
            key_path: &str,
            _: impl Fn(&Fit) -> CGPoint,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }

        fn rect(
            &self,
            key_path: &str,
            _: impl Fn(&Fit) -> CGRect,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }

        fn number(&self, key_path: &str, _: impl Fn(&Fit) -> f64) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }

        fn path(
            &self,
            key_path: &str,
            _: impl Fn(&Fit) -> CFRetained<CGPath>,
        ) -> Retained<CAPropertyAnimation> {
            bare(key_path)
        }

        fn changes(&self, f: impl Fn(&Fit) -> f64) -> bool { f(&self.0) != f(&self.1) }
    }

    /// Each animation on `layer` as its key and the key path it animates, sorted by key.
    fn installed(layer: &CALayer) -> Vec<(String, String)> {
        assert_core_animation_held();
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
        assert!(
            same(layers.shadow.shadowPath(), &silhouette),
            "moved, not reshaped"
        );
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
        assert!(
            !shaped_at(&layers, size, 16.0),
            "another corner differs at the same bounds"
        );
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
            let style = Style::of(focused, Ring::None, &Decoration::default());
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
        active: Paint::Solid(Colour {
            r: 0.5,
            g: 0.78,
            b: 1.0,
            a: 1.0,
        }),
        inactive: Paint::Solid(Colour {
            r: 0.31,
            g: 0.31,
            b: 0.31,
            a: 1.0,
        }),
        urgent: Paint::Solid(Colour {
            r: 0.61,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
    };

    const BORDER: crate::decorations::domain::style::Band =
        crate::decorations::domain::style::Band {
            width: 4.0,
            active: Paint::Solid(Colour {
                r: 1.0,
                g: 0.78,
                b: 0.5,
                a: 1.0,
            }),
            inactive: Paint::Solid(Colour {
                r: 0.31,
                g: 0.31,
                b: 0.31,
                a: 1.0,
            }),
            urgent: Paint::Solid(Colour {
                r: 0.61,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
        };

    fn ringed() -> Decoration {
        Decoration {
            ring: Some(RING),
            ..Decoration::default()
        }
    }

    fn both() -> Decoration {
        Decoration {
            border: Some(BORDER),
            ..ringed()
        }
    }

    fn ring(layers: &DecorationLayers) -> &Band {
        layers.bands.ring.as_ref().expect("worn with the ring")
    }

    fn border(layers: &DecorationLayers) -> &Band {
        layers.bands.border.as_ref().expect("worn with the border")
    }

    fn bare_of_bands(layers: &DecorationLayers) -> bool {
        layers.bands.ring.is_none() && layers.bands.border.is_none()
    }

    /// Layers worn for a window wearing the ring and the border.
    fn banded() -> DecorationLayers {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.wear(&both(), 2.0);
        layers
    }

    /// Whether `layer` shows in `paint`'s colour, its border drawn in it and no gradient over it.
    fn painted(layer: &CAGradientLayer, paint: Paint) -> bool {
        let Paint::Solid(c) = paint else { panic!("a colour") };
        !layer.isHidden()
            && layer.colors().is_none()
            && CGColor::equal_to_color(layer.borderColor().as_deref(), Some(&srgb(c)))
    }

    /// The hairline's children, first drawn first.
    fn children(layers: &DecorationLayers) -> Vec<*const CALayer> {
        // SAFETY: a read of the layer's own sublayers, which nothing else mutates.
        let sublayers = unsafe { layers.hairline.sublayers() }.expect("the outline and the bands");
        sublayers.iter().map(|layer| Retained::as_ptr(&layer)).collect()
    }

    /// With the ring and the border off, or for a window no rule gives either, a tile carries no
    /// layer for them; worn with the ring alone, it carries the ring's alone, and with both, both.
    #[test]
    fn a_tile_whose_window_wears_no_band_carries_no_layer_for_one() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        let outline: &CALayer = &layers.outline;
        let outline = outline as *const CALayer;
        assert!(
            bare_of_bands(&layers) && children(&layers) == [outline],
            "made with none"
        );
        layers.wear(&Decoration::default(), 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        layers.show(Style::of(true, Ring::Active, &Decoration::default()));
        layers.install_resize(&between(
            &layers,
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        layers.remove_resize();
        layers.remove_animations();
        assert!(
            bare_of_bands(&layers) && children(&layers) == [outline],
            "dressed and resized with none"
        );
        layers.wear(&ringed(), 2.0);
        let ring_layer: &CALayer = &ring(&layers).layer;
        assert_eq!(
            children(&layers),
            [outline, ring_layer as *const CALayer],
            "the ring's alone, as the ring alone is worn"
        );
        assert!(layers.bands.border.is_none());
        let ring_layer = ring(&layers).layer.clone();
        layers.wear(&both(), 2.0);
        assert_eq!(children(&layers).len(), 3, "the border's beside it");
        let border_layer = border(&layers).layer.clone();
        layers.wear(&ringed(), 2.0);
        assert!(
            border_layer.superlayer().is_none() && children(&layers).len() == 2,
            "the border's taken away alone"
        );
        layers.wear(&Decoration::default(), 2.0);
        assert!(
            bare_of_bands(&layers) && children(&layers) == [outline],
            "taken away again"
        );
        assert!(ring_layer.superlayer().is_none());
    }

    #[test]
    fn the_focus_ring_is_in_front_of_the_hairline_and_the_outline() {
        let layers = banded();
        assert_eq!(
            parent(&ring(&layers).layer),
            Some(&*layers.hairline as *const CALayer)
        );
        let outline: &CALayer = &layers.outline;
        assert_eq!(
            children(&layers)[..2],
            [
                outline as *const CALayer,
                &**ring(&layers).layer as *const CALayer
            ],
            "drawn after the outline"
        );
        layers.stack(0.0);
        assert!(
            layers.hairline.zPosition() > 0.0,
            "and with the hairline, in front of the picture"
        );
    }

    /// In front of the ring, whichever of the two a tile was worn with first.
    #[test]
    fn the_border_is_drawn_in_front_of_the_ring() {
        let layers = banded();
        assert_eq!(
            parent(&border(&layers).layer),
            Some(&*layers.hairline as *const CALayer)
        );
        let order = children(&layers);
        assert_eq!(order.len(), 3);
        assert_eq!(
            order[2],
            &**border(&layers).layer as *const CALayer,
            "after the outline and the ring"
        );
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.wear(
            &Decoration {
                border: Some(BORDER),
                ..Decoration::default()
            },
            2.0,
        );
        layers.wear(&both(), 2.0);
        let outline: &CALayer = &layers.outline;
        let order = [
            outline as *const CALayer,
            &**ring(&layers).layer as *const CALayer,
            &**border(&layers).layer as *const CALayer,
        ];
        assert_eq!(
            children(&layers),
            order,
            "a ring made after the border goes under it"
        );
    }

    #[test]
    fn the_border_is_worn_in_the_paint_focus_gives_it_with_the_ring_outside_it() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        let both = both();
        layers.wear(&both, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        let (ring, border) = (&ring(&layers).layer, &border(&layers).layer);
        assert_eq!(border.frame(), rect(-4.0, -4.0, 867.0, 1089.0));
        assert_eq!((border.borderWidth(), border.cornerRadius()), (4.0, 20.0));
        assert_eq!(
            ring.frame(),
            rect(-8.0, -8.0, 875.0, 1097.0),
            "out past the border"
        );
        assert_eq!(
            (ring.borderWidth(), ring.cornerRadius()),
            (8.0, 24.0),
            "and in under it to the window"
        );
        layers.show(Style::of(true, Ring::Active, &both));
        assert!(painted(border, BORDER.active) && painted(ring, RING.active));
        layers.show(Style::of(false, Ring::None, &both));
        assert!(painted(border, BORDER.inactive), "worn without the ring");
        assert!(ring.isHidden());
    }

    #[test]
    fn the_ring_is_shown_only_in_the_paint_its_style_has() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.wear(&ringed(), 2.0);
        layers.show(Style::of(true, Ring::Active, &ringed()));
        let ring = ring(&layers).layer.clone();
        assert!(painted(&ring, RING.active));
        assert!(layers.bands.border.is_none(), "borders off: no layer for one");
        layers.show(Style::of(false, Ring::Inactive, &ringed()));
        assert!(painted(&ring, RING.inactive));
        layers.show(Style::of(true, Ring::None, &ringed()));
        assert!(ring.isHidden(), "not its workspace's active window");
        layers.wear(&Decoration::default(), 2.0);
        layers.show(Style::of(true, Ring::Active, &Decoration::default()));
        assert!(
            bare_of_bands(&layers) && ring.superlayer().is_none(),
            "rings off: no layer for one"
        );
    }

    #[test]
    fn every_band_rounds_on_the_continuous_curve_at_its_outset() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        layers.wear(
            &Decoration {
                ring: Some(crate::decorations::domain::style::Band { width: 1.3, ..RING }),
                ..Decoration::default()
            },
            2.0,
        );
        layers.place(TileSize::new(859.0, 1081.0));
        let ring = &ring(&layers).layer;
        assert_eq!(ring.borderWidth(), 1.5, "whole device pixels");
        assert_eq!(ring.frame(), rect(-1.5, -1.5, 862.0, 1084.0));
        assert_eq!(ring.cornerRadius(), 17.5);
        // SAFETY: Core Animation's constant string.
        assert_eq!(&*ring.cornerCurve(), unsafe { kCACornerCurveContinuous });
        layers.place(TileSize::new(12.0, 400.0));
        assert_eq!(
            ring.cornerRadius(),
            7.5,
            "the window's clamped corner, out by the width"
        );
    }

    /// A window rule's corner is the bands' alone: the hairline, its outline and the shadow are what
    /// macOS draws at rest, at the window's own corner, and keep it.
    #[test]
    fn a_rule_radius_rounds_the_bands_and_leaves_the_hairline_at_the_windows_corner() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        let banded = crate::decorations::domain::style::Band { width: 4.0, ..RING };
        layers.wear(
            &Decoration {
                ring: Some(RING),
                border: Some(banded),
                radius: Some(8.0),
                urgency: Urgency::Calm,
            },
            2.0,
        );
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(layers.hairline.cornerRadius(), 16.0, "the window server's");
        assert_eq!(
            border(&layers).layer.cornerRadius(),
            12.0,
            "the rule's, out by the border"
        );
        layers.wear(
            &Decoration {
                ring: Some(RING),
                border: Some(banded),
                ..Decoration::default()
            },
            2.0,
        );
        layers.place(TileSize::new(859.0, 1081.0));
        assert_eq!(
            border(&layers).layer.cornerRadius(),
            20.0,
            "a tile worn again with no rule's: the server's"
        );
    }

    /// Banded, each band in a gradient, so its mask rides a resize too.
    fn graded() -> DecorationLayers {
        let layers = banded();
        for band in [ring(&layers), border(&layers)] {
            band.paint(Some(sunset(45.0)));
        }
        layers
    }

    /// What `hold_core_animation` is for: another thread's commit drops the animation off a layer
    /// that no render context holds, and waits while a test holds Core Animation's lock.
    #[test]
    fn a_commit_on_another_thread_drops_a_test_layers_animation_once_the_hold_is_let_go() {
        let layer = CALayer::layer();
        let key = NSString::from_str("rini.test");
        let held = hold_core_animation();
        layer.addAnimation_forKey(&bare("opacity"), Some(&key));
        let (started, commits) = std::sync::mpsc::channel();
        let committer = std::thread::spawn(move || {
            started.send(()).unwrap();
            objc2_quartz_core::CATransaction::begin();
            CALayer::layer().setOpacity(0.5);
            objc2_quartz_core::CATransaction::commit();
        });
        commits.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // SAFETY: a read of the layer's own animation, which retains what it returns.
        let waiting = unsafe { layer.animationForKey(&key) };
        assert!(waiting.is_some(), "the commit waits for the hold");
        drop((waiting, held));
        committer.join().unwrap();
        // SAFETY: as above.
        let committed = unsafe { layer.animationForKey(&key) };
        assert!(committed.is_none(), "then drops the animation");
    }

    #[test]
    #[should_panic(expected = "holds hold_core_animation()")]
    fn a_read_back_without_the_hold_fails_whatever_runs_beside_it() {
        installed(&CALayer::layer());
    }

    #[test]
    fn taking_every_animation_away_clears_every_decoration_layer() {
        let _core_animation = hold_core_animation();
        let layers = graded();
        layers.install_resize(&between(
            &layers,
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        for layer in layers.frame_layers() {
            add_under(layer, &bare("position"), "rini.tile.move");
        }
        assert!(
            !installed(&ring(&layers).mask).is_empty()
                && !installed(&border(&layers).mask).is_empty(),
            "graded, the masks ride it"
        );
        layers.remove_animations();
        for layer in [
            &*layers.shadow,
            &layers.shadow_mask,
            &layers.hairline,
            &layers.outline,
            &ring(&layers).layer,
            &border(&layers).layer,
            &ring(&layers).mask,
            &border(&layers).mask,
        ] {
            assert!(installed(layer).is_empty());
        }
    }

    /// An entrance grows from no width, so its corner changes; a resize between two wide sizes keeps
    /// the corner, and takes away the one an earlier resize animated.
    #[test]
    fn a_resize_installs_each_decoration_under_its_own_key() {
        let _core_animation = hold_core_animation();
        let layers = banded();
        layers.install_resize(&between(
            &layers,
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
        for band in [ring(&layers), border(&layers)] {
            assert_eq!(
                installed(&band.layer),
                pairs([
                    ("rini.tile.band.corner", "cornerRadius"),
                    ("rini.tile.band.size", "bounds")
                ])
            );
            assert!(
                installed(&band.mask).is_empty(),
                "a colour's mask masks nothing, so rides nothing"
            );
        }

        layers.install_resize(&between(
            &layers,
            TileSize::new(1200.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        assert!(installed(&layers.hairline).is_empty(), "the corner holds");
        assert_eq!(
            installed(&layers.outline),
            pairs([("rini.tile.outline.path", "path")])
        );
        for band in [ring(&layers), border(&layers)] {
            assert_eq!(
                installed(&band.layer),
                pairs([("rini.tile.band.size", "bounds")])
            );
            assert!(installed(&band.mask).is_empty());
        }
    }

    /// A ring that widens and moves out of its window through a resize rides its width and its
    /// place with it, the mask a gradient draws it through its width too; a border that keeps both
    /// rides neither, and taking the resize away takes them.
    #[test]
    fn a_band_widening_or_moving_out_through_a_resize_rides_its_width_and_place() {
        let _core_animation = hold_core_animation();
        let mut layers = banded();
        let graded = Decoration {
            ring: Some(crate::decorations::domain::style::Band { active: sunset(45.0), ..RING }),
            border: Some(BORDER),
            ..Decoration::default()
        };
        layers.wear(&graded, 2.0);
        layers.show(Style::of(true, Ring::Active, &graded));
        let size = TileSize::new(859.0, 1081.0);
        let from = layers.fit_at(size);
        let to = Fit {
            ring: Lay {
                width: from.ring.width + 2.0,
                out: 2.0,
                ..from.ring
            },
            ..from
        };
        layers.install_resize(&Between(from, to));
        assert_eq!(
            installed(&ring(&layers).layer),
            pairs([
                ("rini.tile.band.corner", "cornerRadius"),
                ("rini.tile.band.end", "endPoint"),
                ("rini.tile.band.move", "position"),
                ("rini.tile.band.size", "bounds"),
                ("rini.tile.band.start", "startPoint"),
                ("rini.tile.band.width", "borderWidth"),
            ])
        );
        assert_eq!(
            installed(&ring(&layers).mask),
            pairs([
                ("rini.tile.band.corner", "cornerRadius"),
                ("rini.tile.band.size", "bounds"),
                ("rini.tile.band.width", "borderWidth"),
            ])
        );
        assert_eq!(
            installed(&border(&layers).layer),
            pairs([("rini.tile.band.size", "bounds")])
        );
        layers.remove_resize();
        assert!(installed(&ring(&layers).layer).is_empty());
        assert!(installed(&ring(&layers).mask).is_empty());
    }

    /// Laid in place of its window's own, every piece rounds to the laying's corner and each band
    /// is laid as it says; given none again, the window's own.
    #[test]
    fn a_tile_laid_in_place_of_its_window_rounds_and_rings_as_the_laying_has_it() {
        let mut layers = banded();
        let size = TileSize::new(859.0, 1081.0);
        let own = layers.own();
        layers.lay(Some(Laying {
            corner: 36.0,
            ring: Lay {
                width: 6.0,
                corner: 39.0,
                out: 6.0,
            },
            ..own
        }));
        layers.place(size);
        assert_eq!(layers.hairline.cornerRadius(), 36.0);
        let band = ring(&layers).layer.clone();
        assert_eq!(band.frame(), rect(-12.0, -12.0, 883.0, 1105.0));
        assert_eq!(band.cornerRadius(), 45.0);
        assert_eq!(band.borderWidth(), 6.0);
        layers.lay(None);
        layers.place(size);
        assert_eq!(layers.hairline.cornerRadius(), shape::FALLBACK_RADIUS);
        assert_eq!(band.borderWidth(), own.ring.width);
    }

    /// Only the resize's own keys go: what the overlay installs on the frame layers stays.
    #[test]
    fn taking_a_resize_away_leaves_no_decoration_animation() {
        let _core_animation = hold_core_animation();
        let layers = graded();
        layers.install_resize(&between(
            &layers,
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        for layer in layers.frame_layers() {
            add_under(layer, &bare("position"), "rini.tile.move");
        }
        assert!(
            !installed(&ring(&layers).mask).is_empty()
                && !installed(&border(&layers).mask).is_empty(),
            "graded, the masks ride it"
        );
        layers.remove_resize();
        for layer in layers.frame_layers() {
            assert_eq!(installed(layer), pairs([("rini.tile.move", "position")]));
        }
        assert!(installed(&layers.shadow_mask).is_empty());
        assert!(installed(&layers.outline).is_empty());
        for band in [ring(&layers), border(&layers)] {
            assert!(installed(&band.layer).is_empty() && installed(&band.mask).is_empty());
        }
    }

    fn sunset(angle: f64) -> Paint {
        Paint::Gradient(Gradient {
            from: Colour::rgb(0x000000),
            to: Colour::rgb(0xffffff),
            angle,
            space: crate::decorations::domain::paint::ColourSpace::Oklch(
                crate::decorations::domain::paint::Hue::Longer,
            ),
        })
    }

    #[test]
    fn a_gradient_fills_the_band_through_a_border_of_its_shape() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(16.0);
        let graded = Decoration {
            ring: Some(crate::decorations::domain::style::Band { active: sunset(45.0), ..RING }),
            ..Decoration::default()
        };
        layers.wear(&graded, 2.0);
        layers.place(TileSize::new(859.0, 1081.0));
        layers.show(Style::of(true, Ring::Active, &graded));
        let ring = ring(&layers);
        let Paint::Gradient(gradient) = sunset(45.0) else {
            unreachable!()
        };
        assert_eq!(
            ring.layer.colors().map(|colours| colours.count()),
            Some(gradient.stops().len())
        );
        assert_eq!(
            ring.layer.locations().map(|at| at.count()),
            Some(gradient.stops().len())
        );
        assert!(ring.layer.borderColor().is_none(), "no border drawn over it");
        let mask = ring.layer.mask().expect("masked to the band");
        assert_eq!(Retained::as_ptr(&mask), Retained::as_ptr(&ring.mask));
        assert_eq!(
            (mask.frame(), mask.borderWidth(), mask.cornerRadius()),
            (rect(0.0, 0.0, 867.0, 1089.0), 4.0, 20.0),
            "the band's own shape"
        );
        // SAFETY: Core Animation's constant string.
        assert_eq!(&*mask.cornerCurve(), unsafe { kCACornerCurveContinuous });
        let axis = gradient.axis(TileSize::new(867.0, 1089.0));
        assert_eq!(
            (ring.layer.startPoint(), ring.layer.endPoint()),
            (axis.start.to_cg(), axis.end.to_cg())
        );
        layers.show(Style::of(false, Ring::Inactive, &graded));
        assert!(
            painted(&ring.layer, RING.inactive),
            "a colour drawn as a border again"
        );
        assert!(ring.layer.mask().is_none());
    }

    /// RGBA of `layer` rendered at 2x into a bitmap of `size`, top row first.
    fn rendered_colour(layer: &CALayer, size: TileSize) -> Vec<[u8; 4]> {
        use objc2_core_graphics::{
            CGBitmapContextCreate, CGBitmapContextGetData, CGColorSpace, CGContext,
            CGImageAlphaInfo,
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
        unsafe { std::slice::from_raw_parts(data, w * h * 4) }
            .chunks(4)
            .map(|p| [p[0], p[1], p[2], p[3]])
            .collect()
    }

    /// Core Animation draws a gradient's lines across at right angles to its axis in unit space; the
    /// axis `Gradient::axis` lays makes them right angles in points, as niri's are.
    #[test]
    fn a_gradient_is_drawn_at_niris_angle_in_points() {
        let size = TileSize::new(220.0, 120.0);
        let root = CALayer::layer();
        root.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size.to_cg()));
        let holder = CALayer::layer();
        holder.setAnchorPoint(CGPoint::new(0.0, 0.0));
        holder.setFrame(rect(10.0, 10.0, 200.0, 100.0));
        root.addSublayer(&holder);
        let band = Band::new(&holder, None);
        band.set_width(10.0);
        band.place(TileSize::new(200.0, 100.0), Lay {
            width: 10.0,
            corner: 0.0,
            out: 0.0,
        });
        let Paint::Gradient(grey) = sunset(45.0) else {
            unreachable!()
        };
        band.paint(Some(Paint::Gradient(Gradient {
            space: crate::decorations::domain::paint::ColourSpace::Srgb,
            ..grey
        })));
        let pixels = rendered_colour(&root, size);
        let at = |x: f64, y: f64| pixels[(y * 2.0) as usize * 440 + (x * 2.0) as usize];
        let (top, right) = (at(115.0, 5.0), at(215.0, 105.0));
        assert!(
            top[0].abs_diff(right[0]) <= 2,
            "one line across at 45 degrees in points: {top:?} and {right:?}"
        );
        assert!(
            at(5.0, 115.0)[0] < 10 && at(215.0, 5.0)[0] > 245,
            "black at the bottom left, white at the top right"
        );
        assert_eq!(at(110.0, 60.0)[3], 0, "nothing drawn inside the band");
    }

    #[test]
    fn a_gradient_at_an_angle_rides_its_axis_through_a_resize() {
        let _core_animation = hold_core_animation();
        let mut layers = DecorationLayers::new(&CALayer::layer());
        let graded = |angle| Decoration {
            border: Some(crate::decorations::domain::style::Band { active: sunset(angle), ..RING }),
            ..Decoration::default()
        };
        layers.wear(&graded(45.0), 2.0);
        let resize = between(
            &layers,
            TileSize::new(1200.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        );
        layers.show(Style::of(true, Ring::None, &graded(45.0)));
        layers.install_resize(&resize);
        let (border, mask) = (&border(&layers).layer, &border(&layers).mask);
        assert_eq!(
            installed(border),
            pairs([
                ("rini.tile.band.end", "endPoint"),
                ("rini.tile.band.size", "bounds"),
                ("rini.tile.band.start", "startPoint")
            ])
        );
        assert_eq!(
            installed(mask),
            pairs([("rini.tile.band.size", "bounds")]),
            "the mask it is drawn through rides the resize"
        );
        layers.show(Style::of(true, Ring::None, &graded(90.0)));
        assert_eq!(
            installed(border),
            pairs([("rini.tile.band.size", "bounds")]),
            "turned to another angle mid-resize, the axis holds"
        );
        assert_eq!(installed(mask), pairs([("rini.tile.band.size", "bounds")]));
        layers.install_resize(&resize);
        assert_eq!(
            installed(border),
            pairs([("rini.tile.band.size", "bounds")]),
            "a right angle's axis is the same at every size"
        );
    }

    /// A gradient that takes over from a colour mid-resize attaches the mask the colour left bare,
    /// and the mask takes the resize its band plays; a colour taking over leaves it bare again.
    #[test]
    fn a_mask_attached_mid_resize_rides_its_bands_resize() {
        let _core_animation = hold_core_animation();
        let mut layers = DecorationLayers::new(&CALayer::layer());
        let graded = Decoration {
            ring: Some(crate::decorations::domain::style::Band { active: sunset(45.0), ..RING }),
            ..Decoration::default()
        };
        layers.wear(&graded, 2.0);
        layers.show(Style::of(false, Ring::Inactive, &graded));
        layers.install_resize(&between(
            &layers,
            TileSize::new(0.0, 1081.0),
            TileSize::new(859.0, 1081.0),
        ));
        let ring = ring(&layers);
        assert!(installed(&ring.mask).is_empty(), "in a colour");
        layers.show(Style::of(true, Ring::Active, &graded));
        let resize = pairs([
            ("rini.tile.band.corner", "cornerRadius"),
            ("rini.tile.band.size", "bounds"),
        ]);
        assert_eq!(installed(&ring.layer)[..2], resize[..]);
        assert_eq!(installed(&ring.mask), resize, "as its band plays it");
        layers.show(Style::of(false, Ring::Inactive, &graded));
        assert!(
            ring.layer.mask().is_none() && installed(&ring.mask).is_empty(),
            "taken off bare"
        );
    }

    /// A colour is made a `CGColor` once, however many bands are painted in it and however often.
    #[test]
    fn a_colour_painted_twice_is_one_cg_color() {
        let layers = banded();
        let colour = Colour {
            r: 0.25,
            g: 0.5,
            b: 0.75,
            a: 1.0,
        };
        let (ring, border) = (ring(&layers), border(&layers));
        ring.paint(Some(Paint::Solid(colour)));
        let first = ring.layer.borderColor().expect("painted");
        ring.paint(Some(Paint::Solid(Colour::rgb(0x505050))));
        ring.paint(Some(Paint::Solid(colour)));
        border.paint(Some(Paint::Solid(colour)));
        for band in [ring, border] {
            let again = band.layer.borderColor().expect("painted");
            assert!(std::ptr::eq(&*again, &*first), "the one made first");
        }
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
            CGBitmapContextCreate, CGBitmapContextGetData, CGColorSpace, CGContext,
            CGImageAlphaInfo,
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
        unsafe { std::slice::from_raw_parts(data, w * h * 4) }
            .chunks(4)
            .map(|p| p[3])
            .collect()
    }

    /// Core Animation's own corner on a tile of `size`: a white layer clipped at `corner` on `curve`.
    fn clipped(
        size: TileSize,
        corner: f64,
        curve: &objc2_quartz_core::CALayerCornerCurve,
    ) -> Vec<u8> {
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
        let circle = unsafe {
            CGPath::with_rounded_rect(CGRect::new(at, wide.to_cg()), 26.0, 26.0, std::ptr::null())
        };
        // SAFETY: Core Animation's constant strings.
        let floor = farthest(
            &clipped(wide, 26.0, unsafe { kCACornerCurveCircular }),
            &filled(wide, &circle, at, None),
        );
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
            let mask = filled(
                size,
                &shadow_mask_path(size, corner),
                into_tile,
                Some(unsafe { kCAFillRuleEvenOdd }),
            );
            let hole: Vec<u8> = mask.iter().map(|alpha| 255 - alpha).collect();
            assert!(
                farthest(&theirs, &silhouette) <= floor,
                "silhouette {w}x{h} at {corner}: {} past {floor}",
                farthest(&theirs, &silhouette)
            );
            assert!(
                farthest(&theirs, &hole) <= floor,
                "hole {w}x{h} at {corner}: {} past {floor}",
                farthest(&theirs, &hole)
            );
        }
    }

    #[test]
    fn the_hairline_rounds_on_the_continuous_curve_at_every_size() {
        let mut layers = DecorationLayers::new(&CALayer::layer());
        layers.radius = Some(26.0);
        for size in [
            TileSize::new(860.0, 1079.0),
            TileSize::new(52.0, 400.0),
            TileSize::new(0.5, 400.0),
        ] {
            layers.place(size);
            // SAFETY: an immutable `NSString` constant exported by Core Animation.
            let continuous = unsafe { kCACornerCurveContinuous };
            assert_eq!(&*layers.hairline.cornerCurve(), continuous, "{size:?}");
        }
    }
}
