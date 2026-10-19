//! The tint and the blur the overview lays over the desktop behind its thumbs: the card's backdrop
//! (`chrome::domain::card::BACKDROP`) across the whole overlay, between the desktop and every tile,
//! at the score's `Node::Held` share of `DESKTOP_ALPHA`, and the desktop itself blurred by Core
//! Animation's Gaussian filter at that share of `DESKTOP_BLUR`. Neither between overviews, the
//! filter taken off the desktop. The floating container and its twin fade the other way on the same
//! share (`fade`): whole at rest, as under a flight, and gone once the overview is held all the
//! way, as its floating windows are while held.

use std::sync::Once;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSArray, NSNumber, NSString};
use objc2_quartz_core::CALayer;
use tracing::warn;

use super::{animation, as_number, number_object};
use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{Channel, Edit, Node, Score, Track};
use crate::chrome::domain::card::{BACKDROP, DESKTOP_ALPHA, DESKTOP_BLUR};

/// The one key the tint's opacity plays under.
pub(super) const TINT_KEY: &str = "rini.tint";

/// The one key the desktop's blur plays under.
pub(super) const BLUR_KEY: &str = "rini.blur";

/// The blur's filter, by its name on the desktop's `filters`: Core Animation's own Gaussian
/// (`CAFilter`, not a public class), cheaper a frame than a `CIFilter`
/// (`src/overview/docs/README.md`, "The desktop darkens and blurs on the score too").
const BLUR_FILTER: &str = "gaussianBlur";

/// The key path the blur's radius plays on.
pub(super) const BLUR_RADIUS: &str = "filters.gaussianBlur.inputRadius";

/// The one key the floating container's fade plays under.
pub(super) const FADE_KEY: &str = "rini.fade";

/// The layer's opacity for a tint `share` of the way, from none to `DESKTOP_ALPHA`.
pub(super) fn opacity(share: f64) -> f64 { DESKTOP_ALPHA * share.clamp(0.0, 1.0) }

/// The desktop's blur radius `share` of the way, from none to `DESKTOP_BLUR`.
pub(super) fn radius(share: f64) -> f64 { DESKTOP_BLUR * share.clamp(0.0, 1.0) }

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
    /// The desktop the blur's filter is laid on.
    desktop: Retained<CALayer>,
}

impl Tint {
    /// Over `root`'s whole `size` at `z`, over `desktop`, nothing tinted or blurred.
    pub(super) fn new(root: &CALayer, desktop: &CALayer, size: CGSize, z: f64) -> Tint {
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size));
        layer.setZPosition(z);
        let (red, green, blue) = BACKDROP.rgb();
        layer.setBackgroundColor(Some(&CGColor::new_srgb(red, green, blue, 1.0)));
        layer.setOpacity(0.0);
        root.addSublayer(&layer);
        Tint {
            layer,
            desktop: objc2::Message::retain(desktop),
        }
    }

    pub(super) fn layer(&self) -> &CALayer { &self.layer }

    /// Tinted and blurred `share` of the way once nothing plays.
    pub(super) fn set(&self, share: f64) {
        self.layer.setOpacity(opacity(share) as f32);
        if self.blurs() {
            set_radius(&self.desktop, radius(share));
        }
    }

    /// Takes the tint's and the blur's animations off.
    pub(super) fn cut(&self) {
        self.layer.removeAnimationForKey(&NSString::from_str(TINT_KEY));
        self.desktop.removeAnimationForKey(&NSString::from_str(BLUR_KEY));
    }

    /// `track`, how far the overview is held, played as the tint and as the blur.
    pub(super) fn play(&self, track: &Track) {
        let opacity = |value| number_object(opacity(as_number(value)));
        let tint = animation(track, "opacity", opacity);
        self.layer.addAnimation_forKey(&tint, Some(&NSString::from_str(TINT_KEY)));
        if self.blurs() {
            let radius = |value| number_object(radius(as_number(value)));
            let blur = animation(track, BLUR_RADIUS, radius);
            self.desktop.addAnimation_forKey(&blur, Some(&NSString::from_str(BLUR_KEY)));
        }
    }

    /// Nothing tinted, nothing blurred, nothing playing, and no filter left on the desktop for a
    /// flight to draw.
    pub(super) fn clear(&self) {
        self.cut();
        self.layer.setOpacity(0.0);
        // SAFETY: `filters` takes an array of filters, and none is one.
        unsafe { self.desktop.setFilters(None) };
    }

    /// Whether the desktop carries the blur's filter, laid on it here unless it does: false where
    /// Core Animation has no such filter to lay.
    fn blurs(&self) -> bool {
        if self.desktop.filters().is_some_and(|filters| filters.count() > 0) {
            return true;
        }
        let Some(filter) = gaussian() else {
            static MISSING: Once = Once::new();
            MISSING.call_once(|| {
                warn!(
                    "Core Animation has no Gaussian filter; the overview's desktop is not blurred"
                )
            });
            return false;
        };
        // SAFETY: an array holding one Core Animation filter, the type `filters` takes.
        unsafe { self.desktop.setFilters(Some(&NSArray::from_retained_slice(&[filter]))) };
        true
    }
}

/// Core Animation's Gaussian filter at no radius, its edges normalised so the desktop's own edges
/// do not darken as the black beyond them is blurred in; none where it has none.
fn gaussian() -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"CAFilter")?;
    let kind = NSString::from_str(BLUR_FILTER);
    // SAFETY: `filterWithType:` is `CAFilter`'s class factory, taking a filter type's name and
    // answering an autoreleased filter or nil.
    let filter: Option<Retained<AnyObject>> = unsafe { msg_send![class, filterWithType: &*kind] };
    let filter = filter?;
    let normalise = NSNumber::numberWithBool(true);
    let key = NSString::from_str("inputNormalizeEdges");
    // SAFETY: key-value coding on the filter, whose `inputNormalizeEdges` takes a number.
    let _: () = unsafe { msg_send![&filter, setValue: &*normalise, forKey: &*key] };
    Some(filter)
}

/// `desktop`'s blur at `radius`, through its filter's key path.
fn set_radius(desktop: &CALayer, radius: f64) {
    let radius = NSNumber::numberWithDouble(radius);
    // SAFETY: key-value coding on the layer, the key path naming its blur filter's radius, which
    // takes a number.
    let _: () = unsafe {
        msg_send![desktop, setValue: &*radius, forKeyPath: &*NSString::from_str(BLUR_RADIUS)]
    };
}

/// `desktop`'s blur radius as its model has it, none without the filter.
#[cfg(test)]
pub(super) fn blurred(desktop: &CALayer) -> f64 {
    // SAFETY: key-value coding on the layer, the key path naming its blur filter's radius, nil
    // without the filter.
    let radius: Option<Retained<AnyObject>> =
        unsafe { msg_send![desktop, valueForKeyPath: &*NSString::from_str(BLUR_RADIUS)] };
    radius
        .and_then(|radius| radius.downcast::<NSNumber>().ok())
        .map_or(0.0, |radius| radius.doubleValue())
}
