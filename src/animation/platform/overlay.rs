//! The animation overlay: one rini-owned opaque window holding a picture of every animating window.
//! Real windows are placed at their final frames once, underneath it; tiles ride containers, one
//! per rigid piece of the flight. It plays a flight's score as it is handed it: frame zero posed,
//! then each edit, folded into a replica it reads only to install, and decides nothing. No method
//! commits: the renderer (`crate::animation::platform::renderer`) holds the one transaction they run
//! in, and sets the window's alpha and background outside it. See "The overlay engine" in
//! `src/animation/docs/animation-smoothness.md` and the overlay's own measurements in
//! `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSNumber, NSString, NSValue};
use objc2_quartz_core::{
    CAAnimation, CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming,
    CAMediaTimingFunction, CAPropertyAnimation, CATransform3D, NSValueCATransform3DAdditions,
    kCAAnimationDiscrete, kCAMediaTimingFunctionLinear,
};
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{
    ContentsRect, FromCg, GroupRect, OverlayPoint, OverlayRect, Respace, ScreenPoint, ScreenRect,
    TileRect, TileSize, ToCg,
};
use rini_runloop::turns;

use crate::animation::domain::choreography::overview::Outline;
use crate::animation::domain::motion::easing::CubicBezier;
use crate::animation::domain::motion::holes::{band_hole_in, hole_in};
use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{
    Channel, Curve, Edit, Form, Node, Score, Track, Value, Zoom,
};
use crate::animation::domain::motion::tile::{self, Anchors, ContentMode, content_mode, crop};
use crate::animation::domain::motion::z_group::{Band, container_z};
use crate::animation::event::Crops;
use crate::decorations::domain::style::{Band as DecorationBand, Worn};
use crate::decorations::platform::layers::{
    DecorationLayers, add_under, remove_under, round_continuous,
};
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::screen::primary_display_height;
use crate::pictures::domain::region::{WHOLE, within};
use crate::pictures::platform::layer::{
    clear_contents, contents_of, ground_colour, set_layer_contents, set_layer_ground,
};
use crate::pictures::platform::picture::Picture;

mod card;
mod lift;
mod outline;
mod resize;
mod tint;

use lift::Lift;
use resize::{install_tile_resize, remove_resize, resizes};

/// Above every managed window (CG layer 0), below utility panels and notification banners (19+).
/// See "Level and coverage" in `src/animation/docs/capture-overlay-research.md`.
const OVERLAY_LEVEL: isize = 18;

define_class!(
    /// An `NSView` with a top-left origin, so the layer tree agrees with CoreGraphics coordinates.
    /// `setGeometryFlipped` on a view-backed layer is silently ineffective.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniFlippedOverlayView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// A tile drawn on a captured picture.
pub type OverlayTile = tile::OverlayTile<Picture>;

/// Above every tile; tiles sit at `-depth`, never above zero.
const BAR_Z: f64 = 10_000.0;

/// Below the deepest tile the depth model can produce. See "The overlay engine" in
/// `src/animation/docs/animation-smoothness.md`.
const BACKDROP_Z: f64 =
    -((crate::animation::domain::motion::z_group::MAX_TILE_DEPTH + 1024) as f64);

/// Over the desktop and under every container, so under every tile.
const TINT_Z: f64 = BACKDROP_Z + 1.0;

/// One key per tile movement, so a retarget replaces the animation in flight instead of stacking.
const TILE_ANIMATION_KEY: &str = "rini.tile.move";

/// One key per container movement, for the same reason.
const GROUP_ANIMATION_KEY: &str = "rini.group.move";

/// One key per container bounce, separate from the movement's so the two compose.
const BOUNCE_ANIMATION_KEY: &str = "rini.group.bounce";

/// One key for a move's nudge, apart from the bounce's so neither stops the other.
const NUDGE_ANIMATION_KEY: &str = "rini.group.nudge";

/// One key for the strip's zoom, on the strip layer.
const STRIP_ZOOM_KEY: &str = "rini.strip.zoom";

/// The log target `TileOverlay::trace` writes under, which the engine asks is on before it sends a
/// `Render::Trace`.
pub const TRACE_TARGET: &str = "rini::animation::trace";

/// The key a container's track on `channel` is installed under.
fn container_key(channel: Channel) -> &'static str {
    match channel {
        Channel::Base => GROUP_ANIMATION_KEY,
        Channel::Bounce => BOUNCE_ANIMATION_KEY,
        Channel::Nudge => NUDGE_ANIMATION_KEY,
    }
}

/// The media clock Core Animation's begin times are on.
fn media_now() -> f64 { objc2_quartz_core::CACurrentMediaTime() }

/// A curve in Core Animation form, from the four numbers a `Leg` and the score evaluate, so the
/// render server runs the same curve.
fn timing_function(c: CubicBezier) -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(
        c.x1 as f32,
        c.y1 as f32,
        c.x2 as f32,
        c.y2 as f32,
    )
}

/// A score segment's curve as the timing function Core Animation runs it on.
fn curve_function(curve: Curve) -> Retained<CAMediaTimingFunction> {
    match curve {
        Curve::Bezier(c) => timing_function(c),
        Curve::Linear => {
            // SAFETY: an immutable `NSString` constant exported by Core Animation.
            CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionLinear })
        }
    }
}

/// `track` as the animation of `key_path` Core Animation plays it with, each key handed over as
/// `value` maps it: `Between` as a basic animation, `Samples` as keyframes with values only, and
/// `Keyframes` with their key times and a timing function per segment. Begin, length and whether
/// it adds to the model are the track's.
fn animation(
    track: &Track,
    key_path: &str,
    value: impl Fn(Value) -> Retained<AnyObject>,
) -> Retained<CAPropertyAnimation> {
    let key_path = NSString::from_str(key_path);
    let keys = track.keys();
    let animation = match track.form() {
        Form::Between => {
            let basic = CABasicAnimation::animationWithKeyPath(Some(&key_path));
            // SAFETY: `value` maps each key to the type Core Animation expects for `key_path`.
            unsafe {
                basic.setFromValue(Some(&value(keys[0].1)));
                basic.setToValue(Some(&value(keys[keys.len() - 1].1)));
            }
            basic.setTimingFunction(Some(&curve_function(track.curves()[0])));
            Retained::into_super(basic)
        }
        Form::Samples | Form::Keyframes => {
            let keyframes = CAKeyframeAnimation::animationWithKeyPath(Some(&key_path));
            let values: Vec<Retained<AnyObject>> = keys.iter().map(|(_, v)| value(*v)).collect();
            // SAFETY: as for `Between`.
            unsafe { keyframes.setValues(Some(&NSArray::from_retained_slice(&values))) };
            if track.form() == Form::Keyframes {
                let times: Vec<Retained<NSNumber>> =
                    keys.iter().map(|(t, _)| NSNumber::numberWithDouble(*t)).collect();
                keyframes.setKeyTimes(Some(&NSArray::from_retained_slice(&times)));
                let curves: Vec<Retained<CAMediaTimingFunction>> =
                    track.curves().iter().map(|curve| curve_function(*curve)).collect();
                keyframes.setTimingFunctions(Some(&NSArray::from_retained_slice(&curves)));
            }
            Retained::into_super(keyframes)
        }
    };
    animation.setAdditive(track.channel().is_additive());
    animation.setDuration(track.seconds());
    animation.setBeginTime(track.begin());
    animation
}

/// `track` as `animation` hands it over, its keys mapped by `before` up to the corner of a leg in
/// two steps (`tile::corner_key`) and by `after` from it, the corner taken by both at one key time, so
/// what they map it to cuts there. A leg in one movement is `animation`'s, mapped by `after`.
fn turning(
    track: &Track,
    key_path: &str,
    before: impl Fn(Value) -> Retained<AnyObject>,
    after: impl Fn(Value) -> Retained<AnyObject>,
) -> Retained<CAPropertyAnimation> {
    let Some(corner) = tile::corner_key(track) else {
        return animation(track, key_path, after);
    };
    let keys = track.keys();
    let values = [
        before(keys[0].1),
        before(keys[1].1),
        after(keys[1].1),
        after(keys[2].1),
    ];
    let times = [0.0, corner, corner, 1.0].map(NSNumber::numberWithDouble);
    let curves = [track.curves()[0], Curve::Linear, track.curves()[1]].map(curve_function);
    let keyframes = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: `before` and `after` map each key to the type Core Animation expects for `key_path`.
    unsafe { keyframes.setValues(Some(&NSArray::from_retained_slice(&values))) };
    keyframes.setKeyTimes(Some(&NSArray::from_retained_slice(&times)));
    keyframes.setTimingFunctions(Some(&NSArray::from_retained_slice(&curves)));
    keyframes.setDuration(track.seconds());
    keyframes.setBeginTime(track.begin());
    Retained::into_super(keyframes)
}

/// `key_path` held at `before` up to the corner of `track`, at key time `corner`, and at `after` from
/// it to its end: a cut on the corner's frame, nothing between.
fn switch(
    track: &Track,
    corner: f64,
    key_path: &str,
    before: Retained<AnyObject>,
    after: Retained<AnyObject>,
) -> Retained<CAPropertyAnimation> {
    let keyframes = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: the caller's two values are of the type `key_path` takes.
    unsafe { keyframes.setValues(Some(&NSArray::from_retained_slice(&[before, after]))) };
    let times = [0.0, corner, 1.0].map(NSNumber::numberWithDouble);
    keyframes.setKeyTimes(Some(&NSArray::from_retained_slice(&times)));
    // SAFETY: an immutable `NSString` constant exported by Core Animation.
    keyframes.setCalculationMode(unsafe { kCAAnimationDiscrete });
    keyframes.setDuration(track.seconds());
    keyframes.setBeginTime(track.begin());
    Retained::into_super(keyframes)
}

fn point_object(point: CGPoint) -> Retained<AnyObject> {
    // SAFETY: any point makes an `NSValue`.
    Retained::into_super(Retained::into_super(unsafe { NSValue::valueWithPoint(point) }))
}

fn rect_object(rect: CGRect) -> Retained<AnyObject> {
    // SAFETY: any rect makes an `NSValue`.
    Retained::into_super(Retained::into_super(unsafe { NSValue::valueWithRect(rect) }))
}

fn number_object(number: f64) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(Retained::into_super(
        NSNumber::numberWithDouble(number),
    )))
}

/// A Core Foundation value, a path, a picture or a colour, as an object: a `CGPath`, `CGImage` or
/// `CGColor` pointer handed to `msg_send!` encodes as a struct pointer, which objc2 refuses for
/// `setFromValue:` and `setValues:` in any build with debug assertions.
fn cf_object(value: &objc2_core_foundation::CFType) -> Retained<AnyObject> { Retained::from(value) }

fn as_point(value: Value) -> OverlayPoint {
    match value {
        Value::Point(point) => point,
        _ => unreachable!("a container holds a point"),
    }
}

fn as_frame(value: Value) -> GroupRect {
    match value {
        Value::Rect(frame) => frame,
        _ => unreachable!("a tile holds a frame"),
    }
}

fn as_number(value: Value) -> f64 {
    match value {
        Value::Number(number) => number,
        _ => unreachable!("a lift holds a number"),
    }
}

/// `zoom` as the strip layer's transform, which draws its point `p` at `p * scale + shift`. A scale
/// and a translation alone interpolate part by part, as the score does, whether Core Animation
/// decomposes the matrix or not.
fn zoom_transform(zoom: Zoom) -> CATransform3D {
    CATransform3D {
        m11: zoom.scale,
        m12: 0.0,
        m13: 0.0,
        m14: 0.0,
        m21: 0.0,
        m22: zoom.scale,
        m23: 0.0,
        m24: 0.0,
        m31: 0.0,
        m32: 0.0,
        m33: 1.0,
        m34: 0.0,
        m41: zoom.shift.x,
        m42: zoom.shift.y,
        m43: 0.0,
        m44: 1.0,
    }
}

fn zoom_object(value: Value) -> Retained<AnyObject> {
    let Value::Zoom(zoom) = value else {
        unreachable!("the strip holds a zoom")
    };
    // SAFETY: any transform makes an `NSValue`.
    Retained::into_super(Retained::into_super(unsafe {
        NSValue::valueWithCATransform3D(zoom_transform(zoom))
    }))
}

/// Whether `f` of a track's keys differs between any two of them.
fn changes<T: PartialEq>(track: &Track, f: impl Fn(Value) -> T) -> bool {
    let mut values = track.keys().iter().map(|(_, value)| f(*value));
    let Some(first) = values.next() else { return false };
    values.any(|value| value != first)
}

/// When an animation plays on the media clock: from `begin`, `offset` into its own time, for
/// `active` seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Playing {
    begin: f64,
    offset: f64,
    active: f64,
}

impl Playing {
    /// When `track` plays as Core Animation is first handed it.
    fn of(track: &Track) -> Self {
        Playing {
            begin: track.begin(),
            offset: 0.0,
            active: track.seconds(),
        }
    }

    fn apply(self, animation: &CAAnimation) {
        animation.setBeginTime(self.begin);
        animation.setTimeOffset(self.offset);
        animation.setRepeatDuration(self.active);
    }

    /// The same animation begun again at `at`, as far into its own time as it has got, for what is
    /// left of it. None when it begins after `at` already, or has ended.
    fn again_at(self, at: f64) -> Option<Playing> {
        let ends = self.begin + self.active;
        (self.begin <= at && at < ends).then_some(Playing {
            begin: at,
            offset: self.offset + (at - self.begin),
            active: ends - at,
        })
    }
}

/// Installs a container's track on `layer` under its channel's key; one of no length takes away
/// what is there instead. A movement begins every accent in `riding` again with it, each as far
/// into its own time as it has got, so they go on where they were: Core Animation composes a
/// layer's animations in order of their begin times, ties in the order added, and a non-additive
/// movement hides every additive one before it until it ends. See "The score" in
/// `src/animation/docs/animation-smoothness.md`.
fn install_container_track(layer: &CALayer, track: &Track, riding: &[Track]) {
    let key = NSString::from_str(container_key(track.channel()));
    if track.seconds() <= 0.0 {
        layer.removeAnimationForKey(&key);
        return;
    }
    let position = |value: Value| point_object(as_point(value).to_cg());
    layer.addAnimation_forKey(&animation(track, "position", position), Some(&key));
    if track.channel() != Channel::Base {
        return;
    }
    for accent in riding.iter().filter(|accent| accent.channel().is_additive()) {
        let Some(again) = Playing::of(accent).again_at(track.begin()) else {
            continue;
        };
        let accent_animation = animation(accent, "position", position);
        again.apply(&accent_animation);
        let key = NSString::from_str(container_key(accent.channel()));
        layer.addAnimation_forKey(&accent_animation, Some(&key));
    }
}

// What a resize installs on a tile beside its position and its decorations' shapes, by the layer it
// rides.
const TILE_SIZE_KEY: &str = "rini.tile.size";
const PICTURE_CORNER_KEY: &str = "rini.tile.picture.corner";
const PIECE_MOVE_KEY: &str = "rini.piece.move";
const PIECE_SIZE_KEY: &str = "rini.piece.size";
const PIECE_CONTENTS_KEY: &str = "rini.piece.contents";
// What a leg in two steps cuts at its corner: each piece's picture and the ground's colour.
const PIECE_PICTURE_KEY: &str = "rini.piece.picture";
const GROUND_COLOUR_KEY: &str = "rini.piece.ground";

fn size_of(value: Value) -> TileSize { as_frame(value).size.respace() }

/// Installs a tile's track on every layer that draws the tile, each value mapped from the frame
/// the key holds: the picture and the frame layers of its decorations ride its position; a resize
/// from one key to the next carries their bounds, the crop pieces' frames and contents, the
/// decorations' shapes (`DecorationLayers::install_resize`) and, while the corner changes, the
/// picture's corner with the hairline's, so a clipped picture is cut at the corner every piece
/// rounds to at each size (Core Animation's own rounding past half a side is not the clamp's). A
/// track of no length takes away everything installed, and one that does not resize what only a
/// resize installs, so nothing of an earlier track is left drawing. A leg in two steps given its
/// `corner` pictures cuts its crop grid from one to the other at its corner (`install_crop_grid`),
/// and how long that cut's ground took to read is returned. See "Resizes through the overlay" in
/// `src/animation/docs/animation-smoothness.md`.
fn install_tile_track(tile: &mut Tile, track: &Track, corner: Option<Corner>) -> Option<Duration> {
    if track.seconds() <= 0.0 {
        for layer in tile.layers() {
            remove_under(layer, &[TILE_ANIMATION_KEY]);
        }
        remove_resize(tile);
        return None;
    }
    let position = animation(track, "position", |value| {
        point_object(as_frame(value).origin.to_cg())
    });
    for layer in tile.layers() {
        add_under(layer, &position, TILE_ANIMATION_KEY);
    }
    if !resizes(track) {
        remove_resize(tile);
        return None;
    }
    let origin = CGPoint::new(0.0, 0.0);
    let bounds = animation(track, "bounds", |value| {
        rect_object(CGRect::new(origin, size_of(value).to_cg()))
    });
    for layer in tile.layers() {
        add_under(layer, &bounds, TILE_SIZE_KEY);
    }
    install_tile_resize(tile, track);
    install_crop_grid(tile, track, corner)
}

/// The two pictures a tile's leg in two steps flies: `before` up to its corner, the picture the tile
/// starts with, and `after` from it, its repaint.
#[derive(Clone, Copy)]
struct Corner<'a> {
    before: &'a Picture,
    after: &'a Picture,
}

/// Carries a crop-drawn tile's pieces and its ground along `track`: at every key each is where the
/// grid of that key's frame puts it, a piece's contents composed with the tile's region. A leg in two
/// steps given its `corner` pictures is drawn with `after` from its corner on and `before` up to it,
/// the grid of each against its own picture and region: the tile's picture, its region and its
/// ground become `after`'s, what the leg ends with, and each piece's picture and the ground's
/// colour are held at `before`'s up to the corner and cut to `after`'s there, so the cut lands on
/// the corner's frame, whatever `after` fits (`cut_onto`). Any other leg, or a tile drawn stretched,
/// takes away a cut an earlier one installed. Returns how long the ground `after` is cut onto took
/// to read.
fn install_crop_grid(tile: &mut Tile, track: &Track, corner: Option<Corner>) -> Option<Duration> {
    let grid = tile.crop_grid.as_ref()?;
    for layer in grid.layers() {
        remove_under(layer, &[PIECE_PICTURE_KEY, GROUND_COLOUR_KEY]);
    }
    let turn = corner.zip(tile::corner_key(track)).filter(|_| tile.crop_of.is_some());
    let ground = turn.and_then(|(corner, _)| {
        let end = track.keys()[track.keys().len() - 1].1;
        cut_onto(tile, corner.after, as_frame(end).size.respace())
    });
    let (Some(grid), Some(picture)) = (&tile.crop_grid, tile.crop_of) else {
        return ground;
    };
    let after = (picture, tile.region);
    let before = turn.map_or(after, |(corner, _)| {
        (
            corner.before.coverage.covered.respace(),
            corner.before.image.unit_rect(),
        )
    });
    let laid = |(picture, _): (TileSize, ContentsRect), value: Value| {
        crop(picture, as_frame(value).size.respace(), tile.anchors)
    };
    for (index, layer) in grid.pieces.iter().enumerate() {
        install_frame(
            layer,
            track,
            |at, value| laid(at, value).pieces[index].frame,
            (before, after),
        );
        let contents = |at: (TileSize, ContentsRect)| {
            move |value| rect_object(within(at.1, laid(at, value).pieces[index].contents).to_cg())
        };
        let contents = turning(track, "contentsRect", contents(before), contents(after));
        add_under(layer, &contents, PIECE_CONTENTS_KEY);
    }
    for (index, layer) in grid.ground.iter().enumerate() {
        install_frame(
            layer,
            track,
            |at, value| laid(at, value).ground[index],
            (before, after),
        );
    }
    let Some((corner, key)) = turn else { return ground };
    let (before, after) = (contents_of(corner.before), contents_of(corner.after));
    for layer in &grid.pieces {
        let cut = switch(track, key, "contents", cf_object(before), cf_object(after));
        add_under(layer, &cut, PIECE_PICTURE_KEY);
    }
    // Both grounds are read by now, `before`'s filling the grid it flew and `after`'s by `cut_onto`.
    let (Some(before), Some(after)) = (ground_colour(corner.before), ground_colour(corner.after))
    else {
        return ground;
    };
    for layer in &grid.ground {
        let cut = switch(
            track,
            key,
            "backgroundColor",
            cf_object(&before),
            cf_object(&after),
        );
        add_under(layer, &cut, GROUND_COLOUR_KEY);
    }
    ground
}

/// Carries one layer of a crop grid along `track` to where `frame` lays it at every key, against the
/// first of `pictures` up to a corner of the leg and the second from it.
fn install_frame<P: Copy>(
    layer: &CALayer,
    track: &Track,
    frame: impl Fn(P, Value) -> TileRect,
    (before, after): (P, P),
) {
    let origin = CGPoint::new(0.0, 0.0);
    let position = |at: P| {
        let frame = &frame;
        move |value| point_object(frame(at, value).origin.to_cg())
    };
    add_under(
        layer,
        &turning(track, "position", position(before), position(after)),
        PIECE_MOVE_KEY,
    );
    let sizes = track
        .keys()
        .iter()
        .flat_map(|(_, value)| [frame(before, *value), frame(after, *value)]);
    let mut sizes = sizes.map(|laid| laid.size);
    let first = sizes.next();
    if sizes.any(|size| Some(size) != first) {
        let bounds = |at: P| {
            let frame = &frame;
            move |value| rect_object(CGRect::new(origin, frame(at, value).size.to_cg()))
        };
        add_under(
            layer,
            &turning(track, "bounds", bounds(before), bounds(after)),
            PIECE_SIZE_KEY,
        );
    } else {
        remove_under(layer, &[PIECE_SIZE_KEY]);
    }
}

/// One window's picture and what the window wears: a caster behind the picture carrying only the
/// shadow, and the hairline in front of it. See "The hairline and the shadow, as drawn" in
/// `src/decorations/docs/README.md`.
struct Tile {
    picture: Retained<CALayer>,
    /// Beside the picture rather than in it, so a corner clip (`clip_corners`) leaves the outline
    /// and the ring outside the window whole.
    decorations: DecorationLayers,
    /// What its window wears that rini draws and how it ends, as last dressed or worn since.
    worn: Worn,
    /// Whether it is drawn focused, as last dressed or turned since.
    focused: bool,
    /// The nine crop pieces and the ground, created on first crop-drawn animation and pooled with
    /// the tile.
    crop_grid: Option<CropGrid>,
    /// Size in points of the picture the crop pieces show; `None` when stretching.
    crop_of: Option<TileSize>,
    /// The edges the crop pieces stand still against, picked for the tile's leg.
    anchors: Anchors,
    /// The part of the contents image that is the tile's picture, which the crop pieces' contents
    /// are composed with: all of it unless the picture is a region of a composite.
    region: ContentsRect,
    /// The shadow it casts lifted off the strip, once the score has lifted it.
    lift: Option<Lift>,
    /// The wash under it, once it is drawn as the card.
    card: Option<card::Card>,
    /// Whether its picture is clipped to its corner, as last dressed or swapped; else the picture
    /// keeps its window's own corner.
    clipped: bool,
}

/// The layers a crop-drawn tile is composed of, children of the tile's picture layer: the nine
/// pieces of its picture, and the three its ground fills where the picture ends.
struct CropGrid {
    pieces: [Retained<CALayer>; 9],
    ground: [Retained<CALayer>; 3],
}

impl CropGrid {
    fn layers(&self) -> impl Iterator<Item = &CALayer> {
        self.pieces.iter().chain(&self.ground).map(|layer| &**layer)
    }
}

/// What a flight is drawn with: the desktop behind it, the strip layer, the floating container and
/// its twin under `root`, every other container under the strip layer, the tiles under their
/// containers, and the replica, the score of everything installed and placed on them. Apart from the
/// window, so it hangs off any root layer and a test builds one without a window. Every method that
/// changes a layer leaves the transaction to its caller.
struct Tree {
    root: Retained<CALayer>,
    /// The display's size: every container covers it.
    size: CGSize,
    scale: f64,
    /// The real desktop, drawn behind everything and held still.
    backdrop: Retained<CALayer>,
    /// What every container but the floating one and its twin hangs under, anchored at the
    /// overlay's origin: its `transform` is the strip's zoom.
    strip: Retained<CALayer>,
    /// One layer per rigid piece of a flight; a container's `position` is the only animated
    /// translation its members get.
    containers: HashMap<GroupKey, Retained<CALayer>>,
    /// The floating container's twin, drawn in front of the strip: where the floating windows that come
    /// forward with the focus are drawn (`Banding::lifted`).
    ///
    /// A twin rather than a second plan group because it is the same rigid piece — every floating window
    /// rides one translation — and only its depth differs. Tiles are children of their container, and a
    /// container's zPosition alone decides its children's order against every other container, so one
    /// floating container could never put one application in front of the strip and another behind it.
    /// Opened, moved, animated and dropped with the floating container, so a tile moved between the two
    /// lands exactly where it was.
    lifted: Option<Retained<CALayer>>,
    tile_layers: HashMap<WindowId, Tile>,
    /// The fold of every edit played: what is drawn is read from here, never from the presentation
    /// layer.
    replica: Score,
    /// What the drain playing has spent on crop-drawn tiles, taken when it is answered.
    crops: Crops,
    /// The overview's outlines of its empty workspaces.
    outlines: outline::Outlines,
    /// The overview's darkening and blur of the desktop.
    tint: tint::Tint,
    /// The zoom the overview holds the overlay under, every lift cast under it and every tile drawn
    /// as the card (`card`); none for a flight, whose lifts are cast under no zoom.
    held: Option<f64>,
    /// The latest media time it was handed, by the overlay as it is drawn, a track's begin or a
    /// picture's swap: what a dress along the zoom is judged playing at (`card::dressing`).
    now: f64,
    /// When the last dress along the zoom installed ends, while one may still play.
    dressed_until: Option<f64>,
}

pub struct TileOverlay {
    window: Retained<NSWindow>,
    tree: Tree,
    /// The bar, redrawn on top with its own alpha since the overlay covers the real one. See "The
    /// bar has to be captured on its own" in `src/pictures/docs/capture-research.md`.
    bar: Retained<CALayer>,
    /// Whether the bar has ever been drawn, so a skipped capture keeps it rather than hiding it.
    bar_drawn: bool,
    /// Display frame in CoreGraphics coordinates. Full display: the tree's root draws in these
    /// coordinates, and the window is the part below `top_band`.
    frame: CGRect,
    /// Points reserved at the top for the bar, so the window starts below it.
    top_band: f64,
    visible: bool,
    /// Whether the window's background is clear, as it is while the window is holed.
    clear: bool,
    mtm: MainThreadMarker,
}

/// A window's number as the window server's id; `None` for a window it has not numbered, which
/// AppKit gives as 0 or less.
fn numbered(number: isize) -> Option<WindowServerId> {
    u32::try_from(number)
        .ok()
        .filter(|number| *number != 0)
        .map(WindowServerId::new)
}

/// The overlay window's rect: the display below the bar's band. In CoreGraphics coordinates, y grows
/// downward, so the top band is removed by moving the origin down and shrinking the height.
fn window_rect(frame: CGRect, top_band: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.origin.x, frame.origin.y + top_band),
        CGSize::new(frame.size.width, (frame.size.height - top_band).max(0.0)),
    )
}

/// Whether an overlay laid out for `held` is laid out for `wanted`: each a display's frame, the band
/// the bar reserves at its top, and the scale. `set_frame` with the layout held moves nothing and
/// keeps its holes.
pub fn same_layout(held: (ScreenRect, f64, f64), wanted: (ScreenRect, f64, f64)) -> bool {
    held.0 == wanted.0 && held.1 == wanted.1 && (held.2 - wanted.2).abs() < f64::EPSILON
}

/// Where the overlay window over `display` starts on screen, as `window_rect` places it, which a
/// picture of the window alone starts at.
pub fn window_origin(display: ScreenRect, top_band: f64) -> ScreenPoint {
    ScreenPoint::from_cg(window_rect(display.to_cg(), top_band).origin)
}

/// Where the content layer sits inside the window: the full display, lifted so the reserved band is
/// above the window's top edge and clipped away. The view is flipped, so its layer's y grows
/// downward and a negative origin lifts the content.
fn canvas_rect(frame: CGRect, top_band: f64) -> CGRect {
    CGRect::new(CGPoint::new(0.0, -top_band), frame.size)
}

/// A rect in the overlay's coordinates, the full display, in the view's: the canvas sits a band
/// above the view's top edge.
fn in_view(rect: CGRect, top_band: f64) -> CGRect {
    CGRect::new(CGPoint::new(rect.origin.x, rect.origin.y - top_band), rect.size)
}

/// A mask covering `bounds` but for `holes`: one rect and the holes, filled even-odd, so each hole
/// is left out.
fn holes_mask(bounds: CGRect, holes: &[CGRect]) -> Retained<objc2_quartz_core::CAShapeLayer> {
    let path = objc2_core_graphics::CGMutablePath::new();
    // SAFETY: a null transform means the rects are taken as given.
    unsafe {
        objc2_core_graphics::CGMutablePath::add_rect(Some(&path), std::ptr::null(), bounds);
        for hole in holes {
            objc2_core_graphics::CGMutablePath::add_rect(Some(&path), std::ptr::null(), *hole);
        }
    }
    let mask = objc2_quartz_core::CAShapeLayer::layer();
    mask.setFrame(bounds);
    mask.setPath(Some(&path));
    // SAFETY: Core Animation's own fill-rule constant.
    mask.setFillRule(unsafe { objc2_quartz_core::kCAFillRuleEvenOdd });
    mask
}

impl TileOverlay {
    /// Creates the overlay once, ordered in but fully transparent. `frame` must be the display's
    /// full bounds, and `top_band` the points the bar reserves at the top. See "Level and coverage"
    /// in `src/animation/docs/capture-overlay-research.md`.
    pub fn new(
        frame: ScreenRect,
        top_band: f64,
        scale: f64,
        mtm: MainThreadMarker,
    ) -> Option<Self> {
        let frame = frame.to_cg();
        let converter = CoordinateConverter::from_height(primary_display_height());
        let cocoa_frame = converter.convert_rect(window_rect(frame, top_band))?;

        // SAFETY: a fresh allocation on the main thread, which `mtm` proves, and a window never
        // closed, so AppKit's release when closed never frees what the `Retained` owns.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                cocoa_frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // NOT opaque, even though it is a black slab across the whole display when it is up. The
        // opaque flag promises the window server that nothing behind this window contributes to what
        // is composited, and the overlay stays ordered in at alpha 0 between animations, so the
        // promise would hold for windows ABOVE it as well: the backdrop a blurred surface above it
        // samples would be truncated here, leaving the switcher's blur nothing to blur. The black
        // comes from the background colour below, which is what fills an undrawn gap.
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::blackColor()));
        window.setHasShadow(false);
        window.setIgnoresMouseEvents(true);
        window.setLevel(OVERLAY_LEVEL);
        // Stationary, or the overlay slides along with macOS's own Space animation.
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenNone,
        );
        // Alpha is the show/hide mechanism: ordering in and out costs about 14ms each way.
        window.setAlphaValue(0.0);

        // SAFETY: `initWithFrame:` takes the one `CGRect` given, and as an init consumes the fresh
        // allocation and returns it retained, on the main thread `mtm` proves.
        let view: Retained<FlippedView> = unsafe {
            msg_send![
                FlippedView::alloc(mtm),
                initWithFrame: CGRect::new(CGPoint::new(0.0, 0.0), window_rect(frame, top_band).size)
            ]
        };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));

        // The view's own layer clips; `root` holds the content in FULL-display coordinates, lifted so
        // the reserved top band falls above the view and is clipped away. Everything downstream —
        // the backdrop, the tiles, the strip surface — keeps working in full-display coordinates and
        // needs no offset of its own.
        let clip = view.layer()?;
        clip.setMasksToBounds(true);
        clip.setContentsScale(scale);
        let root = CALayer::layer();
        root.setAnchorPoint(CGPoint::new(0.0, 0.0));
        root.setFrame(canvas_rect(frame, top_band));
        root.setContentsScale(scale);
        clip.addSublayer(&root);

        let tree = Tree::new(root.clone(), frame.size, scale);

        let bar = CALayer::layer();
        bar.setAnchorPoint(CGPoint::new(0.0, 0.0));
        bar.setContentsScale(scale);
        bar.setZPosition(BAR_Z);
        bar.setHidden(true);
        root.addSublayer(&bar);

        window.orderFrontRegardless();
        turns::changed("overlay.open");

        Some(Self {
            window,
            tree,
            bar,
            bar_drawn: false,
            frame,
            top_band,
            visible: false,
            clear: false,
            mtm,
        })
    }

    /// Draws the bar on top of the moving tiles; `strip` is its rect. A failed capture keeps the
    /// previous picture rather than hiding the bar.
    pub fn set_bar(&mut self, snapshot: Option<&Picture>, strip: Option<OverlayRect>) {
        let Some(strip) = strip else {
            self.bar.setHidden(true);
            return;
        };
        match snapshot {
            Some(snapshot) => {
                self.bar.setContentsScale(self.tree.scale);
                set_layer_contents(&self.bar, snapshot);
                self.bar.setFrame(bar_frame(strip.to_cg(), snapshot.coverage.covered.to_cg()));
                self.bar_drawn = true;
                self.bar.setHidden(false);
            }
            None => self.bar.setHidden(!self.bar_drawn),
        }
    }

    /// Sets the still image drawn behind the moving tiles, at its own covered size so it stays in
    /// register with the real desktop. With none, the backdrop stays as the lift left it: empty.
    pub fn set_backdrop(&mut self, snapshot: Option<&Picture>) {
        if let Some(snapshot) = snapshot {
            self.tree.set_backdrop(snapshot);
        }
    }

    /// Repoints the overlay at a different display, a resolution change, or a change of the band the
    /// bar reserves at the top, and puts its window back wherever macOS has moved it.
    pub fn set_frame(&mut self, frame: ScreenRect, top_band: f64, scale: f64) {
        let held = (ScreenRect::from_cg(self.frame), self.top_band, self.tree.scale);
        let laid_out = same_layout(held, (frame, top_band, scale));
        let frame = frame.to_cg();
        let converter = CoordinateConverter::from_height(primary_display_height());
        if let Some(cocoa) = converter.reframe(window_rect(frame, top_band), self.window.frame()) {
            self.window.setFrame_display(cocoa, false);
            turns::changed("overlay.frame");
        }
        if laid_out {
            return;
        }
        self.clear_holes();
        self.frame = frame;
        self.top_band = top_band;
        self.tree.size = frame.size;
        self.tree.scale = scale;
        self.tree.tint.layer().setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        if let Some(view) = self.window.contentView() {
            view.setFrame(CGRect::new(
                CGPoint::new(0.0, 0.0),
                window_rect(frame, top_band).size,
            ));
        }
        self.tree.root.setFrame(canvas_rect(frame, top_band));
        self.tree.root.setContentsScale(scale);
    }

    /// Frame zero: the tree rebuilt as `score` has it, every tile dressed from `tiles`, and the
    /// replica `score`. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    pub fn pose(&mut self, score: &Score, tiles: &[OverlayTile]) { self.now().pose(score, tiles); }

    /// Plays `edits`: each folded into the replica, then carried out on the layers. A tile first seen
    /// is made and dressed from `tiles`.
    pub fn play(&mut self, edits: &[Edit], tiles: &[OverlayTile]) { self.now().play(edits, tiles); }

    /// Swaps a fresh picture into a tile mid-flight, `clipped` to its corner or whole, leaving its
    /// movement alone: a crop-drawn tile on a track of its own has its grid re-keyed along that
    /// track, so only the pixels cut.
    pub fn swap(&mut self, window: WindowId, picture: &Picture, clipped: bool) {
        self.tree.swap(window, picture, clipped, media_now());
    }

    /// What the drain played since the last take spent on crop-drawn tiles: the renderer takes it
    /// as it answers each drain.
    pub fn take_crops(&mut self) -> Crops { self.tree.take_crops() }

    /// Draws a tile focused or not, a hard cut: its hairline's white and its shadow's depth, its ring
    /// and its border left as they are. For focus handing over mid-flight; every dressing draws what
    /// its tile says.
    pub fn set_tile_focused(&mut self, window: WindowId, focused: bool) {
        self.tree.turn(window, focused);
    }

    /// Paints a tile's ring and border as `worn` has them where the flight now ends, a hard cut, on
    /// the bands it was dressed with. For a merge moving focus mid-flight.
    pub fn set_tile_worn(&mut self, window: WindowId, worn: Worn) { self.now().wear(window, worn); }

    /// `window`'s tile dressed again in `worn`'s decoration, its bands laid at their widths.
    pub fn rewear_tile(&mut self, window: WindowId, worn: Worn) { self.now().rewear(window, worn); }

    /// The overview holds the overlay under a zoom by `under` until it lets its tiles go: every
    /// lift cast as wide on screen as at rest under it, and every tile drawn as the card.
    pub fn hold(&mut self, under: f64) { self.now().hold(under); }

    /// The tree, told the media time it is drawn from now.
    fn now(&mut self) -> &mut Tree {
        self.tree.now = self.tree.now.max(media_now());
        &mut self.tree
    }

    /// The overview's outlines of its empty workspaces, ringed as `ring` is, in place of those
    /// before.
    pub fn set_outlines(&mut self, outlines: &[Outline], ring: Option<DecorationBand>) {
        let tree = &mut self.tree;
        tree.outlines.draw(outlines, ring, &tree.containers, tree.scale);
        tree.dress_outlines();
    }

    /// Logs where each container is presented beside where the replica draws it, and the gap
    /// between them, under `TRACE_TARGET` at trace level: the measurement behind "One
    /// flight however many presses" in `src/animation/docs/animation-smoothness.md`. Asked for only
    /// while that target is on. The one read of the presentation layer left, and nothing is decided
    /// by it.
    pub fn trace(&self) {
        let at = media_now();
        for (key, layer) in &self.tree.containers {
            // SAFETY: `presentationLayer` returns a read-only copy of the layer.
            let presented =
                unsafe { layer.presentationLayer() }.map_or(layer.position(), |p| p.position());
            let Some(Value::Point(drawn)) = self.tree.replica.sample(Node::Container(*key), at)
            else {
                continue;
            };
            let gap = (presented.x - drawn.x).abs().max((presented.y - drawn.y).abs());
            tracing::trace!(
                target: TRACE_TARGET,
                key = format!("{key:?}"),
                x = format!("{:.1}", presented.x),
                y = format!("{:.1}", presented.y),
                score_x = format!("{:.1}", drawn.x),
                score_y = format!("{:.1}", drawn.y),
                gap = format!("{gap:.2}"),
                "presented"
            );
        }
    }

    /// The overlay's window as the window server numbers it, which a handover check pictures alone.
    pub fn server_id(&self) -> Option<WindowServerId> { numbered(self.window.windowNumber()) }

    /// Shows the overlay; only an alpha change.
    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        self.window.setAlphaValue(1.0);
        self.window.orderFrontRegardless();
        turns::changed("overlay.show");
        self.visible = true;
    }

    /// Hides the overlay, revealing the real windows already sitting at their final frames; only an
    /// alpha change.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.window.setAlphaValue(0.0);
        turns::changed("overlay.hide");
        self.visible = false;
    }

    /// Punches a hole through the overlay over each window ending at one of `ends`, and over the band
    /// of the outset given with it that it wears at rest, replacing the holes there were;
    /// `paint_background` then clears the window's background so the holes show what is below. The
    /// caller names the windows that end wholly on screen. See "Holes" in
    /// `src/animation/docs/animation-smoothness.md`.
    pub fn set_holes(&mut self, ends: &[(OverlayRect, f64)]) {
        if ends.is_empty() {
            self.clear_holes();
            return;
        }
        let Some(clip) = self.window.contentView().and_then(|view| view.layer()) else {
            return;
        };
        let holes: Vec<CGRect> = ends
            .iter()
            .flat_map(|&(end, outset)| {
                std::iter::once(hole_in(end))
                    .chain((outset > 0.0).then(|| band_hole_in(end, outset)))
            })
            .map(|hole| in_view(hole.to_cg(), self.top_band))
            .collect();
        // SAFETY: a shape layer we just made, as the mask of the view's own layer.
        unsafe { clip.setMask(Some(&holes_mask(clip.bounds(), &holes))) };
    }

    /// Fills the holes in again; `paint_background` then puts the window's black background back.
    pub fn clear_holes(&mut self) {
        let Some(clip) = self.window.contentView().and_then(|view| view.layer()) else {
            return;
        };
        if clip.mask().is_none() {
            return;
        }
        // SAFETY: removing the mask this overlay set.
        unsafe { clip.setMask(None) };
    }

    /// The window's background as its holes need it once their mask is committed: clear while it is
    /// holed, black otherwise. Like the alpha, it is the window's, outside the layers' transaction.
    pub fn paint_background(&mut self) {
        let clip = self.window.contentView().and_then(|view| view.layer());
        let holed = clip.is_some_and(|clip| clip.mask().is_some());
        if holed == self.clear {
            return;
        }
        let colour = if holed {
            NSColor::clearColor()
        } else {
            NSColor::blackColor()
        };
        self.window.setBackgroundColor(Some(&colour));
        turns::changed("overlay.background");
        self.clear = holed;
    }

    /// Frees the tile contents and the desktop behind them without destroying the overlay, and the
    /// replica with them. The pictures service keeps the desktop the next flight is dressed with.
    pub fn release(&mut self) {
        self.tree.release();
        let _ = self.mtm;
    }
}

impl Tree {
    fn new(root: Retained<CALayer>, size: CGSize, scale: f64) -> Tree {
        // Not geometryFlipped: that flips a layer's contents too, drawing the desktop upside down.
        let backdrop = CALayer::layer();
        backdrop.setAnchorPoint(CGPoint::new(0.0, 0.0));
        backdrop.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size));
        backdrop.setContentsScale(scale);
        backdrop.setZPosition(BACKDROP_Z);
        root.addSublayer(&backdrop);
        let tint = tint::Tint::new(&root, &backdrop, size, TINT_Z);
        let strip = new_container(&root);
        strip.setPosition(CGPoint::new(0.0, 0.0));
        Tree {
            root,
            size,
            scale,
            backdrop,
            strip,
            containers: HashMap::new(),
            lifted: None,
            tile_layers: HashMap::new(),
            replica: Score::default(),
            crops: Crops::default(),
            outlines: outline::Outlines::default(),
            tint,
            held: None,
            now: f64::NEG_INFINITY,
            dressed_until: None,
        }
    }

    /// The tree as `score` has it: what it lacks dropped, every container at its model with nothing
    /// riding it, every tile dressed from `tiles` and placed at its model in its container, and then
    /// every track it has installed.
    fn pose(&mut self, score: &Score, tiles: &[OverlayTile]) {
        let stale: Vec<WindowId> = self
            .tile_layers
            .keys()
            .copied()
            .filter(|window| score.parent(*window).is_none())
            .collect();
        for window in stale {
            self.drop_node(Node::Tile(window));
        }
        let empty: Vec<GroupKey> = self
            .containers
            .keys()
            .copied()
            .filter(|key| score.model(Node::Container(*key)).is_none())
            .collect();
        for key in empty {
            self.drop_node(Node::Container(key));
        }
        self.replica = score.clone();
        self.place_strip();
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), self.size);
        for key in score.containers() {
            self.open(key);
            let Some(Value::Point(at)) = score.model(Node::Container(key)) else {
                continue;
            };
            for layer in self.layers_of(key) {
                layer.removeAllAnimations();
                layer.setBounds(bounds);
                layer.setPosition(at.to_cg());
            }
        }
        for (window, key) in score.tiles() {
            let Some(tile) = tiles.iter().find(|tile| tile.window == window) else {
                continue;
            };
            let parent = self.parent_layer(window, key);
            let scale = self.scale;
            let entry = self.tile_layers.entry(window).or_insert_with(|| new_tile(&parent));
            self.crops.add(dress(entry, tile, scale));
            entry.reparent(&parent);
            entry.remove_animations();
            if let Some(Value::Rect(frame)) = score.model(Node::Tile(window)) {
                self.place_tile(window, frame);
            }
            match score.model(Node::Lift(window)) {
                Some(lifted) => self.place_lift(window, as_number(lifted)),
                None => self.drop_node(Node::Lift(window)),
            }
        }
        self.tint.clear();
        if let Some(tinted) = score.model(Node::Held) {
            self.tint.set(as_number(tinted));
        }
        let tracks: Vec<Track> = [Node::Strip, Node::Held]
            .into_iter()
            .chain(score.containers().map(Node::Container))
            .chain(score.tiles().map(|(window, _)| Node::Tile(window)))
            .chain(score.tiles().map(|(window, _)| Node::Lift(window)))
            .flat_map(|node| score.tracks(node).cloned().collect::<Vec<_>>())
            .collect();
        for track in &tracks {
            self.now = self.now.max(track.begin());
            self.install(track, tiles, true);
        }
        self.rebank();
        let windows: Vec<WindowId> = self.tile_layers.keys().copied().collect();
        self.dress_cards(&windows);
        self.dress_outlines();
        self.fade_floating();
    }

    /// The floating container and its twin faded the other way to how far the replica holds the
    /// overview.
    fn fade_floating(&self) { tint::fade(&self.replica, &self.layers_of(GroupKey::Floating)) }

    /// Folds each edit into the replica and carries it out.
    fn play(&mut self, edits: &[Edit], tiles: &[OverlayTile]) {
        let mut zoomed = false;
        let mut moved = Vec::new();
        for edit in edits {
            match edit.node() {
                Some(Node::Strip | Node::Held) => zoomed = true,
                Some(Node::Tile(window) | Node::Lift(window)) if !moved.contains(&window) => {
                    moved.push(window)
                }
                _ => {}
            }
            if let Edit::Add(track) = edit {
                self.now = self.now.max(track.begin());
            }
            let new_leg = !matches!(edit, Edit::Add(track) if self.moves_only_the_end(track));
            self.replica.apply(edit);
            match edit {
                Edit::Place(Node::Strip, _) => self.strip.setTransform(self.strip_model()),
                Edit::Place(Node::Container(key), value) => {
                    self.open(*key);
                    for layer in self.layers_of(*key) {
                        layer.setPosition(as_point(*value).to_cg());
                    }
                }
                Edit::Place(Node::Tile(window), value) => {
                    self.place_tile(*window, as_frame(*value))
                }
                Edit::Place(Node::Lift(window), value) => {
                    self.place_lift(*window, as_number(*value))
                }
                Edit::Place(Node::Held, value) => self.tint.set(as_number(*value)),
                Edit::Add(track) => self.install(track, tiles, new_leg),
                Edit::Cut(node, channel) => self.cut(*node, *channel),
                Edit::Ride(window, key) => self.ride(*window, *key, tiles),
                Edit::Drop(node) => self.drop_node(*node),
                Edit::Bank(_) => self.rebank(),
            }
        }
        if zoomed {
            moved = self.tile_layers.keys().copied().collect();
            self.dress_outlines();
        }
        self.dress_cards(&moved);
        if edits.iter().any(tint::fades) {
            self.fade_floating();
        }
    }

    /// The container for `key` and, for the floating container, its twin, made covering the overlay
    /// unless the tree has them.
    fn open(&mut self, key: GroupKey) {
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), self.size);
        if !self.containers.contains_key(&key) {
            let parent = if key == GroupKey::Floating {
                &self.root
            } else {
                &self.strip
            };
            let layer = new_container(parent);
            layer.setBounds(bounds);
            self.containers.insert(key, layer);
        }
        if key == GroupKey::Floating && self.lifted.is_none() {
            let twin = new_container(&self.root);
            twin.setBounds(bounds);
            self.lifted = Some(twin);
        }
    }

    /// Every layer that carries `key`'s translation: its container, and for the floating container its
    /// twin in front of the strip. Anything that moves or animates one moves or animates both.
    fn layers_of(&self, key: GroupKey) -> Vec<Retained<CALayer>> {
        let twin = (key == GroupKey::Floating).then(|| self.lifted.clone()).flatten();
        self.containers.get(&key).cloned().into_iter().chain(twin).collect()
    }

    /// The layer `window`'s tile hangs under riding `key`: the floating container's twin for a
    /// window the replica's banding lifts, its container otherwise, the root with none.
    fn parent_layer(&self, window: WindowId, key: GroupKey) -> Retained<CALayer> {
        let lifted = key == GroupKey::Floating && self.replica.banding().lifted.contains(&window);
        let twin = self.lifted.as_ref().filter(|_| lifted);
        twin.or(self.containers.get(&key)).unwrap_or(&self.root).clone()
    }

    /// A tile into `key`'s container, made and dressed from `tiles` when it is first seen.
    fn ride(&mut self, window: WindowId, key: GroupKey, tiles: &[OverlayTile]) {
        let parent = self.parent_layer(window, key);
        if !self.tile_layers.contains_key(&window) {
            let Some(tile) = tiles.iter().find(|tile| tile.window == window) else {
                return;
            };
            let mut entry = new_tile(&parent);
            self.crops.add(dress(&mut entry, tile, self.scale));
            self.tile_layers.insert(window, entry);
        }
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        entry.reparent(&parent);
    }

    /// Puts `window`'s tile at `frame` in its container, at the depth the replica's banding gives it.
    fn place_tile(&self, window: WindowId, frame: GroupRect) {
        let Some(entry) = self.tile_layers.get(&window) else {
            return;
        };
        let within = self.replica.banding().within.get(&window).copied().unwrap_or(0);
        place_tile(entry, frame, -(within as f64));
    }

    /// `window`'s tile lifted `share` of the way once nothing plays on its lift.
    fn place_lift(&mut self, window: WindowId, share: f64) {
        let under = self.lifts_under();
        if let Some(entry) = self.tile_layers.get_mut(&window) {
            entry.lift(under).set(share);
        }
    }

    /// The zoom every lift is cast under.
    fn lifts_under(&self) -> f64 { self.held.unwrap_or(1.0) }

    /// Whether `track` is the replica's track on its node with only its end moved, as a snap makes it.
    fn moves_only_the_end(&self, track: &Track) -> bool {
        let end = track.keys()[track.keys().len() - 1].1;
        self.replica
            .track(track.node(), track.channel())
            .is_some_and(|playing| playing.ending_at(end) == *track)
    }

    /// Installs `track` on the layers that draw its node. A container's movement begins every accent
    /// riding it again from the replica; a tile's new leg first picks its crop's anchors from its
    /// first and last key, laying the grid at the tile's model again when they change, and judges
    /// its content mode again against them, setting it only when it changes, while the leg it is on
    /// with its end moved keeps the tile drawn as it was. A tile carrying a picture from the corner
    /// of its leg in two steps flies its own up to the corner and that one from it, clipped if either
    /// is, and how long the ground that one is cut onto took to read counts in `crops`.
    fn install(&mut self, track: &Track, tiles: &[OverlayTile], new_leg: bool) {
        match track.node() {
            Node::Strip => {
                let key = NSString::from_str(STRIP_ZOOM_KEY);
                if track.seconds() <= 0.0 {
                    self.strip.removeAnimationForKey(&key);
                    return;
                }
                let zoom = animation(track, "transform", zoom_object);
                self.strip.addAnimation_forKey(&zoom, Some(&key));
            }
            Node::Container(key) => {
                let riding: Vec<Track> = match track.channel() {
                    Channel::Base => self.replica.tracks(track.node()).cloned().collect(),
                    Channel::Bounce | Channel::Nudge => Vec::new(),
                };
                for layer in self.layers_of(key) {
                    install_container_track(&layer, track, &riding);
                }
            }
            Node::Tile(window) => {
                assert_eq!(track.channel(), Channel::Base, "no accent rides a tile");
                let scale = self.scale;
                let model = self.replica.model(Node::Tile(window)).map(as_frame);
                let Some(entry) = self.tile_layers.get_mut(&window) else {
                    return;
                };
                let keys = track.keys();
                let (first, last) = (as_frame(keys[0].1), as_frame(keys[keys.len() - 1].1));
                let anchors = Anchors::of_leg(first, last);
                if new_leg && anchors != entry.anchors {
                    entry.anchors = anchors;
                    if let Some(model) = model {
                        layout_crop_grid(entry, model.size.respace());
                    }
                }
                let tile = tiles.iter().find(|tile| tile.window == window);
                if new_leg && let Some(tile) = tile {
                    let snapshot = &tile.snapshot;
                    let (covered, source) = (snapshot.coverage.covered, snapshot.source);
                    let mode = content_mode(covered, source, first.size, last.size);
                    if (mode == ContentMode::Crop) != entry.crop_of.is_some() {
                        let size = last.size.respace();
                        self.crops.add(set_tile_content(entry, &tile.snapshot, mode, size, scale));
                    }
                }
                let corner = tile.and_then(|tile| {
                    let after = tile.from_corner.as_ref()?;
                    clip_corners(entry, tile.clipped);
                    Some(Corner { before: &tile.snapshot, after })
                });
                self.crops.add(install_tile_track(entry, track, corner));
            }
            Node::Lift(window) => {
                let under = self.lifts_under();
                let Some(entry) = self.tile_layers.get_mut(&window) else {
                    return;
                };
                let lift = entry.lift(under);
                if track.seconds() <= 0.0 {
                    lift.cut();
                    return;
                }
                let opacity = |value| number_object(lift::opacity(as_number(value)));
                lift.play(&animation(track, "shadowOpacity", opacity));
            }
            Node::Held => {
                if track.seconds() <= 0.0 {
                    self.tint.cut();
                    return;
                }
                self.tint.play(track);
            }
        }
    }

    /// Takes away what `node`'s track on `channel` installed.
    fn cut(&self, node: Node, channel: Channel) {
        match node {
            Node::Strip if channel == Channel::Base => {
                self.strip.removeAnimationForKey(&NSString::from_str(STRIP_ZOOM_KEY))
            }
            Node::Strip => {}
            Node::Container(key) => {
                let key_path = NSString::from_str(container_key(channel));
                for layer in self.layers_of(key) {
                    layer.removeAnimationForKey(&key_path);
                }
            }
            Node::Tile(window) => {
                assert_eq!(channel, Channel::Base, "no accent rides a tile");
                let Some(entry) = self.tile_layers.get(&window) else {
                    return;
                };
                for layer in entry.layers() {
                    remove_under(layer, &[TILE_ANIMATION_KEY]);
                }
                remove_resize(entry);
            }
            Node::Lift(window) => {
                if let Some(lift) = self.tile_layers.get(&window).and_then(|e| e.lift.as_ref()) {
                    lift.cut();
                }
            }
            Node::Held => self.tint.cut(),
        }
    }

    /// Takes a tile's layers out, or a container's with its twin.
    fn drop_node(&mut self, node: Node) {
        match node {
            Node::Strip => self.place_strip(),
            Node::Tile(window) => {
                if let Some(entry) = self.tile_layers.remove(&window) {
                    entry.remove();
                }
            }
            Node::Lift(window) => {
                let entry = self.tile_layers.get_mut(&window);
                if let Some(lift) = entry.and_then(|entry| entry.lift.take()) {
                    lift.layer().removeFromSuperlayer();
                }
            }
            Node::Held => self.tint.clear(),
            Node::Container(key) => {
                for layer in self.layers_of(key) {
                    layer.removeFromSuperlayer();
                }
                self.containers.remove(&key);
                if key == GroupKey::Floating {
                    self.lifted = None;
                }
            }
        }
    }

    /// Writes every container's and tile's `zPosition` from the replica's banding, and hangs each
    /// floating tile under the floating container or its twin. A hard cut: z does not interpolate.
    fn rebank(&self) {
        let banding = self.replica.banding();
        let off_strip = banding.focus_off_strip;
        for (key, layer) in &self.containers {
            let z = match key {
                GroupKey::Floating => container_z(Band::Behind, off_strip),
                key => {
                    let index = banding
                        .group_order
                        .iter()
                        .position(|k| k == key)
                        .unwrap_or(banding.group_order.len());
                    container_z(Band::Strip, off_strip) - index as f64 * 0.25
                }
            };
            layer.setZPosition(z);
        }
        if let Some(lifted) = self.lifted.as_ref() {
            lifted.setZPosition(container_z(Band::Lifted, off_strip));
        }
        self.strip.setZPosition(container_z(Band::Strip, off_strip));
        for (window, tile) in &self.tile_layers {
            if self.replica.parent(*window) == Some(GroupKey::Floating) {
                tile.reparent(&self.parent_layer(*window, GroupKey::Floating));
            }
            let Some(&within) = banding.within.get(window) else {
                continue;
            };
            tile.stack(-(within as f64));
        }
    }

    /// `window`'s tile drawn focused or not.
    fn turn(&mut self, window: WindowId, focused: bool) {
        if let Some(entry) = self.tile_layers.get_mut(&window) {
            entry.focused = focused;
            entry.decorations.light(entry.worn.style(focused));
        }
    }

    /// `window`'s tile painted as `worn` has its ring and border, with the decoration it was
    /// dressed with: a band's layer is made only as a tile is dressed.
    fn wear(&mut self, window: WindowId, worn: Worn) {
        if let Some(entry) = self.tile_layers.get_mut(&window) {
            entry.worn = Worn {
                decoration: entry.worn.decoration,
                ..worn
            };
            entry.decorations.show(entry.worn.style(entry.focused));
        }
        self.dress_cards(&[window]);
    }

    /// `window`'s tile wearing `worn` with its decoration too, unlike `wear`: each band it wears
    /// made or taken off, and every one laid again around the tile's model at its width.
    fn rewear(&mut self, window: WindowId, worn: Worn) {
        let scale = self.scale;
        let model = self.replica.model(Node::Tile(window)).map(as_frame);
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        entry.worn = worn;
        entry.decorations.wear(&worn.decoration, scale);
        if let Some(model) = model {
            entry.decorations.place(model.size.respace());
        }
        entry.decorations.show(worn.style(entry.focused));
        self.dress_cards(&[window]);
    }

    /// `picture` onto `window`'s tile, `clipped` or whole, re-keyed along the tile's own track while
    /// it plays at `at`.
    fn swap(&mut self, window: WindowId, picture: &Picture, clipped: bool, at: f64) {
        self.now = self.now.max(at);
        let node = Node::Tile(window);
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        let Some(Value::Rect(frame)) = self.replica.model(node) else {
            return;
        };
        let playing =
            self.replica.track(node, Channel::Base).filter(|track| track.at(at).is_some());
        clip_corners(entry, clipped);
        self.crops.add(swap_onto(
            entry,
            picture,
            frame.size.respace(),
            playing,
            self.scale,
        ));
        self.dress_cards(&[window]);
    }

    /// What the drain played since the last take spent on crop-drawn tiles, counted again from none.
    fn take_crops(&mut self) -> Crops { std::mem::take(&mut self.crops) }

    /// The desktop drawn behind the tiles, at its own covered size so it stays in register with the
    /// real one.
    fn set_backdrop(&mut self, snapshot: &Picture) {
        self.backdrop.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            snapshot.coverage.covered.to_cg(),
        ));
        self.backdrop.setContentsScale(self.scale);
        set_layer_contents(&self.backdrop, snapshot);
        self.backdrop.setHidden(false);
    }

    /// Every tile, container and the desktop let go of, and the replica with them.
    fn release(&mut self) {
        self.outlines.clear();
        self.tint.clear();
        self.held = None;
        self.dressed_until = None;
        clear_contents(&self.backdrop);
        self.backdrop.setHidden(true);
        for (_, entry) in self.tile_layers.drain() {
            entry.remove();
        }
        for (_, container) in self.containers.drain() {
            container.removeFromSuperlayer();
        }
        if let Some(lifted) = self.lifted.take() {
            lifted.removeFromSuperlayer();
        }
        self.replica = Score::default();
        self.place_strip();
    }

    /// The strip layer drawn at the replica's zoom, at rest without one, nothing playing on it.
    fn place_strip(&self) {
        self.strip.removeAnimationForKey(&NSString::from_str(STRIP_ZOOM_KEY));
        self.strip.setTransform(self.strip_model());
    }

    /// The replica's zoom as the strip layer's transform, at rest without one.
    fn strip_model(&self) -> CATransform3D {
        match self.replica.model(Node::Strip) {
            Some(Value::Zoom(zoom)) => zoom_transform(zoom),
            _ => zoom_transform(Zoom::REST),
        }
    }
}

/// Dresses a tile for `tile`: its picture, fitted to how it starts and ends and clipped or whole,
/// its focus, its decoration and its radius, and how long its ground took to read if it is
/// crop-drawn. A tile posed again within its flight keeps its layers and is dressed again, so every
/// per-window property is written.
fn dress(entry: &mut Tile, tile: &OverlayTile, scale: f64) -> Option<Duration> {
    entry.decorations.radius = tile.radius;
    entry.worn = tile.worn;
    entry.focused = tile.focused;
    entry.picture.setContentsScale(scale);
    entry.decorations.wear(&entry.worn.decoration, scale);
    clip_corners(entry, tile.clipped);
    let (covered, source) = (tile.snapshot.coverage.covered, tile.snapshot.source);
    let mode = content_mode(covered, source, tile.from.size, tile.to.size);
    entry.anchors = Anchors::of_leg(tile.from, tile.to);
    let ground = set_tile_content(entry, &tile.snapshot, mode, tile.from.size.respace(), scale);
    entry.decorations.show(tile.worn.style(tile.focused));
    entry.picture.setHidden(false);
    ground
}

/// Puts `snapshot` on a tile, leaving its movement alone: a hard cut, on purpose, since a crossfade
/// pulses a translucent window's net opacity (see "Mid-flight passes" in
/// `src/transitions/docs/coordination.md`). The tile is judged again against `snapshot` (`land`):
/// drawn stretched where `snapshot` fits both ends of `playing`, its own track while it plays, or
/// `size`, its model, with none, so a crop-drawn tile riding its container cut from a stale picture
/// onto one that fits draws it on its picture layer alone and reads no ground; cut onto the crop
/// grid otherwise, as a stand-in's real picture landing on a resize is, the grid laid again for
/// `size` and re-keyed along `playing`, so the frame keeps its curve and only the pixels cut,
/// `snapshot` on from now on a leg in two steps too, whatever it was to cut to at the corner. A
/// resize's picture fits at most one end of its leg, so its tile stays cropped. Returns how long the
/// ground took to read when the tile is crop-drawn. Callers hold the transaction.
fn swap_onto(
    entry: &mut Tile,
    snapshot: &Picture,
    size: TileSize,
    playing: Option<&Track>,
    scale: f64,
) -> Option<Duration> {
    let ground = land(entry, snapshot, size, playing, scale);
    if entry.crop_of.is_some()
        && let Some(track) = playing
    {
        install_tile_track(entry, track, None);
    }
    ground
}

/// Puts `snapshot` on a tile as it is drawn on `playing`, or at `size` with none: stretched where
/// it fits both ends (`content_mode`), cut onto the crop grid of a crop-drawn tile (`cut_onto`),
/// and laid on a crop grid of its own otherwise. Returns how long the ground took to read when the
/// tile is crop-drawn.
fn land(
    entry: &mut Tile,
    snapshot: &Picture,
    size: TileSize,
    playing: Option<&Track>,
    scale: f64,
) -> Option<Duration> {
    let (from, to) = playing.map_or((size, size), |track| {
        let keys = track.keys();
        let end = |at: usize| -> TileSize { as_frame(keys[at].1).size.respace() };
        (end(0), end(keys.len() - 1))
    });
    let mode = content_mode(snapshot.coverage.covered, snapshot.source, from, to);
    match mode {
        ContentMode::Crop if entry.crop_of.is_some() && entry.crop_grid.is_some() => {
            cut_onto(entry, snapshot, size)
        }
        _ => set_tile_content(entry, snapshot, mode, size, scale),
    }
}

/// Cuts a crop-drawn tile onto `snapshot`: each piece's picture and the ground's colour become its,
/// and the grid is laid against it for `size`, the grid built against the old picture drawing the new
/// one squashed. Returns how long the ground took to read (`fill_ground`).
fn cut_onto(entry: &mut Tile, snapshot: &Picture, size: TileSize) -> Option<Duration> {
    let ground = entry.crop_grid.as_ref().map(|grid| {
        for piece in &grid.pieces {
            set_layer_contents(piece, snapshot);
        }
        fill_ground(grid, snapshot)
    });
    crop_to(entry, snapshot);
    layout_crop_grid(entry, size);
    ground
}

/// The bar's picture at the strip's origin, at the size it covers, never stretched. See "The bar
/// has to be captured on its own" in `src/pictures/docs/capture-research.md`.
fn bar_frame(strip: CGRect, covered: CGSize) -> CGRect { CGRect::new(strip.origin, covered) }

/// A tile: its decorations, then the picture. The picture's contents cannot spill, and it clips
/// only to round its corners (`clip_corners`).
fn new_tile(container: &CALayer) -> Tile {
    let decorations = DecorationLayers::new(container);
    let picture = CALayer::layer();
    picture.setAnchorPoint(CGPoint::new(0.0, 0.0));
    round_continuous(&picture);
    // SAFETY: Core Animation's own filter-name constants.
    unsafe {
        picture.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
        picture.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
    }
    container.addSublayer(&picture);

    Tile {
        picture,
        decorations,
        worn: Worn::default(),
        focused: false,
        crop_grid: None,
        crop_of: None,
        anchors: Anchors::default(),
        region: WHOLE,
        lift: None,
        card: None,
        clipped: false,
    }
}

impl Tile {
    /// The layers that ride the tile's frame: the picture, the caster, the hairline, the lift's
    /// caster and the card's wash.
    fn layers(&self) -> Vec<&CALayer> {
        let [shadow, hairline] = self.decorations.frame_layers();
        let mut layers = vec![&*self.picture, shadow, hairline];
        layers.extend(self.lift.as_ref().map(Lift::layer));
        layers.extend(self.card.as_ref().map(card::Card::layer));
        layers
    }

    /// The lift's caster, made beside the tile's own cast under a zoom by `under` and laid with it
    /// unless it has one.
    fn lift(&mut self, under: f64) -> &Lift {
        let (picture, decorations) = (&self.picture, &self.decorations);
        self.lift.get_or_insert_with(|| {
            let container = picture.superlayer().unwrap_or_else(|| picture.clone());
            let lift = Lift::new(&container, under);
            let frame = picture.frame();
            let size = TileSize::from_cg(frame.size);
            lift.layer().setFrame(frame);
            lift.shape(size, decorations.corner_at(size));
            lift.stack(picture.zPosition());
            resize::ride_as(lift.layer(), picture);
            lift
        })
    }

    /// Its lift, if it has one, cast under a zoom by `under`.
    fn recast_lift(&mut self, under: f64) {
        let size = TileSize::from_cg(self.picture.frame().size);
        let corner = self.decorations.corner_at(size);
        if let Some(lift) = &mut self.lift {
            lift.recast(under, size, corner);
        }
    }

    /// Moves the tile's layers under `container` unless they are already there.
    fn reparent(&self, container: &CALayer) {
        for layer in self.layers() {
            reparent(layer, container);
        }
    }

    fn remove(&self) {
        for layer in self.layers() {
            layer.removeFromSuperlayer();
        }
    }

    /// Takes every animation off every layer of the tile.
    fn remove_animations(&self) {
        self.picture.removeAllAnimations();
        self.decorations.remove_animations();
        if let Some(lift) = &self.lift {
            lift.remove_animations();
        }
        if let Some(card) = &self.card {
            card.remove_animations();
        }
        if let Some(grid) = &self.crop_grid {
            for layer in grid.layers() {
                layer.removeAllAnimations();
            }
        }
    }

    /// Draws the tile at depth `z`, its decorations stacked against its picture.
    fn stack(&self, z: f64) {
        self.picture.setZPosition(z);
        self.decorations.stack(z);
        if let Some(lift) = &self.lift {
            lift.stack(z);
        }
        if let Some(card) = &self.card {
            card.stack(z);
        }
    }
}

/// Clips a tile's picture, with the crop pieces under it, to its corner, or not at all; a card is
/// always clipped. The corner is the hairline's, laid at every placement (`place_tile`); unclipped
/// it draws nothing, the picture having no background or border.
fn clip_corners(tile: &mut Tile, clipped: bool) {
    tile.clipped = clipped;
    tile.picture.setMasksToBounds(clipped || tile.card.is_some());
}

/// Puts a tile's layers at `frame`, stacked at `z`, its decorations shaped for its size and its
/// picture's corner rounded with them. The crop grid is laid again only when the size changes.
fn place_tile(tile: &Tile, frame: GroupRect, z: f64) {
    let size: TileSize = frame.size.respace();
    let resized = tile.picture.bounds().size != size.to_cg();
    for layer in tile.layers() {
        layer.setFrame(frame.to_cg());
    }
    tile.stack(z);
    tile.decorations.place(size);
    tile.picture.setCornerRadius(tile.decorations.corner_at(size));
    if let Some(lift) = &tile.lift {
        lift.shape(size, tile.decorations.corner_at(size));
    }
    if resized {
        layout_crop_grid(tile, size);
    }
}

/// Points a tile's contents at `mode`, and returns how long the ground took to read on the crop
/// grid. Always set explicitly: pooled tile layers would otherwise carry a crop into the next
/// flight's stretch.
fn set_tile_content(
    entry: &mut Tile,
    snapshot: &Picture,
    mode: ContentMode,
    frame: TileSize,
    scale: f64,
) -> Option<Duration> {
    match mode {
        ContentMode::Crop => {
            clear_contents(&entry.picture);
            let grid = entry.crop_grid.get_or_insert_with(|| new_crop_grid(&entry.picture));
            for piece in &grid.pieces {
                piece.setContentsScale(scale);
                set_layer_contents(piece, snapshot);
            }
            let ground = fill_ground(grid, snapshot);
            for layer in grid.layers() {
                layer.setHidden(false);
            }
            crop_to(entry, snapshot);
            layout_crop_grid(entry, frame);
            Some(ground)
        }
        ContentMode::Stretch => {
            entry.crop_of = None;
            if let Some(grid) = &entry.crop_grid {
                for layer in grid.layers() {
                    layer.setHidden(true);
                }
            }
            set_layer_contents(&entry.picture, snapshot);
            None
        }
    }
}

/// Fills a crop grid's ground layers with `snapshot`'s ground, and how long reading it took: the
/// first fill of a picture reads it (`Picture::ground`), every later one finds it read.
fn fill_ground(grid: &CropGrid, snapshot: &Picture) -> Duration {
    let began = Instant::now();
    snapshot.ground();
    let read = began.elapsed();
    for layer in &grid.ground {
        set_layer_ground(layer, snapshot);
    }
    read
}

/// Points a crop-drawn tile at `snapshot`: the size its pieces cut, and the part of the contents
/// image that is the picture.
fn crop_to(entry: &mut Tile, snapshot: &Picture) {
    entry.crop_of = Some(snapshot.coverage.covered.respace());
    entry.region = snapshot.image.unit_rect();
}

/// Lays the crop grid out for the tile's current frame size.
fn layout_crop_grid(entry: &Tile, frame: TileSize) {
    let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) else {
        return;
    };
    let laid = crop(picture, frame, entry.anchors);
    for (layer, piece) in grid.pieces.iter().zip(laid.pieces) {
        layer.setFrame(piece.frame.to_cg());
        layer.setContentsRect(within(entry.region, piece.contents).to_cg());
    }
    for (layer, span) in grid.ground.iter().zip(laid.ground) {
        layer.setFrame(span.to_cg());
    }
}

/// Creates the nine crop pieces and the three ground layers under a tile's picture layer.
fn new_crop_grid(container: &CALayer) -> CropGrid {
    let layer = || {
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        container.addSublayer(&layer);
        layer
    };
    let pieces = std::array::from_fn(|_| {
        let piece = layer();
        // SAFETY: Core Animation's own filter-name constants.
        unsafe {
            piece.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
            piece.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
        }
        piece
    });
    CropGrid {
        pieces,
        ground: std::array::from_fn(|_| layer()),
    }
}

/// An empty container anchored at its origin, under `parent`.
fn new_container(parent: &CALayer) -> Retained<CALayer> {
    let layer = CALayer::layer();
    layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
    layer.setMasksToBounds(false);
    parent.addSublayer(&layer);
    layer
}

/// Moves `layer` under `container` unless it is already there.
fn reparent(layer: &CALayer, container: &CALayer) {
    let already = layer.superlayer().is_some_and(|current| {
        std::ptr::eq(&*current as *const CALayer, container as *const CALayer)
    });
    if already {
        return;
    }
    layer.removeFromSuperlayer();
    container.addSublayer(layer);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use objc2_core_foundation::CFRetained;
    use rini_geometry::space::{FromCg, OverlayVector, Point, Rect, Size};

    use super::resize::TileResize;
    use super::*;
    use crate::animation::domain::motion::bounce::Bounce;
    use crate::animation::domain::motion::easing::{EASE_IN_OUT, MOTION_CURVE};
    use crate::animation::domain::motion::glide::Leg;
    use crate::animation::domain::motion::nudge::nudge_track;
    use crate::animation::domain::motion::plan::Resolved;
    use crate::animation::domain::motion::score::SAMPLE_STEP;
    use crate::animation::domain::motion::tile::Anchor;
    use crate::decorations::domain::shape::{self, Fit, shadow_mask_frame};
    use crate::decorations::domain::stack::SHADOW_Z;
    use crate::decorations::platform::layers::{
        Resize, assert_core_animation_held, hold_core_animation, shadow_mask_path, silhouette_path,
    };
    use crate::pictures::platform::layer::test_components;

    /// The window starts a band below the display's top and loses that much height; the canvas holds
    /// the whole display, lifted by the band so the top is clipped. With no band the two are the
    /// display itself.
    #[test]
    fn the_window_sits_below_the_band_and_the_canvas_spans_the_display() {
        let display = CGRect::new(CGPoint::new(-670.0, -1692.0), CGSize::new(3008.0, 1692.0));
        let window = window_rect(display, 32.0);
        assert_eq!(window.origin, CGPoint::new(-670.0, -1660.0));
        assert_eq!(window.size, CGSize::new(3008.0, 1660.0));
        let canvas = canvas_rect(display, 32.0);
        assert_eq!(canvas.origin, CGPoint::new(0.0, -32.0));
        assert_eq!(canvas.size, display.size);

        assert_eq!(window_rect(display, 0.0), display);
        assert_eq!(canvas_rect(display, 0.0).origin, CGPoint::new(0.0, 0.0));
    }

    /// A display names the overlay's layout by its frame, its band and its scale: the same three move
    /// nothing, and any other one moves the window or lays it out again.
    #[test]
    fn a_display_moves_the_overlay_unless_its_frame_band_and_scale_are_the_ones_held() {
        let display: ScreenRect = typed(0.0, 0.0, 3008.0, 1692.0);
        let other: ScreenRect = typed(3008.0, 0.0, 1920.0, 1080.0);
        assert!(same_layout((display, 32.0, 2.0), (display, 32.0, 2.0)));
        assert!(
            !same_layout((display, 32.0, 2.0), (other, 32.0, 2.0)),
            "another display"
        );
        assert!(
            !same_layout((display, 32.0, 2.0), (display, 0.0, 2.0)),
            "the bar's band gone"
        );
        assert!(
            !same_layout((display, 32.0, 2.0), (display, 32.0, 1.0)),
            "another scale"
        );
    }

    /// A picture of the overlay alone starts where its window does: the display's top-left, a band
    /// down.
    #[test]
    fn the_window_starts_a_band_below_the_displays_top_left() {
        let display: ScreenRect = typed(-670.0, -1692.0, 3008.0, 1692.0);
        assert_eq!(window_origin(display, 32.0), ScreenPoint::new(-670.0, -1660.0));
        assert_eq!(window_origin(display, 0.0), ScreenPoint::new(-670.0, -1692.0));
    }

    /// A handover check pictures the overlay by its number, which AppKit gives as 0 or less until the
    /// window server has numbered the window.
    #[test]
    fn only_a_numbered_window_has_a_server_id() {
        assert_eq!(numbered(4812), Some(WindowServerId::new(4812)));
        assert_eq!(numbered(0), None);
        assert_eq!(numbered(-1), None);
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn typed<U>(x: f64, y: f64, w: f64, h: f64) -> Rect<U> {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    /// `timing_function` hands Core Animation the four numbers a `Leg` and the score evaluate, so
    /// they must solve the same Bezier: the motion curve, and the ease-in-out a stepping move's strip
    /// crosses on.
    #[test]
    fn a_leg_the_score_and_the_render_server_run_one_curve() {
        for step in 0..=1000 {
            let s = step as f64 / 1000.0;
            let (x, y) = MOTION_CURVE.at(s);
            assert!(
                (MOTION_CURVE.ease(x) - y).abs() < 1e-6,
                "the curves diverge at s={s}: ease({x})={} y={y}",
                MOTION_CURVE.ease(x)
            );
            let (x, y) = EASE_IN_OUT.at(s);
            assert!(
                (EASE_IN_OUT.ease(x) - y).abs() < 1e-6,
                "the ease-in-out diverges at s={s}"
            );
        }
        // Ease-out cubic has a closed form: the thirds Bezier.
        let cubic = CubicBezier {
            x1: 1.0 / 3.0,
            y1: 1.0,
            x2: 2.0 / 3.0,
            y2: 1.0,
        };
        for step in 0..=100 {
            let t = step as f64 / 100.0;
            assert!((cubic.ease(t) - (1.0 - (1.0 - t).powi(3))).abs() < 1e-6, "t={t}");
        }
    }

    #[test]
    fn every_possible_tile_draws_between_the_backdrop_and_the_bar() {
        use crate::animation::domain::motion::z_group::{MAX_TILE_DEPTH, Stacked, stack};
        use crate::windows::domain::raise_order::StackGroup;
        let app = |pid: i32, idx: u32| rini_core::ids::WindowId {
            pid,
            idx: std::num::NonZeroU32::new(idx).unwrap(),
        };
        // The deepest a tile can go: unreported, off the strip, behind it, with focus off the strip so
        // there are three bands in use.
        let windows = [
            Stacked {
                window: app(1, 1),
                group: StackGroup::Floating,
                server_order: Some(0),
            },
            Stacked {
                window: app(2, 2),
                group: StackGroup::Tiled,
                server_order: Some(1),
            },
            Stacked {
                window: app(3, 3),
                group: StackGroup::Floating,
                server_order: None,
            },
        ];
        let placements = stack(&windows, Some(app(1, 1)));
        let deepest = placements[2].depth;
        let shallowest = placements[0].depth;
        assert_eq!(deepest, MAX_TILE_DEPTH, "MAX_TILE_DEPTH is the real bound");
        assert!(
            -(deepest as f64) > BACKDROP_Z,
            "the deepest tile clears the backdrop"
        );
        assert!(
            -(shallowest as f64) < BAR_Z,
            "the shallowest tile stays under the bar"
        );
        // The shadow caster sits behind its picture.
        assert!(-(deepest as f64) + SHADOW_Z > BACKDROP_Z);
    }

    #[test]
    fn the_bar_is_drawn_over_every_tile_and_the_desktop_under_them() {
        let deepest_strip_tile = -64.0;
        const { assert!(BAR_Z > 0.0) };
        assert!(
            BACKDROP_Z < deepest_strip_tile,
            "the desktop is under every tile"
        );
    }

    #[test]
    fn the_bar_is_drawn_at_the_strip_it_occupies() {
        let placed = bar_frame(rect(0.0, 0.0, 1728.0, 32.0), CGSize::new(1728.0, 32.0));
        assert_eq!(placed, rect(0.0, 0.0, 1728.0, 32.0));
    }

    #[test]
    fn a_bar_that_does_not_start_at_the_corner_is_drawn_where_it_is() {
        let placed = bar_frame(rect(217.0, 8.0, 1504.0, 24.0), CGSize::new(1504.0, 24.0));
        assert_eq!(placed.origin.x, 217.0);
        assert_eq!(placed.origin.y, 8.0);
    }

    #[test]
    fn the_bar_picture_is_never_stretched_to_the_strip() {
        let placed = bar_frame(rect(0.0, 0.0, 1728.0, 32.0), CGSize::new(1504.0, 32.0));
        assert_eq!(placed.size.width, 1504.0);
        assert_eq!(placed.size.height, 32.0);
    }

    /// Core Animation replaces an animation added under a key already in use, so a nudge sharing
    /// the bounce's key would stop a bounce playing, and the other way round.
    #[test]
    fn the_nudge_and_the_bounce_ride_under_keys_of_their_own() {
        assert_ne!(NUDGE_ANIMATION_KEY, BOUNCE_ANIMATION_KEY);
        assert_ne!(NUDGE_ANIMATION_KEY, GROUP_ANIMATION_KEY);
        assert_ne!(BOUNCE_ANIMATION_KEY, GROUP_ANIMATION_KEY);
    }

    /// The `float[2]` a control point is written into: passed as a pointer, encoded as the array.
    #[repr(transparent)]
    struct Pair(*mut f32);

    // SAFETY: a C array parameter is passed as a pointer to its first element.
    unsafe impl objc2::encode::Encode for Pair {
        const ENCODING: objc2::encode::Encoding =
            objc2::encode::Encoding::Array(2, &<f32 as objc2::encode::Encode>::ENCODING);
    }

    /// The four numbers Core Animation runs a timing function on.
    fn control_points(function: &CAMediaTimingFunction) -> [f32; 4] {
        let (mut first, mut second) = ([0.0_f32; 2], [0.0_f32; 2]);
        // SAFETY: indices 1 and 2 are the two control points, each written as two floats.
        unsafe {
            let _: () = msg_send![function, getControlPointAtIndex: 1_usize, values: Pair(first.as_mut_ptr())];
            let _: () = msg_send![function, getControlPointAtIndex: 2_usize, values: Pair(second.as_mut_ptr())];
        }
        [first[0], first[1], second[0], second[1]]
    }

    fn bezier_points(curve: Curve) -> [f32; 4] {
        match curve {
            Curve::Bezier(c) => [c.x1 as f32, c.y1 as f32, c.x2 as f32, c.y2 as f32],
            Curve::Linear => [0.0, 0.0, 1.0, 1.0],
        }
    }

    /// An installed animation read back as the track it plays: the class and whether it has key
    /// times say its form, and each value is as `read` has it.
    #[derive(Debug, PartialEq)]
    struct Installed<T> {
        form: Form,
        begin: f64,
        seconds: f64,
        additive: bool,
        keys: Vec<(f64, T)>,
        curves: Vec<[f32; 4]>,
    }

    fn installed<T>(layer: &CALayer, key: &str, read: impl Fn(&AnyObject) -> T) -> Installed<T> {
        assert_core_animation_held();
        // SAFETY: a read of the layer's own animation.
        let animation = unsafe { layer.animationForKey(&NSString::from_str(key)) }
            .unwrap_or_else(|| panic!("nothing under {key}"));
        let (begin, seconds) = (animation.beginTime(), animation.duration());
        let animation = animation.downcast::<CAPropertyAnimation>().expect("a property animation");
        let additive = animation.isAdditive();
        let animation = match animation.downcast::<CABasicAnimation>() {
            Ok(basic) => {
                let value = |v: Option<Retained<AnyObject>>| read(&v.expect("a value"));
                return Installed {
                    form: Form::Between,
                    begin,
                    seconds,
                    additive,
                    keys: vec![
                        (0.0, value(basic.fromValue())),
                        (1.0, value(basic.toValue())),
                    ],
                    curves: vec![control_points(&basic.timingFunction().expect("a curve"))],
                };
            }
            Err(other) => other,
        };
        let keyframes = animation.downcast::<CAKeyframeAnimation>().expect("keyframes");
        let values: Vec<T> = keyframes.values().expect("values").iter().map(|v| read(&v)).collect();
        let last = values.len().saturating_sub(1).max(1) as f64;
        let (form, times): (Form, Vec<f64>) = match keyframes.keyTimes() {
            Some(times) => (Form::Keyframes, times.iter().map(|t| t.doubleValue()).collect()),
            None => (
                Form::Samples,
                (0..values.len()).map(|k| k as f64 / last).collect(),
            ),
        };
        let curves = match keyframes.timingFunctions() {
            Some(functions) => functions.iter().map(|f| control_points(&f)).collect(),
            None => vec![bezier_points(Curve::Linear); values.len() - 1],
        };
        Installed {
            form,
            begin,
            seconds,
            additive,
            keys: times.into_iter().zip(values).collect(),
            curves,
        }
    }

    fn expected<T>(track: &Track, map: impl Fn(Value) -> T) -> Installed<T> {
        Installed {
            form: track.form(),
            begin: track.begin(),
            seconds: track.seconds(),
            additive: track.channel().is_additive(),
            keys: track.keys().iter().map(|(t, v)| (*t, map(*v))).collect(),
            curves: track.curves().iter().map(|c| bezier_points(*c)).collect(),
        }
    }

    fn point_in(value: &AnyObject) -> CGPoint {
        // SAFETY: the value was made from a point.
        unsafe { value.downcast_ref::<NSValue>().expect("an NSValue").pointValue() }
    }

    fn rect_in(value: &AnyObject) -> CGRect {
        // SAFETY: the value was made from a rect.
        unsafe { value.downcast_ref::<NSValue>().expect("an NSValue").rectValue() }
    }

    fn number_in(value: &AnyObject) -> f64 {
        value.downcast_ref::<NSNumber>().expect("a number").doubleValue()
    }

    /// The bounding box of a path handed over as an object.
    fn path_in(value: &AnyObject) -> CGRect {
        objc2_core_graphics::CGPath::bounding_box(Some(&path_of(value)))
    }

    /// The path an animation value holds, compared whole, corners included, where its bounding
    /// box cannot see a radius.
    fn path_of(value: &AnyObject) -> CFRetained<objc2_core_graphics::CGPath> {
        // SAFETY: the value was made from a CGPath, which is what it is read back as.
        let path: &objc2_core_graphics::CGPath =
            unsafe { &*(value as *const AnyObject as *const objc2_core_graphics::CGPath) };
        CFRetained::from(path)
    }

    /// The shadow mask's path at `size` and `corner`, as a plain path, the type a layer hands back.
    fn mask_path(size: TileSize, corner: f64) -> CFRetained<objc2_core_graphics::CGPath> {
        CFRetained::from(&*shadow_mask_path(size, corner) as &objc2_core_graphics::CGPath)
    }

    fn container() -> Node { Node::Container(GroupKey::STILL) }

    fn container_value(value: &AnyObject) -> Value {
        Value::Point(OverlayPoint::from_cg(point_in(value)))
    }

    fn at(x: f64, y: f64) -> Value { Value::Point(OverlayPoint::new(x, y)) }

    fn tile_track(from: GroupRect, to: GroupRect, begin: f64, seconds: f64) -> Track {
        Track::between(
            Node::Tile(WindowId::new(1, 1)),
            Channel::Base,
            Value::Rect(from),
            Value::Rect(to),
            begin,
            seconds,
            Curve::Bezier(MOTION_CURVE),
        )
    }

    /// Every form goes to Core Animation as the class its constructor names and comes back as
    /// the same track: a curve as a basic animation, samples as keyframes with values only, and
    /// keyframes with their key times and a curve per segment, at the track's begin and length and
    /// additive exactly when it is an accent.
    #[test]
    fn every_track_is_installed_as_itself() {
        let _core_animation = hold_core_animation();
        let still = GroupKey::STILL;
        let tracks = [
            Track::between(
                container(),
                Channel::Base,
                at(0.0, 0.0),
                at(-861.0, 0.0),
                5.0,
                0.35,
                Curve::Bezier(EASE_IN_OUT),
            ),
            Leg::toward(
                None,
                OverlayPoint::new(0.0, 0.0),
                OverlayPoint::new(-861.0, 0.0),
                5.0,
                30.6,
            )
            .track(still, SAMPLE_STEP),
            nudge_track(still, OverlayVector::new(-576.0, 0.0), 5.1, 0.455, SAMPLE_STEP),
            Bounce::from_rest(OverlayVector::new(0.0, -72.0), Duration::from_millis(350))
                .track(still, 5.2),
            Track::keyframes(
                container(),
                Channel::Base,
                vec![
                    (0.0, at(0.0, 0.0)),
                    (0.25, at(100.0, 0.0)),
                    (1.0, at(0.0, 0.0)),
                ],
                vec![Curve::Linear, Curve::Bezier(MOTION_CURVE)],
                6.0,
                1.0,
            ),
        ];
        for track in &tracks {
            let layer = CALayer::layer();
            install_container_track(&layer, track, &[]);
            assert_eq!(
                installed(&layer, container_key(track.channel()), container_value),
                expected(track, |v| v),
                "{:?} on {:?}",
                track.form(),
                track.channel()
            );
        }
        let forms: Vec<Form> = tracks.iter().map(Track::form).collect();
        for form in [Form::Between, Form::Samples, Form::Keyframes] {
            assert!(forms.contains(&form), "{form:?}");
        }
    }

    /// A tile's track rides every layer that draws the tile, on the track's begin, length and
    /// curve, each value mapped from the frame its key holds: a move only the picture, the caster
    /// and the hairline, and a resize their bounds, the silhouette, the mask, the crop pieces and
    /// the ground, the hairline's and the picture's corner and the outline's path as well, each
    /// piece at the tile's corner. A move after a resize takes the resize's keys away, or they
    /// would go on drawing the old sizes.
    #[test]
    fn a_tile_track_is_installed_on_every_layer_of_the_tile() {
        let _core_animation = hold_core_animation();
        let origin = CGPoint::new(0.0, 0.0);
        let mut tile = strip_tile(typed(865.0, 32.0, 861.0, 1081.0));
        tile.decorations.radius = Some(16.0);
        let moving = tile_track(
            typed(1726.0, 32.0, 861.0, 1081.0),
            typed(865.0, 32.0, 861.0, 1081.0),
            100.0,
            0.35,
        );
        install_tile_track(&mut tile, &moving, None);
        for layer in tile.layers() {
            assert_eq!(
                installed(layer, TILE_ANIMATION_KEY, point_in),
                expected(&moving, |v| as_frame(v).origin.to_cg())
            );
            assert_eq!(animation_keys(layer), [TILE_ANIMATION_KEY]);
        }
        assert!(animation_keys(&tile.decorations.shadow_mask).is_empty());
        assert!(animation_keys(&tile.decorations.outline).is_empty());

        tile.crop_grid = Some(new_crop_grid(&tile.picture));
        let picture = TileSize::new(1200.0, 1081.0);
        tile.crop_of = Some(picture);
        tile.anchors = Anchors { x: Anchor::End, y: Anchor::End };
        let resizing = tile_track(
            typed(1726.0, 32.0, 1200.0, 1081.0),
            typed(865.0, 32.0, 20.0, 1081.0),
            100.0,
            0.35,
        );
        install_tile_track(&mut tile, &resizing, None);
        let size = |v: Value| -> TileSize { as_frame(v).size.respace() };
        let corner = |v: Value| shape::radius(tile.decorations.radius, size(v));
        for layer in tile.layers() {
            assert_eq!(
                installed(layer, TILE_ANIMATION_KEY, point_in),
                expected(&resizing, |v| as_frame(v).origin.to_cg())
            );
            assert_eq!(
                installed(layer, "rini.tile.size", rect_in),
                expected(&resizing, |v| CGRect::new(origin, size(v).to_cg()))
            );
        }
        assert_eq!(
            installed(&tile.decorations.shadow, "rini.tile.shadow.path", path_of),
            expected(&resizing, |v| silhouette_path(size(v), corner(v)))
        );
        let mask = |v: Value| shadow_mask_frame(size(v)).to_cg();
        assert_eq!(
            installed(&tile.decorations.shadow_mask, "rini.tile.mask.move", point_in),
            expected(&resizing, |v| mask(v).origin)
        );
        assert_eq!(
            installed(&tile.decorations.shadow_mask, "rini.tile.mask.size", rect_in),
            expected(&resizing, |v| CGRect::new(origin, mask(v).size))
        );
        assert_eq!(
            installed(&tile.decorations.shadow_mask, "rini.tile.mask.path", path_of),
            expected(&resizing, |v| mask_path(size(v), corner(v)))
        );
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for (index, layer) in grid.pieces.iter().enumerate() {
            let piece = |v: Value| crop(picture, size(v), tile.anchors).pieces[index];
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                expected(&resizing, |v| piece(v).frame.origin.to_cg())
            );
            assert_eq!(
                installed(layer, "rini.piece.contents", rect_in),
                expected(&resizing, |v| within(tile.region, piece(v).contents).to_cg())
            );
        }
        for (index, layer) in grid.ground.iter().enumerate() {
            let span = |v: Value| crop(picture, size(v), tile.anchors).ground[index];
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                expected(&resizing, |v| span(v).origin.to_cg())
            );
        }
        assert_eq!(
            installed(
                &tile.decorations.hairline,
                "rini.tile.hairline.corner",
                number_in
            ),
            expected(&resizing, corner)
        );
        assert_eq!(
            installed(&tile.picture, "rini.tile.picture.corner", number_in),
            expected(&resizing, corner)
        );
        assert_eq!(
            installed(
                &tile.decorations.hairline,
                "rini.tile.hairline.corner",
                number_in
            )
            .keys[1]
                .1,
            10.0
        );
        assert_eq!(
            installed(&tile.decorations.outline, "rini.tile.outline.path", path_in),
            expected(&resizing, |v| shape::outline(size(v), corner(v)).0.to_cg())
        );

        install_tile_track(&mut tile, &moving, None);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for layer in tile.layers() {
            assert_eq!(
                animation_keys(layer),
                [TILE_ANIMATION_KEY],
                "the resize's keys go"
            );
        }
        for layer in [
            &*tile.decorations.shadow_mask as &CALayer,
            &tile.decorations.outline,
        ]
        .into_iter()
        .chain(grid.layers())
        {
            assert!(animation_keys(layer).is_empty());
        }
    }

    /// A track of no length is a cut in the score and 0.25s in Core Animation, so nothing is
    /// installed for one, and what played under its key goes, as the score's track does.
    #[test]
    fn a_track_of_no_length_installs_nothing() {
        let _core_animation = hold_core_animation();
        let layer = CALayer::layer();
        let moving = Track::between(
            container(),
            Channel::Base,
            at(0.0, 0.0),
            at(-861.0, 0.0),
            5.0,
            0.35,
            Curve::Bezier(MOTION_CURVE),
        );
        install_container_track(&layer, &moving, &[]);
        let cut = Track::between(
            container(),
            Channel::Base,
            at(-400.0, 0.0),
            at(-861.0, 0.0),
            5.1,
            0.0,
            Curve::Bezier(MOTION_CURVE),
        );
        install_container_track(&layer, &cut, &[]);
        assert!(animation_keys(&layer).is_empty());
        let bounce = Bounce::from_rest(OverlayVector::new(-72.0, 0.0), Duration::ZERO)
            .track(GroupKey::STILL, 5.0);
        install_container_track(&layer, &bounce, &[]);
        assert!(animation_keys(&layer).is_empty());

        let mut tile = strip_tile(typed(865.0, 32.0, 861.0, 1081.0));
        tile.crop_grid = Some(new_crop_grid(&tile.picture));
        tile.crop_of = Some(TileSize::new(1200.0, 1081.0));
        let (from, to) = (
            typed(4.0, 32.0, 1200.0, 1081.0),
            typed(865.0, 32.0, 861.0, 1081.0),
        );
        install_tile_track(&mut tile, &tile_track(from, to, 100.0, 0.35), None);
        install_tile_track(&mut tile, &tile_track(from, to, 100.1, 0.0), None);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for layer in tile
            .layers()
            .into_iter()
            .chain([
                &*tile.decorations.shadow_mask as &CALayer,
                &tile.decorations.outline,
            ])
            .chain(grid.pieces.iter().map(|piece| &**piece))
        {
            assert!(animation_keys(layer).is_empty(), "{:?}", animation_keys(layer));
        }
    }

    /// What `install_container_track` hands Core Animation for a bounce is `Bounce::track`, key
    /// for key and curve for curve, from rest and pushed mid-way, so the domain's tests pin what
    /// is drawn.
    #[test]
    fn the_bounce_installed_is_its_track() {
        let _core_animation = hold_core_animation();
        let wall = OverlayVector::new(-72.0, 0.0);
        let plays = Duration::from_millis(350);
        let first = Bounce::from_rest(wall, plays);
        let pushed = Bounce::from_drawn(first.at(Duration::from_millis(90)), wall, plays);
        for bounce in [first, pushed] {
            let track = bounce.track(GroupKey::STILL, 7.0);
            let layer = CALayer::layer();
            install_container_track(&layer, &track, &[]);
            assert_core_animation_held();
            // SAFETY: a read of the layer's own animation.
            let animation =
                unsafe { layer.animationForKey(&NSString::from_str(BOUNCE_ANIMATION_KEY)) }
                    .expect("the bounce")
                    .downcast::<CAKeyframeAnimation>()
                    .expect("keyframes");
            assert_eq!(
                animation.keyPath().map(|k| k.to_string()),
                Some("position".into())
            );
            let read = installed(&layer, BOUNCE_ANIMATION_KEY, container_value);
            assert_eq!(read.keys, track.keys());
            assert_eq!(read, expected(&track, |v| v));
            assert!(read.additive);
            assert_eq!((read.begin, read.seconds), (7.0, track.seconds()));
        }
    }

    #[test]
    fn a_linear_segment_runs_on_the_linear_function() {
        assert_eq!(
            control_points(&curve_function(Curve::Linear)),
            bezier_points(Curve::Linear)
        );
        assert_eq!(
            control_points(&curve_function(Curve::Bezier(EASE_IN_OUT))),
            bezier_points(Curve::Bezier(EASE_IN_OUT))
        );
    }

    fn animation_keys(layer: &CALayer) -> Vec<String> {
        assert_core_animation_held();
        layer
            .animationKeys()
            .map(|keys| keys.iter().map(|key| key.to_string()).collect())
            .unwrap_or_default()
    }

    /// When the animation under `key` plays, as Core Animation was handed it.
    fn playing(layer: &CALayer, key: &str) -> Playing {
        assert_core_animation_held();
        // SAFETY: a read of the layer's own animation.
        let animation = unsafe { layer.animationForKey(&NSString::from_str(key)) }
            .expect("an animation under the key");
        let repeat = animation.repeatDuration();
        Playing {
            begin: animation.beginTime(),
            offset: animation.timeOffset(),
            active: if repeat > 0.0 {
                repeat
            } else {
                animation.duration()
            },
        }
    }

    /// A container riding its movement from 5.0, a bounce from 5.1 and a nudge from 5.2, and the
    /// replica of what it plays.
    fn accented_container() -> (Retained<CALayer>, Score) {
        let layer = CALayer::layer();
        let wall = OverlayVector::new(-72.0, 0.0);
        let mut replica = Score::default();
        replica.apply(&Edit::Place(container(), Value::Point(wall.to_point())));
        let tracks = [
            Track::between(
                container(),
                Channel::Base,
                at(0.0, 0.0),
                Value::Point(wall.to_point()),
                5.0,
                0.35,
                Curve::Bezier(MOTION_CURVE),
            ),
            Bounce::from_rest(wall, Duration::from_millis(350)).track(GroupKey::STILL, 5.1),
            nudge_track(
                GroupKey::STILL,
                OverlayVector::new(-576.0, 0.0),
                5.2,
                0.455,
                SAMPLE_STEP,
            ),
        ];
        for track in tracks {
            install_container_track(&layer, &track, &[]);
            replica.apply(&Edit::Add(track));
        }
        (layer, replica)
    }

    fn new_leg(begin: f64) -> Track {
        Track::between(
            container(),
            Channel::Base,
            at(-30.0, 0.0),
            at(-861.0, 0.0),
            begin,
            0.4,
            Curve::Bezier(MOTION_CURVE),
        )
    }

    fn close(a: Playing, b: Playing) -> bool {
        (a.begin - b.begin).abs() < 1e-9
            && (a.offset - b.offset).abs() < 1e-9
            && (a.active - b.active).abs() < 1e-9
    }

    /// A merge gives a container a new movement, which begins after the bounce and the nudge
    /// riding it and so would hide them until it ends. They begin again with it from the replica's
    /// tracks, each as far into its own time as it had got and for what is left of it, so they go
    /// on where they were; begun again a second time, they are still where they were.
    #[test]
    fn a_replaced_movement_begins_its_accents_again_from_the_replica() {
        let _core_animation = hold_core_animation();
        let (layer, replica) = accented_container();
        let riding: Vec<Track> = replica.tracks(container()).cloned().collect();
        install_container_track(&layer, &new_leg(5.3), &riding);
        assert_eq!(animation_keys(&layer), [
            GROUP_ANIMATION_KEY,
            BOUNCE_ANIMATION_KEY,
            NUDGE_ANIMATION_KEY
        ]);
        assert_eq!(playing(&layer, GROUP_ANIMATION_KEY).begin, 5.3);
        let bounce = Playing {
            begin: 5.3,
            offset: 0.2,
            active: 5.1 + 0.35 - 5.3,
        };
        let nudge = Playing {
            begin: 5.3,
            offset: 0.1,
            active: 5.2 + 0.455 - 5.3,
        };
        assert!(close(playing(&layer, BOUNCE_ANIMATION_KEY), bounce));
        assert!(close(playing(&layer, NUDGE_ANIMATION_KEY), nudge));

        install_container_track(&layer, &new_leg(5.4), &riding);
        let again = Playing {
            begin: 5.4,
            offset: 0.3,
            active: 5.1 + 0.35 - 5.4,
        };
        assert!(
            close(playing(&layer, BOUNCE_ANIMATION_KEY), again),
            "begun again a second time, still where it was"
        );
    }

    /// An accent that begins after the new movement already sorts after it; one that has ended is
    /// left for Core Animation to remove.
    #[test]
    fn an_accent_begun_after_the_movement_or_ended_is_left_alone() {
        let accent = Playing {
            begin: 5.1,
            offset: 0.0,
            active: 0.35,
        };
        assert_eq!(accent.again_at(5.0), None);
        assert_eq!(accent.again_at(5.45), None);
        let tie = accent.again_at(5.1).expect("begun with the movement, added again after it");
        assert!(close(tie, accent), "a tie sorts in the order added");
    }

    /// What `install_container_track` answers: a layer's animations sort by begin time, not by the
    /// order they were added, so a movement added plainly goes after accents that began before it.
    #[test]
    fn core_animation_orders_a_layers_animations_by_begin_time() {
        let _core_animation = hold_core_animation();
        let (layer, _) = accented_container();
        assert_eq!(animation_keys(&layer), [
            GROUP_ANIMATION_KEY,
            BOUNCE_ANIMATION_KEY,
            NUDGE_ANIMATION_KEY
        ]);
        let position = |v: Value| point_object(as_point(v).to_cg());
        let key = NSString::from_str(GROUP_ANIMATION_KEY);
        layer.addAnimation_forKey(&animation(&new_leg(5.3), "position", position), Some(&key));
        assert_eq!(animation_keys(&layer), [
            BOUNCE_ANIMATION_KEY,
            NUDGE_ANIMATION_KEY,
            GROUP_ANIMATION_KEY
        ]);
        layer.addAnimation_forKey(&animation(&new_leg(5.15), "position", position), Some(&key));
        assert_eq!(
            animation_keys(&layer),
            [
                BOUNCE_ANIMATION_KEY,
                GROUP_ANIMATION_KEY,
                NUDGE_ANIMATION_KEY
            ],
            "added last, sorted between"
        );
    }

    /// The canvas starts a band above the view, so a hole moves up by the band into the view's
    /// coordinates.
    #[test]
    fn a_hole_moves_into_the_view_by_the_band() {
        assert_eq!(
            in_view(rect(869.0, 572.5, 1.0, 1.0), 32.0),
            rect(869.0, 540.5, 1.0, 1.0)
        );
        assert_eq!(
            in_view(rect(869.0, 572.5, 1.0, 1.0), 0.0),
            rect(869.0, 572.5, 1.0, 1.0)
        );
    }

    /// The mask covers the whole view but for each hole: filled even-odd, a point in a hole is left
    /// out and every other point is kept.
    #[test]
    fn the_mask_leaves_out_exactly_the_holes() {
        let bounds = rect(0.0, 0.0, 1728.0, 1085.0);
        let ends: [OverlayRect; 2] = [
            typed(868.0, 32.0, 856.0, 1081.0),
            typed(4.0, 32.0, 856.0, 1081.0),
        ];
        let holes: Vec<CGRect> =
            ends.iter().map(|end| in_view(hole_in(*end).to_cg(), 32.0)).collect();
        let mask = holes_mask(bounds, &holes);
        // SAFETY: Core Animation's own fill-rule constant.
        assert_eq!(*mask.fillRule(), *unsafe {
            objc2_quartz_core::kCAFillRuleEvenOdd
        });
        assert_eq!(mask.frame(), bounds);
        let path = mask.path().expect("a path");
        // SAFETY: a null transform means the point is taken as given.
        let kept = |x: f64, y: f64| unsafe {
            objc2_core_graphics::CGPath::contains_point(
                Some(&path),
                std::ptr::null(),
                CGPoint::new(x, y),
                true,
            )
        };
        for hole in &holes {
            assert!(!kept(hole.origin.x + 0.5, hole.origin.y + 0.5), "{hole:?}");
            assert!(kept(hole.origin.x + 2.0, hole.origin.y + 0.5), "beside {hole:?}");
            assert!(kept(hole.origin.x + 0.5, hole.origin.y - 1.0), "above {hole:?}");
        }
        assert!(kept(10.0, 10.0));
        assert!(kept(1700.0, 1080.0));
    }

    /// A crop-drawn tile whose picture is a region of a composite shows that region in every
    /// piece: each piece's contents composed with it, at rest and at both ends of a resize.
    #[test]
    fn a_region_pictures_crop_pieces_show_only_the_region() {
        let _core_animation = hold_core_animation();
        let container = CALayer::layer();
        let mut tile = new_tile(&container);
        tile.crop_grid = Some(new_crop_grid(&tile.picture));
        let picture = TileSize::new(856.0, 1081.0);
        tile.crop_of = Some(picture);
        tile.region = typed(0.5, 0.0, 0.5, 1.0);
        tile.anchors = Anchors {
            x: Anchor::End,
            y: Anchor::Start,
        };
        let frame = TileSize::new(600.0, 1081.0);
        layout_crop_grid(&tile, frame);
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop(picture, frame, tile.anchors).pieces) {
            let shown = within(tile.region, piece.contents).to_cg();
            assert_eq!(layer.contentsRect(), shown);
            assert!(
                layer.contentsRect().origin.x >= 0.5,
                "{:?}",
                layer.contentsRect()
            );
        }
        let to = TileSize::new(300.0, 1081.0);
        let origin = Point::new(4.0, 32.0);
        let resize = tile_track(
            GroupRect::new(origin, frame.respace()),
            GroupRect::new(origin, to.respace()),
            5.0,
            0.35,
        );
        install_crop_grid(&mut tile, &resize, None);
        let grid = tile.crop_grid.as_ref().unwrap();
        let ends = crop(picture, frame, tile.anchors)
            .pieces
            .into_iter()
            .zip(crop(picture, to, tile.anchors).pieces);
        for (layer, (a, b)) in grid.pieces.iter().zip(ends) {
            let contents = installed(layer, "rini.piece.contents", rect_in);
            assert_eq!(contents.form, Form::Between, "a basic animation");
            let shown = |piece: ContentsRect| within(tile.region, piece).to_cg();
            assert_eq!(contents.keys[0].1, shown(a.contents));
            assert_eq!(contents.keys[1].1, shown(b.contents));
        }
        tile.region = WHOLE;
        layout_crop_grid(&tile, frame);
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop(picture, frame, tile.anchors).pieces) {
            assert_eq!(
                layer.contentsRect(),
                piece.contents.to_cg(),
                "a whole picture is unchanged"
            );
        }
    }

    /// A crop-drawn tile takes its region from its picture: a region of a composite crops inside
    /// it, and the next picture, whole, puts the region back to all of the image.
    #[test]
    fn a_crop_drawn_tile_takes_its_region_from_its_picture() {
        use crate::pictures::platform::picture::{test_picture, test_region};
        let container = CALayer::layer();
        let mut tile = new_tile(&container);
        let frame = TileSize::new(2.0, 2.0);
        let region = test_region();
        set_tile_content(&mut tile, &region, ContentMode::Crop, frame, 2.0);
        assert_eq!(tile.region, region.image.unit_rect());
        assert_eq!(tile.region.to_cg(), rect(0.5, 0.0, 0.5, 1.0));
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop(frame, frame, tile.anchors).pieces) {
            let shown = within(tile.region, piece.contents).to_cg();
            assert_eq!(layer.contentsRect(), shown);
        }
        set_tile_content(&mut tile, &test_picture(frame), ContentMode::Crop, frame, 2.0);
        assert_eq!(tile.region, WHOLE);
    }

    /// A picture landing on a crop-drawn tile on a track of its own is cut in, its grid laid for
    /// the tile's model and the new picture with the leg's anchors, and the pieces re-keyed on the
    /// track's own begin and curve, so only the pixels cut. A tile riding its container has no
    /// track to re-key along.
    #[test]
    fn a_swap_re_keys_a_crop_grid_on_its_tiles_track() {
        let _core_animation = hold_core_animation();
        use crate::pictures::platform::picture::test_picture;
        let (from, to): (GroupRect, GroupRect) = (
            typed(263.0, 32.0, 600.0, 1081.0),
            typed(4.0, 32.0, 859.0, 1081.0),
        );
        let stale = test_picture(from.size);
        let landed = TileSize::new(859.0, 1081.0);
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        assert_eq!(tile.anchors.x, Anchor::End, "widening leftwards");
        set_tile_content(&mut tile, &stale, ContentMode::Crop, from.size.respace(), 2.0);
        let track = tile_track(from, to, 100.0, 0.35);
        install_tile_track(&mut tile, &track, None);
        swap_onto(
            &mut tile,
            &test_picture(landed),
            to.size.respace(),
            Some(&track),
            2.0,
        );
        assert_eq!(tile.crop_of, Some(landed), "the grid cuts the new picture");
        assert_eq!(tile.anchors, Anchors::of_leg(from, to), "on the leg's anchors");
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for (index, layer) in grid.pieces.iter().enumerate() {
            let piece =
                |v: Value| crop(landed, as_frame(v).size.respace(), tile.anchors).pieces[index];
            assert_eq!(
                installed(layer, "rini.piece.contents", rect_in),
                expected(&track, |v| within(tile.region, piece(v).contents).to_cg())
            );
            let laid = crop(landed, to.size.respace(), tile.anchors).pieces[index].frame;
            assert_eq!(layer.frame(), laid.to_cg());
        }
        for (index, layer) in grid.ground.iter().enumerate() {
            let span =
                |v: Value| crop(landed, as_frame(v).size.respace(), tile.anchors).ground[index];
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                expected(&track, |v| span(v).origin.to_cg())
            );
            assert_eq!(
                layer.frame(),
                span(Value::Rect(to)).to_cg(),
                "none where it covers"
            );
        }

        let mut riding = strip_tile(to);
        set_tile_content(&mut riding, &stale, ContentMode::Crop, to.size.respace(), 2.0);
        swap_onto(&mut riding, &test_picture(landed), to.size.respace(), None, 2.0);
        let grid = riding.crop_grid.as_ref().expect("a grid");
        for layer in riding.layers().into_iter().chain(grid.layers()) {
            assert!(animation_keys(layer).is_empty(), "nothing to re-key");
        }
    }

    /// The landing of a shrink whose tile starts with no picture covering its start, swapped in as
    /// it comes, is smaller than the frame still to come: it is cut onto the crop grid on the leg's
    /// anchors and re-keyed on the tile's track, and the window's ground fills the span between its
    /// content and the moving edge's band until the frame reaches its size. A leg growing on one axis
    /// as it shrinks on the other is in two steps and never flies its repaint over the shrink
    /// (`a_leg_in_two_steps_cuts_its_crop_grid_to_its_repaint_at_the_corner`).
    #[test]
    fn a_landing_smaller_than_the_frame_to_come_is_grounded_by_the_moving_edge() {
        let _core_animation = hold_core_animation();
        use crate::pictures::platform::picture::test_picture;
        let column: GroupRect = typed(4.0, 3.0, 859.0, 1074.0);
        for (case, to, anchored_top) in [
            ("a top row shrinking up", typed(4.0, 3.0, 859.0, 535.0), true),
            (
                "a bottom row shrinking down",
                typed(4.0, 542.0, 859.0, 535.0),
                false,
            ),
        ] {
            let mut tile = strip_tile(to);
            tile.anchors = Anchors::of_leg(column, to);
            let before = test_picture(column.size);
            set_tile_content(&mut tile, &before, ContentMode::Crop, column.size.respace(), 2.0);
            let track = tile_track(column, to, 100.0, 0.35);
            install_tile_track(&mut tile, &track, None);
            let landed: TileSize = to.size.respace();
            swap_onto(&mut tile, &test_picture(landed), landed, Some(&track), 2.0);
            assert_eq!(tile.crop_of, Some(landed), "{case}");
            assert_eq!(
                tile.anchors,
                Anchors::of_leg(column, to),
                "{case}: the leg's anchors"
            );
            let laid = |v: Value| crop(landed, as_frame(v).size.respace(), tile.anchors);
            let grid = tile.crop_grid.as_ref().expect("a grid");
            for (index, layer) in grid.pieces.iter().enumerate() {
                assert_eq!(
                    installed(layer, "rini.piece.move", point_in),
                    expected(&track, |v| laid(v).pieces[index].frame.origin.to_cg()),
                    "{case}: piece {index}"
                );
            }
            for (index, layer) in grid.ground.iter().enumerate() {
                assert_eq!(
                    installed(layer, "rini.piece.move", point_in),
                    expected(&track, |v| laid(v).ground[index].origin.to_cg()),
                    "{case}: ground {index}"
                );
            }
            let start = laid(Value::Rect(column));
            let band = start.pieces[0].frame.size.height;
            let (tall, high) = (column.size.height, landed.height);
            let (top, bottom) = if anchored_top {
                (high - band, tall - band)
            } else {
                (band, tall - high + band)
            };
            let bare: f64 = start.ground.iter().map(|g| g.size.width * g.size.height).sum();
            assert!(
                (bare - column.size.width * (tall - high)).abs() < 1e-6,
                "{case}: {bare}"
            );
            for span in start.ground.iter().filter(|g| g.size.width * g.size.height > 0.0) {
                assert_eq!(
                    (span.min_y(), span.max_y()),
                    (top, bottom),
                    "{case}: by the moving edge"
                );
            }
            let end = laid(Value::Rect(to));
            assert!(
                end.ground.iter().all(|g| g.size.width * g.size.height == 0.0),
                "{case}: none at the end"
            );
        }
    }

    /// fold1's lab-C folding (`docs/data/2026-10-10-fold`), its repaint held for the corner: the crop
    /// grid flies the picture from the press up to the corner and the repaint from it, each piece and
    /// ground span laid against its own picture at each key and cut from one grid to the other on the
    /// corner's key time, every piece's picture and the ground's colour held at the press's and cut
    /// to the repaint's there, and the tile left drawing the repaint as its leg ends. A later leg in
    /// one movement takes the cut away; a leg in two steps given no repaint flies its one picture
    /// over every key.
    #[test]
    fn a_leg_in_two_steps_cuts_its_crop_grid_to_its_repaint_at_the_corner() {
        let _core_animation = hold_core_animation();
        use crate::animation::domain::motion::tile::{Crop, corner_key, leg, two_steps};
        use crate::pictures::domain::ground::Ground;
        use crate::pictures::platform::picture::{LazyGround, Pixels, test_picture};
        let (from, to): (GroupRect, GroupRect) = (
            typed(866.0, 35.0, 860.0, 1079.0),
            typed(3.0, 576.0, 1148.0, 538.0),
        );
        let corner = two_steps(from, to).expect("two steps").corner;
        let track = leg(WindowId::new(1, 1), from, to, 100.0, 0.35, MOTION_CURVE);
        let key = corner_key(&track).expect("a corner");
        let picture = |size: GroupRect, ground: f64| {
            let mut picture = test_picture(size.size);
            picture.ground = LazyGround::known(Ground {
                r: ground,
                g: ground,
                b: ground,
                a: 1.0,
            });
            picture
        };
        let (pressed, repainted) = (picture(from, 0.1), picture(to, 0.2));
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        set_tile_content(&mut tile, &pressed, ContentMode::Crop, from.size.respace(), 2.0);
        let pictures = Corner {
            before: &pressed,
            after: &repainted,
        };
        install_tile_track(&mut tile, &track, Some(pictures));

        let (before, after): (TileSize, TileSize) = (from.size.respace(), to.size.respace());
        assert_eq!(tile.crop_of, Some(after), "drawing the repaint as the leg ends");
        let image = |of: &Picture| match &of.image {
            Pixels::Bitmap(image) => &**image as *const _ as usize,
            _ => unreachable!("a test picture is a bitmap"),
        };
        let address = |object: &AnyObject| object as *const AnyObject as usize;
        let anchors = tile.anchors;
        let laid = |picture: TileSize, at: GroupRect| crop(picture, at.size.respace(), anchors);
        let keyed = |frame: &dyn Fn(Crop) -> CGPoint| Installed {
            form: Form::Keyframes,
            begin: 100.0,
            seconds: 0.35,
            additive: false,
            keys: vec![
                (0.0, frame(laid(before, from))),
                (key, frame(laid(before, corner))),
                (key, frame(laid(after, corner))),
                (1.0, frame(laid(after, to))),
            ],
            curves: [track.curves()[0], Curve::Linear, track.curves()[1]]
                .map(bezier_points)
                .to_vec(),
        };
        let switched = |layer: &CALayer, key_path: &str| {
            assert_core_animation_held();
            // SAFETY: a read of the layer's own animation.
            let animation = unsafe { layer.animationForKey(&NSString::from_str(key_path)) }
                .unwrap_or_else(|| panic!("nothing under {key_path}"));
            let cut = animation.downcast::<CAKeyframeAnimation>().expect("keyframes");
            // SAFETY: an immutable `NSString` constant exported by Core Animation.
            let discrete = unsafe { kCAAnimationDiscrete };
            assert_eq!(&*cut.calculationMode(), discrete, "held, then cut");
            let times: Vec<f64> =
                cut.keyTimes().expect("times").iter().map(|t| t.doubleValue()).collect();
            assert_eq!(times, [0.0, key, 1.0]);
            assert_eq!((cut.beginTime(), cut.duration()), (100.0, 0.35));
            cut.values()
                .expect("values")
                .iter()
                .map(|value| Retained::as_ptr(&value) as usize)
                .collect::<Vec<_>>()
        };
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for (index, layer) in grid.pieces.iter().enumerate() {
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                keyed(&|grid| grid.pieces[index].frame.origin.to_cg()),
                "piece {index}"
            );
            let held = switched(layer, PIECE_PICTURE_KEY);
            assert_eq!(held, [image(&pressed), image(&repainted)], "piece {index}");
            // SAFETY: a read of the layer's own property.
            let shown = unsafe { layer.contents() }.map(|object| address(&object));
            assert_eq!(
                shown,
                Some(image(&repainted)),
                "piece {index}: the repaint once it ends"
            );
            assert_eq!(layer.frame(), laid(after, to).pieces[index].frame.to_cg());
        }
        for (index, layer) in grid.ground.iter().enumerate() {
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                keyed(&|grid| grid.ground[index].origin.to_cg()),
                "ground {index}"
            );
            let held = switched(layer, GROUND_COLOUR_KEY);
            let fill = |colour: usize| {
                // SAFETY: the address of a `CGColor` among the values of an animation the layer
                // still holds, and the Core Animation hold keeps a commit from dropping it.
                test_components(unsafe { &*(colour as *const objc2_core_graphics::CGColor) })
            };
            let fills: Vec<Vec<f64>> = held.iter().map(|colour| fill(*colour)).collect();
            assert_eq!(
                fills,
                [[0.1, 0.1, 0.1, 1.0], [0.2, 0.2, 0.2, 1.0]],
                "ground {index}"
            );
        }

        let onward = tile_track(to, typed(3.0, 576.0, 1148.0, 600.0), 100.4, 0.35);
        install_tile_track(&mut tile, &onward, None);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for layer in grid.layers() {
            let keys = animation_keys(layer);
            assert!(
                !keys.iter().any(|key| key == PIECE_PICTURE_KEY || key == GROUND_COLOUR_KEY),
                "{keys:?}"
            );
        }

        let mut alone = strip_tile(to);
        alone.anchors = Anchors::of_leg(from, to);
        set_tile_content(&mut alone, &pressed, ContentMode::Crop, from.size.respace(), 2.0);
        install_tile_track(&mut alone, &track, None);
        assert_eq!(alone.crop_of, Some(before), "the press picture to the end");
        let grid = alone.crop_grid.as_ref().expect("a grid");
        for (index, layer) in grid.pieces.iter().enumerate() {
            let frame = |at: GroupRect| laid(before, at).pieces[index].frame.origin.to_cg();
            let moved = installed(layer, "rini.piece.move", point_in);
            assert_eq!(moved.keys, [
                (0.0, frame(from)),
                (key, frame(corner)),
                (key, frame(corner)),
                (1.0, frame(to))
            ]);
            assert!(
                !animation_keys(layer).iter().any(|key| key == PIECE_PICTURE_KEY),
                "nothing to cut to"
            );
        }
    }

    /// A leg in two steps installed with a repaint from its corner returns how long reading the
    /// repaint's ground took as its grid was cut onto it, the press picture's read already paid by
    /// its dressing.
    #[test]
    fn a_corner_cut_returns_how_long_its_repaints_ground_took_to_read() {
        use crate::animation::domain::motion::tile::leg;
        use crate::pictures::platform::picture::test_picture;
        let (from, to): (GroupRect, GroupRect) = (
            typed(866.0, 35.0, 860.0, 1079.0),
            typed(3.0, 576.0, 1148.0, 538.0),
        );
        let track = leg(WindowId::new(1, 1), from, to, 100.0, 0.35, MOTION_CURVE);
        let (pressed, repainted) = (test_picture(from.size), test_picture(to.size));
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        set_tile_content(&mut tile, &pressed, ContentMode::Crop, from.size.respace(), 2.0);

        let pictures = Corner {
            before: &pressed,
            after: &repainted,
        };
        let read = install_tile_track(&mut tile, &track, Some(pictures)).expect("a cut");
        assert!(
            read > Duration::ZERO,
            "the repaint's ground read, unread till the cut"
        );
    }

    /// A leg in two steps flies its press picture up to the corner whatever its repaint fits: one
    /// fitting both its ends, a stand-in's, is cut onto the grid at the corner like any other, never
    /// drawn stretched over the whole leg from its start.
    #[test]
    fn a_leg_in_two_steps_cuts_to_its_repaint_at_the_corner_whatever_the_repaint_fits() {
        let _core_animation = hold_core_animation();
        use crate::animation::domain::motion::tile::leg;
        use crate::pictures::platform::picture::{stand_in_picture, test_bitmap, test_picture};
        let (from, to): (GroupRect, GroupRect) = (
            typed(866.0, 35.0, 860.0, 1079.0),
            typed(3.0, 576.0, 1148.0, 538.0),
        );
        let track = leg(WindowId::new(1, 1), from, to, 100.0, 0.35, MOTION_CURVE);
        let pressed = test_picture(from.size);
        let stand_in = stand_in_picture(to.size.respace(), test_bitmap());
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        set_tile_content(&mut tile, &pressed, ContentMode::Crop, from.size.respace(), 2.0);

        let pictures = Corner {
            before: &pressed,
            after: &stand_in,
        };
        assert!(
            install_tile_track(&mut tile, &track, Some(pictures)).is_some(),
            "its ground read"
        );
        assert!(tile.crop_of.is_some(), "still crop-drawn");
        let grid = tile.crop_grid.as_ref().expect("a crop grid");
        let cut = |layer: &Retained<CALayer>| {
            animation_keys(layer).iter().any(|key| key == PIECE_PICTURE_KEY)
        };
        assert!(grid.pieces.iter().all(cut), "every piece cut at the corner");
    }

    /// A stretched tile stays stretched for a picture fitting both ends of its leg, so a pan draws
    /// as before and is not re-keyed, and a stand-in's real picture landing on a resize is cut onto
    /// the crop grid and keyed along the leg rather than stretched over it.
    #[test]
    fn a_stretched_tile_takes_a_picture_that_does_not_fit_its_leg_cropped() {
        let _core_animation = hold_core_animation();
        use crate::pictures::platform::picture::{stand_in_picture, test_bitmap, test_picture};
        let (from, to): (GroupRect, GroupRect) =
            (typed(4.0, 542.0, 859.0, 535.0), typed(4.0, 3.0, 859.0, 1074.0));
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        let stand_in = stand_in_picture(to.size.respace(), test_bitmap());
        set_tile_content(
            &mut tile,
            &stand_in,
            ContentMode::Stretch,
            from.size.respace(),
            2.0,
        );
        let track = tile_track(from, to, 100.0, 0.35);
        install_tile_track(&mut tile, &track, None);
        let landed: TileSize = to.size.respace();
        swap_onto(&mut tile, &test_picture(landed), landed, Some(&track), 2.0);
        assert_eq!(tile.crop_of, Some(landed), "cropped");
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for (index, layer) in grid.pieces.iter().enumerate() {
            let piece =
                |v: Value| crop(landed, as_frame(v).size.respace(), tile.anchors).pieces[index];
            assert_eq!(
                installed(layer, "rini.piece.move", point_in),
                expected(&track, |v| piece(v).frame.origin.to_cg())
            );
            assert!(!layer.isHidden());
        }

        let (parked, slot): (GroupRect, GroupRect) = (
            typed(1726.0, 3.0, 859.0, 1074.0),
            typed(865.0, 3.0, 859.0, 1074.0),
        );
        let mut panning = strip_tile(slot);
        let picture = test_picture(slot.size);
        set_tile_content(
            &mut panning,
            &picture,
            ContentMode::Stretch,
            slot.size.respace(),
            2.0,
        );
        let pan = tile_track(parked, slot, 100.0, 0.35);
        swap_onto(
            &mut panning,
            &test_picture(slot.size),
            slot.size.respace(),
            Some(&pan),
            2.0,
        );
        assert_eq!(panning.crop_of, None, "a pan stays stretched");
        assert!(panning.crop_grid.is_none(), "and never builds a grid");
        assert!(
            panning.layers().into_iter().all(|layer| animation_keys(layer).is_empty()),
            "nor is re-keyed"
        );
    }

    /// A crop-drawn tile is filled with its picture's ground where the picture ends, in layers
    /// under the picture layer, so its corner clip covers them; a repaint landing on it as its
    /// resize plays colours them again, and a stretched tile shows none of the grid.
    #[test]
    fn a_crop_drawn_tile_is_grounded_where_its_picture_ends() {
        use crate::pictures::domain::ground::Ground;
        use crate::pictures::platform::picture::{LazyGround, test_picture};
        let fill = |layer: &CALayer| test_components(&layer.backgroundColor().expect("filled"));
        let mut before = test_picture(TileSize::new(859.0, 535.0));
        before.ground = LazyGround::known(Ground {
            r: 0.1,
            g: 0.1,
            b: 0.12,
            a: 0.95,
        });
        let frame = TileSize::new(859.0, 1074.0);
        let mut tile = strip_tile(typed(4.0, 3.0, 859.0, 1074.0));
        set_tile_content(&mut tile, &before, ContentMode::Crop, frame, 2.0);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        let laid = crop(TileSize::new(859.0, 535.0), frame, tile.anchors);
        for (layer, span) in grid.ground.iter().zip(laid.ground) {
            assert_eq!(
                layer.superlayer().as_deref(),
                Some(&*tile.picture),
                "under the picture"
            );
            assert_eq!(layer.frame(), span.to_cg());
            assert_eq!(fill(layer), [0.1, 0.1, 0.12, 0.95]);
            assert!(!layer.isHidden());
        }
        assert_eq!(laid.ground[1].size.height, 1074.0 - 535.0, "the growth");

        let mut after = test_picture(frame);
        after.ground = LazyGround::known(Ground { r: 0.2, g: 0.2, b: 0.2, a: 1.0 });
        let grow = tile_track(
            typed(4.0, 3.0, 859.0, 535.0),
            typed(4.0, 3.0, 859.0, 1074.0),
            100.0,
            0.35,
        );
        swap_onto(&mut tile, &after, frame, Some(&grow), 2.0);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        for layer in &grid.ground {
            assert_eq!(fill(layer), [0.2, 0.2, 0.2, 1.0], "coloured again");
            assert_eq!(
                layer.frame().size.width * layer.frame().size.height,
                0.0,
                "it covers"
            );
        }

        set_tile_content(&mut tile, &after, ContentMode::Stretch, frame, 2.0);
        let grid = tile.crop_grid.as_ref().expect("a grid");
        assert!(grid.layers().all(|layer| layer.isHidden()), "stretched");
    }

    /// A tile riding its container, crop-drawn on a stale picture, given a picture that fits its
    /// frame draws it stretched on its picture layer, as a pan draws any fitting picture: its grid
    /// hidden, so no ground is filled or read for it.
    #[test]
    fn a_crop_drawn_riding_tile_given_a_fitting_picture_draws_it_stretched() {
        use crate::pictures::platform::picture::test_picture;
        let slot: GroupRect = typed(865.0, 3.0, 859.0, 1074.0);
        let mut tile = strip_tile(slot);
        let stale = test_picture(TileSize::new(600.0, 1074.0));
        set_tile_content(&mut tile, &stale, ContentMode::Crop, slot.size.respace(), 2.0);
        assert!(tile.crop_of.is_some(), "cut from the stale picture");

        let fitting = test_picture(slot.size);
        swap_onto(&mut tile, &fitting, slot.size.respace(), None, 2.0);
        assert_eq!(tile.crop_of, None, "stretched");
        let grid = tile.crop_grid.as_ref().expect("the grid stays pooled");
        assert!(
            grid.layers().all(|layer| layer.isHidden()),
            "none of the grid shows"
        );
        let crate::pictures::platform::picture::Pixels::Bitmap(image) = &fitting.image else {
            unreachable!("a test picture is a bitmap")
        };
        // SAFETY: a read of the layer's own property.
        let shown =
            unsafe { tile.picture.contents() }.map(|object| Retained::as_ptr(&object) as usize);
        assert_eq!(
            shown,
            Some(&**image as *const _ as usize),
            "on the picture layer"
        );
    }

    /// A resizing tile's repaint fits the end of its leg and not its start, so it lands on the crop
    /// grid and the tile stays cropped: only a picture fitting both ends of a movement is scaled.
    #[test]
    fn a_resizing_tile_stays_cropped_when_its_repaint_lands() {
        use crate::pictures::platform::picture::test_picture;
        let (from, to): (GroupRect, GroupRect) =
            (typed(4.0, 3.0, 859.0, 1074.0), typed(4.0, 3.0, 1148.0, 1074.0));
        let mut tile = strip_tile(to);
        tile.anchors = Anchors::of_leg(from, to);
        set_tile_content(
            &mut tile,
            &test_picture(from.size),
            ContentMode::Crop,
            from.size.respace(),
            2.0,
        );
        let track = tile_track(from, to, 100.0, 0.35);
        install_tile_track(&mut tile, &track, None);

        let repaint = test_picture(to.size);
        swap_onto(&mut tile, &repaint, to.size.respace(), Some(&track), 2.0);
        assert_eq!(tile.crop_of, Some(to.size.respace()), "cropped on the repaint");
        let grid = tile.crop_grid.as_ref().expect("a grid");
        assert!(
            grid.pieces.iter().all(|piece| !piece.isHidden()),
            "the pieces show"
        );
        // SAFETY: a read of the layer's own property, which retains what it returns.
        let contents = unsafe { tile.picture.contents() };
        assert!(contents.is_none(), "nothing on the picture layer");
    }

    /// A clipped picture's corners are cut at its corner, and only the picture is: the shadow
    /// caster behind it is never clipped. Unclipped, the picture draws whole.
    #[test]
    fn a_tiles_picture_clips_to_its_corners_and_its_shadow_does_not() {
        let container = CALayer::layer();
        let mut tile = new_tile(&container);
        assert!(!tile.picture.masksToBounds(), "whole by default");
        clip_corners(&mut tile, true);
        assert!(tile.picture.masksToBounds());
        assert!(!tile.decorations.shadow.masksToBounds());
        assert_eq!(tile.decorations.shadow.cornerRadius(), 0.0);
        clip_corners(&mut tile, false);
        assert!(!tile.picture.masksToBounds());
    }

    /// The hairline rides beside the picture, not in it, so a corner clip on the picture leaves the
    /// outline, which lies outside the window, whole.
    #[test]
    fn a_corner_clip_leaves_the_hairline_whole() {
        let container = CALayer::layer();
        let mut tile = new_tile(&container);
        clip_corners(&mut tile, true);
        let parent = |layer: &CALayer| layer.superlayer().map(|parent| Retained::as_ptr(&parent));
        assert_eq!(
            parent(&tile.decorations.hairline),
            Some(&*container as *const CALayer)
        );
    }

    /// A pooled tile placed at a new size wears its hairline at that frame, in front of its
    /// picture.
    #[test]
    fn the_hairline_is_laid_at_the_tiles_frame() {
        let tile = strip_tile(typed(4.0, 32.0, 1720.0, 1081.0));
        let frame = typed(4.0, 32.0, 859.0, 1081.0);
        place_tile(&tile, frame, -2.0);
        let hairline = &tile.decorations.hairline;
        assert_eq!(hairline.frame(), frame.to_cg());
        assert!(hairline.zPosition() > tile.picture.zPosition());
    }

    /// An entrance grows from no width: the hairline's bounds ride the tile's track, and its corner
    /// and the outline's path go from the zero-width shape to the slot's on the same timing.
    #[test]
    fn a_resize_carries_the_hairline_on_the_tiles_timing() {
        let _core_animation = hold_core_animation();
        let (from, to): (GroupRect, GroupRect) =
            (typed(4.0, 32.0, 0.0, 1081.0), typed(4.0, 32.0, 859.0, 1081.0));
        let mut tile = strip_tile(to);
        install_tile_track(&mut tile, &tile_track(from, to, 100.0, 0.35), None);
        let (start, end, begin) =
            animation_value(&tile.decorations.hairline, "rini.tile.size", rect_of);
        assert_eq!(
            (start.size, end.size, begin),
            (from.size.to_cg(), to.size.to_cg(), 100.0)
        );

        let corner = basic(&tile.decorations.hairline, "rini.tile.hairline.corner");
        let number = |value: Option<Retained<objc2::runtime::AnyObject>>| {
            value
                .expect("a value")
                .downcast_ref::<NSNumber>()
                .expect("a number")
                .doubleValue()
        };
        assert_eq!(
            (
                number(corner.fromValue()),
                number(corner.toValue()),
                corner.beginTime()
            ),
            (0.0, 16.0, 100.0)
        );

        let path = basic(&tile.decorations.outline, "rini.tile.outline.path");
        assert_eq!(path_bounds(path.fromValue()), rect(-0.25, -0.25, 0.5, 1081.5));
        assert_eq!(path_bounds(path.toValue()), rect(-0.25, -0.25, 859.5, 1081.5));
        assert_eq!(path.beginTime(), 100.0);
    }

    /// An entrance grows from no width, so its corner grows with it: at every key the silhouette,
    /// the mask's hole, the hairline and the picture round to the one corner, clamped to that key's
    /// size. A resize after it that keeps the corner takes the picture's corner away with the
    /// hairline's, or it would go on growing from none.
    #[test]
    fn a_resize_carries_one_corner_on_every_piece() {
        let _core_animation = hold_core_animation();
        let (from, to): (GroupRect, GroupRect) =
            (typed(4.0, 32.0, 0.0, 1081.0), typed(4.0, 32.0, 859.0, 1081.0));
        let mut tile = strip_tile(to);
        tile.decorations.radius = Some(16.0);
        let track = tile_track(from, to, 100.0, 0.35);
        install_tile_track(&mut tile, &track, None);
        let corner = |v: Value| shape::radius(Some(16.0), size_of(v));
        let hairline = installed(
            &tile.decorations.hairline,
            "rini.tile.hairline.corner",
            number_in,
        );
        assert_eq!(hairline, expected(&track, corner));
        assert_eq!(hairline.keys.iter().map(|key| key.1).collect::<Vec<_>>(), [
            0.0, 16.0
        ]);
        assert_eq!(
            installed(&tile.picture, "rini.tile.picture.corner", number_in),
            hairline
        );
        assert_eq!(
            installed(&tile.decorations.shadow, "rini.tile.shadow.path", path_of),
            expected(&track, |v| silhouette_path(size_of(v), corner(v)))
        );
        assert_eq!(
            installed(&tile.decorations.shadow_mask, "rini.tile.mask.path", path_of),
            expected(&track, |v| mask_path(size_of(v), corner(v)))
        );

        let held = tile_track(typed(4.0, 32.0, 1200.0, 1081.0), to, 100.0, 0.35);
        install_tile_track(&mut tile, &held, None);
        assert_eq!(
            animation_keys(&tile.picture),
            [TILE_ANIMATION_KEY, TILE_SIZE_KEY],
            "the corner holds"
        );
        assert!(
            !animation_keys(&tile.decorations.hairline)
                .contains(&"rini.tile.hairline.corner".to_owned())
        );
    }

    /// A resize animates the shadow's silhouette, its mask and the outline as paths. Handed over as
    /// pointers they failed objc2's encoding check, so a debug build panicked on the first resize.
    #[test]
    fn a_resize_hands_its_paths_to_core_animation_as_objects() {
        let (from, to) = (TileSize::new(1200.0, 1081.0), TileSize::new(861.0, 1081.0));
        let track = tile_track(
            GroupRect::new(Point::origin(), from.respace()),
            GroupRect::new(Point::origin(), to.respace()),
            100.0,
            0.35,
        );
        let decorations = DecorationLayers::new(&CALayer::layer());
        let animation = TileResize(&track, |size| decorations.fit_at(size))
            .path("shadowPath", |fit| {
                // SAFETY: a null transform means the path is taken as given.
                unsafe {
                    objc2_core_graphics::CGPath::with_rect(
                        CGRect::new(CGPoint::new(0.0, 0.0), fit.size.to_cg()),
                        std::ptr::null(),
                    )
                }
            })
            .downcast::<CABasicAnimation>()
            .expect("a basic animation");
        assert_eq!(path_bounds(animation.fromValue()).size, from.to_cg());
        assert_eq!(path_bounds(animation.toValue()).size, to.to_cg());
    }

    /// The bounding box of a path an animation was given.
    fn path_bounds(value: Option<Retained<objc2::runtime::AnyObject>>) -> CGRect {
        path_in(&value.expect("a path"))
    }

    fn outline_bounds(tile: &Tile) -> CGRect {
        objc2_core_graphics::CGPath::bounding_box(tile.decorations.outline.path().as_deref())
    }

    fn basic(layer: &CALayer, key: &str) -> Retained<CABasicAnimation> {
        assert_core_animation_held();
        // SAFETY: a read of the layer's own animation.
        unsafe { layer.animationForKey(&NSString::from_str(key)) }
            .expect("an animation under the key")
            .downcast::<CABasicAnimation>()
            .expect("a basic animation")
    }

    fn animation_value<T>(layer: &CALayer, key: &str, read: impl Fn(&NSValue) -> T) -> (T, T, f64) {
        let animation = basic(layer, key);
        let value = |v: Option<Retained<objc2::runtime::AnyObject>>| {
            read(v.expect("a value").downcast_ref::<NSValue>().expect("an NSValue"))
        };
        (
            value(animation.fromValue()),
            value(animation.toValue()),
            animation.beginTime(),
        )
    }

    fn rect_of(value: &NSValue) -> CGRect {
        // SAFETY: the value was made from a rect.
        unsafe { value.rectValue() }
    }

    fn strip_tile(frame: GroupRect) -> Tile {
        let container = CALayer::layer();
        let tile = new_tile(&container);
        place_tile(&tile, frame, 0.0);
        tile
    }

    mod tree {
        use rini_geometry::space::{ScreenRect, ScreenSize};
        use rini_geometry::to_overlay_space;

        use super::*;
        use crate::animation::domain::choreography::{Cue, Look, Pass, Stage};
        use crate::animation::domain::motion::accents::Accent;
        use crate::animation::domain::motion::plan::{self, Banding, FlightPlan, reflow_plan};
        use crate::animation::domain::motion::testing::{DISPLAY, VIEWPORT};
        use crate::pictures::platform::picture::test_picture;

        fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

        /// A lifted overlay holds no desktop: the backdrop lets go of its picture at the release.
        #[test]
        fn a_released_tree_lets_go_of_the_desktop_behind_it() {
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let desktop = test_picture(DISPLAY.size);
            let crate::pictures::platform::picture::Pixels::Bitmap(image) = &desktop.image else {
                unreachable!("a test picture is a bitmap")
            };
            let held_elsewhere = image.retain_count();
            tree.set_backdrop(&desktop);
            assert_eq!(
                image.retain_count(),
                held_elsewhere + 1,
                "drawn behind the tiles"
            );
            assert!(!tree.backdrop.isHidden());
            tree.release();
            // SAFETY: a read of the layer's own property.
            assert!(unsafe { tree.backdrop.contents() }.is_none());
            assert!(tree.backdrop.isHidden());
            assert_eq!(image.retain_count(), held_elsewhere, "let go of");
        }

        /// Each swap a drain makes onto the crop grid counts a crop-drawn tile and the time its
        /// picture's ground took to read; a picture fitting its tile counts nothing, and taking the
        /// count starts the next drain from none.
        #[test]
        fn a_tree_counts_the_crop_drawn_tiles_a_drain_swaps_and_their_ground_reads() {
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let frame: GroupRect = typed(865.0, 3.0, 859.0, 1074.0);
            tree.replica.apply(&Edit::Place(Node::Tile(wid(1)), Value::Rect(frame)));
            let tile = new_tile(&tree.root);
            tree.tile_layers.insert(wid(1), tile);

            tree.swap(wid(1), &test_picture(TileSize::new(600.0, 1074.0)), false, 0.0);
            let stale = tree.crops;
            assert_eq!(stale.fills, 1, "a stale picture is crop-drawn");
            assert!(stale.ground > Duration::ZERO, "and its ground read");
            tree.swap(wid(1), &test_picture(frame.size), false, 0.0);
            assert_eq!(
                tree.crops, stale,
                "a fitting picture is stretched and reads nothing"
            );

            assert_eq!(tree.take_crops(), stale);
            assert_eq!(tree.crops, Crops::default(), "the next drain counts from none");
        }

        /// A leg in two steps installed with a repaint from its corner reads the repaint's ground as
        /// its grid is cut onto it, and the drain counts that cut beside the tile's dressing; with
        /// no repaint it counts the dressing alone.
        #[test]
        fn a_tree_counts_the_ground_a_corner_cut_reads() {
            use crate::animation::domain::motion::tile::leg;
            let window = wid(1);
            let (from, to): (GroupRect, GroupRect) = (
                typed(866.0, 35.0, 860.0, 1079.0),
                typed(3.0, 576.0, 1148.0, 538.0),
            );
            let folding = leg(window, from, to, 100.0, 0.35, MOTION_CURVE);
            let drawn = |from_corner: Option<Picture>| {
                let tiles = [OverlayTile {
                    from_corner,
                    ..overlay_tile(window, plan::on_overlay(from), plan::on_overlay(to), false)
                }];
                let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
                tree.play(
                    &[
                        Edit::Place(
                            Node::Container(GroupKey::STILL),
                            Value::Point(OverlayPoint::origin()),
                        ),
                        Edit::Ride(window, GroupKey::STILL),
                        Edit::Place(Node::Tile(window), Value::Rect(to)),
                        Edit::Add(folding.clone()),
                    ],
                    &tiles,
                );
                tree.take_crops()
            };
            let dressed = drawn(None);
            assert_eq!(dressed.fills, 1, "the press picture, crop-drawn");
            let turning = drawn(Some(test_picture(to.size)));
            assert_eq!(turning.fills, 2, "and the repaint cut onto at the corner");
            assert!(turning.ground > Duration::ZERO, "its ground read");
        }

        /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
        fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

        fn overlay_tile(
            window: WindowId,
            from: OverlayRect,
            to: OverlayRect,
            floating: bool,
        ) -> OverlayTile {
            OverlayTile {
                window,
                from,
                to,
                snapshot: test_picture(from.size),
                from_corner: None,
                floating,
                server_order: Some(0),
                depth: 0,
                focused: false,
                worn: Worn::default(),
                radius: None,
                clipped: false,
            }
        }

        /// A focus ring `width` wide in Kai's ember, the only band worn.
        fn ringed(width: f64) -> crate::decorations::domain::style::Decoration {
            use crate::decorations::domain::paint::{Colour, Paint};
            let paint = Paint::Solid(Colour::rgb(0xff7c50));
            crate::decorations::domain::style::Decoration {
                ring: Some(crate::decorations::domain::style::Band {
                    width,
                    active: paint,
                    inactive: paint,
                    urgent: paint,
                }),
                ..Default::default()
            }
        }

        fn banding(windows: &[WindowId], plan: &FlightPlan) -> Banding {
            Banding {
                within: windows.iter().enumerate().map(|(i, w)| (*w, i)).collect(),
                group_order: plan.groups.iter().map(|g| g.key).collect(),
                ..Banding::default()
            }
        }

        fn rini_keys(layer: &CALayer) -> Vec<String> {
            let mut keys: Vec<String> = animation_keys(layer)
                .into_iter()
                .filter(|key| key.starts_with("rini."))
                .collect();
            keys.sort();
            keys
        }

        fn sorted(keys: impl IntoIterator<Item = &'static str>) -> Vec<String> {
            let mut keys: Vec<String> = keys.into_iter().map(String::from).collect();
            keys.sort();
            keys
        }

        /// What the tree draws is exactly its replica: a container and a tile layer for every node,
        /// every container but the floating one and its twin under the strip layer, each tile under
        /// its container, a floating one lifted in front of the strip under the floating
        /// container's twin, the strip layer and every container and tile at the depth the
        /// replica's banding gives it, the strip layer at the replica's zoom, every track installed
        /// as itself under its key (an accent a later movement begins again as far into its own
        /// time as it had got), and no `rini.*` key the replica has no track for.
        fn plays_exactly(tree: &Tree) {
            let replica = &tree.replica;
            plays_its_zoom(tree);
            plays_its_tint(tree);
            plays_its_fade(tree);
            let mut containers: Vec<String> =
                tree.containers.keys().map(|key| format!("{key:?}")).collect();
            let mut placed: Vec<String> =
                replica.containers().map(|key| format!("{key:?}")).collect();
            containers.sort();
            placed.sort();
            assert_eq!(containers, placed);
            assert_eq!(tree.tile_layers.len(), replica.tiles().count());
            let banding = replica.banding();
            let off = banding.focus_off_strip;
            for (window, key) in replica.tiles() {
                let entry = &tree.tile_layers[&window];
                let parent = entry.picture.superlayer().expect("hung under its container");
                let lifted = key == GroupKey::Floating && banding.lifted.contains(&window);
                let under = match lifted {
                    true => tree.lifted.as_ref().expect("the twin"),
                    false => &tree.containers[&key],
                };
                assert!(
                    std::ptr::eq(&*parent, &**under),
                    "{window:?} under {key:?}, lifted {lifted}"
                );
                let within = banding.within.get(&window).copied().unwrap_or(0);
                let z = -(within as f64);
                assert_eq!(entry.picture.zPosition(), z, "{window:?}");
                assert_eq!(entry.decorations.shadow.zPosition(), z + SHADOW_Z, "{window:?}");
            }
            for (key, layer) in &tree.containers {
                let z = match key {
                    GroupKey::Floating => container_z(Band::Behind, off),
                    key => {
                        let order = &banding.group_order;
                        let index = order.iter().position(|k| k == key).unwrap_or(order.len());
                        container_z(Band::Strip, off) - index as f64 * 0.25
                    }
                };
                assert_eq!(layer.zPosition(), z, "{key:?}");
            }
            if let Some(twin) = tree.lifted.as_ref() {
                assert_eq!(twin.zPosition(), container_z(Band::Lifted, off));
            }
            for key in replica.containers() {
                let node = Node::Container(key);
                let layer = &tree.containers[&key];
                let base = replica.track(node, Channel::Base);
                for track in replica.tracks(node) {
                    let mut want = expected(track, |v| v);
                    if let Some(again) = base
                        .filter(|base| {
                            track.channel().is_additive() && base.begin() > track.begin()
                        })
                        .and_then(|base| Playing::of(track).again_at(base.begin()))
                    {
                        want.begin = again.begin;
                        assert!(close(playing(layer, container_key(track.channel())), again));
                    }
                    assert_eq!(
                        installed(layer, container_key(track.channel()), container_value),
                        want
                    );
                }
                let mut wanted: Vec<String> =
                    replica.tracks(node).map(|t| container_key(t.channel()).to_string()).collect();
                if key == GroupKey::Floating && fading(replica).is_some() {
                    wanted.push(tint::FADE_KEY.to_string());
                }
                wanted.sort();
                assert_eq!(rini_keys(layer), wanted, "{key:?}");
            }
            for (window, _) in replica.tiles() {
                let entry = &tree.tile_layers[&window];
                let track = replica.track(Node::Tile(window), Channel::Base);
                let lift = replica.track(Node::Lift(window), Channel::Base);
                let lifting = lift.is_some_and(|track| track.seconds() > 0.0);
                let carded = entry.card.is_some();
                let resizing = track
                    .is_some_and(|track| resizes(track) && (!carded || track.ends() > tree.now));
                let sampled = tree.held.and_then(|_| card_frames(tree, window));
                let turns = sampled
                    .as_ref()
                    .is_some_and(|(_, fits)| fits.iter().any(|fit| fit.corner != fits[0].corner));
                let lift_layer = entry.lift.as_ref().map(|lift| lift.layer() as *const CALayer);
                for (index, layer) in entry.layers().into_iter().enumerate() {
                    let caster = index == 1;
                    let mut keys = Vec::new();
                    let is_lift = lift_layer == Some(layer as *const CALayer);
                    if is_lift && lifting {
                        keys.push(lift::LIFT_KEY);
                    }
                    if is_lift && (resizing || turns) {
                        keys.push(lift::LIFT_PATH_KEY);
                    }
                    if let Some(track) = track {
                        assert_eq!(
                            installed(layer, TILE_ANIMATION_KEY, point_in),
                            expected(track, |v| as_frame(v).origin.to_cg())
                        );
                        keys.push(TILE_ANIMATION_KEY);
                        if resizing {
                            keys.push(TILE_SIZE_KEY);
                        }
                    }
                    if caster && (resizing || sampled.is_some()) {
                        keys.push("rini.tile.shadow.path");
                    }
                    if turns && index == 0 {
                        keys.push(PICTURE_CORNER_KEY);
                    }
                    if turns && index == 2 {
                        keys.push("rini.tile.hairline.corner");
                    }
                    assert_eq!(rini_keys(layer), sorted(keys), "{window:?} layer {index}");
                }
            }
            plays_its_lifts(tree);
            plays_its_cards(tree);
        }

        /// While the overview holds the tree, `window`'s tile along the zoom: when its dress plays
        /// and the fit at each frame.
        fn card_frames(tree: &Tree, window: WindowId) -> Option<((f64, f64), Vec<Fit>)> {
            let (span, frames) = card_dresses(tree, window)?;
            let fits = frames.iter().map(|(size, dress)| dress.laying.fit(*size)).collect();
            Some((span, fits))
        }

        /// A tile's size and dress at one frame along the zoom.
        type Dressed = (
            TileSize,
            crate::animation::domain::choreography::overview::card::Dress,
        );
        type Dresses = Vec<Dressed>;

        /// `window`'s tile's dress at each frame along the zoom, while the overview holds the tree;
        /// none for a floating window's, which no card dresses.
        fn card_dresses(tree: &Tree, window: WindowId) -> Option<((f64, f64), Dresses)> {
            use crate::animation::domain::choreography::overview::card::{dressed_over, dressing};
            if tree.replica.parent(window) == Some(GroupKey::Floating) {
                return None;
            }
            let entry = &tree.tile_layers[&window];
            let span = dressing(&tree.replica, window, tree.now)?;
            let frames = dressed_over(&tree.replica, window, entry.thumb(), span);
            (frames.len() > 1).then_some((span, frames))
        }

        /// While the overview holds the tree, every tile is the card: its picture clipped and
        /// rounded, its wash showing, as its dress has them once the replica's tracks end, the
        /// selected one alone washed, and along the zoom its picture's corner and its wash at its
        /// dress at every frame. A flight's tiles carry no card, nor a floating window's.
        fn plays_its_cards(tree: &Tree) {
            use crate::animation::domain::choreography::overview::card::dressed;
            for (window, entry) in &tree.tile_layers {
                if tree.replica.parent(*window) == Some(GroupKey::Floating) {
                    assert!(entry.card.is_none(), "{window:?} floating, a card");
                    continue;
                }
                let Some(card) = &entry.card else {
                    assert!(tree.held.is_none(), "{window:?} held, a card");
                    continue;
                };
                assert!(tree.held.is_some(), "{window:?} no card in a flight");
                assert!(entry.picture.masksToBounds(), "{window:?} clipped");
                let held = dressed(&tree.replica, *window, entry.thumb());
                let Some(frame) = tree.replica.model(Node::Tile(*window)).map(as_frame) else {
                    continue;
                };
                let size: TileSize = frame.size.respace();
                let fit = held.laying.fit(size);
                assert_eq!(entry.picture.cornerRadius(), fit.corner, "{window:?}");
                assert_eq!(entry.decorations.hairline.cornerRadius(), fit.corner);
                assert_eq!(f64::from(card.wash().opacity()), held.wash.share as f32 as f64);
                assert_eq!(card.wash().cornerRadius(), held.wash.corner);
                let out = held.wash.out;
                let wash = rect(-out, -out, size.width + 2.0 * out, size.height + 2.0 * out);
                assert_eq!(card.wash().frame(), wash, "{window:?}");
                let track = tree.replica.track(Node::Tile(*window), Channel::Base);
                let Some(((begin, end), fits)) = card_frames(tree, *window) else {
                    let live = |track: &Track| resizes(track) && track.ends() > tree.now;
                    let rides = held.wash.share > 0.0 && track.is_some_and(live);
                    let riding = if rides {
                        vec!["rini.card.wash.size"]
                    } else {
                        Vec::new()
                    };
                    assert_eq!(rini_keys(card.wash()), sorted(riding), "{window:?} its wash");
                    continue;
                };
                let (_, dresses) = card_dresses(tree, *window).expect("dressed along the zoom");
                let moves = |f: &dyn Fn(&Dressed) -> f64| {
                    let shows = dresses.iter().any(|(_, dress)| dress.wash.share > 0.0);
                    shows && dresses.iter().any(|frame| f(frame) != f(&dresses[0]))
                };
                let mut washing = Vec::new();
                if moves(&|(_, dress)| dress.wash.share) {
                    washing.push("rini.card.wash.opacity");
                }
                if moves(&|(_, dress)| dress.wash.out) {
                    washing.push("rini.card.wash.move");
                }
                if moves(&|(_, dress)| dress.wash.out)
                    || moves(&|(size, _)| size.width)
                    || moves(&|(size, _)| size.height)
                {
                    washing.push("rini.card.wash.size");
                }
                if moves(&|(_, dress)| dress.wash.corner) {
                    washing.push("rini.card.wash.corner");
                }
                assert_eq!(rini_keys(card.wash()), sorted(washing), "{window:?} its wash");
                let corner = installed(&entry.picture, PICTURE_CORNER_KEY, number_in);
                let last = (fits.len() - 1) as f64;
                assert_eq!((corner.begin, corner.seconds), (begin, end - begin));
                let corners: Vec<(f64, f64)> =
                    fits.iter().enumerate().map(|(k, fit)| (k as f64 / last, fit.corner)).collect();
                assert_eq!(corner.keys, corners, "{window:?}");
                if let Some(lift) = &entry.lift {
                    let shadow =
                        installed(&entry.decorations.shadow, "rini.tile.shadow.path", path_of);
                    let cast = installed(lift.layer(), lift::LIFT_PATH_KEY, path_of);
                    assert_eq!(cast.keys.len(), shadow.keys.len(), "{window:?}");
                    for ((_, cast), (_, shadow)) in cast.keys.iter().zip(&shadow.keys) {
                        assert!(
                            objc2_core_graphics::CGPath::equal_to_path(Some(cast), Some(shadow)),
                            "{window:?} its lift cut as its own shadow is"
                        );
                    }
                    let mask = rini_keys(lift.mask());
                    assert!(
                        mask.contains(&"rini.tile.lift.mask.path".to_string()),
                        "{mask:?}"
                    );
                }
            }
        }

        /// The track on how far the overview is held, where it has a length to play.
        fn fading(replica: &Score) -> Option<&Track> {
            replica.track(Node::Held, Channel::Base).filter(|track| track.seconds() > 0.0)
        }

        /// The floating container and its twin at the opposite of how far the replica holds the
        /// overview, whole where it is not held, its track installed on each as its opposite on
        /// `opacity`.
        fn plays_its_fade(tree: &Tree) {
            let model = tree.replica.model(Node::Held).map_or(0.0, as_number);
            for layer in tree.layers_of(GroupKey::Floating) {
                assert_eq!(f64::from(layer.opacity()), tint::faded(model) as f32 as f64);
                match fading(&tree.replica) {
                    Some(track) => {
                        let faded = |value: Value| tint::faded(as_number(value));
                        assert_eq!(
                            installed(&layer, tint::FADE_KEY, number_in),
                            expected(track, faded)
                        );
                        assert_eq!(key_path(&layer, tint::FADE_KEY), "opacity");
                    }
                    None => assert!(!rini_keys(&layer).contains(&tint::FADE_KEY.to_string())),
                }
            }
        }

        /// The tint lies over the desktop and under every container at the replica's share of the
        /// desktop's darkening, none where the replica has no tint, its track installed as itself
        /// on `opacity` unless it has no length; the desktop is blurred at that share of the blur,
        /// its track installed as itself on the filter's radius, and carries no filter where the
        /// replica has no tint.
        fn plays_its_tint(tree: &Tree) {
            plays_its_blur(tree);
            let layer = tree.tint.layer();
            assert!(TINT_Z > BACKDROP_Z && TINT_Z < BAR_Z && layer.zPosition() == TINT_Z);
            for container in tree.containers.values().chain([&tree.strip]) {
                assert!(
                    layer.zPosition() < container.zPosition(),
                    "under every container"
                );
            }
            let model = tree.replica.model(Node::Held).map_or(0.0, as_number);
            assert_eq!(f64::from(layer.opacity()), tint::opacity(model) as f32 as f64);
            match tree.replica.track(Node::Held, Channel::Base) {
                Some(track) if track.seconds() > 0.0 => {
                    let opacity = |value: Value| tint::opacity(as_number(value));
                    assert_eq!(
                        installed(layer, tint::TINT_KEY, number_in),
                        expected(track, opacity)
                    );
                    assert_eq!(key_path(layer, tint::TINT_KEY), "opacity");
                }
                _ => assert!(!rini_keys(layer).contains(&tint::TINT_KEY.to_string())),
            }
        }

        fn plays_its_blur(tree: &Tree) {
            let desktop = &tree.backdrop;
            let Some(model) = tree.replica.model(Node::Held).map(as_number) else {
                assert!(desktop.filters().is_none(), "no blur between overviews");
                assert!(rini_keys(desktop).is_empty());
                return;
            };
            let radius = tint::blurred(desktop);
            assert!((radius - tint::radius(model)).abs() < 1e-6, "{model}: {radius}");
            match tree.replica.track(Node::Held, Channel::Base) {
                Some(track) if track.seconds() > 0.0 => {
                    let radius = |value: Value| tint::radius(as_number(value));
                    assert_eq!(
                        installed(desktop, tint::BLUR_KEY, number_in),
                        expected(track, radius)
                    );
                    assert_eq!(key_path(desktop, tint::BLUR_KEY), tint::BLUR_RADIUS);
                    assert_eq!(rini_keys(desktop), sorted([tint::BLUR_KEY]));
                }
                _ => assert!(rini_keys(desktop).is_empty()),
            }
        }

        /// Every tile the replica lifts casts the lift's shadow and no other does: its caster under
        /// the tile's container at the tile's frame, behind its picture as the tile's own caster is,
        /// shadowed as `LIFT_SHADOW` under the zoom the tree casts lifts under and shaped and masked
        /// for the tile's size `LIFT_REACH` out, grown with the blur, at the replica's share of the
        /// lift's opacity, its track installed as itself on `shadowOpacity` unless it has no
        /// length.
        fn plays_its_lifts(tree: &Tree) {
            use crate::decorations::domain::shape::{LIFT_REACH, mask_frame};
            use crate::decorations::domain::style::LIFT_SHADOW;
            let cast = LIFT_SHADOW.under(tree.lifts_under());
            let replica = &tree.replica;
            for (window, _) in replica.tiles() {
                let entry = &tree.tile_layers[&window];
                let model = replica.model(Node::Lift(window));
                assert_eq!(entry.lift.is_some(), model.is_some(), "{window:?}");
                let (Some(lift), Some(model)) = (&entry.lift, model) else {
                    continue;
                };
                let caster = lift.layer();
                let (parent, under) = (caster.superlayer(), entry.picture.superlayer());
                assert!(parent.zip(under).is_some_and(|(p, u)| std::ptr::eq(&*p, &*u)));
                assert_eq!(caster.frame(), entry.picture.frame(), "{window:?}");
                assert_eq!(caster.zPosition(), entry.picture.zPosition() + SHADOW_Z);
                assert_eq!(caster.shadowRadius(), cast.radius);
                assert_eq!(caster.shadowOffset(), CGSize::new(0.0, cast.offset_y));
                let opacity = |value: Value| lift::opacity(as_number(value));
                assert_eq!(f64::from(caster.shadowOpacity()), opacity(model) as f32 as f64);
                let size = TileSize::from_cg(entry.picture.frame().size);
                let silhouette = caster.shadowPath().expect("a silhouette");
                let bounds = objc2_core_graphics::CGPath::bounding_box(Some(&silhouette));
                assert_eq!(bounds.size, size.to_cg(), "{window:?}");
                let mask = caster.mask().expect("masked");
                let reach = LIFT_REACH * cast.radius / LIFT_SHADOW.radius;
                assert_eq!(mask.frame(), mask_frame(size, reach).to_cg(), "{window:?}");
                match replica.track(Node::Lift(window), Channel::Base) {
                    Some(track) if track.seconds() > 0.0 => {
                        assert_eq!(
                            installed(caster, lift::LIFT_KEY, number_in),
                            expected(track, opacity)
                        );
                        assert_eq!(key_path(caster, lift::LIFT_KEY), "shadowOpacity");
                    }
                    _ => assert!(!rini_keys(caster).contains(&lift::LIFT_KEY.to_string())),
                }
            }
        }

        /// The strip layer under the root at the strip's depth, every container but the floating
        /// one and its twin under it, its transform the replica's zoom, at rest with none, and its
        /// zoom's track installed as itself on `transform` under its key, the only key it has.
        fn plays_its_zoom(tree: &Tree) {
            let (replica, strip) = (&tree.replica, &tree.strip);
            let under = |layer: &CALayer, parent: &CALayer| {
                layer.superlayer().is_some_and(|hung| std::ptr::eq(&*hung, parent))
            };
            assert!(under(strip, &tree.root));
            let off = replica.banding().focus_off_strip;
            assert_eq!(strip.zPosition(), container_z(Band::Strip, off));
            for (key, layer) in &tree.containers {
                let parent = if *key == GroupKey::Floating {
                    &tree.root
                } else {
                    strip
                };
                assert!(under(layer, parent), "{key:?}");
            }
            if let Some(twin) = tree.lifted.as_ref() {
                assert!(under(twin, &tree.root), "the twin");
            }
            let model = replica.model(Node::Strip).unwrap_or(Value::Zoom(Zoom::REST));
            assert_eq!(zoom_in_transform(strip.transform()), model);
            match replica.track(Node::Strip, Channel::Base) {
                Some(track) => {
                    assert_eq!(installed(strip, STRIP_ZOOM_KEY, zoom_in), expected(track, |v| v));
                    assert_eq!(key_path(strip, STRIP_ZOOM_KEY), "transform");
                    assert_eq!(rini_keys(strip), [STRIP_ZOOM_KEY]);
                }
                None => assert!(rini_keys(strip).is_empty()),
            }
        }

        /// The key path the animation under `key` plays.
        fn key_path(layer: &CALayer, key: &str) -> String {
            // SAFETY: a read of the layer's own animation.
            let animation = unsafe { layer.animationForKey(&NSString::from_str(key)) }
                .unwrap_or_else(|| panic!("nothing under {key}"));
            let animation =
                animation.downcast::<CAPropertyAnimation>().expect("a property animation");
            animation.keyPath().expect("a key path").to_string()
        }

        /// A transform read back as the zoom it draws, checked to be a scale and a translation
        /// alone.
        fn zoom_in_transform(t: CATransform3D) -> Value {
            let identity = zoom_transform(Zoom::REST);
            let rest = [
                t.m12, t.m13, t.m14, t.m21, t.m23, t.m24, t.m31, t.m32, t.m33, t.m34,
            ];
            let want = [
                identity.m12,
                identity.m13,
                identity.m14,
                identity.m21,
                identity.m23,
                identity.m24,
                identity.m31,
                identity.m32,
                identity.m33,
                identity.m34,
            ];
            assert_eq!((rest, t.m22, t.m43, t.m44), (want, t.m11, 0.0, 1.0), "{t:?}");
            Value::Zoom(Zoom {
                scale: t.m11,
                shift: OverlayVector::new(t.m41, t.m42),
            })
        }

        fn zoom_in(value: &AnyObject) -> Value {
            let value = value.downcast_ref::<NSValue>().expect("an NSValue");
            // SAFETY: the value was made from a transform.
            zoom_in_transform(unsafe { value.CATransform3DValue() })
        }

        /// A zoom of the strip as a zoom look would play one, over every look's flight: out to 0.88
        /// and back on a sin² arc about the display's centre, its camera panning a column, in 1/120s
        /// samples from `begin`.
        fn zoom_arc(begin: f64, seconds: f64) -> Vec<Edit> {
            let centre = OverlayPoint::new(864.0, 558.5);
            let steps = (seconds / SAMPLE_STEP).ceil() as usize;
            let arc = (0..=steps).map(|k| {
                let f = k as f64 / steps as f64;
                let scale = 1.0 - 0.12 * (std::f64::consts::PI * f).sin().powi(2);
                let camera = OverlayVector::new(-863.0 * (1.0 - f), 0.0);
                Value::Zoom(Zoom::about(scale, centre, camera))
            });
            vec![
                Edit::Place(Node::Strip, Value::Zoom(Zoom::REST)),
                Edit::Add(Track::samples(Node::Strip, Channel::Base, arc, begin, seconds)),
            ]
        }

        /// Every look's flight posed and played with the strip zoomed over it, then bent by a pan
        /// with a window joining and the floating window leaving, the zoom sent home from where it
        /// is drawn: the tree plays exactly its replica, the replica is the fold of the edits it was
        /// handed, the joiner rides a container in it, the floating window has no tile in it and
        /// every container in it carries a tile.
        #[test]
        fn the_overlay_plays_exactly_its_score() {
            let _core_animation = hold_core_animation();
            for look in Look::ALL {
                plays_exactly_its_score(look);
            }
        }

        fn plays_exactly_its_score(look: Look) {
            let column = |i: f64, w: f64| -> ScreenRect { typed(4.0 + i * 863.0, 32.0, w, 1081.0) };
            let step = |frame: ScreenRect| -> ScreenRect {
                typed(
                    frame.origin.x - 863.0,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                )
            };
            let (a, b, c, d, e, f) = (wid(1), wid(2), wid(3), wid(4), wid(5), wid(6));
            let requests = [
                Resolved::tiled(a, column(0.0, 859.0), step(column(0.0, 859.0))),
                Resolved::tiled(b, column(1.0, 859.0), step(column(1.0, 859.0))),
                Resolved::tiled(c, column(2.0, 859.0), step(column(2.0, 859.0))),
                Resolved::tiled(d, column(1.0, 400.0), column(1.0, 600.0)),
                Resolved::floating(
                    e,
                    typed(224.0, 95.0, 600.0, 400.0),
                    typed(264.0, 95.0, 600.0, 400.0),
                ),
            ];
            let mut tiles: Vec<OverlayTile> = requests
                .iter()
                .map(|r| overlay_tile(r.window, on(r.start), on(r.end), r.floating))
                .collect();
            let plan = FlightPlan::from(reflow_plan(&requests, DISPLAY));
            let windows: Vec<WindowId> = tiles.iter().map(|t| t.window).collect();

            let mut choreography = look.choreography();
            let mut score = choreography.pose(&Stage {
                plan: &plan,
                drawn: &windows,
                seconds: 0.35,
                cue: Cue::Plain,
                accents: &[],
                viewport: VIEWPORT,
            });
            score.apply(&Edit::Bank(Banding {
                focus_off_strip: true,
                lifted: vec![e],
                ..banding(&windows, &plan)
            }));
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            assert_eq!(tree.replica, score, "{look:?}");
            assert!(
                tree.lifted.is_some(),
                "{look:?}: e is drawn in front of the strip"
            );
            plays_exactly(&tree);

            let steps = [Accent::Step {
                offset: OverlayVector::new(-576.0, 0.0),
                seconds: 0.35,
            }];
            let stage = Stage {
                plan: &plan,
                drawn: &windows,
                seconds: 0.35,
                cue: Cue::Plain,
                accents: &steps,
                viewport: VIEWPORT,
            };
            let mut started = choreography.compose(&score, &stage, 100.0);
            started.extend(zoom_arc(100.0, 0.35));
            for edit in &started {
                score.apply(edit);
            }
            tree.play(&started, &tiles);
            assert_eq!(tree.replica, score, "{look:?}");
            plays_exactly(&tree);

            let joined = column(3.0, 859.0);
            tiles.push(overlay_tile(f, on(joined), on(step(joined)), false));
            let incoming = reflow_plan(
                &[
                    Resolved::tiled(a, step(column(0.0, 859.0)), step(step(column(0.0, 859.0)))),
                    Resolved::tiled(b, step(column(1.0, 859.0)), step(step(column(1.0, 859.0)))),
                    Resolved::tiled(c, step(column(2.0, 859.0)), step(step(column(2.0, 859.0)))),
                    Resolved::tiled(f, joined, step(joined)),
                ],
                DISPLAY,
            );
            let drawn = [a, b, c, d, f];
            let pass = Pass {
                plan: &incoming,
                pan: Some(OverlayVector::new(-863.0, 0.0)),
                focus: None,
                viewport: VIEWPORT,
                drawn: &drawn,
                seconds: 0.35,
                cue: Cue::Plain,
                accent: None,
            };
            let mut bent = choreography.retarget(&score, &plan, &pass, 100.1);
            bent.edits.push(Edit::Bank(banding(&drawn, &bent.plan)));
            let zoomed = score.sample(Node::Strip, 100.1).expect("zoomed");
            assert_ne!(zoomed, Value::Zoom(Zoom::REST), "{look:?}: mid-arc");
            bent.edits.push(Edit::Add(Track::between(
                Node::Strip,
                Channel::Base,
                zoomed,
                Value::Zoom(Zoom::REST),
                100.1,
                0.35,
                Curve::Bezier(EASE_IN_OUT),
            )));
            for edit in &bent.edits {
                score.apply(edit);
            }
            tree.play(&bent.edits, &tiles);
            assert_eq!(tree.replica, score, "{look:?}");
            assert!(tree.replica.parent(f).is_some(), "{look:?}: f joins");
            assert!(tree.replica.parent(e).is_none(), "{look:?}: e leaves");
            let empty = tree
                .replica
                .containers()
                .find(|key| !tree.replica.tiles().any(|(_, rides)| rides == *key));
            assert_eq!(empty, None, "{look:?}: a container no tile rides");
            assert!(
                tree.lifted.is_none(),
                "{look:?}: the twin goes with the floating container"
            );
            plays_exactly(&tree);
        }

        /// Dressed again, a tile's bands are made at their new width and laid again around its
        /// frame at it; a band its new decoration lacks is taken off.
        #[test]
        fn a_rewear_lays_the_bands_again_at_their_new_width() {
            use crate::decorations::domain::paint::{Colour, Paint};
            use crate::decorations::domain::style::{Band, Decoration, Ring};
            let _core_animation = hold_core_animation();
            let window = wid(1);
            let drawn = typed(4.0, 32.0, 859.0, 1081.0);
            let at: GroupRect = typed(4.0, 32.0, 859.0, 1081.0);
            let paint = Paint::Solid(Colour::rgb(0xff7c50));
            let worn = |width: Option<f64>| Worn {
                decoration: Decoration {
                    ring: width.map(|width| Band {
                        width,
                        active: paint,
                        inactive: paint,
                        urgent: paint,
                    }),
                    ..Decoration::default()
                },
                wears_ring: Ring::Active,
                holds_focus: true,
            };
            let mut tile = overlay_tile(window, drawn, drawn, false);
            tile.worn = worn(Some(1.0));
            let tiles = [tile];
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(at)),
                ],
                &tiles,
            );
            let ring = |tree: &Tree| {
                let bands = &tree.tile_layers[&window].decorations.bands;
                bands.ring.as_ref().map(|band| band.layer.frame())
            };
            assert_eq!(ring(&tree), Some(rect(-1.0, -1.0, 861.0, 1083.0)));
            tree.wear(window, worn(Some(4.5)));
            assert_eq!(
                ring(&tree),
                Some(rect(-1.0, -1.0, 861.0, 1083.0)),
                "a wear keeps it"
            );
            tree.rewear(window, worn(Some(4.5)));
            assert_eq!(ring(&tree), Some(rect(-4.5, -4.5, 868.0, 1090.0)));
            tree.rewear(window, worn(None));
            assert_eq!(ring(&tree), None);
        }

        /// The overview's outline of an empty workspace hangs in its row's container over its
        /// tiling area, ringed in the active paint while its row is selected and in the inactive
        /// one otherwise; drawn again, the ones before go, and with no ring nothing is drawn.
        #[test]
        fn an_empty_workspace_is_outlined_in_its_rows_container() {
            use crate::animation::domain::choreography::overview::Outline;
            use crate::decorations::domain::paint::{Colour, Paint};
            use crate::decorations::domain::style::Band;
            let _core_animation = hold_core_animation();
            let (active, inactive) = (Colour::rgb(0xffa500), Colour::rgb(0x505050));
            let band = Band {
                width: 4.0,
                active: Paint::Solid(active),
                inactive: Paint::Solid(inactive),
                urgent: Paint::Solid(inactive),
            };
            let key = GroupKey::Rigid(5);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[Edit::Place(
                    Node::Container(key),
                    Value::Point(OverlayPoint::new(0.0, 2400.0)),
                )],
                &[],
            );
            let frame: GroupRect = typed(16.0, 48.0, 1696.0, 1053.0);
            let outline = |selected| Outline { key, frame, selected };
            let painted = |ring: &objc2_quartz_core::CAGradientLayer, c: Colour| {
                let colour = objc2_core_graphics::CGColor::new_srgb(
                    c.r.into(),
                    c.g.into(),
                    c.b.into(),
                    c.a.into(),
                );
                objc2_core_graphics::CGColor::equal_to_color(
                    ring.borderColor().as_deref(),
                    Some(&colour),
                )
            };
            for (selected, paint) in [(true, active), (false, inactive)] {
                let containers = tree.containers.clone();
                tree.outlines.draw(&[outline(selected)], Some(band), &containers, 2.0);
                let drawn = tree.outlines.drawn();
                assert_eq!(drawn.len(), 1, "the one before goes");
                let (holder, ring) = &drawn[0];
                assert_eq!(holder.frame(), frame.to_cg());
                let parent = holder.superlayer().expect("hung");
                assert!(
                    std::ptr::eq(&*parent, &*tree.containers[&key]),
                    "in its row's container"
                );
                let ring = ring.as_ref().expect("ringed");
                assert!(!ring.isHidden() && painted(ring, paint), "selected: {selected}");
                assert_eq!(ring.frame(), rect(-4.0, -4.0, 1704.0, 1061.0), "around it");
            }
            let containers = tree.containers.clone();
            tree.outlines.draw(&[outline(true)], None, &containers, 2.0);
            assert!(tree.outlines.drawn().is_empty());
        }

        /// An empty workspace's outline keeps its ring's width on screen under the strip's zoom:
        /// drawn at rest at its width, and through a zoom out at every frame its width divided by
        /// the zoom, ending at that under the zoom held.
        #[test]
        fn an_empty_workspace_s_outline_keeps_its_width_on_screen_through_the_zoom() {
            use crate::animation::domain::choreography::overview::Outline;
            let _core_animation = hold_core_animation();
            let key = GroupKey::Rigid(5);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let rest = Value::Zoom(Zoom::REST);
            tree.play(
                &[
                    Edit::Place(Node::Strip, rest),
                    Edit::Place(Node::Container(key), Value::Point(OverlayPoint::new(0.0, 0.0))),
                ],
                &[],
            );
            let frame: GroupRect = typed(16.0, 48.0, 1696.0, 1053.0);
            let outline = Outline { key, frame, selected: true };
            let ring = ringed(1.0).ring;
            let containers = tree.containers.clone();
            tree.outlines.draw(&[outline], ring, &containers, 2.0);
            tree.dress_outlines();
            let band = |tree: &Tree| tree.outlines.drawn()[0].1.clone().expect("ringed");
            assert_eq!(band(&tree).borderWidth(), 1.0, "its width at rest");
            let deep = Zoom {
                scale: 0.3,
                shift: OverlayVector::zero(),
            };
            let zoom = Track::between(
                Node::Strip,
                Channel::Base,
                rest,
                Value::Zoom(deep),
                100.0,
                0.35,
                Curve::Bezier(crate::animation::domain::motion::easing::MOTION_CURVE),
            );
            tree.play(
                &[
                    Edit::Place(Node::Strip, Value::Zoom(deep)),
                    Edit::Add(zoom.clone()),
                ],
                &[],
            );
            let ring = band(&tree);
            assert!(
                (ring.borderWidth() * 0.3 - 1.0).abs() < 1e-9,
                "held at its width on screen"
            );
            let widths = installed(&ring, "rini.tile.band.width", number_in);
            assert_eq!(widths.begin, zoom.begin());
            for (unit, width) in &widths.keys {
                let Some(Value::Zoom(drawn)) =
                    tree.replica.sample(Node::Strip, 100.0 + 0.35 * unit)
                else {
                    panic!("zoomed")
                };
                assert!((width * drawn.scale - 1.0).abs() < 1e-9, "{unit}: {width}");
            }
        }

        /// The overview's session on the tree: frame zero, the zoom out, a press bending it, a
        /// step that sends a column into an edge stack and brings one out, a push against the
        /// strip's end, a window moving to the row below and the zoom back in. The tree plays
        /// exactly its replica, which is the score, after each.
        #[test]
        fn the_overview_plays_exactly_its_score() {
            use crate::animation::domain::choreography::overview::{
                Motion, Press, at_rest, held, pose, pushes, toward,
            };
            use crate::animation::domain::motion::plan::{EDGE_BOUNCE_OVERSHOOT, on_overlay};
            use crate::overview::domain::fixture::{Col, overview, row, strip, w};
            use crate::overview::domain::scene::{Scene, lay_out};

            let _core_animation = hold_core_animation();
            static IDS: [u32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
            let scene = |current: u32, below: bool| -> (Scene, Vec<(WindowId, OverlayRect)>) {
                let ids: Vec<u32> = (1..=8).filter(|id| !below || *id != current).collect();
                let columns: Vec<Col> = ids
                    .iter()
                    .map(|&id| Col {
                        width: if id % 2 == 0 { 1400.0 } else { 800.0 },
                        windows: std::slice::from_ref(&IDS[id as usize]),
                    })
                    .collect();
                let mut second: Vec<Col> = vec![Col { width: 1400.0, windows: &[11] }];
                if below {
                    second.push(Col {
                        width: 800.0,
                        windows: std::slice::from_ref(&IDS[current as usize]),
                    });
                }
                let first_current = if below { ids[0] } else { current };
                let input = overview(
                    vec![
                        row(strip(&columns, 0.0), Some(first_current)),
                        row(strip(&second, 0.0), Some(if below { current } else { 11 })),
                    ],
                    usize::from(below),
                );
                let shown = input.rows[input.active]
                    .columns
                    .iter()
                    .flat_map(|column| &column.windows)
                    .map(|laid| (laid.window, to_overlay_space(laid.frame, DISPLAY)))
                    .collect();
                (lay_out(&input), shown)
            };
            let mut tiles: Vec<OverlayTile> = Vec::new();
            let mut wear = |places: &crate::animation::domain::choreography::overview::Places| {
                for (window, _, frame) in &places.tiles {
                    if !tiles.iter().any(|tile| tile.window == *window) {
                        let rect = on_overlay(*frame);
                        let mut tile = overlay_tile(*window, rect, rect, false);
                        tile.radius = Some(10.0);
                        tile.worn = Worn {
                            decoration: ringed(1.0),
                            wears_ring: if *window == w(3) {
                                crate::decorations::domain::style::Ring::Active
                            } else {
                                crate::decorations::domain::style::Ring::None
                            },
                            holds_focus: *window == w(3),
                        };
                        tiles.push(tile);
                    }
                }
                tiles.clone()
            };
            let (opened, shown) = scene(3, false);
            let rest = at_rest(&opened, &shown);
            let mut score = pose(&rest);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &wear(&rest));
            assert_eq!(tree.replica, score);
            plays_exactly(&tree);
            tree.hold(opened.scale);
            plays_exactly(&tree);

            let mut play = |score: &mut Score, edits: Vec<Edit>, tiles: Vec<OverlayTile>| {
                for edit in &edits {
                    score.apply(edit);
                }
                tree.play(&edits, &tiles);
                assert_eq!(tree.replica, *score);
                plays_exactly(&tree);
            };
            let held_open = held(&opened);
            let edits = toward(&score, &held_open, 100.0, Motion::Slide { seconds: 0.35 });
            play(&mut score, edits, wear(&held_open));

            let (stepped, _) = scene(4, false);
            let places = held(&stepped);
            let edits = toward(&score, &places, 100.1, Motion::Slide { seconds: 0.35 });
            play(&mut score, edits, wear(&places));

            let (far, _) = scene(6, false);
            let places = held(&far);
            let mut edits = toward(&score, &places, 101.0, Motion::Slide { seconds: 0.35 });
            let push = Press::Pushed {
                press: Cue::Plain,
                overshoot: OverlayVector::new(EDGE_BOUNCE_OVERSHOOT, 0.0),
            };
            edits.extend(pushes(&score, &far, &[push], 0.35, 101.0));
            play(&mut score, edits, wear(&places));

            let (moved, landing) = scene(6, true);
            let places = held(&moved);
            let edits = toward(&score, &places, 102.0, Motion::Slide { seconds: 0.35 });
            assert!(edits.contains(&Edit::Ride(
                w(6),
                crate::animation::domain::choreography::overview::strip_of(1)
            )));
            play(&mut score, edits, wear(&places));

            let places = at_rest(&moved, &landing);
            let edits = toward(&score, &places, 103.0, Motion::Slide { seconds: 0.35 });
            play(&mut score, edits, wear(&places));
        }

        /// Held, the overview darkens the desktop behind its thumbs with the card's backdrop at the
        /// desktop's darkening, between the desktop and every tile, and blurs the desktop; posed at
        /// rest, and once let go, neither, the lift taking the blur's filter off the desktop.
        #[test]
        fn the_overview_darkens_and_blurs_the_desktop_behind_its_thumbs() {
            use crate::animation::domain::choreography::overview::{at_rest, held, pose};
            use crate::chrome::domain::card::{BACKDROP, DESKTOP_ALPHA, DESKTOP_BLUR};
            use crate::overview::domain::fixture::{Col, overview, row, strip};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [Col { width: 800.0, windows: &[1] }];
            let scene = lay_out(&overview(vec![row(strip(&columns, 0.0), Some(1))], 0));
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let layer = objc2::Message::retain(tree.tint.layer());
            assert!(layer.zPosition() > tree.backdrop.zPosition(), "over the desktop");
            let (red, green, blue) = BACKDROP.rgb();
            let colour = objc2_core_graphics::CGColor::new_srgb(red, green, blue, 1.0);
            assert!(objc2_core_graphics::CGColor::equal_to_color(
                layer.backgroundColor().as_deref(),
                Some(&colour)
            ));
            let tiles = |places: &crate::animation::domain::choreography::overview::Places| {
                let tiles: Vec<OverlayTile> = places
                    .tiles
                    .iter()
                    .map(|(window, _, frame)| {
                        let rect = crate::animation::domain::motion::plan::on_overlay(*frame);
                        overlay_tile(*window, rect, rect, false)
                    })
                    .collect();
                tiles
            };
            let (held, rest) = (held(&scene), at_rest(&scene, &[]));
            tree.pose(&pose(&held), &tiles(&held));
            plays_exactly(&tree);
            assert_eq!(f64::from(layer.opacity()), DESKTOP_ALPHA as f32 as f64);
            assert!((tint::blurred(&tree.backdrop) - DESKTOP_BLUR).abs() < 1e-6);
            tree.pose(&pose(&rest), &tiles(&rest));
            plays_exactly(&tree);
            assert_eq!(layer.opacity(), 0.0);
            assert_eq!(tint::blurred(&tree.backdrop), 0.0);
            tree.pose(&pose(&held), &tiles(&held));
            tree.release();
            assert_eq!(layer.opacity(), 0.0, "a lift takes it away");
            assert!(tree.backdrop.filters().is_none(), "and the blur with it");
        }

        /// The zoom out darkens and blurs the desktop from none at frame zero to the desktop's
        /// darkening and blur held, on the zoom's own clock, and the zoom back in clears both to
        /// none at the lift, so neither hand-over to the real desktop jumps.
        #[test]
        fn the_desktop_dims_and_blurs_with_the_zoom_from_none_at_frame_zero_to_none_at_the_lift() {
            use crate::animation::domain::choreography::overview::{
                Motion, at_rest, held, pose, toward,
            };
            use crate::chrome::domain::card::{DESKTOP_ALPHA, DESKTOP_BLUR};
            use crate::overview::domain::fixture::{Col, overview, row, strip};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [Col { width: 800.0, windows: &[1] }];
            let scene = lay_out(&overview(vec![row(strip(&columns, 0.0), Some(1))], 0));
            let rest = at_rest(&scene, &[]);
            let tiles: Vec<OverlayTile> = rest
                .tiles
                .iter()
                .map(|(window, _, frame)| {
                    let rect = crate::animation::domain::motion::plan::on_overlay(*frame);
                    overlay_tile(*window, rect, rect, false)
                })
                .collect();
            let score = pose(&rest);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            tree.hold(scene.scale);
            let slide = Motion::Slide { seconds: 0.35 };
            let play = |tree: &mut Tree, score: &Score, edits: Vec<Edit>| {
                let mut score = score.clone();
                for edit in &edits {
                    score.apply(edit);
                }
                tree.play(&edits, &tiles);
                plays_exactly(tree);
                score
            };
            let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
            let ends = |tree: &Tree, zoom: &Track| {
                let tint = installed(tree.tint.layer(), tint::TINT_KEY, number_in);
                let blur = installed(&tree.backdrop, tint::BLUR_KEY, number_in);
                for played in [&tint, &blur] {
                    assert_eq!((played.begin, played.seconds), (zoom.begin(), zoom.seconds()));
                }
                let first = |played: &Installed<f64>| played.keys[0].1;
                let last = |played: &Installed<f64>| played.keys[played.keys.len() - 1].1;
                ((first(&tint), last(&tint)), (first(&blur), last(&blur)))
            };

            let out = play(&mut tree, &score, toward(&score, &held(&scene), 100.0, slide));
            let zoom = out.track(Node::Strip, Channel::Base).expect("a zoom").clone();
            let ((dark_from, dark_to), (blur_from, blur_to)) = ends(&tree, &zoom);
            assert!(
                dark_from == 0.0 && blur_from == 0.0,
                "frame zero is the plain desktop"
            );
            assert!(close(dark_to, DESKTOP_ALPHA) && close(blur_to, DESKTOP_BLUR));

            let edits = toward(&out, &at_rest(&scene, &[]), 101.0, slide);
            let back = play(&mut tree, &out, edits);
            let zoom = back.track(Node::Strip, Channel::Base).expect("a zoom").clone();
            let ((dark_from, dark_to), (blur_from, blur_to)) = ends(&tree, &zoom);
            assert!(close(dark_from, DESKTOP_ALPHA) && close(blur_from, DESKTOP_BLUR));
            assert!(dark_to == 0.0 && blur_to == 0.0, "the lift is the plain desktop");
        }

        /// The overview's thumbs, read back off the tree: frame zero is the screen, every thumb at
        /// its window's corner and ring and no wash; the zoom out eases each, on screen, into the
        /// card's 11pt corner, the selected one's ring into the card's at its selection and its
        /// wash in, with the zoom's own begin and length, from the window's at its first frame;
        /// held, each is the card, and a selected thumb resizing carries its wash and its lift with
        /// it; and the zoom back in eases them out again onto the window's at its last frame, so
        /// neither hand-over to the real windows jumps. A thumb's ring keeps its width on screen
        /// through the zoom rather than thinning with it.
        #[test]
        fn the_overview_s_thumbs_ease_into_the_card_with_the_zoom_and_out_of_it() {
            use crate::animation::domain::choreography::overview::{
                Motion, at_rest, held, pose, toward,
            };
            use crate::animation::domain::motion::plan::on_overlay;
            use crate::chrome::domain::card::{CORNER, RING, SELECTION_CORNER, SELECTION_OUTSET};
            use crate::decorations::domain::style::Ring;
            use crate::overview::domain::fixture::{Col, overview, row, strip, w};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [
                Col { width: 800.0, windows: &[1] },
                Col { width: 800.0, windows: &[2] },
                Col { width: 800.0, windows: &[3] },
            ];
            let input = overview(vec![row(strip(&columns, 0.0), Some(2))], 0);
            let scene = lay_out(&input);
            let depth = scene.scale;
            let shown: Vec<(WindowId, OverlayRect)> = input.rows[0]
                .columns
                .iter()
                .flat_map(|column| &column.windows)
                .map(|laid| (laid.window, to_overlay_space(laid.frame, DISPLAY)))
                .collect();
            let rest = at_rest(&scene, &shown);
            let tiles: Vec<OverlayTile> = rest
                .tiles
                .iter()
                .map(|(window, _, frame)| {
                    let rect = on_overlay(*frame);
                    let mut tile = overlay_tile(*window, rect, rect, false);
                    tile.radius = Some(10.0);
                    let selected = *window == w(2);
                    tile.worn = Worn {
                        decoration: ringed(1.0),
                        wears_ring: if selected { Ring::Active } else { Ring::None },
                        holds_focus: selected,
                    };
                    tile
                })
                .collect();
            let mut score = pose(&rest);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            tree.hold(depth);
            plays_exactly(&tree);
            let entry = |tree: &Tree,
                         window: WindowId|
             -> (Retained<CALayer>, Retained<CALayer>, Retained<CALayer>) {
                let entry = &tree.tile_layers[&window];
                let ring = entry.decorations.bands.ring.as_ref().expect("ringed");
                let card = entry.card.as_ref().expect("a card");
                (
                    entry.picture.clone(),
                    Retained::into_super(ring.layer.clone()),
                    objc2::Message::retain(card.wash()),
                )
            };
            let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
            for window in [w(1), w(2), w(3)] {
                let (picture, ring, wash) = entry(&tree, window);
                assert!(picture.masksToBounds(), "{window:?} clipped");
                assert_eq!(
                    picture.cornerRadius(),
                    10.0,
                    "{window:?} the window's own at rest"
                );
                assert_eq!(ring.borderWidth(), 1.0);
                assert_eq!(wash.opacity(), 0.0, "{window:?} no wash at rest");
            }

            let slide = Motion::Slide { seconds: 0.35 };
            let play = |tree: &mut Tree, score: &mut Score, edits: Vec<Edit>| {
                for edit in &edits {
                    score.apply(edit);
                }
                tree.play(&edits, &tiles);
                plays_exactly(tree);
            };
            let eased = |tree: &Tree, score: &Score, (from, to): (f64, f64)| {
                let zoom = score.track(Node::Strip, Channel::Base).expect("a zoom").clone();
                let scale = |unit: f64| match score
                    .sample(Node::Strip, zoom.begin() + zoom.seconds() * unit)
                {
                    Some(Value::Zoom(zoom)) => zoom.scale,
                    other => panic!("{other:?}"),
                };
                let rising = to > from;
                let monotone = |values: &[f64]| {
                    values.windows(2).all(|pair| {
                        if rising {
                            pair[1] >= pair[0] - 1e-9
                        } else {
                            pair[1] <= pair[0] + 1e-9
                        }
                    })
                };
                let ease = |own: f64, card: f64, share: f64| own + (card - own) * share;
                let (picture, ring, wash) = entry(tree, w(2));
                let corner = installed(&picture, PICTURE_CORNER_KEY, number_in);
                assert!(corner.begin == zoom.begin() && close(corner.seconds, zoom.seconds()));
                let on_screen: Vec<f64> =
                    corner.keys.iter().map(|(unit, corner)| corner * scale(*unit)).collect();
                assert!(close(on_screen[0], ease(10.0, CORNER, from)), "{on_screen:?}");
                assert!(close(on_screen[on_screen.len() - 1], ease(10.0, CORNER, to)));
                assert!(monotone(&on_screen), "{on_screen:?}");
                let washed = installed(&wash, "rini.card.wash.opacity", number_in);
                let shares: Vec<f64> = washed.keys.iter().map(|(_, share)| *share).collect();
                assert!(close(shares[0], from) && close(shares[shares.len() - 1], to));
                assert!(monotone(&shares), "{shares:?}");
                let reach = installed(&wash, "rini.card.wash.move", point_in);
                let outer = installed(&ring, "rini.tile.band.move", point_in);
                assert_eq!(reach.keys.len(), outer.keys.len());
                for ((unit, wash), (_, ring)) in reach.keys.iter().zip(&outer.keys) {
                    assert!(
                        -wash.x * scale(*unit) <= -ring.x * scale(*unit) + 1e-9,
                        "the wash past the ring at {unit}: {wash:?} {ring:?}"
                    );
                }
                let widths = installed(&ring, "rini.tile.band.width", number_in);
                let widths: Vec<f64> =
                    widths.keys.iter().map(|(unit, width)| width * scale(*unit)).collect();
                assert!(close(widths[0], ease(1.0, RING, from)), "{widths:?}");
                assert!(close(widths[widths.len() - 1], ease(1.0, RING, to)));
                assert!(monotone(&widths), "{widths:?}");
                let (_, beside, _) = entry(tree, w(1));
                let beside = installed(&beside, "rini.tile.band.width", number_in);
                assert!(
                    beside.keys.iter().all(|(unit, width)| close(width * scale(*unit), 1.0)),
                    "a ring as wide on screen through the zoom: {:?}",
                    beside.keys
                );
            };

            let edits = toward(&score, &held(&scene), 100.0, slide);
            play(&mut tree, &mut score, edits);
            eased(&tree, &score, (0.0, 1.0));
            for window in [w(1), w(2), w(3)] {
                let (picture, ..) = entry(&tree, window);
                assert!(
                    close(picture.cornerRadius() * depth, CORNER),
                    "{window:?} the card's"
                );
            }
            let (_, ring, wash) = entry(&tree, w(2));
            assert!(close(ring.borderWidth() * depth, RING));
            let outer = ring.frame().origin.x * depth;
            assert!(
                close(-outer, SELECTION_OUTSET),
                "at the card's selection: {outer}"
            );
            assert!(close(ring.cornerRadius() * depth, SELECTION_CORNER));
            assert_eq!(wash.opacity(), 1.0, "the selected one washed");
            assert!(close(-wash.frame().origin.x * depth, SELECTION_OUTSET));
            assert!(close(wash.cornerRadius() * depth, SELECTION_CORNER));
            let (_, _, beside) = entry(&tree, w(1));
            assert_eq!(beside.opacity(), 0.0, "only the selected one");

            let Some(Value::Rect(from)) = score.model(Node::Tile(w(2))) else {
                panic!("placed")
            };
            let to = GroupRect::new(
                from.origin,
                from.size + rini_geometry::space::Size::new(300.0, 0.0),
            );
            let resize = Track::between(
                Node::Tile(w(2)),
                Channel::Base,
                Value::Rect(from),
                Value::Rect(to),
                100.5,
                0.35,
                Curve::Bezier(crate::animation::domain::motion::easing::MOTION_CURVE),
            );
            let edits = vec![
                Edit::Place(Node::Tile(w(2)), Value::Rect(to)),
                Edit::Add(resize),
            ];
            play(&mut tree, &mut score, edits);
            let (_, _, wash) = entry(&tree, w(2));
            let washed = installed(&wash, "rini.card.wash.size", rect_in);
            let out = 2.0 * SELECTION_OUTSET / depth;
            let (first, last) = (washed.keys[0].1.size, washed.keys[washed.keys.len() - 1].1.size);
            assert!(close(first.width, from.width() + out) && close(last.width, to.width() + out));
            let lift = tree.tile_layers[&w(2)].lift.as_ref().expect("lifted");
            assert!(rini_keys(lift.layer()).contains(&lift::LIFT_PATH_KEY.to_string()));

            let edits = toward(&score, &at_rest(&scene, &shown), 101.0, slide);
            play(&mut tree, &mut score, edits);
            eased(&tree, &score, (1.0, 0.0));
            for window in [w(1), w(2), w(3)] {
                let (picture, ring, wash) = entry(&tree, window);
                assert_eq!(picture.cornerRadius(), 10.0, "{window:?} landed on its window's");
                assert_eq!(ring.borderWidth(), 1.0);
                assert_eq!(wash.opacity(), 0.0);
            }
        }

        /// Held, a thumb whose picture keeps its window's 26pt corner rounds to it on screen, wider
        /// than the card's at the overview's shallowest depth, and a clipped one to the card's; a
        /// picture swapped on with the other clip turns its thumb to the other corner.
        #[test]
        fn a_thumb_keeping_its_window_s_wider_corner_rounds_to_it_and_a_clipped_one_to_the_card_s()
        {
            use crate::animation::domain::choreography::overview::{at_rest, held, pose};
            use crate::animation::domain::motion::plan::on_overlay;
            use crate::chrome::domain::card::CORNER;
            use crate::overview::domain::fixture::{Col, overview, row, strip, w};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [Col { width: 800.0, windows: &[1] }, Col {
                width: 800.0,
                windows: &[2],
            }];
            let scene = lay_out(&overview(vec![row(strip(&columns, 0.0), Some(1))], 0));
            let depth = scene.scale;
            assert!(
                26.0 * depth > CORNER,
                "a depth the window's corner is the wider at"
            );
            let rest = at_rest(&scene, &[]);
            let tiles: Vec<OverlayTile> = rest
                .tiles
                .iter()
                .map(|(window, _, frame)| {
                    let rect = on_overlay(*frame);
                    let mut tile = overlay_tile(*window, rect, rect, false);
                    tile.radius = Some(26.0);
                    tile.clipped = *window == w(2);
                    tile
                })
                .collect();
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&pose(&held(&scene)), &tiles);
            tree.hold(depth);
            plays_exactly(&tree);
            let corner = |tree: &Tree, window: WindowId| {
                tree.tile_layers[&window].picture.cornerRadius() * depth
            };
            let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
            assert!(close(corner(&tree, w(1)), 26.0 * depth), "its own");
            assert!(close(corner(&tree, w(2)), CORNER), "the card's");
            let size = |window: WindowId| -> TileSize {
                match tree.replica.model(Node::Tile(window)) {
                    Some(Value::Rect(frame)) => frame.size.respace(),
                    other => panic!("{other:?}"),
                }
            };
            let (one, two) = (size(w(1)), size(w(2)));
            tree.swap(w(1), &test_picture(one), true, 0.0);
            tree.swap(w(2), &test_picture(two), false, 0.0);
            plays_exactly(&tree);
            assert!(close(corner(&tree, w(1)), CORNER));
            assert!(close(corner(&tree, w(2)), 26.0 * depth));
        }

        /// The overview's floating windows ride the floating container, which fades the other way
        /// to the tint: whole at rest, gone held, and played back in as the tint lightens, the twin
        /// carrying the one in front of the strip with it. Each wears what it wears on screen the
        /// whole way, the focused one's active ring included, and never the card's dress, which a
        /// thumb turned floating while held takes off.
        #[test]
        fn the_overview_s_floating_windows_fade_out_as_the_desktop_darkens() {
            use crate::animation::domain::choreography::overview::{
                Floating, Motion, at_rest, held, pose, toward, with_floating,
            };
            use crate::decorations::domain::style::Ring;
            use crate::overview::domain::fixture::{Col, overview, row, strip, w};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [Col { width: 800.0, windows: &[1] }];
            let scene = lay_out(&overview(vec![row(strip(&columns, 0.0), Some(1))], 0));
            let floating = [
                Floating {
                    window: w(20),
                    frame: typed(300.0, 200.0, 600.0, 400.0),
                    in_front: true,
                },
                Floating {
                    window: w(21),
                    frame: typed(900.0, 500.0, 400.0, 300.0),
                    in_front: false,
                },
            ];
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let tiles = |places: &crate::animation::domain::choreography::overview::Places| {
                let tiles: Vec<OverlayTile> = places
                    .tiles
                    .iter()
                    .map(|(window, key, frame)| {
                        let rect = crate::animation::domain::motion::plan::on_overlay(*frame);
                        let mut tile =
                            overlay_tile(*window, rect, rect, *key == GroupKey::Floating);
                        tile.radius = Some(10.0);
                        if *window == w(20) {
                            tile.worn = Worn {
                                decoration: ringed(1.0),
                                wears_ring: Ring::Active,
                                holds_focus: true,
                            };
                        }
                        tile
                    })
                    .collect();
                tiles
            };
            let rest = with_floating(at_rest(&scene, &[]), &floating);
            let hold = with_floating(held(&scene), &floating);
            let opacity = |tree: &Tree| -> Vec<f32> {
                tree.layers_of(GroupKey::Floating).iter().map(|layer| layer.opacity()).collect()
            };
            let as_on_screen = |tree: &Tree, window: WindowId| {
                let entry = &tree.tile_layers[&window];
                assert!(entry.card.is_none(), "{window:?} no card");
                assert!(!entry.picture.masksToBounds(), "{window:?} as it was given");
                assert_eq!(entry.picture.cornerRadius(), 10.0, "{window:?} its own corner");
                assert!(!rini_keys(&entry.picture).contains(&PICTURE_CORNER_KEY.to_string()));
            };
            tree.pose(&pose(&rest), &tiles(&rest));
            tree.hold(scene.scale);
            plays_exactly(&tree);
            assert_eq!(opacity(&tree), [1.0, 1.0], "on screen at rest, the twin with it");
            let out = toward(&tree.replica, &hold, 0.0, Motion::Slide { seconds: 0.35 });
            tree.play(&out, &tiles(&hold));
            plays_exactly(&tree);
            assert_eq!(opacity(&tree), [0.0, 0.0], "gone once darkened");
            for window in [w(20), w(21)] {
                as_on_screen(&tree, window);
            }
            assert!(tree.tile_layers[&w(1)].card.is_some(), "the thumb a card");
            let mut turned = hold.clone();
            for (window, key, frame) in &mut turned.tiles {
                if *window == w(1) {
                    *key = GroupKey::Floating;
                    *frame = GroupRect::new(
                        rini_geometry::space::Point::new(100.0, 100.0),
                        rini_geometry::space::Size::new(700.0, 500.0),
                    );
                }
            }
            turned.lifts.retain(|(window, _)| *window != w(1));
            let turn = toward(&tree.replica, &turned, 0.5, Motion::Slide { seconds: 0.35 });
            tree.play(&turn, &tiles(&turned));
            plays_exactly(&tree);
            as_on_screen(&tree, w(1));
            let resize = tree.replica.track(Node::Tile(w(1)), Channel::Base).expect("its turn");
            assert!(resizes(resize) && resize.ends() > tree.now, "turned mid-resize");
            let size = |v: Value| -> TileSize { as_frame(v).size.respace() };
            assert_eq!(
                installed(
                    &tree.tile_layers[&w(1)].decorations.shadow,
                    "rini.tile.shadow.path",
                    path_of
                ),
                expected(resize, |v| silhouette_path(
                    size(v),
                    shape::radius(Some(10.0), size(v))
                )),
                "its resize laid as its window lays it"
            );
            let back = toward(&tree.replica, &rest, 1.0, Motion::Slide { seconds: 0.35 });
            tree.play(&back, &tiles(&rest));
            plays_exactly(&tree);
            assert_eq!(opacity(&tree), [1.0, 1.0], "back as it lightens");
            for window in [w(20), w(21)] {
                as_on_screen(&tree, window);
            }
            tree.pose(&pose(&hold), &tiles(&hold));
            plays_exactly(&tree);
            assert_eq!(opacity(&tree), [0.0, 0.0], "posed held, gone");
            for window in [w(20), w(21)] {
                as_on_screen(&tree, window);
            }
        }

        /// Held, every thumb of the overview casts the lift's shadow, the selected one's at its
        /// full opacity and every other's fainter, each as wide and falling as far on screen under
        /// the zoom as a flight's lift at rest, whether cast before its thumb was lifted or after;
        /// posed at rest, none casts any; let go, the next lift is cast under no zoom.
        #[test]
        fn the_overview_s_thumbs_float_the_selected_one_highest() {
            use crate::animation::domain::choreography::overview::{
                SELECTED_LIFT, THUMB_LIFT, at_rest, held, pose,
            };
            use crate::animation::domain::motion::plan::on_overlay;
            use crate::decorations::domain::style::LIFT_SHADOW;
            use crate::overview::domain::fixture::{Col, overview, row, strip, w};
            use crate::overview::domain::scene::lay_out;

            let _core_animation = hold_core_animation();
            let columns = [
                Col { width: 800.0, windows: &[1] },
                Col { width: 800.0, windows: &[2] },
                Col { width: 800.0, windows: &[3] },
            ];
            let input = overview(vec![row(strip(&columns, 0.0), Some(2))], 0);
            let scene = lay_out(&input);
            let opacity = |tree: &Tree, window: WindowId| {
                let lift = tree.tile_layers[&window].lift.as_ref().expect("a caster");
                f64::from(lift.layer().shadowOpacity())
            };
            let tiles = |places: &crate::animation::domain::choreography::overview::Places| {
                places
                    .tiles
                    .iter()
                    .map(|(window, _, frame)| {
                        let rect = on_overlay(*frame);
                        overlay_tile(*window, rect, rect, false)
                    })
                    .collect::<Vec<OverlayTile>>()
            };
            let drawn = |places: &crate::animation::domain::choreography::overview::Places| {
                let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
                tree.pose(&pose(places), &tiles(places));
                tree.hold(scene.scale);
                plays_exactly(&tree);
                tree
            };
            let on_screen = |tree: &Tree| {
                for window in [w(1), w(2), w(3)] {
                    let caster = tree.tile_layers[&window].lift.as_ref().expect("a caster");
                    let caster = caster.layer();
                    let radius = caster.shadowRadius() * scene.scale;
                    let fall = caster.shadowOffset().height * scene.scale;
                    assert!((radius - LIFT_SHADOW.radius).abs() < 1e-9, "{window:?} {radius}");
                    assert!((fall - LIFT_SHADOW.offset_y).abs() < 1e-9, "{window:?} {fall}");
                }
            };
            let tree = drawn(&held(&scene));
            on_screen(&tree);
            let mut cast_first = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            cast_first.hold(scene.scale);
            cast_first.pose(&pose(&held(&scene)), &tiles(&held(&scene)));
            plays_exactly(&cast_first);
            on_screen(&cast_first);
            cast_first.release();
            cast_first.pose(&pose(&held(&scene)), &tiles(&held(&scene)));
            plays_exactly(&cast_first);
            let caster = cast_first.tile_layers[&w(2)].lift.as_ref().expect("a caster");
            assert_eq!(
                caster.layer().shadowRadius(),
                LIFT_SHADOW.radius,
                "under no zoom"
            );
            let full = f64::from(LIFT_SHADOW.opacity);
            assert!((opacity(&tree, w(2)) - full * SELECTED_LIFT).abs() < 1e-6);
            for window in [w(1), w(3)] {
                assert!(
                    (opacity(&tree, window) - full * THUMB_LIFT).abs() < 1e-6,
                    "{window:?}"
                );
                assert!(
                    opacity(&tree, window) < opacity(&tree, w(2)),
                    "the selected one highest"
                );
            }
            let shown: Vec<(WindowId, OverlayRect)> = input.rows[0]
                .columns
                .iter()
                .flat_map(|column| &column.windows)
                .map(|laid| (laid.window, to_overlay_space(laid.frame, DISPLAY)))
                .collect();
            let tree = drawn(&at_rest(&scene, &shown));
            for window in [w(1), w(2), w(3)] {
                assert_eq!(opacity(&tree, window), 0.0, "{window:?} flat at rest");
            }
        }

        /// A zoom look's flight on its own arc, two columns swapped and a push waiting, then a pan
        /// folded into the arc: the strip layer's zoom and every container's movement begin on one
        /// clock, and the tree plays exactly its replica, which is the score, after each.
        #[test]
        fn a_zoom_flight_plays_its_arc_exactly() {
            use crate::animation::domain::motion::accents::Swap;
            let _core_animation = hold_core_animation();
            let column = |i: f64| -> ScreenRect { typed(4.0 + i * 863.0, 32.0, 859.0, 1081.0) };
            let shifted = |frame: ScreenRect, dx: f64| -> ScreenRect {
                typed(
                    frame.origin.x + dx,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                )
            };
            let (a, b, c) = (wid(1), wid(2), wid(3));
            let requests = [
                Resolved::tiled(a, column(1.0), shifted(column(0.0), -863.0)),
                Resolved::tiled(b, column(0.0), shifted(column(1.0), -863.0)),
                Resolved::tiled(c, column(2.0), shifted(column(2.0), -863.0)),
            ];
            let tiles: Vec<OverlayTile> = requests
                .iter()
                .map(|r| overlay_tile(r.window, on(r.start), on(r.end), r.floating))
                .collect();
            let plan = FlightPlan::from(reflow_plan(&requests, DISPLAY));
            let windows = [a, b, c];
            let accents = [
                Accent::Arc {
                    depth: 0.7516,
                    share: 0.5,
                    swap: Some(Swap {
                        moved: a,
                        passed: b,
                        moved_by: OverlayVector::new(-863.0, 0.0),
                        passed_by: OverlayVector::new(863.0, 0.0),
                    }),
                    seconds: 0.35,
                },
                Accent::Push {
                    wall: OverlayVector::new(-72.0, 0.0),
                    seconds: 0.35,
                },
            ];
            let stage = Stage {
                plan: &plan,
                drawn: &windows,
                seconds: 0.35,
                cue: Cue::Focus { from: a, to: a },
                accents: &accents,
                viewport: VIEWPORT,
            };
            let mut choreography = Look::Zoom.choreography();
            let mut score = choreography.pose(&stage);
            score.apply(&Edit::Bank(banding(&windows, &plan)));
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            let started = choreography.compose(&score, &stage, 100.0);
            for edit in &started {
                score.apply(edit);
            }
            tree.play(&started, &tiles);
            assert_eq!(tree.replica, score);
            plays_exactly(&tree);
            let base: Vec<&Track> = started
                .iter()
                .filter_map(|edit| match edit {
                    Edit::Add(track) if track.channel() == Channel::Base => Some(track),
                    _ => None,
                })
                .collect();
            assert!(base.iter().any(|t| t.node() == Node::Strip), "the arc");
            assert!(
                base.iter().any(|t| t.node() == Node::Lift(a)),
                "the moved window lifted"
            );
            assert!(base.len() >= 3, "the arc and the containers: {}", base.len());
            for track in &base {
                assert_eq!((track.begin(), track.seconds()), (100.0, 0.35 * 2.2 / 3.0));
            }

            let incoming = reflow_plan(
                &requests.map(|r| Resolved::tiled(r.window, r.end, shifted(r.end, -863.0))),
                DISPLAY,
            );
            let pass = Pass {
                plan: &incoming,
                pan: Some(OverlayVector::new(-863.0, 0.0)),
                focus: None,
                viewport: VIEWPORT,
                drawn: &windows,
                seconds: 0.35,
                cue: Cue::Plain,
                accent: None,
            };
            let mut bent = choreography.retarget(&score, &plan, &pass, 100.05);
            bent.edits.push(Edit::Bank(banding(&windows, &bent.plan)));
            for edit in &bent.edits {
                score.apply(edit);
            }
            tree.play(&bent.edits, &tiles);
            assert_eq!(tree.replica, score);
            plays_exactly(&tree);
        }

        /// A zoom flight with the strips above and below beside the one it moves draws each under
        /// the strip layer, so the strip's zoom draws it, in a container of its own the arc gives
        /// no movement, the strip's zoom installed on the strip layer.
        #[test]
        fn the_strips_beside_ride_the_strip_layer_under_its_zoom() {
            use crate::animation::domain::motion::surface::SurfaceGeometry;
            use crate::transitions::domain::request::SurfaceWindow;
            let _core_animation = hold_core_animation();
            let window = |id: u32, x: f64, y: f64, row: Option<usize>| SurfaceWindow {
                window: wid(id),
                server_id: rini_core::ids::WindowServerId::new(id),
                frame: typed(x, y, 859.0, 1077.0),
                pinned: false,
                floating: false,
                from: None,
                worn: Worn::default(),
                row,
            };
            let windows = [
                window(1, 4.0, 36.0, None),
                window(2, 867.0, 36.0, None),
                window(3, 4.0, 36.0 - 1109.0, Some(0)),
                window(4, 867.0, 36.0 + 1109.0, Some(2)),
            ];
            use rini_geometry::space::SurfacePoint;
            let (from_offset, to_offset) = (SurfacePoint::new(-863.0, 0.0), SurfacePoint::origin());
            let plan = FlightPlan::from(plan::surface_plan(&windows, from_offset, to_offset));
            let tiles: Vec<OverlayTile> = windows
                .iter()
                .map(|w| {
                    let (from, to) = w.travel(from_offset, to_offset);
                    overlay_tile(w.window, from, to, false)
                })
                .collect();
            let drawn: Vec<WindowId> = windows.iter().map(|w| w.window).collect();
            let accents = [Accent::Arc {
                depth: 0.88,
                share: 0.5,
                swap: None,
                seconds: 0.35,
            }];
            let stage = Stage {
                plan: &plan,
                drawn: &drawn,
                seconds: 0.35,
                cue: Cue::Focus { from: wid(1), to: wid(2) },
                accents: &accents,
                viewport: VIEWPORT,
            };
            let mut choreography = Look::Zoom.choreography();
            let mut score = choreography.pose(&stage);
            score.apply(&Edit::Bank(banding(&drawn, &plan)));
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            let started = choreography.compose(&score, &stage, 100.0);
            for edit in &started {
                score.apply(edit);
            }
            tree.play(&started, &tiles);
            assert_eq!(tree.replica, score);
            plays_exactly(&tree);
            assert_eq!(rini_keys(&tree.strip), [STRIP_ZOOM_KEY], "the strip zooms");
            let key_of = |window| tree.replica.parent(window).expect("drawn");
            let strip = key_of(wid(1));
            for row in [wid(3), wid(4)] {
                let key = key_of(row);
                assert_ne!(key, strip, "{row:?}: a container of its own");
                let container = &tree.containers[&key];
                let hung = container.superlayer().expect("hung");
                assert!(
                    std::ptr::eq(&*hung, &*tree.strip),
                    "{row:?} under the strip layer"
                );
                assert!(
                    rini_keys(container).is_empty(),
                    "{row:?}: the arc moves it nowhere"
                );
                let picture = tree.tile_layers[&row].picture.superlayer().expect("hung");
                assert!(std::ptr::eq(&*picture, &**container), "{row:?} in its container");
            }
            assert!(!rini_keys(&tree.containers[&strip]).is_empty(), "the strip pans");
        }

        /// A zoom posed plays on the strip layer as one played does; a track of no length, a cut of
        /// its channel and a drop each take it off, the drop putting the strip back at rest, a cut of
        /// another channel leaves it playing, and a place draws the strip at its model.
        #[test]
        fn the_strip_layer_lets_go_of_its_zoom() {
            let _core_animation = hold_core_animation();
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let deep = Value::Zoom(Zoom::about(
                0.88,
                OverlayPoint::new(864.0, 558.5),
                OverlayVector::zero(),
            ));
            let home = |seconds: f64| {
                Edit::Add(Track::between(
                    Node::Strip,
                    Channel::Base,
                    deep,
                    Value::Zoom(Zoom::REST),
                    5.0,
                    seconds,
                    Curve::Linear,
                ))
            };
            let mut posed = Score::default();
            for edit in zoom_arc(5.0, 0.35) {
                posed.apply(&edit);
            }
            tree.pose(&posed, &[]);
            assert_eq!(tree.replica, posed);
            plays_exactly(&tree);
            tree.play(&[Edit::Place(Node::Strip, deep)], &[]);
            assert_eq!(zoom_in_transform(tree.strip.transform()), deep);
            assert!(
                !rini_keys(&tree.strip).is_empty(),
                "a place leaves the zoom playing"
            );
            plays_exactly(&tree);
            tree.play(&[home(0.0)], &[]);
            assert!(rini_keys(&tree.strip).is_empty(), "no length");
            tree.play(&[home(0.35), Edit::Cut(Node::Strip, Channel::Nudge)], &[]);
            assert!(
                !rini_keys(&tree.strip).is_empty(),
                "a cut of a channel the zoom is not on"
            );
            plays_exactly(&tree);
            tree.play(&[Edit::Cut(Node::Strip, Channel::Base)], &[]);
            assert!(rini_keys(&tree.strip).is_empty(), "cut");
            plays_exactly(&tree);
            tree.play(&[home(0.35), Edit::Drop(Node::Strip)], &[]);
            assert!(rini_keys(&tree.strip).is_empty(), "dropped");
            assert_eq!(
                zoom_in_transform(tree.strip.transform()),
                Value::Zoom(Zoom::REST)
            );
            plays_exactly(&tree);
        }

        /// A tile the score lifts casts the lift's shadow as the score has it: placed and played, a
        /// track of no length and a cut taking the lift's animation off and leaving its caster, a
        /// ride carrying the caster to the tile's new container, a pose without the lift and a drop
        /// of it taking the caster away, a pose with it making one again, and a drop of the tile
        /// taking the tile's lift with it.
        #[test]
        fn a_lifted_tile_casts_the_lifts_shadow_as_its_score_has_it() {
            let _core_animation = hold_core_animation();
            let window = wid(1);
            let frame: GroupRect = typed(4.0, 32.0, 859.0, 1081.0);
            let tiles = [overlay_tile(
                window,
                typed(4.0, 32.0, 859.0, 1081.0),
                typed(4.0, 32.0, 859.0, 1081.0),
                false,
            )];
            let lifting = |begin: f64, seconds: f64| {
                let bell = (0..=30)
                    .map(|k| Value::Number((std::f64::consts::PI * k as f64 / 30.0).sin().powi(2)));
                Edit::Add(Track::samples(
                    Node::Lift(window),
                    Channel::Base,
                    bell,
                    begin,
                    seconds,
                ))
            };
            let other = GroupKey::Rigid(1);
            let opened = [
                Edit::Place(
                    Node::Container(GroupKey::STILL),
                    Value::Point(OverlayPoint::origin()),
                ),
                Edit::Place(
                    Node::Container(other),
                    Value::Point(OverlayPoint::new(10.0, 0.0)),
                ),
                Edit::Ride(window, GroupKey::STILL),
                Edit::Place(Node::Tile(window), Value::Rect(frame)),
            ];
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            let mut score = Score::default();
            let mut play = |tree: &mut Tree, edits: &[Edit]| {
                for edit in edits {
                    score.apply(edit);
                }
                tree.play(edits, &tiles);
                assert_eq!(tree.replica, score);
                plays_exactly(tree);
                score.clone()
            };
            play(&mut tree, &opened);
            assert!(tree.tile_layers[&window].lift.is_none(), "nothing lifted");
            play(&mut tree, &[
                Edit::Place(Node::Lift(window), Value::Number(0.0)),
                lifting(5.0, 0.257),
            ]);
            let caster = |tree: &Tree| {
                tree.tile_layers[&window].lift.as_ref().map(|l| Retained::from(l.layer()))
            };
            let lifted = caster(&tree).expect("a caster");
            play(&mut tree, &[lifting(5.0, 0.0)]);
            assert!(rini_keys(&lifted).is_empty(), "no length");
            play(&mut tree, &[
                lifting(5.0, 0.257),
                Edit::Cut(Node::Lift(window), Channel::Base),
            ]);
            assert!(rini_keys(&lifted).is_empty(), "cut");
            let held = play(&mut tree, &[
                Edit::Place(Node::Lift(window), Value::Number(0.5)),
                lifting(5.0, 0.257),
                Edit::Ride(window, other),
            ]);
            assert!(
                caster(&tree).is_some_and(|c| std::ptr::eq(&*c, &*lifted)),
                "the same caster"
            );
            let mut set_down = held.clone();
            set_down.apply(&Edit::Drop(Node::Lift(window)));
            tree.pose(&set_down, &tiles);
            assert!(caster(&tree).is_none(), "posed without the lift");
            tree.pose(&held, &tiles);
            plays_exactly(&tree);
            assert!(caster(&tree).is_some(), "posed with it");
            play(&mut tree, &[Edit::Drop(Node::Lift(window))]);
            assert!(caster(&tree).is_none(), "dropped");
            play(&mut tree, &[
                Edit::Place(Node::Lift(window), Value::Number(0.0)),
                lifting(5.0, 0.257),
            ]);
            let lifted = caster(&tree).expect("a caster");
            play(&mut tree, &[Edit::Drop(Node::Tile(window))]);
            assert!(lifted.superlayer().is_none(), "gone with its tile");
        }

        /// For every look, a snap moves where a tile's own leg ends and keeps how the tile is drawn,
        /// even where the snapped leg judged again would draw it otherwise, so it never turns
        /// mid-flight. A resize of under a percent whose picture fits both ends stretches; snapped
        /// 4pt past the plan, its picture misfits where it ends and it stays stretched. A new leg is
        /// judged again: a shrink then crops the picture it has.
        #[test]
        fn a_snap_keeps_how_a_tile_is_drawn_and_a_new_leg_judges_it_again() {
            for look in Look::ALL {
                snaps_keeping_how_a_tile_is_drawn(look);
            }
        }

        fn snaps_keeping_how_a_tile_is_drawn(look: Look) {
            use crate::animation::domain::motion::tile;
            let window = wid(1);
            let (from, to): (ScreenRect, ScreenRect) = (
                typed(4.0, 32.0, 1000.0, 1081.0),
                typed(4.0, 32.0, 1008.0, 1081.0),
            );
            let mut tiles = vec![overlay_tile(window, on(from), on(to), false)];
            tiles[0].snapshot = test_picture(ScreenSize::new(1004.0, 1081.0));
            let plan = FlightPlan::from(reflow_plan(&[Resolved::tiled(window, from, to)], DISPLAY));
            let mut choreography = look.choreography();
            let stage = Stage {
                plan: &plan,
                drawn: &[window],
                seconds: 0.35,
                cue: Cue::Plain,
                accents: &[],
                viewport: VIEWPORT,
            };
            let mut score = choreography.pose(&stage);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            let started = choreography.compose(&score, &stage, 100.0);
            let mut play = |tree: &mut Tree, edits: &[Edit], tiles: &[OverlayTile]| {
                for edit in edits {
                    score.apply(edit);
                }
                tree.play(edits, tiles);
            };
            play(&mut tree, &started, &tiles);
            let stretched = |tree: &Tree| tree.tile_layers[&window].crop_of.is_none();
            assert!(stretched(&tree), "{look:?}: the picture fits both ends");

            let real = on(typed(4.0, 32.0, 1012.0, 1081.0));
            let snapped = tile::snap(&tree.replica, window, real, 100.15);
            assert!(
                snapped.iter().any(|edit| matches!(edit, Edit::Add(_))),
                "{look:?}: on its leg"
            );
            play(&mut tree, &snapped, &tiles);
            assert!(stretched(&tree), "{look:?}: drawn as it was");

            let shrink = Track::between(
                Node::Tile(window),
                Channel::Base,
                Value::Rect(plan::installed(real)),
                Value::Rect(plan::installed(on(typed(4.0, 32.0, 900.0, 1081.0)))),
                100.2,
                0.35,
                Curve::Bezier(MOTION_CURVE),
            );
            play(&mut tree, &[Edit::Add(shrink)], &tiles);
            assert!(!stretched(&tree), "{look:?}: a new leg crops the picture it has");
        }

        /// Every new leg picks its crop's anchors again, a crop-drawn tile staying crop-drawn
        /// included, and lays its grid at the tile's model with them; a snap, the leg with only its
        /// end moved, keeps them, even where judging the snapped leg again would turn them.
        #[test]
        fn a_new_leg_anchors_its_crop_again_and_a_snap_keeps_its_anchors() {
            let window = wid(1);
            let (whole, bottom): (GroupRect, GroupRect) =
                (typed(4.0, 3.0, 859.0, 1074.0), typed(4.0, 542.0, 859.0, 535.0));
            let tiles = [overlay_tile(
                window,
                plan::on_overlay(whole),
                plan::on_overlay(bottom),
                false,
            )];
            let picture: TileSize = whole.size.respace();
            let leg = |from: GroupRect, to: GroupRect, begin: f64| {
                Track::between(
                    Node::Tile(window),
                    Channel::Base,
                    Value::Rect(from),
                    Value::Rect(to),
                    begin,
                    0.35,
                    Curve::Bezier(MOTION_CURVE),
                )
            };
            let laid = |tree: &Tree, at: GroupRect| {
                let tile = &tree.tile_layers[&window];
                assert_eq!(tile.crop_of, Some(picture), "crop-drawn");
                let grid = tile.crop_grid.as_ref().expect("a grid");
                let pieces = crop(picture, at.size.respace(), tile.anchors).pieces;
                for (layer, piece) in grid.pieces.iter().zip(pieces) {
                    assert_eq!(layer.frame(), piece.frame.to_cg());
                    assert_eq!(layer.contentsRect(), piece.contents.to_cg());
                }
                tile.anchors
            };
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(bottom)),
                    Edit::Add(leg(whole, bottom, 100.0)),
                ],
                &tiles,
            );
            let bottom_up = Anchors {
                x: Anchor::Start,
                y: Anchor::End,
            };
            assert_eq!(laid(&tree, bottom), bottom_up, "a bottom row shrinking down");

            let shorter: GroupRect = typed(4.0, 552.0, 859.0, 500.0);
            let shrinking = leg(bottom, shorter, 100.1);
            tree.play(
                &[
                    Edit::Place(Node::Tile(window), Value::Rect(shorter)),
                    Edit::Add(shrinking.clone()),
                ],
                &tiles,
            );
            assert_eq!(
                laid(&tree, shorter),
                Anchors::default(),
                "its bottom moving more"
            );

            let snapped: GroupRect = typed(4.0, 552.0, 859.0, 520.0);
            assert_eq!(
                Anchors::of_leg(bottom, snapped),
                bottom_up,
                "judged again, it would turn"
            );
            tree.play(
                &[
                    Edit::Place(Node::Tile(window), Value::Rect(snapped)),
                    Edit::Add(shrinking.ending_at(Value::Rect(snapped))),
                ],
                &tiles,
            );
            assert_eq!(laid(&tree, snapped), Anchors::default(), "a snap keeps them");
        }

        /// A tile whose leg is in two steps, carrying a repaint from its corner, cuts its crop grid
        /// to it at the corner as its leg is installed, and again as a snap moves its end; once it
        /// carries none, a new leg flies the picture it has and cuts nothing.
        #[test]
        fn a_tile_carrying_a_picture_from_its_corner_cuts_to_it_there_and_a_snap_keeps_the_cut() {
            let _core_animation = hold_core_animation();
            use crate::animation::domain::motion::tile::leg;
            let window = wid(1);
            let (from, to): (GroupRect, GroupRect) = (
                typed(866.0, 35.0, 860.0, 1079.0),
                typed(3.0, 576.0, 1148.0, 538.0),
            );
            let mut tiles = [OverlayTile {
                from_corner: Some(test_picture(to.size)),
                ..overlay_tile(window, plan::on_overlay(from), plan::on_overlay(to), false)
            }];
            let cuts = |tree: &Tree| {
                let grid = tree.tile_layers[&window].crop_grid.as_ref().expect("a grid");
                grid.pieces
                    .iter()
                    .all(|layer| animation_keys(layer).iter().any(|key| key == PIECE_PICTURE_KEY))
            };
            let folding = leg(window, from, to, 100.0, 0.35, MOTION_CURVE);
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(to)),
                    Edit::Add(folding.clone()),
                ],
                &tiles,
            );
            assert_eq!(
                tree.tile_layers[&window].crop_of,
                Some(to.size.respace()),
                "the repaint as it ends"
            );
            assert!(cuts(&tree), "cut at the corner");

            let snapped: GroupRect = typed(4.0, 576.0, 1147.0, 538.0);
            let snap = folding.ending_at(Value::Rect(snapped));
            tree.play(
                &[
                    Edit::Place(Node::Tile(window), Value::Rect(snapped)),
                    Edit::Add(snap),
                ],
                &tiles,
            );
            assert!(cuts(&tree), "a snap keeps the cut");

            tiles[0].snapshot = tiles[0].from_corner.take().expect("carried");
            let taller: GroupRect = typed(4.0, 476.0, 1147.0, 638.0);
            let growing = Track::between(
                Node::Tile(window),
                Channel::Base,
                Value::Rect(snapped),
                Value::Rect(taller),
                100.3,
                0.35,
                Curve::Bezier(MOTION_CURVE),
            );
            tree.play(
                &[
                    Edit::Place(Node::Tile(window), Value::Rect(taller)),
                    Edit::Add(growing),
                ],
                &tiles,
            );
            let grid = tree.tile_layers[&window].crop_grid.as_ref().expect("a grid");
            let none = |layer: &Retained<CALayer>| {
                !animation_keys(layer).iter().any(|key| key == PIECE_PICTURE_KEY)
            };
            assert!(grid.pieces.iter().all(none), "nothing to cut to");
            assert_eq!(
                tree.tile_layers[&window].crop_of,
                Some(to.size.respace()),
                "the repaint it has"
            );
        }

        /// A tile riding into a flight is dressed as it rides: clipped, its picture is cut at the
        /// corner its hairline rounds to at every placement, clamped to the tile, and a picture swapped
        /// onto it whole is drawn whole.
        #[test]
        fn a_tile_ridden_in_clipped_wears_its_corners() {
            let window = wid(1);
            let drawn = typed(4.0, 32.0, 859.0, 1081.0);
            let at: GroupRect = typed(4.0, 32.0, 859.0, 1081.0);
            let mut tile = overlay_tile(window, drawn, drawn, false);
            tile.radius = Some(16.0);
            tile.clipped = true;
            let tiles = [tile];
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(at)),
                ],
                &tiles,
            );
            let picture = &tree.tile_layers[&window].picture;
            assert!(picture.masksToBounds());
            assert_eq!(picture.cornerRadius(), 16.0);
            assert_eq!(
                picture.cornerRadius(),
                tree.tile_layers[&window].decorations.hairline.cornerRadius()
            );
            let hairline = tree.tile_layers[&window].decorations.hairline.clone();
            // SAFETY: an immutable `NSString` constant exported by Core Animation.
            let continuous = unsafe { objc2_quartz_core::kCACornerCurveContinuous };
            assert_eq!(&*picture.cornerCurve(), continuous);
            assert_eq!(picture.cornerCurve(), hairline.cornerCurve());
            tree.play(
                &[Edit::Place(
                    Node::Tile(window),
                    Value::Rect(typed(4.0, 32.0, 12.0, 400.0)),
                )],
                &tiles,
            );
            assert_eq!(
                tree.tile_layers[&window].picture.cornerRadius(),
                6.0,
                "half the shorter side"
            );
            assert_eq!(
                &*tree.tile_layers[&window].picture.cornerCurve(),
                continuous,
                "a continuous 6pt corner reaching past half of 12pt keeps its curve"
            );
            tree.swap(window, &test_picture(ScreenSize::new(12.0, 400.0)), false, 0.0);
            assert!(!tree.tile_layers[&window].picture.masksToBounds(), "whole");
        }

        /// A tile wears the ring its window's decoration has, around the window at its frame, as
        /// dressed and as a merge wears it again; a turn of focus brightens its hairline and leaves
        /// its ring as it is.
        #[test]
        fn a_tile_wears_its_ring_as_dressed_and_as_worn_and_a_turn_leaves_it() {
            use crate::decorations::domain::paint::{Colour, Paint};
            use crate::decorations::domain::style::{Band, Decoration, Ring};
            let window = wid(1);
            let drawn = typed(4.0, 32.0, 859.0, 1081.0);
            let at: GroupRect = typed(4.0, 32.0, 859.0, 1081.0);
            let (active, inactive) = (Colour::rgb(0x7fc8ff), Colour::rgb(0x505050));
            let band = Band {
                width: 4.0,
                active: Paint::Solid(active),
                inactive: Paint::Solid(inactive),
                urgent: Paint::Solid(inactive),
            };
            let mut tile = overlay_tile(window, drawn, drawn, false);
            let worn = |wears_ring, holds_focus| Worn {
                decoration: Decoration {
                    ring: Some(band),
                    ..Decoration::default()
                },
                wears_ring,
                holds_focus,
            };
            tile.worn = worn(Ring::Inactive, false);
            tile.radius = Some(16.0);
            let tiles = [tile];
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(at)),
                ],
                &tiles,
            );
            let bands = &tree.tile_layers[&window].decorations.bands;
            assert!(
                bands.border.is_none(),
                "no layer for the border it does not wear"
            );
            let ring = bands.ring.as_ref().expect("ringed").layer.clone();
            let colour = |c: Colour| {
                objc2_core_graphics::CGColor::new_srgb(
                    c.r.into(),
                    c.g.into(),
                    c.b.into(),
                    c.a.into(),
                )
            };
            let painted = |c: Colour| {
                objc2_core_graphics::CGColor::equal_to_color(
                    ring.borderColor().as_deref(),
                    Some(&colour(c)),
                )
            };
            assert!(!ring.isHidden() && painted(inactive), "worn unfocused");
            assert_eq!(
                ring.frame(),
                rect(-4.0, -4.0, 867.0, 1089.0),
                "outside the window"
            );
            assert_eq!(ring.cornerRadius(), 20.0);
            let hairline = tree.tile_layers[&window].decorations.hairline.clone();
            let lit = |alpha| {
                let white = objc2_core_graphics::CGColor::new_generic_gray(1.0, alpha);
                objc2_core_graphics::CGColor::equal_to_color(
                    hairline.borderColor().as_deref(),
                    Some(&white),
                )
            };
            tree.turn(window, true);
            assert!(
                lit(0.20) && !ring.isHidden() && painted(inactive),
                "the hairline turns, the ring stays"
            );
            tree.wear(window, worn(Ring::Active, true));
            assert!(lit(0.20) && painted(active));
            tree.wear(window, worn(Ring::None, false));
            assert!(lit(0.20) && ring.isHidden());
        }

        /// With the ring and the border off, a tile wears the hairline and the shadow and no layer for
        /// either band.
        #[test]
        fn a_tile_whose_window_wears_no_band_carries_no_band_layer() {
            let window = wid(1);
            let drawn = typed(4.0, 32.0, 859.0, 1081.0);
            let at: GroupRect = typed(4.0, 32.0, 859.0, 1081.0);
            let tiles = [overlay_tile(window, drawn, drawn, false)];
            assert_eq!(
                tiles[0].worn.decoration,
                crate::decorations::domain::style::Decoration::default()
            );
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.play(
                &[
                    Edit::Place(
                        Node::Container(GroupKey::STILL),
                        Value::Point(OverlayPoint::origin()),
                    ),
                    Edit::Ride(window, GroupKey::STILL),
                    Edit::Place(Node::Tile(window), Value::Rect(at)),
                ],
                &tiles,
            );
            let decorations = &tree.tile_layers[&window].decorations;
            assert!(decorations.bands.ring.is_none() && decorations.bands.border.is_none());
            // SAFETY: a read of the hairline's own sublayers, which nothing else mutates.
            let children =
                unsafe { decorations.hairline.sublayers() }.map_or(0, |layers| layers.count());
            assert_eq!(children, 1, "the outline alone");
            tree.turn(window, true);
            tree.wear(window, Worn {
                wears_ring: crate::decorations::domain::style::Ring::Active,
                holds_focus: true,
                ..Worn::default()
            });
            let bands = &tree.tile_layers[&window].decorations.bands;
            assert!(
                bands.ring.is_none() && bands.border.is_none(),
                "nor as focus turns or a merge wears it"
            );
        }

        /// A placed tile moves every layer of the tile at once: the picture, the caster and the hairline,
        /// the shadow's mask and silhouette, and the hairline, and plays nothing.
        #[test]
        fn a_placed_tile_moves_every_layer_of_the_tile() {
            let _core_animation = hold_core_animation();
            let window = wid(1);
            let at: GroupRect = typed(865.0, 32.0, 861.0, 1081.0);
            let drawn = typed(865.0, 32.0, 861.0, 1081.0);
            let tiles = [overlay_tile(window, drawn, drawn, false)];
            let mut score = Score::default();
            for edit in [
                Edit::Place(
                    Node::Container(GroupKey::STILL),
                    Value::Point(OverlayPoint::new(-861.0, 0.0)),
                ),
                Edit::Ride(window, GroupKey::STILL),
                Edit::Place(Node::Tile(window), Value::Rect(at)),
            ] {
                score.apply(&edit);
            }
            let mut tree = Tree::new(CALayer::layer(), DISPLAY.size.to_cg(), 2.0);
            tree.pose(&score, &tiles);
            let snapped: GroupRect = typed(865.0, 32.0, 860.0, 1081.0);
            tree.play(&[Edit::Place(Node::Tile(window), Value::Rect(snapped))], &tiles);
            let tile = &tree.tile_layers[&window];
            assert_eq!(tile.picture.frame(), snapped.to_cg());
            let decorations = &tile.decorations;
            assert_eq!(decorations.shadow.frame(), snapped.to_cg());
            assert_eq!(decorations.hairline.frame(), snapped.to_cg());
            assert_eq!(outline_bounds(tile), rect(-0.25, -0.25, 860.5, 1081.5));
            assert_eq!(
                decorations.shadow_mask.frame(),
                shadow_mask_frame(snapped.size.respace()).to_cg()
            );
            let silhouette = decorations.shadow.shadowPath().expect("a silhouette");
            assert_eq!(
                objc2_core_graphics::CGPath::bounding_box(Some(&silhouette)).size,
                snapped.size.to_cg()
            );
            for layer in tile.layers() {
                assert!(rini_keys(layer).is_empty());
            }
        }
    }
}
