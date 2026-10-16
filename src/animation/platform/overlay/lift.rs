//! A tile's lift as the overlay draws it: a caster beside the tile's own, carrying only the lift's
//! shadow (`LIFT_SHADOW`) at the score's `Node::Lift` share of its opacity, masked to what lies
//! outside the window so it never shows through a translucent one. Made the first time the score
//! lifts its tile, and gone with the tile or its lift.

use objc2::rc::Retained;
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::NSString;
use objc2_quartz_core::{CAAnimation, CALayer, CAShapeLayer, kCAFillRuleEvenOdd};
use rini_geometry::space::{TileSize, ToCg};

use crate::decorations::domain::shape::{LIFT_REACH, mask_frame};
use crate::decorations::domain::stack::SHADOW_Z;
use crate::decorations::domain::style::LIFT_SHADOW;
use crate::decorations::platform::layers::{mask_path, silhouette_path};

/// The one key a lift's opacity plays under, on its caster.
pub(super) const LIFT_KEY: &str = "rini.tile.lift";

/// The shadow's opacity for a tile lifted `share` of the way, from none to `LIFT_SHADOW`'s.
pub(super) fn opacity(share: f64) -> f64 { f64::from(LIFT_SHADOW.opacity) * share.clamp(0.0, 1.0) }

pub(super) struct Lift {
    caster: Retained<CALayer>,
    mask: Retained<CAShapeLayer>,
}

impl Lift {
    /// A caster under `container`, nothing lifted.
    pub(super) fn new(container: &CALayer) -> Lift {
        let caster = CALayer::layer();
        caster.setAnchorPoint(CGPoint::new(0.0, 0.0));
        caster.setShadowRadius(LIFT_SHADOW.radius);
        caster.setShadowOffset(CGSize::new(0.0, LIFT_SHADOW.offset_y));
        caster.setShadowOpacity(0.0);
        let mask = CAShapeLayer::layer();
        mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
        // SAFETY: Core Animation's own fill-rule constant, and a mask layer we just created and own.
        unsafe {
            mask.setFillRule(kCAFillRuleEvenOdd);
            caster.setMask(Some(&mask));
        }
        container.addSublayer(&caster);
        Lift { caster, mask }
    }

    /// The layer that rides the tile's frame.
    pub(super) fn layer(&self) -> &CALayer { &self.caster }

    /// Shaped for a tile of `size` with corners of `corner`.
    pub(super) fn shape(&self, size: TileSize, corner: f64) {
        self.caster.setShadowPath(Some(&silhouette_path(size, corner)));
        self.mask.setFrame(mask_frame(size, LIFT_REACH).to_cg());
        self.mask.setPath(Some(&mask_path(size, corner, LIFT_REACH)));
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
