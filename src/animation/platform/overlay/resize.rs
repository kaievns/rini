//! The resizes the overlay plays a tile's decorations through: along its own track, each key's
//! size laid as the tile's decorations lay it (`TileResize`), and along fits sampled a step apart,
//! as the overview's thumbs are dressed along the zoom (`Sampled`).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::CGPath;
use objc2_foundation::{NSArray, NSString};
use objc2_quartz_core::{CAKeyframeAnimation, CALayer, CAMediaTiming, CAPropertyAnimation};
use rini_geometry::space::TileSize;

use super::{
    GROUND_COLOUR_KEY, PICTURE_CORNER_KEY, PIECE_CONTENTS_KEY, PIECE_MOVE_KEY, PIECE_PICTURE_KEY,
    PIECE_SIZE_KEY, TILE_ANIMATION_KEY, TILE_SIZE_KEY, Tile, animation, as_frame, cf_object,
    changes, number_object, point_object, rect_object, size_of,
};
use crate::animation::domain::motion::score::{Track, Value};
use crate::decorations::domain::shape::Fit;
use crate::decorations::platform::layers::{Resize, add_under, remove_under};

/// A tile's track as the resize its decorations play: each value mapped from the fit `fit` lays
/// the size its key holds at.
pub(super) struct TileResize<'a, F>(pub(super) &'a Track, pub(super) F);

impl<F: Fn(TileSize) -> Fit> TileResize<'_, F> {
    fn fit(&self, value: Value) -> Fit { (self.1)(size_of(value)) }
}

impl<F: Fn(TileSize) -> Fit> Resize for TileResize<'_, F> {
    fn point(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CGPoint,
    ) -> Retained<CAPropertyAnimation> {
        animation(self.0, key_path, |v| point_object(value(&self.fit(v))))
    }

    fn rect(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CGRect,
    ) -> Retained<CAPropertyAnimation> {
        animation(self.0, key_path, |v| rect_object(value(&self.fit(v))))
    }

    fn number(&self, key_path: &str, value: impl Fn(&Fit) -> f64) -> Retained<CAPropertyAnimation> {
        animation(self.0, key_path, |v| number_object(value(&self.fit(v))))
    }

    fn path(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CFRetained<objc2_core_graphics::CGPath>,
    ) -> Retained<CAPropertyAnimation> {
        animation(self.0, key_path, |v| cf_object(&value(&self.fit(v))))
    }

    fn changes(&self, f: impl Fn(&Fit) -> f64) -> bool { changes(self.0, |v| f(&self.fit(v))) }
}

/// Whether `track` resizes its tile between any two of its keys.
pub(super) fn resizes(track: &Track) -> bool {
    track.keys().windows(2).any(|pair| {
        rini_geometry::stretch::is_a_resize(as_frame(pair[0].1).size, as_frame(pair[1].1).size)
    })
}

/// The tile's pieces carried through `track`'s resize as its decorations lay each size: every piece
/// `install_pieces` carries, and a card's wash.
pub(super) fn install_tile_resize(tile: &Tile, track: &Track) {
    let decorations = &tile.decorations;
    let resize = TileResize(track, |size| decorations.fit_at(size));
    install_pieces(tile, &resize);
    if let Some(card) = &tile.card {
        card.ride(&resize);
    }
}

/// The tile's decorations carried through `resize`, its picture's corner with them while it
/// changes, and its lift's shape.
pub(super) fn install_pieces(tile: &Tile, resize: &impl Resize) {
    tile.decorations.install_resize(resize);
    let radius = |fit: &Fit| fit.corner;
    if resize.changes(radius) {
        add_under(
            &tile.picture,
            &resize.number("cornerRadius", radius),
            PICTURE_CORNER_KEY,
        );
    } else {
        remove_under(&tile.picture, &[PICTURE_CORNER_KEY]);
    }
    if let Some(lift) = &tile.lift {
        lift.install_resize(resize);
    }
}

/// `layer` riding whatever `tile`'s frame plays: its move and its resize, as the tile's picture
/// plays them.
pub(super) fn ride_as(layer: &CALayer, tile: &CALayer) {
    for key in [TILE_ANIMATION_KEY, TILE_SIZE_KEY] {
        let key = NSString::from_str(key);
        // SAFETY: a read of the picture's own animation.
        if let Some(riding) = unsafe { tile.animationForKey(&key) } {
            layer.addAnimation_forKey(&riding, Some(&key));
        }
    }
}

/// Takes away every animation only a resize installs on a tile.
pub(super) fn remove_resize(tile: &Tile) {
    for layer in tile.layers() {
        remove_under(layer, &[TILE_SIZE_KEY]);
    }
    remove_under(&tile.picture, &[PICTURE_CORNER_KEY]);
    tile.decorations.remove_resize();
    if let Some(lift) = &tile.lift {
        lift.remove_resize();
    }
    if let Some(card) = &tile.card {
        card.remove_resize();
    }
    if let Some(grid) = &tile.crop_grid {
        for layer in grid.layers() {
            remove_under(layer, &[
                PIECE_MOVE_KEY,
                PIECE_SIZE_KEY,
                PIECE_CONTENTS_KEY,
                PIECE_PICTURE_KEY,
                GROUND_COLOUR_KEY,
            ]);
        }
    }
}

/// Whether `f` of any two of `frames` differs.
pub(super) fn changing<T>(frames: &[T], f: impl Fn(&T) -> f64) -> bool {
    let mut values = frames.iter().map(f);
    let Some(first) = values.next() else { return false };
    values.any(|value| value != first)
}

/// `values` played one a frame apart as `key_path`, from `begin` over `seconds`.
pub(super) fn keyframes(
    key_path: &str,
    values: Vec<Retained<AnyObject>>,
    (begin, seconds): (f64, f64),
) -> Retained<CAPropertyAnimation> {
    let keyframes = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: each caller maps every frame to the type Core Animation expects for `key_path`.
    unsafe { keyframes.setValues(Some(&NSArray::from_retained_slice(&values))) };
    keyframes.setDuration(seconds);
    keyframes.setBeginTime(begin);
    Retained::into_super(keyframes)
}

/// The fits a thumb passes through a frame apart, as the resize its decorations play.
pub(super) struct Sampled<'a> {
    pub(super) begin: f64,
    pub(super) seconds: f64,
    pub(super) fits: &'a [Fit],
}

impl Sampled<'_> {
    fn play(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> Retained<AnyObject>,
    ) -> Retained<CAPropertyAnimation> {
        keyframes(
            key_path,
            self.fits.iter().map(value).collect(),
            (self.begin, self.seconds),
        )
    }
}

impl Resize for Sampled<'_> {
    fn point(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CGPoint,
    ) -> Retained<CAPropertyAnimation> {
        self.play(key_path, |fit| point_object(value(fit)))
    }

    fn rect(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CGRect,
    ) -> Retained<CAPropertyAnimation> {
        self.play(key_path, |fit| rect_object(value(fit)))
    }

    fn number(&self, key_path: &str, value: impl Fn(&Fit) -> f64) -> Retained<CAPropertyAnimation> {
        self.play(key_path, |fit| number_object(value(fit)))
    }

    fn path(
        &self,
        key_path: &str,
        value: impl Fn(&Fit) -> CFRetained<CGPath>,
    ) -> Retained<CAPropertyAnimation> {
        self.play(key_path, |fit| cf_object(&value(fit)))
    }

    fn changes(&self, f: impl Fn(&Fit) -> f64) -> bool { changing(self.fits, f) }
}
