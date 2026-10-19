//! A tile's lift as the overlay draws it: a caster beside the tile's own, carrying only the lift's
//! shadow (`LIFT_SHADOW`) at the score's `Node::Lift` share of its opacity, masked to what lies
//! outside the window so it never shows through a translucent one. Cast under the zoom the overlay
//! names, as wide on screen as at rest under the overview's. Made the first time the score lifts
//! its tile, and gone with the tile or its lift.

use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_quartz_core::{CAAnimation, CALayer, CAShapeLayer, kCAFillRuleEvenOdd};
use rini_geometry::space::{TileSize, ToCg};

use crate::decorations::domain::shape::{LIFT_REACH, mask_frame};
use crate::decorations::domain::stack::SHADOW_Z;
use crate::decorations::domain::style::{LIFT_SHADOW, ShadowStyle};
use crate::decorations::platform::layers::{
    Resize, add_under, mask_path, remove_under, silhouette_path,
};

/// The one key a lift's opacity plays under, on its caster.
pub(super) const LIFT_KEY: &str = "rini.tile.lift";

// What a resize installs on a lift: its caster's silhouette, and its mask's path and size.
pub(super) const LIFT_PATH_KEY: &str = "rini.tile.lift.path";
const LIFT_MASK_PATH_KEY: &str = "rini.tile.lift.mask.path";
const LIFT_MASK_SIZE_KEY: &str = "rini.tile.lift.mask.size";

/// The shadow's opacity for a tile lifted `share` of the way, from none to `LIFT_SHADOW`'s.
pub(super) fn opacity(share: f64) -> f64 { f64::from(LIFT_SHADOW.opacity) * share.clamp(0.0, 1.0) }

pub(super) struct Lift {
    caster: Retained<CALayer>,
    mask: Retained<CAShapeLayer>,
    /// The zoom it is cast under.
    under: f64,
}

impl Lift {
    /// A caster under `container`, cast under a zoom by `under`, nothing lifted.
    pub(super) fn new(container: &CALayer, under: f64) -> Lift {
        let caster = CALayer::layer();
        caster.setAnchorPoint(CGPoint::new(0.0, 0.0));
        caster.setShadowOpacity(0.0);
        let mask = CAShapeLayer::layer();
        mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
        // SAFETY: Core Animation's own fill-rule constant, and a mask layer we just created and own.
        unsafe {
            mask.setFillRule(kCAFillRuleEvenOdd);
            caster.setMask(Some(&mask));
        }
        container.addSublayer(&caster);
        let lift = Lift { caster, mask, under };
        lift.cast();
        lift
    }

    /// Cast under a zoom by `under`, shaped again for a tile of `size` with corners of `corner`.
    pub(super) fn recast(&mut self, under: f64, size: TileSize, corner: f64) {
        self.under = under;
        self.cast();
        self.shape(size, corner);
    }

    fn cast(&self) {
        let shadow = self.shadow();
        self.caster.setShadowRadius(shadow.radius);
        self.caster.setShadowOffset(CGSize::new(0.0, shadow.offset_y));
    }

    /// The shadow it casts lifted all the way, under its zoom.
    pub(super) fn shadow(&self) -> ShadowStyle { LIFT_SHADOW.under(self.under) }

    /// How far out of the tile its mask leaves room for the shadow: `LIFT_REACH`, grown with the
    /// blur under its zoom.
    pub(super) fn reach(&self) -> f64 { LIFT_REACH * self.shadow().radius / LIFT_SHADOW.radius }

    /// The layer that rides the tile's frame.
    pub(super) fn layer(&self) -> &CALayer { &self.caster }

    /// Shaped for a tile of `size` with corners of `corner`.
    pub(super) fn shape(&self, size: TileSize, corner: f64) {
        self.caster.setShadowPath(Some(&silhouette_path(size, corner)));
        self.mask.setFrame(mask_frame(size, self.reach()).to_cg());
        self.mask.setPath(Some(&mask_path(size, corner, self.reach())));
    }

    /// Carried through `resize`, shaped as every fit it passes through lays its tile: the caster's
    /// silhouette and its mask's path while the size or the corner changes, and the mask's size,
    /// one reach out of the tile, while the size does; nothing rides it while neither changes.
    pub(super) fn install_resize(&self, resize: &impl Resize) {
        let reach = self.reach();
        let sized = resize.changes(|fit| fit.size.width) || resize.changes(|fit| fit.size.height);
        if !sized && !resize.changes(|fit| fit.corner) {
            return self.remove_resize();
        }
        let silhouette = resize.path("shadowPath", |fit| silhouette_path(fit.size, fit.corner));
        add_under(&self.caster, &silhouette, LIFT_PATH_KEY);
        let path = resize.path("path", |fit| {
            CFRetained::from(&*mask_path(fit.size, fit.corner, reach))
        });
        add_under(&self.mask, &path, LIFT_MASK_PATH_KEY);
        if sized {
            let bounds = resize.rect("bounds", |fit| {
                CGRect::new(CGPoint::new(0.0, 0.0), mask_frame(fit.size, reach).size.to_cg())
            });
            add_under(&self.mask, &bounds, LIFT_MASK_SIZE_KEY);
        } else {
            remove_under(&self.mask, &[LIFT_MASK_SIZE_KEY]);
        }
    }

    /// The keys of what rides its mask, for the tests.
    #[cfg(test)]
    pub(super) fn mask(&self) -> &CALayer { &self.mask }

    /// Takes every animation off its caster and its mask.
    pub(super) fn remove_animations(&self) {
        self.caster.removeAllAnimations();
        self.mask.removeAllAnimations();
    }

    /// Takes away what `install_resize` put on it.
    pub(super) fn remove_resize(&self) {
        remove_under(&self.caster, &[LIFT_PATH_KEY]);
        remove_under(&self.mask, &[LIFT_MASK_PATH_KEY, LIFT_MASK_SIZE_KEY]);
    }

    /// Drawn behind a tile at depth `z`, as its own caster is.
    pub(super) fn stack(&self, z: f64) { self.caster.setZPosition(z + SHADOW_Z) }

    /// Lifted `share` of the way once nothing plays.
    pub(super) fn set(&self, share: f64) { self.caster.setShadowOpacity(opacity(share) as f32) }

    /// Takes the lift's animation off.
    pub(super) fn cut(&self) { self.caster.removeAnimationForKey(&NSString::from_str(LIFT_KEY)) }

    /// `animation` played as the lift.
    pub(super) fn play(&self, animation: &CAAnimation) {
        self.caster.addAnimation_forKey(animation, Some(&NSString::from_str(LIFT_KEY)));
    }
}
