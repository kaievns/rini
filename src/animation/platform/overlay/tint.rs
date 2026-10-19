//! The tint the overview lays over the desktop behind its thumbs: the card's backdrop
//! (`chrome::domain::card::BACKDROP`) across the whole overlay, between the desktop and every tile,
//! at the score's `Node::Held` share of `BACKDROP_ALPHA`. None between overviews. The floating
//! container and its twin fade the other way on the same share (`fade`): whole at rest, as under a
//! flight, and gone once the overview is held all the way, as its floating windows are while held.

use objc2::rc::Retained;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGColor;
use objc2_foundation::NSString;
use objc2_quartz_core::{CAAnimation, CALayer};

use super::{animation, as_number, number_object};
use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{Channel, Edit, Node, Score};
use crate::chrome::domain::card::{BACKDROP, BACKDROP_ALPHA};

/// The one key the tint's opacity plays under.
pub(super) const TINT_KEY: &str = "rini.tint";

/// The one key the floating container's fade plays under.
pub(super) const FADE_KEY: &str = "rini.fade";

/// The layer's opacity for a tint `share` of the way, from none to `BACKDROP_ALPHA`.
pub(super) fn opacity(share: f64) -> f64 { BACKDROP_ALPHA * share.clamp(0.0, 1.0) }

/// The floating container's opacity held `share` of the way: whole at rest, gone held.
pub(super) fn faded(share: f64) -> f64 { 1.0 - share.clamp(0.0, 1.0) }

/// Whether `edit` changes what `fade` draws: how far the overview is held, or the floating
/// container.
pub(super) fn fades(edit: &Edit) -> bool {
    matches!(
        edit.node(),
        Some(Node::Held | Node::Container(GroupKey::Floating))
    )
}

/// `layers`, the floating container and its twin, at the opposite of how far `score` holds the
/// overview, its track played on them the same way.
pub(super) fn fade(score: &Score, layers: &[Retained<CALayer>]) {
    let key = NSString::from_str(FADE_KEY);
    let share = score.model(Node::Held).map_or(0.0, as_number);
    let playing = score.track(Node::Held, Channel::Base).filter(|track| track.seconds() > 0.0);
    for layer in layers {
        layer.removeAnimationForKey(&key);
        layer.setOpacity(faded(share) as f32);
        if let Some(track) = playing {
            let opacity = |value| number_object(faded(as_number(value)));
            layer.addAnimation_forKey(&animation(track, "opacity", opacity), Some(&key));
        }
    }
}

pub(super) struct Tint {
    layer: Retained<CALayer>,
}

impl Tint {
    /// Over `root`'s whole `size` at `z`, nothing tinted.
    pub(super) fn new(root: &CALayer, size: CGSize, z: f64) -> Tint {
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size));
        layer.setZPosition(z);
        let (red, green, blue) = BACKDROP.rgb();
        layer.setBackgroundColor(Some(&CGColor::new_srgb(red, green, blue, 1.0)));
        layer.setOpacity(0.0);
        root.addSublayer(&layer);
        Tint { layer }
    }

    pub(super) fn layer(&self) -> &CALayer { &self.layer }

    /// Tinted `share` of the way once nothing plays.
    pub(super) fn set(&self, share: f64) { self.layer.setOpacity(opacity(share) as f32) }

    /// Takes the tint's animation off.
    pub(super) fn cut(&self) { self.layer.removeAnimationForKey(&NSString::from_str(TINT_KEY)) }

    /// `animation` played as the tint.
    pub(super) fn play(&self, animation: &CAAnimation) {
        self.layer.addAnimation_forKey(animation, Some(&NSString::from_str(TINT_KEY)));
    }

    /// Nothing tinted, nothing playing.
    pub(super) fn clear(&self) {
        self.cut();
        self.set(0.0);
    }
}
