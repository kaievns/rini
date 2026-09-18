//! The animation overlay: one rini-owned window holding a picture of every animating window.
//!
//! Nothing real moves during an animation. Real windows are placed at their final frames once,
//! underneath an opaque overlay. Tiles ride containers, one per rigid piece of the flight; a
//! container's position is the only translation its members get, so a strip cannot tear.
//! See "The overlay engine" in `docs/animation-smoothness.md`; capture measurements in
//! `docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPopUpMenuWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::{NSArray, NSNumber, NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CATransaction, kCAMediaTimingFunctionEaseInEaseOut,
};

use crate::actor::app::WindowId;
use crate::actor::workspace_animation::plan::{
    Banding, FlightPlan, GroupKey, Member, PlanDelta, group_relative,
};
use crate::model::z_group::{StackGroup, container_z};
use crate::sys::geometry::{Round, SameAs};
use crate::sys::screen::CoordinateConverter;
use crate::ui::edge_dressing::{boundary_layout, tile_corner_radius};
use crate::ui::window_snapshot::{SnapshotImage, WindowSnapshot};


/// Window level for the overlay. Managed windows all sit at CG layer 0, so anything above that
/// covers them.
///
/// Above every window the overlay animates, below the system chrome that must stay interactive
/// and visible while an animation runs. Notification banners sit at level 21 on this system
/// (measured from the window list; the Dock is 20, the menu bar 24, utility panels 19), and at the
/// old `NSPopUpMenuWindowLevel` (101) the overlay blotted them out for every animation.
const OVERLAY_LEVEL: isize = 18;

define_class!(
    /// An `NSView` with a top-left origin, so the layer tree agrees with the CoreGraphics coordinates
    /// used everywhere else. `setGeometryFlipped` on a view-backed layer is silently ineffective.
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

/// One window's picture inside the overlay, and where it should be drawn.
#[derive(Clone)]
pub struct OverlayTile {
    pub window: WindowId,
    /// Where the window starts, in the overlay's coordinate space.
    pub from: CGRect,
    /// Where the window ends up, in the overlay's coordinate space.
    pub to: CGRect,
    pub snapshot: WindowSnapshot,
    /// Off the strip, so in the other z-order band from the strip windows. See `model/z_group.rs`.
    pub floating: bool,
    /// The window server's front-to-back position, 0 frontmost; `None` when unreported.
    pub server_order: Option<usize>,
    /// Front-to-back position in the overlay, 0 being frontmost. Derived from `server_order`,
    /// `floating`, and the flight's focus by the engine's restack, once per flight rather than per
    /// pass. Without it a tile can be drawn behind a window it is really in front of, and the
    /// handover pops.
    pub depth: usize,
    /// A border window riding the window it traces: drawn a quarter-step in front of its window's
    /// depth, and without a shadow, because the real border window casts none.
    pub companion: bool,
    /// Whether this window holds (or is about to hold) focus, which deepens its shadow.
    pub focused: bool,
}

impl OverlayTile {
    /// The zPosition this tile draws at. A companion sits just in front of the window it traces —
    /// a quarter of a depth step, clear of the half-step the shadow casters sit behind.
    pub(crate) fn z(&self) -> f64 {
        -(self.depth as f64) + if self.companion { 0.25 } else { 0.0 }
    }
}

/// How a tile's picture is fitted to the frame it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentMode {
    /// Scaled to fill the frame. Right for a movement whose picture matches the frame's shape.
    Stretch,
    /// Cropped or revealed: the picture draws at native 1:1 scale, and the size change is absorbed
    /// by a seam a band in from the trailing edges — content never stretches, the moving edge
    /// swallows or reveals it, which is how a real resize reads. The trailing band, carrying the
    /// window's rounded corners and border, rides the moving edge intact. See "Resizes through the
    /// overlay" in `docs/animation-smoothness.md`.
    Crop,
}

/// Crop for a resize, and for any picture that no longer matches the frame it starts in; stretched
/// only when picture and frames agree, where stretching is exact.
pub fn content_mode(covered: (f64, f64), from: CGSize, to: CGSize) -> ContentMode {
    if crate::ui::window_snapshot::is_a_resize(from, to)
        || !crate::ui::window_snapshot::fits_frame(covered, (from.width, from.height))
    {
        placeholder_mode(covered, to)
    } else {
        ContentMode::Stretch
    }
}

/// How a tile fits a picture that may not cover its destination: the ordinary crop when it can,
/// stretched when it cannot. The stretch is the placeholder of a grow whose reveal has not landed;
/// every mode fills the whole frame, so a placeholder never shows the backdrop. A top-left crop
/// that left the growth undrawn was tried and read as a hole (see "A grow holds, then reveals"
/// in `docs/animation-smoothness.md`).
pub fn placeholder_mode(covered: (f64, f64), to: CGSize) -> ContentMode {
    if crate::ui::window_snapshot::outgrows(covered, to) {
        ContentMode::Stretch
    } else {
        ContentMode::Crop
    }
}

/// What a fresh hairline does to the layers a tile already wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DressingAction {
    /// Same piece set: new pixels into the existing layers, riding any installed animation.
    SwapInPlace,
    /// Different piece set: drop the ring and build it at the model size.
    Rebuild,
    /// Different piece set while a resize is animating: leave the worn ring alone.
    Defer,
}

/// Whether a harvest may rebuild a tile's ring now. See "A fresh picture or hairline swaps in
/// place" in `docs/animation-smoothness.md`.
pub fn dressing_rebuild_allowed(resize_in_flight: bool, worn_matches: bool) -> DressingAction {
    if worn_matches {
        DressingAction::SwapInPlace
    } else if resize_in_flight {
        DressingAction::Defer
    } else {
        DressingAction::Rebuild
    }
}

/// Whether a tile's resize animation, ending at `resize_until`, is still riding at `now`.
pub fn resize_in_flight(resize_until: Option<Instant>, now: Instant) -> bool {
    resize_until.is_some_and(|until| now < until)
}

/// The trailing band preserved intact when a tile draws cropped, in points.
///
/// Comfortably past the ~10pt corner radius so the crop seam never cuts a corner square, and small
/// against any real column so almost all of the window is drawn 1:1.
const EDGE_BAND: f64 = 40.0;

/// The band that fits the frame being drawn into: shrinks with the frame, so a window growing in
/// from nothing starts with no band at all and gains it continuously — no seam pops mid-flight.
fn crop_band(frame: CGSize) -> f64 {
    EDGE_BAND.min(0.45 * frame.width.min(frame.height)).max(0.0)
}

/// One piece of a crop-drawn tile: where it sits in the tile, and which part of the picture it
/// shows, in the picture's unit coordinates.
///
/// Every mapping is 1:1 — a piece's frame is exactly as large as the picture region it shows — so
/// nothing ever stretches. A region reaching past the picture's edge is deliberate: Core Animation
/// extends the edge pixels outward, which paints a grow with window-coloured pixels instead of a
/// hole.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CropPiece {
    pub frame: CGRect,
    pub contents: CGRect,
}

/// The 2x2 crop grid for a picture drawn into `frame`, split one `crop_band` in from the RIGHT
/// edge and one down from the TOP.
///
/// The anchoring follows how each axis of a real resize reads:
/// - Horizontally, content anchors LEFT and the right edge swallows or reveals it — the right
///   band, carrying the window's right border and corners, rides the moving edge intact.
/// - Vertically, the TITLE BAR stays put and content anchors to the BOTTOM — the prompt of a
///   terminal rides the bottom edge — so the seam sits just below the title-bar band, and the
///   bottom-anchored body slides into or out of it.
///
/// The window's top corners live in the top band at 1:1, its bottom corners in the bottom-anchored
/// body, so all four rounded corners survive any frame the animation passes through.
///
/// Piece frames and contents are LINEAR in `frame` while the band is constant, which is what lets
/// a resize ride plain Core Animation interpolation between the endpoint grids. Every piece maps
/// 1:1 while the frame fits inside the picture. Growing PAST the picture, the seam region
/// stretches the picture's own content instead of reaching beyond its edge: `contentsRect` past
/// the edge extends the outermost pixels, near-transparent on a translucent window, so a grow
/// painted a hole to the backdrop. A picture that cannot cover its destination is not drawn
/// cropped at all (`placeholder_mode`), so this only covers the frames a fitting picture passes
/// through.
pub(crate) fn crop_pieces(picture: CGSize, frame: CGSize) -> [CropPiece; 4] {
    let pw = picture.width.max(1.0);
    let ph = picture.height.max(1.0);
    let band = crop_band(frame).min(pw).min(ph);
    let (bodyw, bodyh) = ((frame.width - band).max(0.0), (frame.height - band).max(0.0));
    // What the anchored regions may show of the picture: everything up to the seam, which
    // belongs to the bands. Capping is what keeps the seam continuous — the body must never
    // duplicate a band's content.
    let leadw = bodyw.min(pw - band);
    let tailh = bodyh.min(ph - band);
    let piece = |x: f64, y: f64, w: f64, h: f64, cx: f64, cy: f64, cw: f64, ch: f64| CropPiece {
        frame: CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)),
        contents: CGRect::new(CGPoint::new(cx / pw, cy / ph), CGSize::new(cw / pw, ch / ph)),
    };
    [
        // Title-bar band: the picture's top edge, pinned to the frame's top.
        piece(0.0, 0.0, bodyw, band, 0.0, 0.0, leadw, band),
        // Top-right corner: pinned top and right.
        piece(bodyw, 0.0, band, band, pw - band, 0.0, band, band),
        // Body: left-anchored horizontally, BOTTOM-anchored vertically — the picture's bottom
        // rows ride the moving bottom edge, and the seam below the title bar absorbs the change.
        piece(0.0, band, bodyw, bodyh, 0.0, ph - tailh, leadw, tailh),
        // Right band: the picture's right edge, riding the moving right edge, bottom-anchored to
        // stay row-continuous with the body.
        piece(bodyw, band, band, bodyh, pw - band, ph - tailh, band, tailh),
    ]
}

/// Interpolates a rect. Separated out and tested because getting this wrong produces an animation
/// that looks almost right, which is much harder to debug than one that is obviously broken.
pub fn lerp_rect(from: CGRect, to: CGRect, t: f64) -> CGRect {
    let l = |a: f64, b: f64| a + (b - a) * t;
    CGRect::new(
        CGPoint::new(l(from.origin.x, to.origin.x), l(from.origin.y, to.origin.y)),
        CGSize::new(l(from.size.width, to.size.width), l(from.size.height, to.size.height)),
    )
}

/// The shadow a real window casts, as three Core Animation numbers.
///
/// Every capture API returns the window without its shadow, so a tile has none and the handover to the real
/// window pops. Two styles because macOS draws two: the key window's shadow is measurably heavier —
/// 1.65x darker at the edge, reaching half again as far, and biased further downward. Both fitted to
/// alpha falloffs read out of shadow-inclusive framed captures of this display; the numbers are in
/// "Shadows are never in the surface" in `docs/capture-overlay-research.md`.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ShadowStyle {
    opacity: f32,
    radius: f64,
    /// Positive is downward: the tiles hang off a flipped view, so the layer's y axis points down.
    offset_y: f64,
}

const UNFOCUSED_SHADOW: ShadowStyle = ShadowStyle { opacity: 0.4, radius: 9.0, offset_y: 5.0 };
const FOCUSED_SHADOW: ShadowStyle = ShadowStyle { opacity: 0.65, radius: 14.0, offset_y: 12.0 };

fn tile_shadow_style(focused: bool) -> ShadowStyle {
    if focused { FOCUSED_SHADOW } else { UNFOCUSED_SHADOW }
}

/// How far the shadow can reach from a window's edge, which is what the mask has to leave room for.
/// The focused ramp is spent by about 54pt (three radii plus the offset); 70pt is generous enough
/// that no blur is clipped at the edge of the ring, for either style.
const SHADOW_REACH: f64 = 70.0;

/// Where the bar sits: above everything the overlay draws. Tiles sit at `-depth`, which never
/// exceeds zero, so any positive value clears them.
const BAR_Z: f64 = 10_000.0;

/// Where the desktop sits: below everything — including the DEEPEST possible tile.
///
/// Derived from the depth model rather than guessed: `tile_depth` reaches almost two group
/// strides for the back group's unreported windows, and a backdrop above that swallowed every
/// floating tile — the Settings window behind the strip was drawn in every animation and visible
/// in none, because its picture sat behind the wallpaper.
const BACKDROP_Z: f64 = -((crate::model::z_group::MAX_TILE_DEPTH + 1024) as f64);

/// Ease-out cubic. Fast at the start and settling at the end, which reads as the strip being flicked
/// rather than dragged, and matches what niri does.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// A frame on whole points. The window server places real windows on whole points (a layout
/// offset of 4592.67 lands a window at x=4593), while a layer at a fractional position is
/// resampled and sits that fraction off. Drawn there, every tile popped by the fraction and
/// sharpened at the lift, on every flight. See "Real windows land before lift" in
/// `docs/animation-smoothness.md`.
fn whole(rect: CGRect) -> CGRect {
    rect.round()
}

fn whole_point(point: CGPoint) -> CGPoint {
    point.round()
}

/// Commits the explicit transaction AND flushes the run loop's implicit one, so the change reaches
/// the render server now. The overlay shares the main thread with the reactor; an explicit commit
/// nested in the run loop's implicit transaction is only sent when that iteration ends, after
/// whatever synchronous AX and window-server calls the reactor makes next. A flight whose start
/// was sent late skipped its first frames. See "Mid-flight passes" in `docs/animation-smoothness.md`.
fn commit_now() {
    CATransaction::commit();
    CATransaction::flush();
}

/// One key for every tile movement, so a retarget replaces the animation in flight rather than
/// stacking a second one on the same property.
const TILE_ANIMATION_KEY: &str = "rini.tile.move";

/// One key per container movement, for the same reason.
const GROUP_ANIMATION_KEY: &str = "rini.group.move";

/// `ease_out_cubic` in Core Animation form — exactly, not approximately. Derivation in
/// `docs/animation-smoothness.md`; the identity is pinned by test.
fn ease_out_cubic_timing() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(1.0 / 3.0, 1.0, 2.0 / 3.0, 1.0)
}

/// When an animation runs: an explicit begin on the media clock plus its length. Every animation
/// of one leg shares one `Timing`, and a leg re-installed with the same `Timing` (a picture swap
/// on a resizing tile) continues on the same curve instead of restarting. See "Mid-flight passes"
/// in `docs/animation-smoothness.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Timing {
    begin: f64,
    seconds: f64,
}

impl Timing {
    fn starting_now(duration: Duration) -> Self {
        Timing { begin: objc2_quartz_core::CACurrentMediaTime(), seconds: duration.as_secs_f64() }
    }

    fn apply(&self, animation: &CABasicAnimation) {
        animation.setTimingFunction(Some(&ease_out_cubic_timing()));
        animation.setDuration(self.seconds);
        animation.setBeginTime(self.begin);
    }

    /// When the leg ends, on the wall clock.
    fn ends_at(&self) -> Instant {
        let left = self.begin + self.seconds - objc2_quartz_core::CACurrentMediaTime();
        Instant::now() + Duration::from_secs_f64(left.max(0.0))
    }
}

/// An explicit position animation from `from` to `to`, in layer coordinates. The caller sets the
/// model to the destination; this carries the presentation there and is removed on completion,
/// revealing the model value — no snap-back, no completion delegate.
fn position_animation(from: CGPoint, to: CGPoint, timing: Timing) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: an NSValue holding a CGPoint is the value type Core Animation expects for the
    // "position" key path.
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithPoint(from)));
        animation.setToValue(Some(&NSValue::valueWithPoint(to)));
    }
    timing.apply(&animation);
    animation
}

/// One key per container bounce, separate from the movement's so the two compose.
const BOUNCE_ANIMATION_KEY: &str = "rini.group.bounce";

/// Where the return leg of a bounce begins, as a fraction of its duration. Out fast, back at
/// leisure: a rubber band snaps taut and eases home.
pub const BOUNCE_TURN: f64 = 0.35;

/// Whether the container `key` names takes part in a bounce by `overshoot`. Strip containers
/// always; the floating container only when the bounce is vertical, the same rule a pan (floating
/// pinned) and a switch (floating carried) follow.
pub fn bounce_carries(key: GroupKey, overshoot: CGPoint) -> bool {
    match key {
        GroupKey::Floating => overshoot.y != 0.0,
        GroupKey::Strip(_) | GroupKey::StripLoose => true,
    }
}

/// The displacement of a bounce at progress `t`, for a unit overshoot: out along an ease-out to
/// the turn, back along an ease-in-out to rest. What `bounce_animation` asks Core Animation to
/// draw, kept here so the shape can be checked on plain numbers.
pub fn bounce_displacement(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t <= BOUNCE_TURN {
        ease_out_cubic(t / BOUNCE_TURN)
    } else {
        let u = (t - BOUNCE_TURN) / (1.0 - BOUNCE_TURN);
        // ease-in-out cubic, from 1 down to 0
        let s = if u < 0.5 { 4.0 * u * u * u } else { 1.0 - (-2.0 * u + 2.0).powi(3) / 2.0 };
        1.0 - s
    }
}

/// An additive position animation that goes `overshoot` from wherever the layer is and comes
/// back: keyframes `0, overshoot, 0` at `0, BOUNCE_TURN, 1`, ease-out then ease-in-out. Additive,
/// so it rides a movement in flight and leaves the model position alone; removed on completion,
/// so the layer is presented at its model position again.
fn bounce_animation(overshoot: CGPoint, timing: Timing) -> Retained<CAKeyframeAnimation> {
    let animation =
        CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    let rest = CGPoint::new(0.0, 0.0);
    // SAFETY: NSValues holding CGPoints are the value type Core Animation expects for "position";
    // the untyped array `setValues` takes is built from exactly those.
    unsafe {
        let values: Vec<Retained<objc2::runtime::AnyObject>> = [rest, overshoot, rest]
            .into_iter()
            .map(|p| Retained::into_super(Retained::into_super(NSValue::valueWithPoint(p))))
            .collect();
        animation.setValues(Some(&NSArray::from_retained_slice(&values)));
    }
    let key_times: Vec<Retained<NSNumber>> =
        [0.0, BOUNCE_TURN, 1.0].into_iter().map(NSNumber::numberWithDouble).collect();
    animation.setKeyTimes(Some(&NSArray::from_retained_slice(&key_times)));
    animation.setTimingFunctions(Some(&NSArray::from_retained_slice(&[
        ease_out_cubic_timing(),
        CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionEaseInEaseOut }),
    ])));
    animation.setAdditive(true);
    animation.setDuration(timing.seconds);
    animation.setBeginTime(timing.begin);
    animation
}

/// An animation between two rect-valued endpoints of `key_path` ("bounds", "contentsRect").
fn rect_animation(
    key_path: &str,
    from: CGRect,
    to: CGRect,
    timing: Timing,
) -> Retained<CABasicAnimation> {
    let animation =
        CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: an NSValue holding a CGRect is the value type Core Animation expects for
    // rect-valued key paths.
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithRect(from)));
        animation.setToValue(Some(&NSValue::valueWithRect(to)));
    }
    timing.apply(&animation);
    animation
}

/// An animation between two path-valued endpoints ("shadowPath", "path"). Core Animation
/// interpolates paths with matching element structure, which every caller here guarantees by
/// building both endpoints with the same constructor.
fn path_animation(
    key_path: &str,
    from: &objc2_core_graphics::CGPath,
    to: &objc2_core_graphics::CGPath,
    timing: Timing,
) -> Retained<CABasicAnimation> {
    let animation =
        CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: a CGPath is the value type Core Animation expects for path-valued key paths; it is
    // toll-free-bridged, and the animation retains what it is given.
    unsafe {
        let from_raw: *const objc2_core_graphics::CGPath = from;
        let to_raw: *const objc2_core_graphics::CGPath = to;
        let _: () = msg_send![&*animation, setFromValue: from_raw];
        let _: () = msg_send![&*animation, setToValue: to_raw];
    }
    timing.apply(&animation);
    animation
}

/// Installs position-and-bounds animations carrying `layer` between two frames. Anchor points are
/// (0,0) throughout, so position is the frame origin.
fn animate_layer_frame(layer: &CALayer, from: CGRect, to: CGRect, timing: Timing, key_prefix: &str) {
    let position = position_animation(from.origin, to.origin, timing);
    layer.addAnimation_forKey(&position, Some(&NSString::from_str(&format!("{key_prefix}.move"))));
    if from.size != to.size {
        let bounds = rect_animation(
            "bounds",
            CGRect::new(CGPoint::new(0.0, 0.0), from.size),
            CGRect::new(CGPoint::new(0.0, 0.0), to.size),
            timing,
        );
        layer.addAnimation_forKey(&bounds, Some(&NSString::from_str(&format!("{key_prefix}.size"))));
    }
}

/// One window's two layers: the picture, and a caster behind it that carries nothing but the shadow.
///
/// Two layers because a Core Animation shadow is drawn behind the WHOLE layer, including under its own
/// area. Under an opaque window that is invisible, but a window with per-pixel alpha shows it through the
/// glass as a wash across the entire window, which is not what a real shadow does: the window server clips
/// a window's shadow to the outside of its shape. The caster is masked to a ring outside the tile, so the
/// shadow reaches the desktop and the neighbours and nothing else.
struct Tile {
    picture: Retained<CALayer>,
    shadow: Retained<CALayer>,
    /// The ring that keeps the shadow off the window itself. Persistent, so a resize can animate
    /// its path instead of swapping in a new mask, which cannot be animated.
    shadow_mask: Retained<objc2_quartz_core::CAShapeLayer>,
    /// The hairline sublayers riding the picture, replaced whenever the picture is re-dressed.
    /// Each carries its index into [`crate::ui::edge_dressing::DressingLayout`] order (strips
    /// 0-3, corners 4-7), so a resize can pair it with its endpoint geometries.
    dressing: Vec<(usize, Retained<CALayer>)>,
    /// The four crop pieces, created on first crop-drawn animation and pooled with the tile.
    crop_grid: Option<CropGrid>,
    /// The size in points of the picture the crop pieces are showing; `None` when stretching.
    crop_of: Option<CGSize>,
    /// When the installed resize animation ends; a mismatched hairline is deferred until then.
    resize_until: Option<Instant>,
    /// The resize leg the tile is riding, so a picture swap re-installs it on the same timing.
    resize_leg: Option<(CGRect, CGRect, Timing)>,
    /// The container the tile's layers hang under; `None` before the first install. Frames on the
    /// layers are in that parent's space.
    key: Option<GroupKey>,
    /// A border window's tile: drawn a quarter step in front of its window, no shadow.
    companion: bool,
}

/// The four layers a crop-drawn tile is composed of, children of the tile's picture layer.
struct CropGrid {
    pieces: [Retained<CALayer>; 4],
}

/// One movement `fly` installs: a container's translation, or one loose tile's own animation.
/// Pure output of [`animation_targets`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AnimationTarget {
    Container { key: GroupKey, from: CGPoint, to: CGPoint },
    Tile { window: WindowId, from: CGRect, to: CGRect },
}

/// What a flight animates, and nothing else: one `Container` per strip group with travel, the
/// floating container when it travels, and a `Tile` per changing, entrance and moving floating
/// member. Rigid members are never named. See "The overlay engine" in `docs/animation-smoothness.md`.
pub(crate) fn animation_targets(plan: &FlightPlan) -> Vec<AnimationTarget> {
    let mut out = Vec::new();
    let position = |key: GroupKey, travel: CGPoint| {
        plan.positions.get(&key).copied().unwrap_or(travel)
    };
    for group in plan.groups.iter().filter(|g| !g.is_still()) {
        let to = position(group.key, group.travel);
        let from = CGPoint::new(to.x - group.travel.x, to.y - group.travel.y);
        out.push(AnimationTarget::Container { key: group.key, from, to });
    }
    let ft = plan.floating_travel;
    if ft.x != 0.0 || ft.y != 0.0 {
        let to = position(GroupKey::Floating, ft);
        let from = CGPoint::new(to.x - ft.x, to.y - ft.y);
        out.push(AnimationTarget::Container { key: GroupKey::Floating, from, to });
    }
    for &(window, from, to) in plan.changing.iter().chain(&plan.entrances) {
        out.push(AnimationTarget::Tile { window, from, to });
    }
    for &(window, from, to) in &plan.floating {
        if !from.same_as(to) {
            out.push(AnimationTarget::Tile { window, from, to });
        }
    }
    out
}

pub struct WorkspaceOverlay {
    window: Retained<NSWindow>,
    /// The layer every tile is added to. Owned by the window's content view, which is layer-backed,
    /// so AppKit presents it on the GPU with no manual rasterisation.
    root: Retained<CALayer>,
    /// One layer per rigid piece of a flight, under `root`. A container's `position` is the only
    /// animated translation its members get; the tiles inside sit at group-relative frames.
    containers: HashMap<GroupKey, Retained<CALayer>>,
    /// The real desktop, drawn behind everything and held still while the tiles move, so the gaps
    /// around strips look like the desktop instead of flickering as a flat colour.
    backdrop: Retained<CALayer>,
    /// The bar, redrawn on top and held still while the tiles move beneath it. The overlay spans the
    /// whole display, so it covers the real bar and has to put it back.
    ///
    /// Drawn from a capture of the bar's own windows, which keeps the bar's own alpha, so the strips show
    /// through it as they scroll under rather than vanishing at its edge.
    bar: Retained<CALayer>,
    /// Whether the bar has ever been drawn, so a skipped capture keeps it rather than hiding it.
    bar_drawn: bool,
    tile_layers: HashMap<WindowId, Tile>,
    /// Display frame in CoreGraphics coordinates, which is what callers speak. Kept so tile rects can
    /// be translated into the overlay's own space.
    frame: CGRect,
    scale: f64,
    visible: bool,
    mtm: MainThreadMarker,
}

impl WorkspaceOverlay {
    /// Creates the overlay once, ordered in but fully transparent.
    ///
    /// `frame` must be the display's FULL bounds in CoreGraphics coordinates, or vertical animations are
    /// invisible in the strip beneath the bar. Built on `NSWindow` rather than a raw window server
    /// window, which cannot have its layer tree bound here and would need a rasterising fallback.
    pub fn new(frame: CGRect, scale: f64, mtm: MainThreadMarker) -> Option<Self> {
        let converter = CoordinateConverter::from_height(primary_display_height());
        let cocoa_frame = converter.convert_rect(frame)?;

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                cocoa_frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setOpaque(true);
        // Without this the gap between strips renders as a bare grey slab, which reads as a glitch
        // rather than as empty space. Opaque is required so the real windows being repositioned
        // underneath stay hidden, so the background has to be drawn rather than left transparent.
        window.setBackgroundColor(Some(&NSColor::blackColor()));
        window.setHasShadow(false);
        // Must never take focus: a workspace animation that changes the active app is a bug.
        window.setIgnoresMouseEvents(true);
        window.setLevel(OVERLAY_LEVEL);
        // canJoinAllSpaces so a Space change does not leave the overlay behind, and stationary so it
        // does not slide along with macOS's own Space animation.
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenNone,
        );
        // Alpha is the show and hide mechanism, so it starts hidden and stays ordered in. Ordering a
        // window in and out costs about 14ms each way, against 0.36ms for an alpha change.
        window.setAlphaValue(0.0);

        // A flipped view, so the layer tree uses the same top-left origin as everything else here.
        let view: Retained<FlippedView> = unsafe {
            objc2::msg_send![
                FlippedView::alloc(mtm),
                initWithFrame: CGRect::new(CGPoint::new(0.0, 0.0), frame.size)
            ]
        };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));

        let root = view.layer()?;
        // No geometryFlipped here: the flipped VIEW already provides the top-left origin, and setting
        // both would cancel out.
        root.setContentsScale(scale);

        // Behind the tiles and never moved. Not geometryFlipped: that flips a layer's own contents
        // too, which drew the captured desktop upside down.
        let backdrop = CALayer::layer();
        backdrop.setAnchorPoint(CGPoint::new(0.0, 0.0));
        backdrop.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        backdrop.setContentsScale(scale);
        backdrop.setZPosition(BACKDROP_Z);
        root.addSublayer(&backdrop);

        // Above the tiles, and never moved: the bar does not scroll with the workspaces. Not
        // geometryFlipped, for the same reason as the backdrop.
        let bar = CALayer::layer();
        bar.setAnchorPoint(CGPoint::new(0.0, 0.0));
        bar.setContentsScale(scale);
        bar.setZPosition(BAR_Z);
        bar.setHidden(true);
        root.addSublayer(&bar);

        window.orderFrontRegardless();

        Some(Self {
            window,
            root,
            backdrop,
            bar,
            bar_drawn: false,
            containers: HashMap::new(),
            tile_layers: HashMap::new(),
            frame,
            scale,
            visible: false,
            mtm,
        })
    }

    pub fn frame(&self) -> CGRect {
        self.frame
    }

    /// Draws the bar on top of the moving tiles, from a picture of the bar itself.
    ///
    /// `strip` is where the bar sits in the overlay's coordinates. The picture keeps its own alpha, so the
    /// strips show through the bar as they scroll under it instead of being cut off at its edge. A failed
    /// capture leaves whatever was there rather than hiding it, so the bar does not blink.
    pub fn set_bar(&mut self, snapshot: Option<&WindowSnapshot>, strip: Option<CGRect>) {
        let Some(strip) = strip else {
            self.bar.setHidden(true);
            return;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        match snapshot {
            Some(snapshot) => {
                let (covered_w, covered_h) = snapshot.coverage.covered;
                self.bar.setContentsScale(self.scale);
                set_layer_contents(&self.bar, snapshot);
                self.bar.setFrame(bar_frame(strip, CGSize::new(covered_w, covered_h)));
                self.bar_drawn = true;
                self.bar.setHidden(false);
            }
            // A capture is skipped whenever the bar is not fully visible to be captured, and hiding the
            // bar instead left the tiles showing through the menu bar strip, which is worse than a
            // slightly stale bar.
            None => self.bar.setHidden(!self.bar_drawn),
        }
        commit_now();
    }

    /// Sets the still image drawn behind the moving tiles.
    ///
    /// Sized from the image's own covered size rather than stretched to the overlay, so it stays in
    /// register with the real desktop. A failed capture keeps whatever was there, or the desktop blinks.
    pub fn set_backdrop(&mut self, snapshot: Option<&WindowSnapshot>) {
        let Some(snapshot) = snapshot else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // The overlay spans the full display now, so the desktop capture matches it exactly and needs
        // no inset correction. It used to be squashed into the shorter usable frame, which is what put
        // it out of register with the real desktop.
        let (covered_w, covered_h) = snapshot.coverage.covered;
        self.backdrop.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(covered_w, covered_h),
        ));
        self.backdrop.setContentsScale(self.scale);
        set_layer_contents(&self.backdrop, snapshot);
        self.backdrop.setHidden(false);
        commit_now();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Repoints the overlay at a different display, or the same one after a resolution change.
    /// Cheaper than recreating it, which would pay the window creation cost again.
    pub fn set_frame(&mut self, frame: CGRect, scale: f64) {
        if self.frame == frame && (self.scale - scale).abs() < f64::EPSILON {
            return;
        }
        self.frame = frame;
        self.scale = scale;
        let converter = CoordinateConverter::from_height(primary_display_height());
        if let Some(cocoa) = converter.convert_rect(frame) {
            self.window.setFrame_display(cocoa, false);
        }
        if let Some(view) = self.window.contentView() {
            view.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        }
        self.root.setContentsScale(scale);
    }

    /// Replaces one tile's picture, leaving its position alone.
    ///
    /// Used mid-flight, once the window being switched into is on screen and can be captured with its
    /// focused appearance. Contents only: changing the frame here would fight the running movement.
    /// Swaps a fresh picture into a tile already on screen. `remaining` is how much of the flight
    /// is left, for a crop-drawn tile whose geometry has to be re-keyed to the new picture.
    pub fn set_tile_picture(
        &mut self,
        window: WindowId,
        snapshot: &WindowSnapshot,
        remaining: Option<Duration>,
    ) {
        let scale = self.scale;
        let Some(entry) = self.tile_layers.get_mut(&window) else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let mut rekey: Option<(CGRect, CGRect, Timing)> = None;
        if entry.crop_of.is_some() {
            // A crop-drawn tile carries its picture on the pieces — and the new picture is the
            // window at its NEW size (the recapture exists precisely because it changed), so the
            // grid mapping built against the old picture would draw it squashed. Re-key: remap
            // to the new picture and re-run the remaining flight from the PRESENTED state, which
            // also replaces the transparent-edge fill of a grow with the window's real content.
            if let Some(grid) = &entry.crop_grid {
                for piece in &grid.pieces {
                    set_layer_contents(piece, snapshot);
                }
            }
            let covered = snapshot.coverage.covered;
            entry.crop_of = Some(CGSize::new(covered.0, covered.1));
            let final_rect = entry.picture.frame();
            layout_crop_grid(entry, final_rect.size);
            // The new picture maps onto a new piece grid, so the leg is re-installed: on the SAME
            // timing when the tile is riding one, so the frame keeps its curve and only the pixels
            // cut; from the presented frame over what is left otherwise.
            if remaining.is_some_and(|left| !left.is_zero()) {
                rekey = Some(match entry.resize_leg {
                    Some((from, to, timing)) => (from, to, timing),
                    None => {
                        // SAFETY: `presentationLayer` returns a read-only copy of the layer as
                        // currently presented.
                        let presented = unsafe { entry.picture.presentationLayer() }
                            .map(|p| CGRect::new(p.position(), p.bounds().size))
                            .unwrap_or(final_rect);
                        (presented, final_rect, Timing::starting_now(remaining.unwrap_or_default()))
                    }
                });
            }
        } else {
            // A hard cut on purpose. A crossfade veil was tried and rejected: stacking two copies
            // of a translucent window pulses its net opacity mid-fade — there is no constant-alpha
            // crossfade with layers. Gratuitous cuts are avoided upstream instead, by skipping
            // swaps whose picture renders the same as the one on screen.
            set_layer_contents(&entry.picture, snapshot);
        }
        // A recapture can carry a fresh hairline too — the focus recapture is exactly the one
        // that changes it. Swapped in place, so it rides any animations already installed.
        if snapshot.dressing.is_some() {
            let size = entry.picture.bounds().size;
            let resizing = resize_in_flight(entry.resize_until, Instant::now());
            apply_edge_dressing(entry, snapshot.dressing.as_ref(), size, scale, true, resizing);
        }
        if let Some((from, to, timing)) = rekey {
            self.animate_tile_resize(window, from, to, timing);
        }
        commit_now();
    }

    /// Swaps one in-flight tile's hairline, for a harvest that landed after its picture did.
    pub fn set_tile_dressing(
        &mut self,
        window: WindowId,
        dressing: &crate::ui::edge_dressing::EdgeDressing,
    ) {
        let scale = self.scale;
        let Some(entry) = self.tile_layers.get_mut(&window) else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let size = entry.picture.bounds().size;
        let resizing = resize_in_flight(entry.resize_until, Instant::now());
        apply_edge_dressing(entry, Some(dressing), size, scale, true, resizing);
        commit_now();
    }

    /// Composes a flight at frame zero as rigid pieces: one container per group, every member at
    /// its group-relative frame inside it; changing, entrance and floating members loose under
    /// `StripLoose` / `Floating`. `tiles` carries the pictures, found by window.
    ///
    /// Layers are pooled per window, since handing one a bitmap is the expensive part. Anything
    /// absent is removed. Pre-flight only: a tile already animating must not pass through here,
    /// because its model sits at the destination while Core Animation carries the presentation.
    /// See "The overlay engine" in `docs/animation-smoothness.md`.
    pub(crate) fn install(&mut self, plan: &FlightPlan, tiles: &[OverlayTile], banding: &Banding) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let mut keep: Vec<WindowId> = Vec::new();
        let find = |window: WindowId| tiles.iter().find(|t| t.window == window);
        for group in &plan.groups {
            if group.members.is_empty() {
                continue;
            }
            self.reset_container(group.key);
            for member in &group.members {
                let Some(tile) = find(member.window) else { continue };
                keep.push(tile.window);
                self.install_tile(tile, member.rel, Some(group.key));
            }
        }
        let strip_loose: Vec<(WindowId, CGRect, CGRect)> =
            plan.changing.iter().chain(&plan.entrances).copied().collect();
        for (key, members) in [(GroupKey::StripLoose, &strip_loose), (GroupKey::Floating, &plan.floating)] {
            if members.is_empty() {
                continue;
            }
            self.reset_container(key);
            for &(window, from, _) in members {
                let Some(tile) = find(window) else { continue };
                keep.push(tile.window);
                self.install_tile(tile, from, Some(key));
            }
        }
        self.remove_stale(&keep);
        self.rebank(banding);
        commit_now();
    }

    /// Writes every container's and tile's `zPosition` from `banding`, nothing else. Containers
    /// sort among themselves (`container_z`, then `strip_order` a quarter step apart); tiles sort
    /// inside their container at `-within`, a companion a quarter step in front of its window, a
    /// shadow half a step behind its picture. A hard cut: z does not interpolate. Callers hold the
    /// transaction. See "The overlay engine" in `docs/animation-smoothness.md`.
    pub(crate) fn rebank(&self, banding: &Banding) {
        let focused = if banding.floating_in_front { StackGroup::Floating } else { StackGroup::Strip };
        for (key, layer) in &self.containers {
            let z = match key {
                GroupKey::Floating => container_z(StackGroup::Floating, focused),
                key => {
                    let index = banding
                        .strip_order
                        .iter()
                        .position(|k| k == key)
                        .unwrap_or(banding.strip_order.len());
                    container_z(StackGroup::Strip, focused) - index as f64 * 0.25
                }
            };
            layer.setZPosition(z);
        }
        for (window, tile) in &self.tile_layers {
            let Some(&within) = banding.within.get(window) else { continue };
            let z = -(within as f64) + if tile.companion { 0.25 } else { 0.0 };
            tile.picture.setZPosition(z);
            tile.shadow.setZPosition(z - 0.5);
        }
    }

    /// The container for `key`, created on first use, put back at the origin with no animation
    /// riding it. Callers hold the transaction.
    fn reset_container(&mut self, key: GroupKey) -> Retained<CALayer> {
        let root = &self.root;
        let layer = self
            .containers
            .entry(key)
            .or_insert_with(|| {
                let layer = CALayer::layer();
                layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
                layer.setMasksToBounds(false);
                root.addSublayer(&layer);
                layer
            })
            .clone();
        layer.removeAllAnimations();
        layer.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), self.frame.size));
        layer.setPosition(CGPoint::new(0.0, 0.0));
        layer
    }

    /// Drops every tile not in `keep`, and every container left with no tile.
    fn remove_stale(&mut self, keep: &[WindowId]) {
        let stale: Vec<WindowId> =
            self.tile_layers.keys().copied().filter(|w| !keep.contains(w)).collect();
        for window in stale {
            if let Some(entry) = self.tile_layers.remove(&window) {
                entry.picture.removeFromSuperlayer();
                entry.shadow.removeFromSuperlayer();
            }
        }
        let occupied: Vec<GroupKey> = self.tile_layers.values().filter_map(|t| t.key).collect();
        let empty: Vec<GroupKey> =
            self.containers.keys().copied().filter(|k| !occupied.contains(k)).collect();
        for key in empty {
            if let Some(layer) = self.containers.remove(&key) {
                layer.removeFromSuperlayer();
            }
        }
    }

    /// Hands a composed flight to Core Animation: exactly the movements `animation_targets` names,
    /// in ONE transaction. Containers get one position animation; loose tiles get their own move or
    /// resize. A zero duration lands everything at its destination with no animation.
    pub(crate) fn fly(&mut self, plan: &FlightPlan, duration: Duration) {
        let timing = Timing::starting_now(duration);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for target in animation_targets(plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    let Some(layer) = self.containers.get(&key) else { continue };
                    let (from, to) = (whole_point(from), whole_point(to));
                    layer.setPosition(to);
                    if !duration.is_zero() {
                        let animation = position_animation(from, to, timing);
                        layer.addAnimation_forKey(
                            &animation,
                            Some(&NSString::from_str(GROUP_ANIMATION_KEY)),
                        );
                    }
                }
                AnimationTarget::Tile { window, from, to } => {
                    let Some(entry) = self.tile_layers.get(&window) else { continue };
                    // Loose members sit in a container too; their frames are in its space.
                    let offset = self.container_position(entry.key);
                    let (from, to) =
                        (whole(group_relative(from, offset)), whole(group_relative(to, offset)));
                    let z = entry.picture.zPosition();
                    if duration.is_zero() {
                        place_tile(entry, to, z);
                    } else {
                        self.animate_tile_movement(window, from, to, z, duration);
                    }
                }
            }
        }
        commit_now();
    }

    /// Nudges the surface by `overshoot` and brings it back over `duration`, on top of whatever
    /// the containers are doing: one additive keyframe animation per container, so a movement in
    /// flight is neither replaced nor disturbed and the model positions stay put. The floating
    /// container rides only a vertical bounce: floating windows are pinned during a pan and carried
    /// by a switch, and the bounce follows the movement it stands in for. See "Edge bounce" in
    /// `docs/animation-smoothness.md`.
    pub(crate) fn bounce(&mut self, overshoot: CGPoint, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let timing = Timing::starting_now(duration);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (key, layer) in &self.containers {
            if !bounce_carries(*key, overshoot) {
                continue;
            }
            let animation = bounce_animation(overshoot, timing);
            layer.addAnimation_forKey(&animation, Some(&NSString::from_str(BOUNCE_ANIMATION_KEY)));
        }
        commit_now();
    }

    /// Model position of the container `key` names; the origin for the root.
    fn container_position(&self, key: Option<GroupKey>) -> CGPoint {
        key.and_then(|k| self.containers.get(&k))
            .map(|layer| layer.position())
            .unwrap_or(CGPoint::new(0.0, 0.0))
    }

    /// Presented position of the container `key` names (model when nothing is presented).
    fn presented_container_position(&self, key: Option<GroupKey>) -> CGPoint {
        let Some(layer) = key.and_then(|k| self.containers.get(&k)) else {
            return CGPoint::new(0.0, 0.0);
        };
        // SAFETY: `presentationLayer` returns a read-only copy of the layer as currently presented.
        unsafe { layer.presentationLayer() }.map(|p| p.position()).unwrap_or(layer.position())
    }

    /// Installs one tile at `at`, in the space of the parent `key` names (the root for `None`).
    /// Callers hold the transaction.
    fn install_tile(&mut self, tile: &OverlayTile, at: CGRect, key: Option<GroupKey>) {
        let parent = match key.and_then(|k| self.containers.get(&k)) {
            Some(container) => container.clone(),
            None => self.root.clone(),
        };
        let entry = self
            .tile_layers
            .entry(tile.window)
            .or_insert_with(|| new_tile(&parent));
        reparent(&entry.picture, &parent);
        reparent(&entry.shadow, &parent);
        entry.key = key;
        entry.companion = tile.companion;
        entry.picture.setContentsScale(self.scale);
        // A fresh install ends whatever resize the pooled tile was riding.
        entry.resize_until = None;
        let covered = tile.snapshot.coverage.covered;
        let mode = content_mode(covered, tile.from.size, tile.to.size);
        set_tile_content(entry, &tile.snapshot, mode, tile.from.size, self.scale);
        apply_edge_dressing(
            entry,
            tile.snapshot.dressing.as_ref(),
            tile.from.size,
            self.scale,
            false,
            false,
        );
        // Set explicitly in both directions, because tile layers are pooled: a tile that carried
        // focus last flight must not keep the deep shadow for an unfocused window.
        let style = tile_shadow_style(tile.focused);
        entry.shadow.setShadowOpacity(style.opacity);
        entry.shadow.setShadowRadius(style.radius);
        entry.shadow.setShadowOffset(CGSize::new(0.0, style.offset_y));
        // Negated so a smaller depth, meaning nearer the front, draws on top.
        place_tile(entry, whole(at), tile.z());
        entry.picture.setHidden(false);
        // A border window casts no shadow, so its tile must not either.
        entry.shadow.setHidden(tile.companion);
    }

    /// Adds one loose tile to a flight in progress under `key`, moving from `tile.from` to
    /// `tile.to` (the container's space) over `duration`: a late entrance gets what is left of the
    /// flight so it lands with its neighbours.
    pub(crate) fn add_tile(
        &mut self,
        tile: &OverlayTile,
        key: GroupKey,
        banding: &Banding,
        duration: Duration,
    ) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.ensure_container(key, CGPoint::new(0.0, 0.0));
        self.install_tile(tile, tile.from, Some(key));
        self.animate_tile_movement(tile.window, whole(tile.from), whole(tile.to), tile.z(), duration);
        self.rebank(banding);
        commit_now();
    }

    /// The container for `key`, created at `position` when the flight has none yet. Unlike
    /// `reset_container` this leaves a moving container alone. Callers hold the transaction.
    fn ensure_container(&mut self, key: GroupKey, position: CGPoint) -> Retained<CALayer> {
        if let Some(layer) = self.containers.get(&key) {
            return layer.clone();
        }
        let layer = self.reset_container(key);
        layer.setPosition(position);
        layer
    }

    /// Whether every container and picture is presented where its model sits, within half a
    /// point: the render server has finished the flight. Read at the lift, because the render
    /// server runs a frame or so behind the actor's clock and lifting on the clock alone showed
    /// the real windows one frame ahead of their tiles.
    pub fn settled(&self) -> bool {
        let close = |a: CGPoint, b: CGPoint| (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5;
        let same_size =
            |a: CGSize, b: CGSize| (a.width - b.width).abs() < 0.5 && (a.height - b.height).abs() < 0.5;
        // SAFETY: `presentationLayer` returns a read-only copy of the layer as currently presented.
        let containers = self.containers.values().all(|layer| {
            unsafe { layer.presentationLayer() }.is_none_or(|p| close(p.position(), layer.position()))
        });
        let tiles = self.tile_layers.values().all(|tile| {
            unsafe { tile.picture.presentationLayer() }.is_none_or(|p| {
                close(p.position(), tile.picture.position())
                    && same_size(p.bounds().size, tile.picture.bounds().size)
            })
        });
        containers && tiles
    }

    /// Presented position of every container, model position when nothing is presented: what
    /// `merge_plans` retargets from.
    pub(crate) fn presented_positions(&self) -> HashMap<GroupKey, CGPoint> {
        self.containers
            .keys()
            .map(|key| (*key, self.presented_container_position(Some(*key))))
            .collect()
    }

    /// Applies one merged pass to a flight in progress, in ONE transaction: containers bend from
    /// their presented position, members changing container are reparented at the frame they are
    /// drawn at, new groups install and fly, loose tiles bend or join. Reads before writes. See
    /// "Mid-flight passes" in `docs/animation-smoothness.md`.
    pub(crate) fn retarget(
        &mut self,
        delta: &PlanDelta,
        plan: &FlightPlan,
        tiles: &[OverlayTile],
        banding: &Banding,
        duration: Duration,
    ) {
        let timing = Timing::starting_now(duration);
        let presented = self.presented_positions();
        let find = |window: WindowId| tiles.iter().find(|t| t.window == window);
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        // New containers, at the install position the merge chose; members the merge did not
        // reparent are fresh tiles.
        for &(key, install) in &delta.new_groups {
            let container = self.reset_container(key);
            container.setPosition(whole_point(install));
            let Some(group) = plan.groups.iter().find(|g| g.key == key) else { continue };
            for member in &group.members {
                if delta.reparented.iter().any(|(w, _, to)| *w == member.window && *to == key) {
                    continue;
                }
                if let Some(tile) = find(member.window) {
                    self.install_tile(tile, member.rel, Some(key));
                }
            }
        }

        // Members changing hands: the merged plan already holds their frame in the new parent.
        let mut placed: Vec<WindowId> = Vec::new();
        for &(window, _, to_key) in &delta.reparented {
            let frame = match plan.member(window) {
                Some(Member::Rigid { rel, .. }) => rel,
                Some(Member::Changing { from, .. })
                | Some(Member::Entrance { from, .. })
                | Some(Member::Floating { from, .. }) => from,
                None => continue,
            };
            let container = self.ensure_container(to_key, plan.positions.get(&to_key).copied().unwrap_or(CGPoint::new(0.0, 0.0)));
            let Some(entry) = self.tile_layers.get_mut(&window) else { continue };
            reparent(&entry.picture, &container);
            reparent(&entry.shadow, &container);
            entry.key = Some(to_key);
            let z = entry.picture.zPosition();
            place_tile(entry, whole(frame), z);
            placed.push(window);
        }

        // Containers: from where they are drawn to the new destination, replacing the running
        // animation under the same key so the group bends instead of restarting.
        for &(key, to) in &delta.retargeted_groups {
            let Some(layer) = self.containers.get(&key) else { continue };
            let from = presented.get(&key).copied().unwrap_or(layer.position());
            let to = whole_point(to);
            layer.setPosition(to);
            if !duration.is_zero() {
                let animation = position_animation(from, to, timing);
                layer.addAnimation_forKey(&animation, Some(&NSString::from_str(GROUP_ANIMATION_KEY)));
            }
        }
        for &(key, install) in &delta.new_groups {
            let Some(layer) = self.containers.get(&key) else { continue };
            let to = whole_point(plan.positions.get(&key).copied().unwrap_or(install));
            let install = whole_point(install);
            layer.setPosition(to);
            if !duration.is_zero() && !install.same_as(to) {
                let animation = position_animation(install, to, timing);
                layer.addAnimation_forKey(&animation, Some(&NSString::from_str(GROUP_ANIMATION_KEY)));
            }
        }

        // Newcomers: a rigid join rides its container from `rel`; a loose join flies on its own.
        for &(window, key) in &delta.joined_tiles {
            let Some(tile) = find(window) else { continue };
            let position = plan.positions.get(&key).copied().unwrap_or(CGPoint::new(0.0, 0.0));
            self.ensure_container(key, position);
            match plan.member(window) {
                Some(Member::Rigid { rel, .. }) => self.install_tile(tile, rel, Some(key)),
                Some(Member::Changing { from, to })
                | Some(Member::Entrance { from, to })
                | Some(Member::Floating { from, to }) => {
                    self.install_tile(tile, from, Some(key));
                    self.animate_tile_movement(window, whole(from), whole(to), tile.z(), duration);
                }
                None => {}
            }
        }

        // Loose tiles bending to a new destination, from where they are drawn.
        for &(window, to) in &delta.retargeted_tiles {
            let Some(tile) = find(window) else { continue };
            let scale = self.scale;
            let Some(entry) = self.tile_layers.get_mut(&window) else { continue };
            let from = if placed.contains(&window) {
                entry.picture.frame()
            } else {
                // SAFETY: `presentationLayer` returns a read-only copy of the layer as presented.
                // Position AND size: a resize retargeted mid-flight continues from the size it is
                // drawn at.
                unsafe { entry.picture.presentationLayer() }
                    .map(|p| CGRect::new(p.position(), p.bounds().size))
                    .unwrap_or_else(|| entry.picture.frame())
            };
            // Judged against the new leg: a retarget can turn a plain move into a resize, and a
            // tile left stretching draws the resize as a stretch.
            let to = whole(to);
            let covered = tile.snapshot.coverage.covered;
            let mode = content_mode(covered, from.size, to.size);
            set_tile_content(entry, &tile.snapshot, mode, from.size, scale);
            let z = entry.picture.zPosition();
            self.animate_tile_movement(window, from, to, z, duration);
        }

        self.remove_stale(&tiles.iter().map(|t| t.window).collect::<Vec<_>>());
        self.rebank(banding);
        commit_now();
    }

    /// Places one tile's model at its destination and installs the movement animation on both of
    /// its layers. Callers hold the transaction.
    fn animate_tile_movement(
        &mut self,
        window: WindowId,
        from: CGRect,
        to: CGRect,
        z: f64,
        duration: Duration,
    ) {
        let Some(entry) = self.tile_layers.get_mut(&window) else { return };
        place_tile(entry, to, z);
        let timing = Timing::starting_now(duration);
        // The proportional tolerance, not equality: a sub-tolerance re-fit rides the plain move
        // and lets its size snap the point it always did.
        if !crate::ui::window_snapshot::is_a_resize(from.size, to.size) {
            // A fresh leg ends whatever resize the tile was riding.
            entry.resize_leg = None;
            // Anchor points are (0,0), so position is the frame origin. addAnimation copies, so
            // one instance serves picture and shadow.
            let animation = position_animation(from.origin, to.origin, timing);
            let key = NSString::from_str(TILE_ANIMATION_KEY);
            entry.picture.addAnimation_forKey(&animation, Some(&key));
            entry.shadow.addAnimation_forKey(&animation, Some(&key));
            return;
        }
        self.animate_tile_resize(window, from, to, timing);
    }

    /// The resize choreography: every layer of the tile rides its own pair of endpoint geometries,
    /// all in the caller's transaction, on the shared curve and duration.
    ///
    /// The endpoints are what make this read as a crop rather than a stretch: the crop pieces'
    /// frames and contentsRects are linear functions of the tile frame (see `crop_pieces`), so
    /// plain interpolation between the endpoint grids IS the per-frame crop layout. The shadow's
    /// path and its ring mask interpolate because both endpoints are built by the same
    /// constructors, and the hairline pieces ride between their two boundary layouts.
    fn animate_tile_resize(&mut self, window: WindowId, from: CGRect, to: CGRect, timing: Timing) {
        let Some(entry) = self.tile_layers.get_mut(&window) else { return };
        // A retarget replaces the leg, so the end moves with it; a re-key keeps the leg's timing.
        entry.resize_until = Some(timing.ends_at());
        entry.resize_leg = Some((from, to, timing));

        // The shadow's movement uses the SAME key prefix as the plain-move branch. Keys are
        // per-layer, so picture and shadow do not collide — but a plain-move leg retargeted into
        // a resize left its old position animation (under the plain key) fighting the resize's
        // one, and the shadow visibly tore away from its tile.
        animate_layer_frame(&entry.picture, from, to, timing, "rini.tile");
        animate_layer_frame(&entry.shadow, from, to, timing, "rini.tile");

        // The shadow's shape and the hole in its ring both follow the window silhouette.
        let origin = CGPoint::new(0.0, 0.0);
        let shadow_path = path_animation(
            "shadowPath",
            &silhouette_path(from.size, origin),
            &silhouette_path(to.size, origin),
            timing,
        );
        entry
            .shadow
            .addAnimation_forKey(&shadow_path, Some(&NSString::from_str("rini.tile.shadow.path")));
        animate_layer_frame(
            &entry.shadow_mask,
            shadow_mask_frame(from.size),
            shadow_mask_frame(to.size),
            timing,
            "rini.tile.mask",
        );
        let mask_path =
            path_animation("path", &ring_path(from.size), &ring_path(to.size), timing);
        entry
            .shadow_mask
            .addAnimation_forKey(&mask_path, Some(&NSString::from_str("rini.tile.mask.path")));

        // The crop pieces carry the picture between the two endpoint grids.
        if let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) {
            let from_pieces = crop_pieces(picture, from.size);
            let to_pieces = crop_pieces(picture, to.size);
            for ((layer, a), b) in grid.pieces.iter().zip(from_pieces).zip(to_pieces) {
                animate_layer_frame(layer, a.frame, b.frame, timing, "rini.piece");
                let contents = rect_animation("contentsRect", a.contents, b.contents, timing);
                layer.addAnimation_forKey(
                    &contents,
                    Some(&NSString::from_str("rini.piece.contents")),
                );
            }
        }

        // The hairline band rides between its two boundary layouts, each layer paired with its
        // endpoint rects by its layout index. Model set to the destination first: Core Animation
        // removes the animation on completion, revealing the model.
        if let (Some(from_layout), Some(to_layout)) =
            (boundary_layout(from.size), boundary_layout(to.size))
        {
            let rect_at = |layout: &crate::ui::edge_dressing::DressingLayout, index: usize| {
                if index < 4 { layout.strips[index] } else { layout.corners[index - 4] }
            };
            for (index, layer) in &entry.dressing {
                let a = rect_at(&from_layout, *index);
                let b = rect_at(&to_layout, *index);
                layer.setFrame(b);
                animate_layer_frame(layer, a, b, timing, "rini.dressing");
            }
        }
    }

    /// Shows the overlay. Costs about 0.36ms, measured, because it is only an alpha change.
    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        self.window.setAlphaValue(1.0);
        self.window.orderFrontRegardless();
        self.visible = true;
    }

    /// Hides the overlay, revealing the real windows already sitting at their final frames.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.window.setAlphaValue(0.0);
        self.visible = false;
    }

    /// Frees the tile contents without destroying the overlay, so an idle rini is not holding window
    /// pictures it no longer needs.
    pub fn release_tiles(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (_, entry) in self.tile_layers.drain() {
            entry.picture.removeFromSuperlayer();
            entry.shadow.removeFromSuperlayer();
        }
        for (_, container) in self.containers.drain() {
            container.removeFromSuperlayer();
        }
        commit_now();
        let _ = self.mtm;
    }
}

/// Where the bar's picture goes: at the strip's origin, at the size the picture actually covers.
///
/// Never stretched to the strip. A composite covers the union of the bar's windows, which is not always
/// the strip the caller measured, and a bar stretched to fit reads as a rendering fault while one drawn
/// slightly short just leaves a gap. Kept separate because drawing it at the overlay's corner instead of
/// the strip's is what put a second bar on screen.
fn bar_frame(strip: CGRect, covered: CGSize) -> CGRect {
    CGRect::new(strip.origin, covered)
}

/// The harvested image for one piece, in `DressingLayout` order: four strips, then four corners.
fn dressing_image(
    dressing: &crate::ui::edge_dressing::EdgeDressing,
    index: usize,
) -> Option<&CFRetained<objc2_core_graphics::CGImage>> {
    if index < 4 { dressing.strips[index].as_ref() } else { dressing.corners[index - 4].as_ref() }
}

/// Which pieces a tile dressed at `size` wears: every harvested piece, whatever the size.
///
/// A run with no extent at this size still gets a (zero-size) layer, so the set never changes
/// between an entrance's zero width and its final width and a fresh harvest swaps in place
/// instead of rebuilding the ring mid-flight. See `docs/animation-smoothness.md`.
fn dressing_piece_indices(
    dressing: &crate::ui::edge_dressing::EdgeDressing,
    size: CGSize,
) -> Vec<usize> {
    if boundary_layout(size).is_none() {
        return Vec::new();
    }
    (0..8).filter(|index| dressing_image(dressing, *index).is_some()).collect()
}

/// Dresses a tile with its window's harvested hairline, or strips it bare.
///
/// The ring rides the picture as thin sublayers — four straight runs and four corner boxes, in
/// [`crate::ui::edge_dressing::DressingLayout`] order — so every movement animation carries it for
/// free. Replaced wholesale because tile layers are pooled: a tile must not wear another window's
/// edge, or a stale one after a recapture.
fn apply_edge_dressing(
    tile: &mut Tile,
    dressing: Option<&crate::ui::edge_dressing::EdgeDressing>,
    size: CGSize,
    scale: f64,
    swap_in_place: bool,
    resize_in_flight: bool,
) {
    // A fresh harvest for the same window swaps pixels INTO the existing layers when the piece
    // sets match, leaving their geometry — and crucially any resize animations riding them —
    // untouched. A mismatched set is deferred while a resize rides (the cached snapshot carries
    // it for the next install); rebuilding mid-flight snapped the border to its final layout
    // while the tile was still travelling. Installs rebuild unconditionally: a pooled tile's
    // layers may belong to another window's geometry entirely.
    if swap_in_place && let Some(new) = dressing {
        let available: Vec<usize> = (0..8).filter(|i| dressing_image(new, *i).is_some()).collect();
        let worn: Vec<usize> = tile.dressing.iter().map(|(i, _)| *i).collect();
        let worn_matches = !worn.is_empty() && worn == available;
        match dressing_rebuild_allowed(resize_in_flight, worn_matches) {
            DressingAction::SwapInPlace => {
                for (index, layer) in &tile.dressing {
                    let image = dressing_image(new, *index).expect("index sets match");
                    // SAFETY: a retained CGImage; Core Animation retains what it draws.
                    unsafe {
                        let raw: *const objc2_core_graphics::CGImage = &**image;
                        let _: () = msg_send![&**layer, setContents: raw];
                    }
                }
                return;
            }
            DressingAction::Defer => return,
            DressingAction::Rebuild => {}
        }
    }
    for (_, layer) in tile.dressing.drain(..) {
        layer.removeFromSuperlayer();
    }
    let Some(dressing) = dressing else { return };
    let Some(layout) = boundary_layout(size) else { return };
    let frames: Vec<CGRect> = layout.strips.into_iter().chain(layout.corners).collect();
    for index in dressing_piece_indices(dressing, size) {
        let image = dressing_image(dressing, index).expect("indices name harvested pieces");
        let frame = frames[index];
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setFrame(frame);
        layer.setContentsScale(scale);
        // Above the crop pieces (which sit at the default 0), whichever order they were created in.
        layer.setZPosition(1.0);
        // SAFETY: a retained CGImage; Core Animation retains what it draws.
        unsafe {
            let raw: *const objc2_core_graphics::CGImage = &**image;
            let _: () = msg_send![&*layer, setContents: raw];
        }
        tile.picture.addSublayer(&layer);
        tile.dressing.push((index, layer));
    }
}

/// A tile: the picture, plus a caster behind it holding the shadow.
///
/// No `masksToBounds` on the picture: it clips the layer's own shadow away, and the contents cannot spill
/// regardless because Core Animation resizes them to the layer's bounds.
fn new_tile(container: &CALayer) -> Tile {
    let shadow = CALayer::layer();
    shadow.setAnchorPoint(CGPoint::new(0.0, 0.0));
    // Shadow colour is left alone: a CALayer's default is opaque black, which is what a window casts. The
    // caster has no contents and no background, so the shadow is all it ever draws. The style numbers
    // are per-tile (focus deepens them) and applied at install.
    let shadow_mask = objc2_quartz_core::CAShapeLayer::layer();
    shadow_mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
    // SAFETY: Core Animation's own fill-rule constant, and a mask layer we just created and own.
    unsafe {
        shadow_mask.setFillRule(objc2_quartz_core::kCAFillRuleEvenOdd);
        shadow.setMask(Some(&shadow_mask));
    }
    container.addSublayer(&shadow);

    let picture = CALayer::layer();
    picture.setAnchorPoint(CGPoint::new(0.0, 0.0));
    // SAFETY: Core Animation's own filter-name constants.
    unsafe {
        picture.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
        picture.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
    }
    container.addSublayer(&picture);

    Tile {
        picture,
        shadow,
        shadow_mask,
        dressing: Vec::new(),
        crop_grid: None,
        crop_of: None,
        resize_until: None,
        resize_leg: None,
        key: None,
        companion: false,
    }
}

/// Puts both of a tile's layers at `frame`, with the caster just behind the picture.
///
/// The shadow's shape is rebuilt only when the size changes, which is once per animation rather than once
/// per frame: a movement changes where a tile is, not how big it is.
fn place_tile(tile: &Tile, frame: CGRect, z: f64) {
    let resized = tile.shadow.bounds().size != frame.size;
    tile.picture.setFrame(frame);
    tile.picture.setZPosition(z);
    tile.shadow.setFrame(frame);
    // Behind its own picture, but still in front of whatever the next tile back is: depths are whole
    // numbers, so half a step cannot collide with another tile.
    tile.shadow.setZPosition(z - 0.5);
    if resized {
        set_tile_shadow(&tile.shadow, &tile.shadow_mask, frame.size);
        layout_crop_grid(tile, frame.size);
    }
}

/// The window's own rounded-rect silhouette, which is both the shadow's shape and the mask's hole.
///
/// Never degenerate: dimensions are floored and the radius is kept strictly positive, because a
/// zero radius makes Core Graphics emit a PLAIN rect — a different element structure — and a path
/// animation between mismatched structures does not interpolate, it cuts. An entrance growing
/// from zero width hit exactly that: its shadow popped instead of riding.
fn silhouette_path(size: CGSize, at: CGPoint) -> CFRetained<objc2_core_graphics::CGPath> {
    let size = CGSize::new(size.width.max(0.5), size.height.max(0.5));
    let radius = tile_corner_radius(size).max(0.01);
    // SAFETY: a null transform means the path is taken as given.
    unsafe {
        objc2_core_graphics::CGPath::with_rounded_rect(
            CGRect::new(at, size),
            radius,
            radius,
            std::ptr::null(),
        )
    }
}

/// The ring: the whole mask, minus the window's own shape, wound so the inside is the hole.
///
/// Built identically for every size — one rect, one rounded rect — so two ring paths always have
/// matching elements and Core Animation can interpolate between them during a resize.
fn ring_path(size: CGSize) -> CFRetained<objc2_core_graphics::CGMutablePath> {
    // Floored for the same reason as `silhouette_path`: the hole's element structure must be
    // identical for every size or the mask's path animation cuts instead of interpolating.
    let size = CGSize::new(size.width.max(0.5), size.height.max(0.5));
    let radius = tile_corner_radius(size).max(0.01);
    let mask_frame = shadow_mask_frame(size);
    let ring = objc2_core_graphics::CGMutablePath::new();
    unsafe {
        objc2_core_graphics::CGMutablePath::add_rect(
            Some(&ring),
            std::ptr::null(),
            CGRect::new(CGPoint::new(0.0, 0.0), mask_frame.size),
        );
        objc2_core_graphics::CGMutablePath::add_rounded_rect(
            Some(&ring),
            std::ptr::null(),
            CGRect::new(CGPoint::new(SHADOW_REACH, SHADOW_REACH), size),
            radius,
            radius,
        );
    }
    ring
}

/// Gives the caster the shadow of a window of `size`, clipped to a ring outside it.
///
/// An explicit path rather than letting Core Animation derive one from the picture's alpha. Deriving is
/// exact for an oddly shaped window, but it is recomputed whenever the contents change, and every window
/// rini manages is a rounded rect.
///
/// The mask is what keeps the shadow off the window itself. Without it a Core Animation shadow covers the
/// whole layer, so a window with per-pixel alpha shows it through the glass as a wash over the entire
/// window. Measured side by side against a masked caster over white: 0.45 grey unmasked against 0.62
/// masked, where the window's own colour over white is 0.55.
fn set_tile_shadow(layer: &CALayer, mask: &objc2_quartz_core::CAShapeLayer, size: CGSize) {
    layer.setShadowPath(Some(&silhouette_path(size, CGPoint::new(0.0, 0.0))));
    mask.setFrame(shadow_mask_frame(size));
    mask.setPath(Some(&ring_path(size)));
}

/// The rect the shadow's mask has to cover, in the caster's own coordinates.
///
/// Wide enough for the shadow to reach its full extent on every side, and offset so the window's own shape
/// sits `SHADOW_REACH` inside it, which is where the ring's hole goes.
fn shadow_mask_frame(size: CGSize) -> CGRect {
    CGRect::new(
        CGPoint::new(-SHADOW_REACH, -SHADOW_REACH),
        CGSize::new(size.width + 2.0 * SHADOW_REACH, size.height + 2.0 * SHADOW_REACH),
    )
}

/// Points a tile's contents at `mode`, handing every piece the snapshot.
///
/// Always set explicitly: tile layers are pooled across animations and across both drawing paths,
/// so a tile left cropping by a resize would otherwise dismember the next animation's stretch.
fn set_tile_content(
    entry: &mut Tile,
    snapshot: &WindowSnapshot,
    mode: ContentMode,
    frame: CGSize,
    scale: f64,
) {
    match mode {
        ContentMode::Crop => {
            // The container draws nothing itself; the grid pieces carry the picture.
            unsafe {
                let _: () = msg_send![&*entry.picture, setContents: std::ptr::null::<NSObject>()];
            }
            let grid = entry.crop_grid.get_or_insert_with(|| new_crop_grid(&entry.picture));
            for piece in &grid.pieces {
                piece.setContentsScale(scale);
                piece.setHidden(false);
                set_layer_contents(piece, snapshot);
            }
            let covered = snapshot.coverage.covered;
            entry.crop_of = Some(CGSize::new(covered.0, covered.1));
            layout_crop_grid(entry, frame);
        }
        ContentMode::Stretch => {
            entry.crop_of = None;
            if let Some(grid) = &entry.crop_grid {
                for piece in &grid.pieces {
                    piece.setHidden(true);
                }
            }
            set_layer_contents(&entry.picture, snapshot);
        }
    }
}

/// Lays the crop grid out for the tile's current frame size.
fn layout_crop_grid(entry: &Tile, frame: CGSize) {
    let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) else { return };
    for (layer, piece) in grid.pieces.iter().zip(crop_pieces(picture, frame)) {
        layer.setFrame(piece.frame);
        layer.setContentsRect(piece.contents);
    }
}

/// Creates the four crop pieces under a tile's picture layer.
fn new_crop_grid(container: &CALayer) -> CropGrid {
    let pieces = std::array::from_fn(|_| {
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        // SAFETY: Core Animation's own filter-name constants.
        unsafe {
            layer.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
            layer.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
        }
        container.addSublayer(&layer);
        layer
    });
    CropGrid { pieces }
}

/// Moves `layer` under `container` unless it is already there.
///
/// The two drawing paths pool the same layers but hang them off different parents, in different
/// coordinate systems. A layer left under the wrong one is positioned with the wrong arithmetic.
fn reparent(layer: &CALayer, container: &CALayer) {
    let already = layer
        .superlayer()
        .is_some_and(|current| std::ptr::eq(&*current as *const CALayer, container as *const CALayer));
    if already {
        return;
    }
    layer.removeFromSuperlayer();
    container.addSublayer(layer);
}

/// Hands a snapshot to a layer as its contents.
///
/// Core Animation accepts either a `CGImage` or an `IOSurface`, so neither kind needs converting, and
/// converting would cost exactly what each capture API exists to avoid.
fn set_layer_contents(layer: &CALayer, snapshot: &WindowSnapshot) {
    unsafe {
        match &snapshot.image {
            SnapshotImage::Bitmap(image) => {
                let raw: *const objc2_core_graphics::CGImage = &**image;
                let _: () = msg_send![layer, setContents: raw];
            }
            SnapshotImage::Surface(surface) => {
                let raw: *const objc2_io_surface::IOSurfaceRef = &**surface;
                let _: () = msg_send![layer, setContents: raw];
            }
        }
    }
}

/// Height of the coordinate space AppKit measures window positions in.
///
/// AppKit uses a bottom-left origin anchored to the primary display, while everything else in rini
/// speaks CoreGraphics top-left coordinates. Flipping needs the primary display's bottom edge, which
/// is its origin plus its height.
fn primary_display_height() -> f64 {
    let bounds = CGDisplayBounds(unsafe { CGMainDisplayID() });
    bounds.origin.y + bounds.size.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// T9 (1.5) of `.kiro/specs/flight-render-stability/bugfix.md`. A harvest with a different
    /// piece set lands while the tile's resize is animating: rebuilding puts the ring at the
    /// destination size over a picture still travelling. Asserts the fixed `Defer`; unfixed
    /// rebuilds.
    #[test]
    fn a_mismatched_dressing_is_deferred_while_a_resize_is_in_flight() {
        let action = dressing_rebuild_allowed(true, false);
        assert_eq!(action, DressingAction::Defer, "hairline rebuilt mid-resize: {action:?}");
    }

    /// Tiles sit on whole points, where the window server puts the real windows: a layout offset
    /// of 4592.67 rounds the same way for every column, so a strip stays rigid.
    #[test]
    fn tiles_sit_on_whole_points_like_the_real_windows() {
        let a = rect(4.0 - 4592.6724 + 4592.0, 32.0, 859.0, 1081.0);
        let b = rect(a.origin.x + 861.0, 32.0, 861.0, 1081.0);
        assert_eq!(whole(a).origin.x, 3.0);
        assert_eq!(whole(b).origin.x - whole(a).origin.x, 861.0, "one rounding for the strip");
        assert_eq!(whole(a).size, a.size);
        assert_eq!(whole_point(CGPoint::new(-861.3276, 0.4)), CGPoint::new(-861.0, 0.0));
        assert_eq!(whole(rect(10.0, 20.0, 100.0, 50.0)), rect(10.0, 20.0, 100.0, 50.0));
    }

    #[test]
    fn dressing_rebuild_allowed_full_table() {
        assert_eq!(dressing_rebuild_allowed(false, true), DressingAction::SwapInPlace);
        assert_eq!(dressing_rebuild_allowed(true, true), DressingAction::SwapInPlace);
        assert_eq!(dressing_rebuild_allowed(false, false), DressingAction::Rebuild);
        assert_eq!(dressing_rebuild_allowed(true, false), DressingAction::Defer);
    }

    /// `animation_targets`: what `fly` installs and nothing else (Requirement 11.4 of
    /// `rigid-strip-groups`). Plans built with `reflow_plan` on a 1728x1117 display at the origin,
    /// so overlay space equals display space.
    mod targets {
        use super::*;
        use crate::actor::workspace_animation::plan::{FlightPlan, reflow_plan};

        const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 7, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn shifted(frame: CGRect, dx: f64) -> CGRect {
            CGRect::new(CGPoint::new(frame.origin.x + dx, frame.origin.y), frame.size)
        }

        fn flight(requests: &[(WindowId, CGRect, CGRect, bool)]) -> FlightPlan {
            FlightPlan::from(reflow_plan(requests, DISPLAY))
        }

        fn containers(targets: &[AnimationTarget]) -> Vec<GroupKey> {
            targets
                .iter()
                .filter_map(|t| match t {
                    AnimationTarget::Container { key, .. } => Some(*key),
                    AnimationTarget::Tile { .. } => None,
                })
                .collect()
        }

        fn tiles(targets: &[AnimationTarget]) -> Vec<WindowId> {
            targets
                .iter()
                .filter_map(|t| match t {
                    AnimationTarget::Tile { window, .. } => Some(*window),
                    AnimationTarget::Container { .. } => None,
                })
                .collect()
        }

        #[test]
        fn a_one_group_plan_is_exactly_one_container_target() {
            let (a, b) = (column(0.0), column(1.0));
            let plan = flight(&[
                (wid(1), a, shifted(a, -859.0), false),
                (wid(2), b, shifted(b, -859.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(
                targets,
                vec![AnimationTarget::Container {
                    key: GroupKey::Strip(1),
                    from: CGPoint::new(0.0, 0.0),
                    to: CGPoint::new(-859.0, 0.0),
                }]
            );
        }

        #[test]
        fn a_still_group_and_two_moving_groups_are_two_container_targets() {
            let (a, b, c) = (column(0.0), column(1.0), column(2.0));
            let plan = flight(&[
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, 300.0), false),
                (wid(3), c, shifted(c, -300.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(containers(&targets), vec![GroupKey::Strip(1), GroupKey::Strip(2)]);
            assert!(tiles(&targets).is_empty(), "no rigid member is a tile target");
        }

        #[test]
        fn a_changing_member_is_a_tile_target_and_rigid_members_are_not() {
            let (a, b) = (column(0.0), column(1.0));
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 400.0, a.size.height);
            let plan = flight(&[
                (wid(1), a, grown, false),
                (wid(2), b, shifted(b, 400.0), false),
            ]);
            let targets = animation_targets(&plan);
            assert_eq!(tiles(&targets), vec![wid(1)]);
            assert_eq!(containers(&targets), vec![GroupKey::Strip(1)]);
            assert!(targets.contains(&AnimationTarget::Tile { window: wid(1), from: a, to: grown }));
        }

        #[test]
        fn the_floating_container_is_a_target_iff_it_travels() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let mut plan = flight(&[(wid(1), settings, settings, true)]);
            assert!(animation_targets(&plan).is_empty(), "a standing floating window: nothing flies");

            plan.floating_travel = CGPoint::new(0.0, -1117.0);
            plan.positions.insert(GroupKey::Floating, plan.floating_travel);
            assert_eq!(
                animation_targets(&plan),
                vec![AnimationTarget::Container {
                    key: GroupKey::Floating,
                    from: CGPoint::new(0.0, 0.0),
                    to: CGPoint::new(0.0, -1117.0),
                }]
            );

            let moved = shifted(settings, -100.0);
            let plan = flight(&[(wid(1), settings, moved, true)]);
            assert_eq!(
                animation_targets(&plan),
                vec![AnimationTarget::Tile { window: wid(1), from: settings, to: moved }],
                "a floating window moving on its own is a tile target, not a container"
            );
        }

        #[test]
        fn zero_travel_groups_are_never_targets() {
            let (a, b) = (column(0.0), column(1.0));
            let plan = flight(&[(wid(1), a, a, false), (wid(2), b, b, false)]);
            assert!(animation_targets(&plan).is_empty());
            assert_eq!(plan.groups.len(), 1, "everything standing is the still group");
        }

    }

    #[test]
    fn a_resize_is_in_flight_until_its_end_instant() {
        let now = Instant::now();
        assert!(resize_in_flight(Some(now + Duration::from_millis(100)), now));
        assert!(!resize_in_flight(Some(now - Duration::from_millis(1)), now));
        assert!(!resize_in_flight(Some(now), now), "the end instant itself is over");
        assert!(!resize_in_flight(None, now));
    }

    /// Exhaustive over worn × available 8-bit piece sets and both resize flags, matched the way
    /// `apply_edge_dressing` matches them: a rebuild never lands while a resize is in flight.
    #[test]
    fn a_resize_in_flight_never_rebuilds_the_hairline() {
        for worn_bits in 0u32..256 {
            for available_bits in 0u32..256 {
                let worn: Vec<usize> = (0..8).filter(|i| worn_bits & (1 << i) != 0).collect();
                let available: Vec<usize> =
                    (0..8).filter(|i| available_bits & (1 << i) != 0).collect();
                let worn_matches = !worn.is_empty() && worn == available;
                for resize_in_flight in [false, true] {
                    let action = dressing_rebuild_allowed(resize_in_flight, worn_matches);
                    if resize_in_flight {
                        assert_ne!(
                            action,
                            DressingAction::Rebuild,
                            "worn {worn:?} available {available:?}"
                        );
                    }
                    if worn_matches {
                        assert_eq!(action, DressingAction::SwapInPlace);
                    } else if !resize_in_flight {
                        assert_eq!(action, DressingAction::Rebuild);
                    }
                }
            }
        }
    }

    /// P-3.7 of `.kiro/specs/flight-render-stability/bugfix.md`. Observed: a harvest whose piece
    /// set matches the worn set swaps pixels in place whether or not a resize is in flight, over
    /// every non-empty piece set, matched the way `apply_edge_dressing` matches them.
    #[test]
    fn a_matching_piece_set_swaps_in_place_in_or_out_of_a_resize() {
        for bits in 1u32..256 {
            let worn: Vec<usize> = (0..8).filter(|i| bits & (1 << i) != 0).collect();
            let available = worn.clone();
            let worn_matches = !worn.is_empty() && worn == available;
            assert!(worn_matches, "pieces {worn:?}");
            for resize_in_flight in [false, true] {
                assert_eq!(
                    dressing_rebuild_allowed(resize_in_flight, worn_matches),
                    DressingAction::SwapInPlace,
                    "pieces {worn:?} resize_in_flight={resize_in_flight}"
                );
            }
        }
    }

    #[test]
    fn lerp_at_zero_is_the_start_frame() {
        let from = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        let got = lerp_rect(from, to, 0.0);
        assert_eq!(got, from);
    }

    #[test]
    fn lerp_at_one_is_the_end_frame() {
        let from = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        assert_eq!(lerp_rect(from, to, 1.0), to);
    }

    #[test]
    fn lerp_midpoint_is_halfway_on_every_axis() {
        let got = lerp_rect(rect(0.0, 0.0, 100.0, 100.0), rect(100.0, 200.0, 200.0, 300.0), 0.5);
        assert_eq!(got, rect(50.0, 100.0, 150.0, 200.0));
    }

    #[test]
    fn lerp_handles_a_negative_origin() {
        // Off-strip windows legitimately sit at negative x, so this is the common case rather than
        // an edge case: the measured strip had columns at x = -1680.
        let got = lerp_rect(rect(-1680.0, 32.0, 859.0, 1081.0), rect(0.0, 32.0, 859.0, 1081.0), 0.5);
        assert_eq!(got.origin.x, -840.0);
        assert_eq!(got.origin.y, 32.0);
    }

    #[test]
    fn easing_is_pinned_at_both_ends() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
    }

    /// The Bezier control points in `ease_out_cubic_timing` must trace `ease_out_cubic` exactly,
    /// or a chained retarget starts with a visible jump. Derivation in
    /// `docs/animation-smoothness.md`.
    #[test]
    fn the_core_animation_curve_is_exactly_ease_out_cubic() {
        let (c1x, c1y, c2x, c2y) = (1.0 / 3.0, 1.0, 2.0 / 3.0, 1.0);
        for step in 0..=1000 {
            let t = step as f64 / 1000.0;
            let b = |p1: f64, p2: f64| {
                3.0 * (1.0 - t).powi(2) * t * p1 + 3.0 * (1.0 - t) * t.powi(2) * p2 + t.powi(3)
            };
            // x controls at the thirds make x(t) = t, so y(t) is progress as a function of time.
            assert!((b(c1x, c2x) - t).abs() < 1e-12, "x(t) is not the identity at t={t}");
            assert!(
                (b(c1y, c2y) - ease_out_cubic(t)).abs() < 1e-12,
                "the curves diverge at t={t}"
            );
        }
    }

    #[test]
    fn easing_is_front_loaded() {
        // Ease-out means most of the distance is covered early. If this ever inverts, the animation
        // reads as sluggish to start and abrupt to finish.
        assert!(ease_out_cubic(0.5) > 0.5);
        assert!(ease_out_cubic(0.25) > 0.25);
    }

    /// A bounce leaves rest, peaks at the turn, and is back at rest at the end, with no
    /// second hump: out is monotone up to the turn, back is monotone down after it. The turn is
    /// early, so the snap out is quicker than the settle home.
    #[test]
    fn a_bounce_goes_out_once_and_comes_home() {
        assert_eq!(bounce_displacement(0.0), 0.0);
        assert!((bounce_displacement(BOUNCE_TURN) - 1.0).abs() < 1e-12);
        assert!(bounce_displacement(1.0).abs() < 1e-12);
        let mut previous = 0.0;
        for i in 1..=1000 {
            let t = i as f64 / 1000.0;
            let d = bounce_displacement(t);
            if t <= BOUNCE_TURN {
                assert!(d >= previous, "still going out at t={t}");
            } else {
                assert!(d <= previous + 1e-12, "coming back at t={t}");
            }
            previous = d;
        }
        assert!(BOUNCE_TURN < 0.5, "out fast, home at leisure");
    }

    /// Strip containers always bounce; the floating container only with the stack (vertical),
    /// the way a pan pins floating windows and a switch carries them.
    #[test]
    fn the_floating_container_bounces_only_vertically() {
        let sideways = CGPoint::new(-36.0, 0.0);
        let upward = CGPoint::new(0.0, -36.0);
        for key in [GroupKey::Strip(0), GroupKey::Strip(3), GroupKey::StripLoose] {
            assert!(bounce_carries(key, sideways), "{key:?}");
            assert!(bounce_carries(key, upward), "{key:?}");
        }
        assert!(!bounce_carries(GroupKey::Floating, sideways));
        assert!(bounce_carries(GroupKey::Floating, upward));
    }

    #[test]
    fn easing_is_monotonic() {
        // A non-monotonic easing curve makes windows visibly step backwards mid-slide.
        let mut previous = -1.0;
        for i in 0..=100 {
            let value = ease_out_cubic(i as f64 / 100.0);
            assert!(value >= previous, "easing went backwards at t = {}", i);
            previous = value;
        }
    }

    #[test]
    fn a_matching_movement_stretches_and_everything_else_crops() {
        let col = CGSize::new(859.0, 1081.0);
        let picture = (859.0, 1081.0);
        assert_eq!(content_mode(picture, col, col), ContentMode::Stretch);
        assert_eq!(
            content_mode((918.0, 1081.0), CGSize::new(918.0, 1081.0), CGSize::new(917.0, 1081.0)),
            ContentMode::Stretch
        );
        // A horizontal grow past the picture is the placeholder: it stretches until the reveal
        // lands. A vertical shrink crops.
        assert_eq!(
            content_mode(picture, col, CGSize::new(1720.0, 1081.0)),
            ContentMode::Stretch
        );
        assert_eq!(content_mode(picture, col, CGSize::new(859.0, 540.0)), ContentMode::Crop);
        // A stale narrow picture in a wider frame is the placeholder case too: the reveal fills it.
        assert_eq!(content_mode((572.0, 1081.0), col, col), ContentMode::Stretch);
        assert_eq!(content_mode((918.0, 1081.0), col, col), ContentMode::Crop, "a wider one crops");
    }

    /// A picture that cannot cover its destination is stretched over it; one that covers it takes
    /// the ordinary crop. Either way the whole frame is drawn.
    #[test]
    fn a_placeholder_never_shows_backdrop() {
        let picture = (859.0, 1081.0);
        assert_eq!(placeholder_mode(picture, CGSize::new(1147.0, 1081.0)), ContentMode::Stretch);
        assert_eq!(placeholder_mode(picture, CGSize::new(859.0, 1300.0)), ContentMode::Stretch);
        assert_eq!(placeholder_mode(picture, CGSize::new(859.0, 1081.0)), ContentMode::Crop);
        assert_eq!(placeholder_mode(picture, CGSize::new(572.0, 1081.0)), ContentMode::Crop);
        // Both modes fill the frame: the match is exhaustive, so a mode that left part of the
        // frame undrawn would have to be added here to compile.
        for w in (1..=40).map(|i| i as f64 * 50.0) {
            for h in (1..=30).map(|i| i as f64 * 50.0) {
                let to = CGSize::new(w, h);
                match placeholder_mode(picture, to) {
                    ContentMode::Stretch => {
                        assert!(crate::ui::window_snapshot::outgrows(picture, to), "{to:?}")
                    }
                    ContentMode::Crop => {
                        assert!(!crate::ui::window_snapshot::outgrows(picture, to), "{to:?}")
                    }
                }
            }
        }
    }

    /// Every crop piece maps 1:1 while the frame fits inside the picture — its frame exactly as
    /// large as the picture region it shows — and the four pieces always tile the frame with no
    /// gap and no overlap. Any violation is a stretch or a seam, which is exactly what this mode
    /// exists to rule out.
    #[test]
    fn crop_pieces_map_one_to_one_and_tile_the_frame() {
        let picture = CGSize::new(859.0, 1081.0);
        let frames = [
            CGSize::new(572.0, 1081.0), // shrink
            CGSize::new(859.0, 540.0),  // vertical
            CGSize::new(20.0, 1081.0),  // narrower than the band
        ];
        for frame in frames {
            let mut area = 0.0;
            for piece in crop_pieces(picture, frame) {
                assert!(
                    (piece.contents.size.width * picture.width - piece.frame.size.width).abs()
                        < 1e-9,
                    "a piece stretches horizontally at {frame:?}"
                );
                assert!(
                    (piece.contents.size.height * picture.height - piece.frame.size.height).abs()
                        < 1e-9,
                    "a piece stretches vertically at {frame:?}"
                );
                area += piece.frame.size.width * piece.frame.size.height;
            }
            assert!(
                (area - frame.width * frame.height).abs() < 1e-6,
                "gap or overlap at {frame:?}"
            );
        }
    }

    /// Growing past the picture, the leading region stretches the picture's own content — never
    /// reaches past its edge, where a translucent window's near-transparent pixels painted the
    /// grow as a hole — and the trailing band stays 1:1 so the corners and hairline never distort.
    #[test]
    fn a_grow_past_the_picture_stretches_the_lead_and_keeps_the_band_intact() {
        let picture = CGSize::new(859.0, 1081.0);
        let frame = CGSize::new(1720.0, 1081.0);
        let pieces = crop_pieces(picture, frame);
        let body = &pieces[0];
        // The body shows everything up to the trailing band and no further.
        assert!((body.contents.origin.x).abs() < 1e-9);
        assert!((body.contents.size.width * picture.width - (859.0 - 40.0)).abs() < 1e-9);
        assert_eq!(body.frame.size.width, 1720.0 - 40.0, "stretched across the grown lead");
        // The trailing band is still the picture's own trailing 40pt at 1:1.
        let band = &pieces[1];
        assert!((band.contents.size.width * picture.width - 40.0).abs() < 1e-9);
        assert_eq!(band.frame.size.width, 40.0);
        // And the pieces still tile the frame exactly.
        let area: f64 =
            pieces.iter().map(|p| p.frame.size.width * p.frame.size.height).sum();
        assert!((area - frame.width * frame.height).abs() < 1e-6);
    }

    /// The right band shows the picture's own right edge pinned to the frame's moving right edge,
    /// so the window's right border and corners ride it intact.
    #[test]
    fn crop_right_band_comes_from_the_pictures_right_edge() {
        let picture = CGSize::new(859.0, 1081.0);
        let pieces = crop_pieces(picture, CGSize::new(572.0, 1081.0));
        let band = &pieces[3];
        assert!((band.contents.origin.x * picture.width - (859.0 - 40.0)).abs() < 1e-9);
        assert_eq!(band.frame.origin.x, 572.0 - 40.0);
        assert_eq!(band.frame.size.width, 40.0);
    }

    /// A vertical resize anchors like the real thing: the title bar stays put at the top, the
    /// bottom content rides the moving bottom edge, and the seam sits just below the title-bar
    /// band. Cutting at the bottom instead read as the window sliding into a slot.
    #[test]
    fn a_vertical_resize_pins_the_title_bar_and_anchors_content_to_the_bottom() {
        let picture = CGSize::new(859.0, 1081.0);
        let frame = CGSize::new(859.0, 540.0);
        let pieces = crop_pieces(picture, frame);
        let title = &pieces[0];
        assert_eq!(title.frame.origin.y, 0.0, "title band pinned to the top");
        assert!((title.contents.origin.y).abs() < 1e-9, "showing the picture's top");
        assert_eq!(title.frame.size.height, 40.0);
        let body = &pieces[2];
        assert_eq!(body.frame.origin.y, 40.0, "body starts at the seam below the title");
        // Bottom-anchored: the contents reach the picture's bottom edge exactly.
        let content_bottom =
            (body.contents.origin.y + body.contents.size.height) * picture.height;
        assert!((content_bottom - 1081.0).abs() < 1e-9, "contents reach the picture's bottom");
        assert_eq!(body.frame.size.height, 540.0 - 40.0);
    }

    /// The band shrinks with the frame and vanishes at zero, so an entrance growing from nothing
    /// starts as a plain reveal and gains its band continuously — no seam pops in mid-flight.
    #[test]
    fn crop_band_fits_any_frame() {
        assert_eq!(crop_band(CGSize::new(859.0, 1081.0)), 40.0);
        assert!((crop_band(CGSize::new(60.0, 1081.0)) - 27.0).abs() < 1e-9);
        assert_eq!(crop_band(CGSize::new(0.0, 1081.0)), 0.0);
    }

    /// The measured miss: the floating Settings window's tile, in the back z-group, landed at
    /// zPosition about -(1<<20) while the backdrop sat at -10000 — drawn in every animation,
    /// visible in none, because it was behind the desktop picture. The backdrop must sit behind
    /// the DEEPEST depth the grouping can produce, and the bar in front of the shallowest.
    #[test]
    fn every_possible_tile_draws_between_the_backdrop_and_the_bar() {
        use crate::model::z_group::{StackGroup, tile_depth};
        let deepest = tile_depth(None, false, StackGroup::Floating, StackGroup::Strip);
        let shallowest = tile_depth(Some(0), true, StackGroup::Strip, StackGroup::Strip);
        assert!(-(deepest as f64) > BACKDROP_Z, "the deepest tile clears the backdrop");
        assert!(-(shallowest as f64) < BAR_Z, "the shallowest tile stays under the bar");
        // The shadow caster sits half a step behind its picture and must clear the backdrop too.
        assert!(-(deepest as f64) - 0.5 > BACKDROP_Z);
    }

    #[test]
    fn the_bar_is_drawn_over_every_tile_and_the_desktop_under_them() {
        let deepest_strip_tile = -64.0;
        assert!(BAR_Z > 0.0);
        assert!(BACKDROP_Z < deepest_strip_tile, "the desktop is under every tile");
    }

    /// The mask has to leave room for the shadow on every side, with the window's own shape sitting exactly
    /// one reach inside it, because that inset is where the ring's hole is punched.
    #[test]
    fn the_shadow_mask_surrounds_the_window_by_one_reach() {
        let size = CGSize::new(859.0, 1081.0);
        let frame = shadow_mask_frame(size);
        assert_eq!(frame.origin.x, -SHADOW_REACH);
        assert_eq!(frame.origin.y, -SHADOW_REACH);
        assert_eq!(frame.size.width, 859.0 + 2.0 * SHADOW_REACH);
        assert_eq!(frame.size.height, 1081.0 + 2.0 * SHADOW_REACH);
    }

    /// The reach has to outrun the blur, or the ring clips the shadow before it has faded and leaves a
    /// visible straight edge in it. Three radii is where a gaussian ramp is visually spent.
    #[test]
    fn the_mask_reaches_further_than_either_shadow_does() {
        for style in [UNFOCUSED_SHADOW, FOCUSED_SHADOW] {
            assert!(SHADOW_REACH > style.radius * 3.0 + style.offset_y);
        }
    }

    #[test]
    fn the_shadows_fall_downward() {
        // The tiles hang off a flipped view, so a positive y offset is down the screen. Getting the sign
        // wrong lights the window from below, which reads as wrong without being obviously wrong.
        assert!(UNFOCUSED_SHADOW.offset_y > 0.0);
        assert!(FOCUSED_SHADOW.offset_y > 0.0);
    }

    /// Focus dresses a shadow up, never down: darker, softer-edged, and deeper below the window,
    /// which is the measured relationship between the two real shadows (1.65x edge darkness, 1.5x
    /// falloff length). A focused style lighter in any dimension means the constants were swapped.
    #[test]
    fn focus_deepens_the_shadow_in_every_dimension() {
        assert!(FOCUSED_SHADOW.opacity > UNFOCUSED_SHADOW.opacity);
        assert!(FOCUSED_SHADOW.radius > UNFOCUSED_SHADOW.radius);
        assert!(FOCUSED_SHADOW.offset_y > UNFOCUSED_SHADOW.offset_y);
    }

    #[test]
    fn focus_picks_the_deep_style() {
        assert_eq!(tile_shadow_style(true), FOCUSED_SHADOW);
        assert_eq!(tile_shadow_style(false), UNFOCUSED_SHADOW);
    }

    #[test]
    fn the_bar_is_drawn_at_the_strip_it_occupies() {
        // Measured strip and capture on this machine: the bar spans the display, 32pt tall.
        let placed = bar_frame(rect(0.0, 0.0, 1728.0, 32.0), CGSize::new(1728.0, 32.0));
        assert_eq!(placed, rect(0.0, 0.0, 1728.0, 32.0));
    }

    #[test]
    fn a_bar_that_does_not_start_at_the_corner_is_drawn_where_it_is() {
        // Drawing this at the overlay's corner instead is what put a second bar on screen: a composite
        // covers only the union of the bar's windows, which starts wherever its leftmost item does.
        let placed = bar_frame(rect(217.0, 8.0, 1504.0, 24.0), CGSize::new(1504.0, 24.0));
        assert_eq!(placed.origin.x, 217.0);
        assert_eq!(placed.origin.y, 8.0);
    }

    #[test]
    fn the_bar_picture_is_never_stretched_to_the_strip() {
        // The strip is measured now and the picture was captured earlier, so the two disagree whenever
        // an item has appeared or gone. Stretching to fit smears the whole bar; drawing it at its own
        // size leaves a gap at one end, which the backdrop already fills.
        let placed = bar_frame(rect(0.0, 0.0, 1728.0, 32.0), CGSize::new(1504.0, 32.0));
        assert_eq!(placed.size.width, 1504.0);
        assert_eq!(placed.size.height, 32.0);
    }

    #[test]
    fn easing_clamps_out_of_range_input() {
        // A time-based driver can hand over t slightly outside 0..1 when a frame is late, and an
        // unclamped cubic would overshoot the target position.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
    }

    /// T3 (bugfix.md 1.5). An entrance is dressed at zero width; if that wears fewer pieces than
    /// the final size, the first fresh harvest rebuilds the ring at final geometry mid-flight and
    /// the hairline leaves the picture's timeline. Written to fail on unfixed code.
    #[test]
    fn the_dressing_piece_set_does_not_depend_on_the_tile_size() {
        use crate::ui::edge_dressing::EdgeDressing;
        use crate::ui::window_snapshot::test_bitmap;
        let piece = || Some(test_bitmap());
        let dressing = EdgeDressing {
            strips: [piece(), piece(), piece(), piece()],
            corners: [piece(), piece(), piece(), piece()],
        };
        let at_zero = dressing_piece_indices(&dressing, CGSize::new(0.0, 1081.0));
        let at_full = dressing_piece_indices(&dressing, CGSize::new(859.0, 1081.0));
        assert_eq!(
            at_zero, at_full,
            "a tile dressed at (0, h) wears {} pieces, at (w, h) {}",
            at_zero.len(),
            at_full.len()
        );
    }

    fn full_dressing() -> crate::ui::edge_dressing::EdgeDressing {
        use crate::ui::window_snapshot::test_bitmap;
        let piece = || Some(test_bitmap());
        crate::ui::edge_dressing::EdgeDressing {
            strips: [piece(), piece(), piece(), piece()],
            corners: [piece(), piece(), piece(), piece()],
        }
    }

    #[test]
    fn a_full_dressing_is_worn_whole_at_zero_one_and_full_width() {
        let dressing = full_dressing();
        let all: Vec<usize> = (0..8).collect();
        for w in [0.0, 1.0, 859.0] {
            assert_eq!(
                dressing_piece_indices(&dressing, CGSize::new(w, 1081.0)),
                all,
                "width {w}"
            );
        }
    }

    #[test]
    fn an_unharvested_piece_still_gets_no_layer() {
        let mut dressing = full_dressing();
        dressing.strips[1] = None;
        dressing.corners[3] = None;
        assert_eq!(
            dressing_piece_indices(&dressing, CGSize::new(0.0, 1081.0)),
            vec![0, 2, 3, 4, 5, 6]
        );
    }

    /// Property: for random sizes with h > 0, the piece set is constant.
    #[test]
    fn the_piece_set_is_constant_over_random_sizes() {
        let dressing = full_dressing();
        let reference = dressing_piece_indices(&dressing, CGSize::new(859.0, 1081.0));
        assert_eq!(reference.len(), 8);
        // Seeded LCG, same constants as `workspace_animation::tests::preservation::Gen`.
        let mut state: u64 = 0x5eed_d2e5_5100;
        let mut next = move |n: u64| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 11) % n
        };
        for _ in 0..500 {
            // Width from zero (an entrance's first frame) up to a full display.
            let w = next(1729) as f64;
            let h = 1.0 + next(1081) as f64;
            let size = CGSize::new(w, h);
            assert_eq!(dressing_piece_indices(&dressing, size), reference, "size {size:?}");
        }
    }
}