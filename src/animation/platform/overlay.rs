//! The animation overlay: one rini-owned opaque window holding a picture of every animating window.
//! Real windows are placed at their final frames once, underneath it; tiles ride containers, one
//! per rigid piece of the flight. See "The overlay engine" in `src/animation/docs/animation-smoothness.md` and
//! the overlay's own measurements in `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSCopying, NSNumber, NSString, NSValue};
use objc2_quartz_core::{
    CAAnimation, CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming,
    CAMediaTimingFunction, CATransaction, kCAMediaTimingFunctionLinear,
};

use crate::animation::domain::motion::bounce::{Bounce, container_bounce};
use crate::animation::domain::motion::easing::{CubicBezier, MOTION_CURVE};
use crate::animation::domain::motion::glide::{Leg, spring_omega};
use crate::animation::domain::motion::hairline::{
    OUTLINE_ALPHA, OUTLINE_PT, RING_PT, corner_radius, outline, ring_alpha,
};
use crate::animation::domain::motion::holes::hole_in;
use crate::animation::domain::motion::plan::{
    AnimationTarget, Banding, FlightPlan, GroupKey, Member, PlanDelta, Presented,
    animation_targets, carries_out_and_back, group_relative, stale_overlay_layers,
};
use crate::animation::domain::motion::nudge::nudge_samples;
use crate::animation::domain::motion::score::{Curve, Value};
use crate::animation::domain::motion::tile::{ContentMode, content_mode, crop_pieces};
use crate::animation::domain::motion::z_group::{Band, container_z};
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::screen::primary_display_height;
use crate::pictures::domain::region::{WHOLE, within};
use crate::pictures::platform::layer::{set_layer_contents, stretch_region};
use crate::pictures::platform::picture::Picture;
use rini_core::ids::WindowId;
use rini_geometry::{Round, SameAs};

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

/// One window's picture inside the overlay, and where it should be drawn.
#[derive(Clone)]
pub struct OverlayTile {
    pub window: WindowId,
    /// Where the window starts, in the overlay's coordinate space.
    pub from: CGRect,
    /// Where the window ends up, in the overlay's coordinate space.
    pub to: CGRect,
    pub snapshot: Picture,
    /// In the other z-order band from the tiled windows. See `crate::animation::domain::motion::z_group`.
    pub floating: bool,
    /// The window server's front-to-back position, 0 frontmost; `None` when unreported.
    pub server_order: Option<usize>,
    /// Front-to-back position in the overlay, 0 frontmost; derived once per flight by the engine.
    pub depth: usize,
    /// For a border window riding the window it traces, that window: a quarter step in front of it,
    /// no shadow.
    pub companion: Option<WindowId>,
    /// Whether this window holds (or is about to hold) focus, which deepens its shadow and
    /// brightens its ring.
    pub focused: bool,
    /// The window's corner radius in points, as `pictures` measured it; `None` until it is.
    pub radius: Option<f64>,
}

impl crate::animation::domain::motion::surface::TileGeometry for OverlayTile {
    fn window(&self) -> WindowId {
        self.window
    }
    fn from(&self) -> CGRect {
        self.from
    }
    fn to(&self) -> CGRect {
        self.to
    }
    fn floating(&self) -> bool {
        self.floating
    }
    fn companion(&self) -> Option<WindowId> {
        self.companion
    }
}

impl OverlayTile {
    /// The zPosition this tile draws at; a companion's quarter step stays clear of the half step
    /// the shadow casters sit behind and of the eighth its ring sits in front (`RING_Z`).
    pub(crate) fn z(&self) -> f64 {
        -(self.depth as f64) + if self.companion.is_some() { 0.25 } else { 0.0 }
    }
}

/// How far in front of its picture a tile's ring is drawn: over the picture, under a companion.
const RING_Z: f64 = 0.125;

/// The shadow a real window casts, which no capture API includes. Fitted to measured falloffs;
/// see "Shadows are never in the surface" in `src/animation/docs/capture-overlay-research.md`.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ShadowStyle {
    opacity: f32,
    radius: f64,
    /// Positive is downward: the tiles hang off a flipped view.
    offset_y: f64,
}

const UNFOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.4,
    radius: 9.0,
    offset_y: 5.0,
};
const FOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.65,
    radius: 14.0,
    offset_y: 12.0,
};

fn tile_shadow_style(focused: bool) -> ShadowStyle {
    if focused {
        FOCUSED_SHADOW
    } else {
        UNFOCUSED_SHADOW
    }
}

/// Room the mask leaves for the shadow: past where the focused blur (three radii plus the offset,
/// about 54pt) is spent, so the ring never clips it.
const SHADOW_REACH: f64 = 70.0;

/// Above every tile; tiles sit at `-depth`, never above zero.
const BAR_Z: f64 = 10_000.0;

/// Below the deepest tile the depth model can produce. See "The overlay engine" in
/// `src/animation/docs/animation-smoothness.md`.
const BACKDROP_Z: f64 =
    -((crate::animation::domain::motion::z_group::MAX_TILE_DEPTH + 1024) as f64);

/// The window server places real windows on whole points; a layer at a fraction is resampled and
/// pops at the lift. See "Whole points" in `src/animation/docs/animation-smoothness.md`.
fn whole(rect: CGRect) -> CGRect {
    rect.round()
}

fn whole_point(point: CGPoint) -> CGPoint {
    point.round()
}

/// Commits and flushes the run loop's implicit transaction too, or the change waits behind the
/// reactor's next synchronous calls. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
fn commit_now() {
    CATransaction::commit();
    CATransaction::flush();
}

/// One key per tile movement, so a retarget replaces the animation in flight instead of stacking.
const TILE_ANIMATION_KEY: &str = "rini.tile.move";

/// One key per container movement, for the same reason.
const GROUP_ANIMATION_KEY: &str = "rini.group.move";

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

/// An explicit begin on the media clock, a length and a curve. Every animation of one leg shares
/// one `Timing`, so a leg re-installed with it continues on the same curve instead of restarting.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Timing {
    begin: f64,
    seconds: f64,
    curve: CubicBezier,
}

impl Timing {
    fn starting_now(duration: Duration) -> Self {
        Timing {
            begin: objc2_quartz_core::CACurrentMediaTime(),
            seconds: duration.as_secs_f64(),
            curve: MOTION_CURVE,
        }
    }

    fn apply(&self, animation: &CABasicAnimation) {
        animation.setTimingFunction(Some(&timing_function(self.curve)));
        animation.setDuration(self.seconds);
        animation.setBeginTime(self.begin);
    }

    /// Whether the leg is still playing at `now` on the media clock.
    fn plays_at(&self, now: f64) -> bool {
        self.begin <= now && now < self.begin + self.seconds
    }
}

/// A position animation in layer coordinates. The caller sets the model to the destination; this
/// carries the presentation there and is removed on completion, revealing the model value.
fn position_animation(from: CGPoint, to: CGPoint, timing: Timing) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: an NSValue holding a CGPoint is the value type Core Animation expects for "position".
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithPoint(from)));
        animation.setToValue(Some(&NSValue::valueWithPoint(to)));
    }
    timing.apply(&animation);
    animation
}

/// Keyframe spacing for a leg Core Animation cannot express as one curve: a 120Hz frame.
const LEG_STEP: f64 = 1.0 / 120.0;

/// A container's leg as evenly spaced keyframes sampled from `Leg` itself, so what the render
/// server draws and what the next retarget reads the velocity off are the same motion.
fn leg_animation(leg: &Leg) -> Retained<CAKeyframeAnimation> {
    let (points, seconds) = leg.samples(LEG_STEP);
    let animation =
        CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: NSValues holding CGPoints are the value type Core Animation expects for "position".
    unsafe {
        let values: Vec<Retained<objc2::runtime::AnyObject>> = points
            .into_iter()
            .map(|p| Retained::into_super(Retained::into_super(NSValue::valueWithPoint(p))))
            .collect();
        animation.setValues(Some(&NSArray::from_retained_slice(&values)));
    }
    animation.setDuration(seconds);
    let begin = match *leg {
        Leg::Curve { begin, .. } | Leg::Spring { begin, .. } => begin,
    };
    animation.setBeginTime(begin);
    animation
}

/// One key per container bounce, separate from the movement's so the two compose.
const BOUNCE_ANIMATION_KEY: &str = "rini.group.bounce";

/// The bounce on the container `key` from `begin`, as `Bounce::track` has it: its keys at their key
/// times and a curve per way, additive so it rides a movement in flight and leaves the model
/// position alone.
fn bounce_animation(bounce: &Bounce, key: GroupKey, begin: f64) -> Retained<CAKeyframeAnimation> {
    let track = bounce.track(key, begin);
    let animation =
        CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: NSValues holding CGPoints are the value type Core Animation expects for "position".
    unsafe {
        let values: Vec<Retained<objc2::runtime::AnyObject>> = track
            .keys()
            .iter()
            .map(|(_, value)| match value {
                Value::Point(p) => *p,
                Value::Rect(_) => unreachable!("a container's track holds points"),
            })
            .map(|p| Retained::into_super(Retained::into_super(NSValue::valueWithPoint(p))))
            .collect();
        animation.setValues(Some(&NSArray::from_retained_slice(&values)));
    }
    let key_times: Vec<Retained<NSNumber>> =
        track.keys().iter().map(|(t, _)| NSNumber::numberWithDouble(*t)).collect();
    animation.setKeyTimes(Some(&NSArray::from_retained_slice(&key_times)));
    let curves: Vec<Retained<CAMediaTimingFunction>> =
        track.curves().iter().map(|curve| curve_function(*curve)).collect();
    animation.setTimingFunctions(Some(&NSArray::from_retained_slice(&curves)));
    animation.setAdditive(track.channel().is_additive());
    animation.setDuration(track.seconds());
    animation.setBeginTime(track.begin());
    animation
}

/// A score segment's curve as the timing function Core Animation runs it on.
fn curve_function(curve: Curve) -> Retained<CAMediaTimingFunction> {
    match curve {
        Curve::Bezier(c) => timing_function(c),
        // SAFETY: Core Animation's own timing function name.
        Curve::Linear => {
            CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionLinear })
        }
    }
}

/// One key for a move's nudge, apart from the bounce's so neither stops the other.
const NUDGE_ANIMATION_KEY: &str = "rini.group.nudge";

/// An additive position animation out to `offset` and back along `nudge_samples`, so it rides a
/// movement in flight and leaves the model position alone.
fn nudge_animation(offset: CGPoint, timing: Timing) -> Retained<CAKeyframeAnimation> {
    let animation =
        CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: NSValues holding CGPoints are the value type Core Animation expects for "position".
    unsafe {
        let values: Vec<Retained<objc2::runtime::AnyObject>> =
            nudge_samples(offset, timing.seconds, LEG_STEP)
                .into_iter()
                .map(|p| Retained::into_super(Retained::into_super(NSValue::valueWithPoint(p))))
                .collect();
        animation.setValues(Some(&NSArray::from_retained_slice(&values)));
    }
    animation.setAdditive(true);
    animation.setDuration(timing.seconds);
    animation.setBeginTime(timing.begin);
    animation
}

/// The accents a container's movement carries, each under its own key.
const ACCENT_KEYS: [&str; 2] = [BOUNCE_ANIMATION_KEY, NUDGE_ANIMATION_KEY];

/// When an animation plays on the media clock: from `begin`, `offset` into its own time, for
/// `active` seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Playing {
    begin: f64,
    offset: f64,
    active: f64,
}

impl Playing {
    fn of(animation: &CAAnimation) -> Self {
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

    fn apply(self, animation: &CAAnimation) {
        animation.setBeginTime(self.begin);
        animation.setTimeOffset(self.offset);
        animation.setRepeatDuration(self.active);
    }

    /// The same animation begun again at `at`, as far into its own time as it has got, for what is
    /// left of it. None when it begins after `at` already, or has ended.
    fn again_at(self, at: f64) -> Option<Playing> {
        let ends = self.begin + self.active;
        (self.begin <= at && at < ends).then(|| Playing {
            begin: at,
            offset: self.offset + (at - self.begin),
            active: ends - at,
        })
    }
}

/// Gives a container `movement` under the movement's key, and begins its accents again with it so
/// they go on where they were. Core Animation composes a layer's animations in order of their begin
/// times, ties in the order added, and a non-additive movement hides every additive one before it
/// until it ends; see "The score" in `src/animation/docs/animation-smoothness.md`.
fn add_movement(layer: &CALayer, movement: &CAAnimation) {
    layer.addAnimation_forKey(movement, Some(&NSString::from_str(GROUP_ANIMATION_KEY)));
    let begin = movement.beginTime();
    for key in ACCENT_KEYS {
        let key = NSString::from_str(key);
        // SAFETY: the layer's own animation, read and copied; the copy is what changes.
        let Some(accent) = (unsafe { layer.animationForKey(&key) }) else {
            continue;
        };
        let Some(again) = Playing::of(&accent).again_at(begin) else {
            continue;
        };
        let copy = accent.copy();
        again.apply(&copy);
        layer.addAnimation_forKey(&copy, Some(&key));
    }
}

/// An animation between two rect-valued endpoints of `key_path` ("bounds", "contentsRect").
fn rect_animation(
    key_path: &str,
    from: CGRect,
    to: CGRect,
    timing: Timing,
) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: an NSValue holding a CGRect is the value type Core Animation expects here.
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithRect(from)));
        animation.setToValue(Some(&NSValue::valueWithRect(to)));
    }
    timing.apply(&animation);
    animation
}

/// An animation between two number-valued endpoints of `key_path` ("cornerRadius").
fn number_animation(
    key_path: &str,
    from: f64,
    to: f64,
    timing: Timing,
) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: an NSNumber is the value type Core Animation expects for a scalar key path.
    unsafe {
        animation.setFromValue(Some(&NSNumber::numberWithDouble(from)));
        animation.setToValue(Some(&NSNumber::numberWithDouble(to)));
    }
    timing.apply(&animation);
    animation
}

/// An animation between two path-valued endpoints ("shadowPath", "path"). Core Animation only
/// interpolates paths with matching element structure; both endpoints must use one constructor.
fn path_animation(
    key_path: &str,
    from: &objc2_core_graphics::CGPath,
    to: &objc2_core_graphics::CGPath,
    timing: Timing,
) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // As objects: a `CGPath` pointer handed to `msg_send!` encodes as a struct pointer, which objc2
    // refuses for `setFromValue:` in any build with debug assertions.
    let (from, to): (&objc2_core_foundation::CFType, &objc2_core_foundation::CFType) = (from, to);
    // SAFETY: a CGPath is the value type Core Animation expects for path-valued key paths, and
    // the animation retains what it is given.
    unsafe {
        animation.setFromValue(Some(from.as_ref()));
        animation.setToValue(Some(to.as_ref()));
    }
    timing.apply(&animation);
    animation
}

/// Installs position-and-bounds animations carrying `layer` between two frames. Anchor points are
/// (0,0) throughout, so position is the frame origin.
fn animate_layer_frame(
    layer: &CALayer,
    from: CGRect,
    to: CGRect,
    timing: Timing,
    key_prefix: &str,
) {
    let position = position_animation(from.origin, to.origin, timing);
    layer.addAnimation_forKey(
        &position,
        Some(&NSString::from_str(&format!("{key_prefix}.move"))),
    );
    if from.size != to.size {
        let bounds = rect_animation(
            "bounds",
            CGRect::new(CGPoint::new(0.0, 0.0), from.size),
            CGRect::new(CGPoint::new(0.0, 0.0), to.size),
            timing,
        );
        layer
            .addAnimation_forKey(&bounds, Some(&NSString::from_str(&format!("{key_prefix}.size"))));
    }
}

/// One window's picture, a caster behind it carrying only the shadow, masked to a ring outside
/// the tile, and the window's hairline in front of it. See "A layer shadow covers the whole layer"
/// in `src/animation/docs/capture-overlay-research.md` and "The hairline" in
/// `src/animation/docs/animation-smoothness.md`.
struct Tile {
    picture: Retained<CALayer>,
    shadow: Retained<CALayer>,
    /// Persistent, so a resize can animate its path; a swapped-in mask cannot be animated.
    shadow_mask: Retained<objc2_quartz_core::CAShapeLayer>,
    /// The hairline's light ring, a border at the tile's frame. Beside the picture rather than in
    /// it, so a corner clip (`clip_corners`) leaves the outline outside the window whole.
    ring: Retained<CALayer>,
    /// The hairline's dark outline, a stroke just outside the ring, as the ring's child.
    outline: Retained<objc2_quartz_core::CAShapeLayer>,
    /// The window's measured corner radius, which the ring and outline round to.
    radius: Option<f64>,
    /// The four crop pieces, created on first crop-drawn animation and pooled with the tile.
    crop_grid: Option<CropGrid>,
    /// Size in points of the picture the crop pieces show; `None` when stretching.
    crop_of: Option<CGSize>,
    /// The part of the contents image that is the tile's picture, in unit coordinates, which the
    /// crop pieces' contents are composed with: all of it unless the picture is a region of a
    /// composite.
    region: CGRect,
    /// The leg the tile was last given, a move or a resize, so a picture swap or a snap
    /// re-installs it on the same timing.
    leg: Option<(CGRect, CGRect, Timing)>,
    /// The container the layers hang under; layer frames are in its space.
    key: Option<GroupKey>,
    companion: bool,
}

/// The four layers a crop-drawn tile is composed of, children of the tile's picture layer.
struct CropGrid {
    pieces: [Retained<CALayer>; 4],
}

pub struct TileOverlay {
    window: Retained<NSWindow>,
    /// The layer-backed content view's layer, so AppKit presents it on the GPU.
    root: Retained<CALayer>,
    /// One layer per rigid piece of a flight; a container's `position` is the only animated
    /// translation its members get.
    containers: HashMap<GroupKey, Retained<CALayer>>,
    /// Each moving container's current leg, so a new destination continues it rather than restarting.
    legs: HashMap<GroupKey, Leg>,
    /// The floating container's twin, drawn in front of the strip: where the floating windows that come
    /// forward with the focus are drawn (`Banding::lifted`).
    ///
    /// A twin rather than a second plan group because it is the same rigid piece — every floating window
    /// rides one translation — and only its depth differs. Tiles are children of their container, and a
    /// container's zPosition alone decides its children's order against every other container, so one
    /// floating container could never put one application in front of the strip and another behind it.
    /// Created, reset, moved, animated and dropped with the floating container, so a tile moved between
    /// the two lands exactly where it was.
    lifted: Option<Retained<CALayer>>,
    /// The real desktop, drawn behind everything and held still.
    backdrop: Retained<CALayer>,
    /// Whether the backdrop holds a picture of the display the overlay is on: until then a desktop
    /// captured without its wallpaper beats black.
    backdrop_drawn: bool,
    /// The bar, redrawn on top with its own alpha since the overlay covers the real one. See "The
    /// bar has to be captured on its own" in `src/pictures/docs/capture-research.md`.
    bar: Retained<CALayer>,
    /// Whether the bar has ever been drawn, so a skipped capture keeps it rather than hiding it.
    bar_drawn: bool,
    tile_layers: HashMap<WindowId, Tile>,
    /// Display frame in CoreGraphics coordinates. Full display: `root` draws in these coordinates,
    /// and the window is the part below `top_band`.
    frame: CGRect,
    /// Points reserved at the top for the bar, so the window starts below it.
    top_band: f64,
    scale: f64,
    visible: bool,
    mtm: MainThreadMarker,
}

/// The overlay window's rect: the display below the bar's band. In CoreGraphics coordinates, y grows
/// downward, so the top band is removed by moving the origin down and shrinking the height.
fn window_rect(frame: CGRect, top_band: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.origin.x, frame.origin.y + top_band),
        CGSize::new(frame.size.width, (frame.size.height - top_band).max(0.0)),
    )
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
    /// full bounds in CoreGraphics coordinates, and `top_band` the points the bar reserves at the
    /// top. See "Level and coverage" in `src/animation/docs/capture-overlay-research.md`.
    pub fn new(frame: CGRect, top_band: f64, scale: f64, mtm: MainThreadMarker) -> Option<Self> {
        let converter = CoordinateConverter::from_height(primary_display_height());
        let cocoa_frame = converter.convert_rect(window_rect(frame, top_band))?;

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
        // opaque flag is a promise to the window server that nothing behind this window contributes to
        // what is composited — and since the overlay stays ordered in at alpha 0 between animations,
        // that promise was being kept for windows ABOVE it as well: the backdrop a
        // blurred surface above it samples was truncated here, so the switcher's blur had nothing to blur.
        // The black comes from the background colour below, which is what fills an undrawn gap; being
        // opaque was never what did that.
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

        let view: Retained<FlippedView> = unsafe {
            objc2::msg_send![
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

        // Not geometryFlipped: that flips a layer's contents too, drawing the desktop upside down.
        let backdrop = CALayer::layer();
        backdrop.setAnchorPoint(CGPoint::new(0.0, 0.0));
        backdrop.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        backdrop.setContentsScale(scale);
        backdrop.setZPosition(BACKDROP_Z);
        root.addSublayer(&backdrop);

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
            backdrop_drawn: false,
            containers: HashMap::new(),
            legs: HashMap::new(),
            lifted: None,
            tile_layers: HashMap::new(),
            frame,
            top_band,
            scale,
            visible: false,
            mtm,
        })
    }

    pub fn frame(&self) -> CGRect {
        self.frame
    }

    /// Draws the bar on top of the moving tiles; `strip` is its rect in the overlay's coordinates.
    /// A failed capture keeps the previous picture rather than hiding the bar.
    pub fn set_bar(&mut self, snapshot: Option<&Picture>, strip: Option<CGRect>) {
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
            None => self.bar.setHidden(!self.bar_drawn),
        }
        commit_now();
    }

    /// Sets the still image drawn behind the moving tiles, at its own covered size so it stays in
    /// register with the real desktop. A failed capture keeps the previous picture.
    pub fn set_backdrop(&mut self, snapshot: Option<&Picture>) {
        let Some(snapshot) = snapshot else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let (covered_w, covered_h) = snapshot.coverage.covered;
        self.backdrop.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(covered_w, covered_h),
        ));
        self.backdrop.setContentsScale(self.scale);
        set_layer_contents(&self.backdrop, snapshot);
        self.backdrop.setHidden(false);
        self.backdrop_drawn = true;
        commit_now();
    }

    pub fn backdrop_drawn(&self) -> bool { self.backdrop_drawn }

    /// Repoints the overlay at a different display, a resolution change, or a change of the band the
    /// bar reserves at the top, and puts its window back wherever macOS has moved it.
    pub fn set_frame(&mut self, frame: CGRect, top_band: f64, scale: f64) {
        let converter = CoordinateConverter::from_height(primary_display_height());
        if let Some(cocoa) = converter.reframe(window_rect(frame, top_band), self.window.frame()) {
            self.window.setFrame_display(cocoa, false);
        }
        if self.frame == frame
            && self.top_band == top_band
            && (self.scale - scale).abs() < f64::EPSILON
        {
            return;
        }
        self.clear_holes();
        self.frame = frame;
        self.top_band = top_band;
        self.scale = scale;
        self.backdrop_drawn = false;
        if let Some(view) = self.window.contentView() {
            view.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), window_rect(frame, top_band).size));
        }
        self.root.setFrame(canvas_rect(frame, top_band));
        self.root.setContentsScale(scale);
    }

    /// Swaps a fresh picture into a tile mid-flight, leaving its movement alone. `remaining` is what
    /// is left of the flight, for a crop-drawn tile whose grid has to be re-keyed to the new picture.
    pub fn set_tile_picture(
        &mut self,
        window: WindowId,
        snapshot: &Picture,
        remaining: Option<Duration>,
    ) {
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let mut rekey: Option<(CGRect, CGRect, Timing)> = None;
        if entry.crop_of.is_some() {
            // The new picture is the window at its new size; the grid built against the old one
            // would draw it squashed.
            if let Some(grid) = &entry.crop_grid {
                for piece in &grid.pieces {
                    set_layer_contents(piece, snapshot);
                }
            }
            crop_to(entry, snapshot);
            let final_rect = entry.picture.frame();
            layout_crop_grid(entry, final_rect.size);
            // Re-installed on the same timing when riding a leg, so only the pixels cut.
            if remaining.is_some_and(|left| !left.is_zero()) {
                rekey = Some(match entry.leg {
                    Some((from, to, timing)) => (from, to, timing),
                    None => {
                        // SAFETY: `presentationLayer` returns a read-only copy of the layer.
                        let presented = unsafe { entry.picture.presentationLayer() }
                            .map(|p| CGRect::new(p.position(), p.bounds().size))
                            .unwrap_or(final_rect);
                        (
                            presented,
                            final_rect,
                            Timing::starting_now(remaining.unwrap_or_default()),
                        )
                    }
                });
            }
        } else {
            // A hard cut on purpose: a crossfade pulses a translucent window's net opacity. See
            // "Border companions" in `src/transitions/docs/coordination.md`.
            set_layer_contents(&entry.picture, snapshot);
        }
        if let Some((from, to, timing)) = rekey {
            animate_tile_resize(entry, from, to, timing);
        }
        commit_now();
    }

    /// Clips a tile's picture to a rounded rect of `radius`, for a picture whose corners carry what
    /// was behind its window, as a crop of a composite of the desktop does; `None` draws it whole,
    /// which every install starts from.
    pub fn set_tile_corner_clip(&mut self, window: WindowId, radius: Option<f64>) {
        let Some(entry) = self.tile_layers.get(&window) else {
            return;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        clip_corners(&entry.picture, radius);
        commit_now();
    }

    /// Draws a tile focused or not, a hard cut: its ring's white and its shadow's depth. For focus
    /// handing over mid-flight; every install draws what its tile says.
    pub fn set_tile_focused(&mut self, window: WindowId, focused: bool) {
        let Some(entry) = self.tile_layers.get(&window) else {
            return;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        show_focus(entry, focused);
        commit_now();
    }

    /// Composes a flight at frame zero: one container per group, loose members under `Loose` /
    /// `Floating`. Pre-flight only; see "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
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
        for (key, members) in [
            (GroupKey::Loose, &strip_loose),
            (GroupKey::Floating, &plan.floating),
        ] {
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

    /// Writes every container's and tile's `zPosition` from `banding`. A hard cut: z does not
    /// interpolate. Callers hold the transaction.
    pub(crate) fn rebank(&self, banding: &Banding) {
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
        if let (Some(lifted), Some(behind)) =
            (self.lifted.as_ref(), self.containers.get(&GroupKey::Floating))
        {
            lifted.setZPosition(container_z(Band::Lifted, off_strip));
            for (window, tile) in &self.tile_layers {
                if tile.key != Some(GroupKey::Floating) {
                    continue;
                }
                let parent = if banding.lifted.contains(window) {
                    lifted
                } else {
                    behind
                };
                tile.reparent(parent);
            }
        }
        for (window, tile) in &self.tile_layers {
            let Some(&within) = banding.within.get(window) else {
                continue;
            };
            let z = -(within as f64) + if tile.companion { 0.25 } else { 0.0 };
            tile.stack(z);
        }
    }

    /// The container for `key`, created on first use, put back at the origin with no animation
    /// riding it. Callers hold the transaction.
    fn reset_container(&mut self, key: GroupKey) -> Retained<CALayer> {
        let root = self.root.clone();
        let layer = self.containers.entry(key).or_insert_with(|| new_container(&root)).clone();
        if key == GroupKey::Floating {
            self.lifted.get_or_insert_with(|| new_container(&root));
        }
        for layer in self.layers_of(key) {
            layer.removeAllAnimations();
            layer.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), self.frame.size));
            layer.setPosition(CGPoint::new(0.0, 0.0));
        }
        self.legs.remove(&key);
        layer
    }

    /// Every layer that carries `key`'s translation: its container, and for the floating container its
    /// twin in front of the strip. Anything that moves or resets one moves or resets both.
    fn layers_of(&self, key: GroupKey) -> Vec<Retained<CALayer>> {
        let twin = (key == GroupKey::Floating).then(|| self.lifted.clone()).flatten();
        self.containers.get(&key).cloned().into_iter().chain(twin).collect()
    }

    /// Drops every tile not in `keep`, and every container left with no tile.
    fn remove_stale(&mut self, keep: &[WindowId]) {
        let tiles: Vec<(WindowId, Option<GroupKey>)> =
            self.tile_layers.iter().map(|(window, tile)| (*window, tile.key)).collect();
        let containers: Vec<GroupKey> = self.containers.keys().copied().collect();
        let (stale_tiles, empty_containers) = stale_overlay_layers(&tiles, keep, &containers);

        for window in stale_tiles {
            if let Some(entry) = self.tile_layers.remove(&window) {
                entry.remove();
            }
        }
        for key in empty_containers {
            for layer in self.layers_of(key) {
                layer.removeFromSuperlayer();
            }
            self.containers.remove(&key);
            self.legs.remove(&key);
            if key == GroupKey::Floating {
                self.lifted = None;
            }
        }
    }

    /// Hands a composed flight to Core Animation in one transaction: exactly the movements
    /// `animation_targets` names, every container on `curve`. A zero duration lands everything with
    /// no animation.
    pub(crate) fn fly(&mut self, plan: &FlightPlan, duration: Duration, curve: CubicBezier) {
        let timing = Timing {
            curve,
            ..Timing::starting_now(duration)
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for target in animation_targets(plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    let (from, to) = (whole_point(from), whole_point(to));
                    for layer in self.layers_of(key) {
                        layer.setPosition(to);
                        if !duration.is_zero() {
                            add_movement(&layer, &position_animation(from, to, timing));
                        }
                    }
                    self.record_leg(key, from, to, timing, duration);
                }
                AnimationTarget::Tile { window, from, to } => {
                    let Some(entry) = self.tile_layers.get(&window) else {
                        continue;
                    };
                    let offset = self.container_position(entry.key);
                    let (from, to) = (
                        whole(group_relative(from, offset)),
                        whole(group_relative(to, offset)),
                    );
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

    /// Plays `bounce` on every container that carries it, replacing the bounce playing there, so a
    /// push starts from where that one is drawn. `before` is the overshoot of the bounce it
    /// replaces: a container that did not carry that one is at rest and starts from there. See "Edge
    /// bounce" in `src/animation/docs/animation-smoothness.md`.
    pub fn bounce(&mut self, bounce: &Bounce, before: Option<CGPoint>) {
        if bounce.duration.is_zero() {
            return;
        }
        let begin = objc2_quartz_core::CACurrentMediaTime();
        let key_name = NSString::from_str(BOUNCE_ANIMATION_KEY);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let keys: Vec<GroupKey> = self.containers.keys().copied().collect();
        for key in keys {
            let Some(own) = container_bounce(key, bounce, before) else {
                continue;
            };
            for layer in self.layers_of(key) {
                layer.addAnimation_forKey(&bounce_animation(&own, key, begin), Some(&key_name));
            }
        }
        commit_now();
    }

    /// Takes every container `carries_out_and_back` names out to `offset` and back, additively, on
    /// top of any movement in flight, under the nudge's own key. See "The move flight" in
    /// `src/animation/docs/animation-smoothness.md`.
    pub fn nudge(&mut self, offset: CGPoint, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let timing = Timing::starting_now(duration);
        let key_name = NSString::from_str(NUDGE_ANIMATION_KEY);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let keys: Vec<GroupKey> = self.containers.keys().copied().collect();
        for key in keys {
            if !carries_out_and_back(key, offset) {
                continue;
            }
            for layer in self.layers_of(key) {
                layer.addAnimation_forKey(&nudge_animation(offset, timing), Some(&key_name));
            }
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
        // SAFETY: `presentationLayer` returns a read-only copy of the layer.
        unsafe { layer.presentationLayer() }
            .map(|p| p.position())
            .unwrap_or(layer.position())
    }

    /// Installs one tile at `at`, in the space of the parent `key` names (the root for `None`).
    /// Callers hold the transaction.
    fn install_tile(&mut self, tile: &OverlayTile, at: CGRect, key: Option<GroupKey>) {
        let parent = match key.and_then(|k| self.containers.get(&k)) {
            Some(container) => container.clone(),
            None => self.root.clone(),
        };
        let entry = self.tile_layers.entry(tile.window).or_insert_with(|| new_tile(&parent));
        entry.reparent(&parent);
        entry.key = key;
        entry.companion = tile.companion.is_some();
        entry.radius = tile.radius;
        entry.picture.setContentsScale(self.scale);
        entry.outline.setContentsScale(self.scale);
        clip_corners(&entry.picture, None);
        entry.leg = None;
        let covered = tile.snapshot.coverage.covered;
        let mode = content_mode(covered, tile.from.size, tile.to.size);
        set_tile_content(entry, &tile.snapshot, mode, tile.from.size, self.scale);
        // Tile layers are pooled, so every per-window property is written on each install.
        show_focus(entry, tile.focused);
        place_tile(entry, whole(at), tile.z());
        entry.picture.setHidden(false);
        entry.shadow.setHidden(tile.companion.is_some());
        entry.ring.setHidden(tile.companion.is_some());
    }

    /// Adds one loose tile to a flight in progress under `key`, flying `tile.from` to `tile.to`
    /// (the container's space) over `duration`.
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
        self.animate_tile_movement(
            tile.window,
            whole(tile.from),
            whole(tile.to),
            tile.z(),
            duration,
        );
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
        for layer in self.layers_of(key) {
            layer.setPosition(position);
        }
        layer
    }

    /// Whether every container and picture is presented within half a point of its model: the
    /// render server runs behind the coordinator's clock. See "Real windows land before lift" in `src/transitions/docs/coordination.md`.
    pub fn settled(&self) -> bool {
        let close = |a: CGPoint, b: CGPoint| (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5;
        let same_size = |a: CGSize, b: CGSize| {
            (a.width - b.width).abs() < 0.5 && (a.height - b.height).abs() < 0.5
        };
        // SAFETY: `presentationLayer` returns a read-only copy of the layer.
        let containers = self.containers.values().chain(self.lifted.as_ref()).all(|layer| {
            unsafe { layer.presentationLayer() }
                .is_none_or(|p| close(p.position(), layer.position()))
        });
        let tiles = self.tile_layers.values().all(|tile| {
            unsafe { tile.picture.presentationLayer() }.is_none_or(|p| {
                close(p.position(), tile.picture.position())
                    && same_size(p.bounds().size, tile.picture.bounds().size)
            })
        });
        containers && tiles
    }

    /// Where every container is: where it is drawn, any bounce or nudge riding it included, and
    /// where its own leg has it, or its model position with none. `merge_plans` starts a member
    /// leaving a container from the first and judges everything else against the second.
    pub(crate) fn presented_positions(&self) -> Presented {
        let now = objc2_quartz_core::CACurrentMediaTime();
        Presented {
            drawn: self
                .containers
                .keys()
                .map(|key| (*key, self.presented_container_position(Some(*key))))
                .collect(),
            along: self
                .containers
                .iter()
                .map(|(key, layer)| {
                    let along = self.legs.get(key).map(|leg| leg.position_at(now));
                    (*key, along.unwrap_or_else(|| layer.position()))
                })
                .collect(),
        }
    }

    /// Applies one merged pass to a flight in progress in one transaction. See "Mid-flight passes"
    /// in `src/animation/docs/animation-smoothness.md`.
    pub(crate) fn retarget(
        &mut self,
        delta: &PlanDelta,
        plan: &FlightPlan,
        tiles: &[OverlayTile],
        banding: &Banding,
        duration: Duration,
    ) -> Duration {
        let timing = Timing::starting_now(duration);
        let mut longest = Duration::ZERO;
        let find = |window: WindowId| tiles.iter().find(|t| t.window == window);
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        // Read before any leg is bent below: a container opened for a member leaving another goes
        // on as fast as that one was going.
        let carried: HashMap<GroupKey, Leg> = delta
            .new_groups
            .iter()
            .filter_map(|group| Some((group.key, *self.legs.get(&group.leaving?)?)))
            .collect();
        for new in &delta.new_groups {
            let key = new.key;
            let container = self.reset_container(key);
            container.setPosition(whole_point(new.install));
            let Some(group) = plan.groups.iter().find(|g| g.key == key) else {
                continue;
            };
            for member in &group.members {
                if delta.reparented.iter().any(|(w, _, to)| *w == member.window && *to == key) {
                    continue;
                }
                if let Some(tile) = find(member.window) {
                    self.install_tile(tile, member.rel, Some(key));
                }
            }
        }

        let mut placed: Vec<WindowId> = Vec::new();
        for &(window, _, to_key) in &delta.reparented {
            let Some(frame) = plan.member(window).map(|member| member.start_frame()) else {
                continue;
            };
            let container = self.ensure_container(
                to_key,
                plan.positions.get(&to_key).copied().unwrap_or(CGPoint::new(0.0, 0.0)),
            );
            let Some(entry) = self.tile_layers.get_mut(&window) else {
                continue;
            };
            entry.reparent(&container);
            entry.key = Some(to_key);
            let z = entry.picture.zPosition();
            place_tile(entry, whole(frame), z);
            placed.push(window);
        }

        // The container carries on from its own leg: where it is and as fast as it is going. See "One
        // flight however many presses" in `src/animation/docs/animation-smoothness.md`.
        let now = timing.begin;
        let omega = spring_omega(duration.as_secs_f64());
        for &(key, to) in &delta.retargeted_groups {
            let to = whole_point(to);
            let model = self.containers.get(&key).map(|layer| layer.position()).unwrap_or(to);
            let leg = Leg::toward(self.legs.get(&key), model, to, now, omega);
            for layer in self.layers_of(key) {
                layer.setPosition(to);
                if !duration.is_zero() {
                    add_movement(&layer, &leg_animation(&leg));
                }
            }
            if duration.is_zero() {
                self.legs.remove(&key);
            } else {
                longest = longest.max(Duration::from_secs_f64(leg.samples(LEG_STEP).1));
                self.legs.insert(key, leg);
            }
        }
        for new in &delta.new_groups {
            let key = new.key;
            let to = whole_point(plan.positions.get(&key).copied().unwrap_or(new.install));
            let install = whole_point(new.install);
            if new.leaving.is_some() && !duration.is_zero() && !install.same_as(to) {
                // A member leaving a container mid-flight keeps its speed; see `Leg::leaving`.
                let leg = Leg::leaving(carried.get(&key), install, to, now, omega);
                for layer in self.layers_of(key) {
                    layer.setPosition(to);
                    add_movement(&layer, &leg_animation(&leg));
                }
                longest = longest.max(Duration::from_secs_f64(leg.samples(LEG_STEP).1));
                self.legs.insert(key, leg);
                continue;
            }
            for layer in self.layers_of(key) {
                layer.setPosition(to);
                if !duration.is_zero() && !install.same_as(to) {
                    add_movement(&layer, &position_animation(install, to, timing));
                }
            }
            self.record_leg(key, install, to, timing, duration);
        }

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

        for &(window, to) in &delta.retargeted_tiles {
            let Some(tile) = find(window) else { continue };
            let scale = self.scale;
            let Some(entry) = self.tile_layers.get_mut(&window) else {
                continue;
            };
            let from = if placed.contains(&window) {
                entry.picture.frame()
            } else {
                // SAFETY: `presentationLayer` returns a read-only copy of the layer.
                unsafe { entry.picture.presentationLayer() }
                    .map(|p| CGRect::new(p.position(), p.bounds().size))
                    .unwrap_or_else(|| entry.picture.frame())
            };
            // Content mode is judged against the new leg: a retarget can turn a move into a resize.
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
        longest
    }

    /// Remembers the leg a container was just given, or forgets it when it was placed outright.
    fn record_leg(
        &mut self,
        key: GroupKey,
        from: CGPoint,
        to: CGPoint,
        timing: Timing,
        duration: Duration,
    ) {
        if duration.is_zero() || from.same_as(to) {
            self.legs.remove(&key);
        } else {
            self.legs.insert(
                key,
                Leg::Curve {
                    from,
                    to,
                    begin: timing.begin,
                    seconds: timing.seconds,
                    curve: timing.curve,
                },
            );
        }
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
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        place_tile(entry, to, z);
        animate_tile_leg(entry, from, to, Timing::starting_now(duration));
    }

    /// Moves a tile onto its window's real frame, `real` in the overlay's coordinates, and says
    /// whether the window has a tile to move. A tile riding a leg of its own goes on along it, on the
    /// same timing, to the real frame; one riding its container moves inside it at once. Shadow,
    /// mask, crop pieces and hairline follow. Whether to snap is the caller's, by `snap_to`, once per
    /// window per generation, and is not checked again here. See "Tiles snap to the real frame" in
    /// `src/animation/docs/animation-smoothness.md`.
    pub fn snap_tile(&mut self, window: WindowId, real: CGRect) -> bool {
        let offset = self.tile_layers.get(&window).map(|entry| self.container_position(entry.key));
        let Some((entry, offset)) = self.tile_layers.get_mut(&window).zip(offset) else {
            return false;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        snap_onto(entry, offset, real, objc2_quartz_core::CACurrentMediaTime());
        commit_now();
        true
    }

    /// Shows the overlay; only an alpha change.
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
        self.clear_holes();
        if !self.visible {
            return;
        }
        self.window.setAlphaValue(0.0);
        self.visible = false;
    }

    /// Punches a hole through the overlay over each window ending at one of `ends`, in the
    /// overlay's coordinates, replacing the holes there were, and clears the window's background so
    /// the holes show what is below. The caller names the windows that end wholly on screen. See
    /// "Holes" in `src/animation/docs/animation-smoothness.md`.
    pub fn set_holes(&mut self, ends: &[CGRect]) {
        if ends.is_empty() {
            self.clear_holes();
            return;
        }
        let Some(clip) = self.window.contentView().and_then(|view| view.layer()) else {
            return;
        };
        let holes: Vec<CGRect> =
            ends.iter().map(|end| in_view(hole_in(*end), self.top_band)).collect();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: a shape layer we just made, as the mask of the view's own layer.
        unsafe { clip.setMask(Some(&holes_mask(clip.bounds(), &holes))) };
        commit_now();
        self.window.setBackgroundColor(Some(&NSColor::clearColor()));
    }

    /// Fills the holes in again and puts the window's black background back.
    pub fn clear_holes(&mut self) {
        let Some(clip) = self.window.contentView().and_then(|view| view.layer()) else {
            return;
        };
        if clip.mask().is_none() {
            return;
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: removing the mask this overlay set.
        unsafe { clip.setMask(None) };
        commit_now();
        self.window.setBackgroundColor(Some(&NSColor::blackColor()));
    }

    /// Frees the tile contents without destroying the overlay.
    pub fn release_tiles(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (_, entry) in self.tile_layers.drain() {
            entry.remove();
        }
        for (_, container) in self.containers.drain() {
            container.removeFromSuperlayer();
        }
        self.legs.clear();
        if let Some(lifted) = self.lifted.take() {
            lifted.removeFromSuperlayer();
        }
        commit_now();
        let _ = self.mtm;
    }
}

/// Carries a tile from `from` to `to` on `timing`, its model already at `to`: a plain move, or a
/// resize when the sizes differ by more than a re-fit. Callers hold the transaction.
fn animate_tile_leg(entry: &mut Tile, from: CGRect, to: CGRect, timing: Timing) {
    // Proportional tolerance, not equality: a sub-tolerance re-fit rides the plain move.
    if rini_geometry::stretch::is_a_resize(from.size, to.size) {
        animate_tile_resize(entry, from, to, timing);
        return;
    }
    entry.leg = Some((from, to, timing));
    // addAnimation copies, so one instance serves every layer.
    let animation = position_animation(from.origin, to.origin, timing);
    let key = NSString::from_str(TILE_ANIMATION_KEY);
    for layer in entry.layers() {
        layer.addAnimation_forKey(&animation, Some(&key));
    }
}

/// The resize: every layer of the tile rides its own pair of endpoint geometries on the shared
/// curve. See "Resizes through the overlay" in `src/animation/docs/animation-smoothness.md`.
fn animate_tile_resize(entry: &mut Tile, from: CGRect, to: CGRect, timing: Timing) {
    entry.leg = Some((from, to, timing));

    // Same key prefix as the plain move, or a move retargeted into a resize leaves its old
    // position animation fighting this one.
    for layer in entry.layers() {
        animate_layer_frame(layer, from, to, timing, "rini.tile");
    }

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
    let mask_path = path_animation("path", &ring_path(from.size), &ring_path(to.size), timing);
    entry
        .shadow_mask
        .addAnimation_forKey(&mask_path, Some(&NSString::from_str("rini.tile.mask.path")));

    animate_crop_grid(entry, from.size, to.size, timing);
    animate_hairline(entry, from.size, to.size, timing);
}

/// Moves a tile under a container at `offset` onto `real`, both in the overlay's coordinates; on its
/// leg's own timing while that plays at `now`. Callers hold the transaction.
fn snap_onto(entry: &mut Tile, offset: CGPoint, real: CGRect, now: f64) {
    let to = group_relative(real, offset);
    place_tile(entry, to, entry.picture.zPosition());
    if let Some((from, _, timing)) = entry.leg.filter(|(_, _, timing)| timing.plays_at(now)) {
        animate_tile_leg(entry, from, to, timing);
    }
}

/// The bar's picture at the strip's origin, at the size it covers, never stretched. See "The bar
/// has to be captured on its own" in `src/pictures/docs/capture-research.md`.
fn bar_frame(strip: CGRect, covered: CGSize) -> CGRect {
    CGRect::new(strip.origin, covered)
}

/// A tile: the picture, a caster behind it holding the shadow, and the ring in front holding the
/// outline. No `masksToBounds` on the caster, which would clip its own shadow, or on the ring, which
/// would clip the outline; the picture's contents cannot spill, and it clips only to round its
/// corners (`clip_corners`).
fn new_tile(container: &CALayer) -> Tile {
    let shadow = CALayer::layer();
    shadow.setAnchorPoint(CGPoint::new(0.0, 0.0));
    // The default shadow colour (opaque black) is what a window casts; style is applied at install.
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

    let ring = CALayer::layer();
    ring.setAnchorPoint(CGPoint::new(0.0, 0.0));
    ring.setBorderWidth(RING_PT);
    let outline = objc2_quartz_core::CAShapeLayer::layer();
    outline.setAnchorPoint(CGPoint::new(0.0, 0.0));
    outline.setFillColor(None);
    outline.setStrokeColor(Some(&objc2_core_graphics::CGColor::new_generic_gray(
        0.0,
        OUTLINE_ALPHA,
    )));
    outline.setLineWidth(OUTLINE_PT);
    ring.addSublayer(&outline);
    container.addSublayer(&ring);

    Tile {
        picture,
        shadow,
        shadow_mask,
        ring,
        outline,
        radius: None,
        crop_grid: None,
        crop_of: None,
        region: WHOLE,
        leg: None,
        key: None,
        companion: false,
    }
}

impl Tile {
    /// The layers that ride the tile's frame: the picture, the caster and the ring.
    fn layers(&self) -> [&CALayer; 3] {
        [&self.picture, &self.shadow, &self.ring]
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

    /// Draws the tile at depth `z`: the caster half a step behind the picture, the ring
    /// `RING_Z` in front of it.
    fn stack(&self, z: f64) {
        self.picture.setZPosition(z);
        self.shadow.setZPosition(z - 0.5);
        self.ring.setZPosition(z + RING_Z);
    }
}

/// Draws a tile focused or not: the ring's white and the caster's shadow.
fn show_focus(tile: &Tile, focused: bool) {
    tile.ring.setBorderColor(Some(&objc2_core_graphics::CGColor::new_generic_gray(
        1.0,
        ring_alpha(focused),
    )));
    let style = tile_shadow_style(focused);
    tile.shadow.setShadowOpacity(style.opacity);
    tile.shadow.setShadowRadius(style.radius);
    tile.shadow.setShadowOffset(CGSize::new(0.0, style.offset_y));
}

/// Clips `picture`, with the crop pieces under it, to a rounded rect of `radius`, or not at all.
/// The clip follows the bounds through a resize.
fn clip_corners(picture: &CALayer, radius: Option<f64>) {
    picture.setCornerRadius(radius.unwrap_or(0.0));
    picture.setMasksToBounds(radius.is_some());
}

/// Puts a tile's layers at `frame`, stacked at `z`. The shadow's shape is rebuilt only when the
/// size changes; the hairline is laid after the frame, every time, since a pooled tile's radius can
/// have been measured since.
fn place_tile(tile: &Tile, frame: CGRect, z: f64) {
    let resized = tile.shadow.bounds().size != frame.size;
    for layer in tile.layers() {
        layer.setFrame(frame);
    }
    tile.stack(z);
    if resized {
        set_tile_shadow(&tile.shadow, &tile.shadow_mask, frame.size);
        layout_crop_grid(tile, frame.size);
    }
    lay_hairline(tile, frame.size);
}

/// Rounds the ring and draws the outline for a tile of `size`, in the ring's coordinates.
fn lay_hairline(tile: &Tile, size: CGSize) {
    let corner = corner_radius(tile.radius, size);
    tile.ring.setCornerRadius(corner);
    tile.outline.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size));
    tile.outline.setPath(Some(&outline_path(size, corner)));
}

/// Carries a tile's ring and outline between sizes on `timing`, the frame's own: the ring's
/// bounds ride the tile's (`animate_tile_resize`), its corner and the outline's path ride here.
fn animate_hairline(tile: &Tile, from: CGSize, to: CGSize, timing: Timing) {
    let (from_corner, to_corner) =
        (corner_radius(tile.radius, from), corner_radius(tile.radius, to));
    if from_corner != to_corner {
        let corner = number_animation("cornerRadius", from_corner, to_corner, timing);
        tile.ring
            .addAnimation_forKey(&corner, Some(&NSString::from_str("rini.tile.ring.corner")));
    }
    let path = path_animation(
        "path",
        &outline_path(from, from_corner),
        &outline_path(to, to_corner),
        timing,
    );
    tile.outline
        .addAnimation_forKey(&path, Some(&NSString::from_str("rini.tile.outline.path")));
}

/// The outline's path around a window of `size` with corners of `corner`. Its radius is never zero,
/// so it is always a rounded rect and two outline paths interpolate.
fn outline_path(size: CGSize, corner: f64) -> CFRetained<objc2_core_graphics::CGPath> {
    let (rect, radius) = outline(size, corner);
    // SAFETY: a null transform means the path is taken as given.
    unsafe {
        objc2_core_graphics::CGPath::with_rounded_rect(rect, radius, radius, std::ptr::null())
    }
}

/// The window's rounded-rect silhouette: the shadow's shape and the mask's hole. Size and radius
/// are floored above zero, or Core Graphics emits a plain rect and path animations cut.
fn silhouette_path(size: CGSize, at: CGPoint) -> CFRetained<objc2_core_graphics::CGPath> {
    let size = CGSize::new(size.width.max(0.5), size.height.max(0.5));
    let radius = corner_radius(None, size).max(0.01);
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

/// The ring: the whole mask minus the window's shape, wound even-odd so the inside is the hole.
/// Always one rect plus one rounded rect, so two ring paths interpolate.
fn ring_path(size: CGSize) -> CFRetained<objc2_core_graphics::CGMutablePath> {
    // Floored for the same reason as `silhouette_path`.
    let size = CGSize::new(size.width.max(0.5), size.height.max(0.5));
    let radius = corner_radius(None, size).max(0.01);
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

/// Gives the caster the shadow of a window of `size`, clipped to a ring outside it. See "A layer
/// shadow covers the whole layer" in `src/animation/docs/capture-overlay-research.md`.
fn set_tile_shadow(layer: &CALayer, mask: &objc2_quartz_core::CAShapeLayer, size: CGSize) {
    layer.setShadowPath(Some(&silhouette_path(size, CGPoint::new(0.0, 0.0))));
    mask.setFrame(shadow_mask_frame(size));
    mask.setPath(Some(&ring_path(size)));
}

/// The shadow mask's rect in the caster's coordinates: the window inset by `SHADOW_REACH` on
/// every side.
fn shadow_mask_frame(size: CGSize) -> CGRect {
    CGRect::new(
        CGPoint::new(-SHADOW_REACH, -SHADOW_REACH),
        CGSize::new(size.width + 2.0 * SHADOW_REACH, size.height + 2.0 * SHADOW_REACH),
    )
}

/// Points a tile's contents at `mode`. Always set explicitly: pooled tile layers would otherwise
/// carry a crop into the next flight's stretch.
fn set_tile_content(
    entry: &mut Tile,
    snapshot: &Picture,
    mode: ContentMode,
    frame: CGSize,
    scale: f64,
) {
    match mode {
        ContentMode::Crop => {
            unsafe {
                let _: () = msg_send![&*entry.picture, setContents: std::ptr::null::<NSObject>()];
            }
            let grid = entry.crop_grid.get_or_insert_with(|| new_crop_grid(&entry.picture));
            for piece in &grid.pieces {
                piece.setContentsScale(scale);
                piece.setHidden(false);
                set_layer_contents(piece, snapshot);
            }
            crop_to(entry, snapshot);
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
    // Written on every install because tile layers are pooled: a layer that last held a stand-in must
    // not stretch the next window's picture nine-slice.
    entry.picture.setContentsCenter(stretch_region(snapshot));
}

/// Points a crop-drawn tile at `snapshot`: the size its pieces cut, and the part of the contents
/// image that is the picture.
fn crop_to(entry: &mut Tile, snapshot: &Picture) {
    let covered = snapshot.coverage.covered;
    entry.crop_of = Some(CGSize::new(covered.0, covered.1));
    entry.region = snapshot.image.unit_rect();
}

/// Lays the crop grid out for the tile's current frame size.
fn layout_crop_grid(entry: &Tile, frame: CGSize) {
    let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) else {
        return;
    };
    for (layer, piece) in grid.pieces.iter().zip(crop_pieces(picture, frame)) {
        layer.setFrame(piece.frame);
        layer.setContentsRect(within(entry.region, piece.contents));
    }
}

/// Carries a crop-drawn tile's pieces between the grids of two frame sizes on `timing`.
fn animate_crop_grid(entry: &Tile, from: CGSize, to: CGSize, timing: Timing) {
    let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) else {
        return;
    };
    let pieces = crop_pieces(picture, from).into_iter().zip(crop_pieces(picture, to));
    for (layer, (a, b)) in grid.pieces.iter().zip(pieces) {
        animate_layer_frame(layer, a.frame, b.frame, timing, "rini.piece");
        let contents = rect_animation(
            "contentsRect",
            within(entry.region, a.contents),
            within(entry.region, b.contents),
            timing,
        );
        layer.addAnimation_forKey(&contents, Some(&NSString::from_str("rini.piece.contents")));
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

/// An empty container covering the overlay, under `root`.
fn new_container(root: &CALayer) -> Retained<CALayer> {
    let layer = CALayer::layer();
    layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
    layer.setMasksToBounds(false);
    root.addSublayer(&layer);
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
    use super::*;
    use crate::animation::domain::motion::easing::EASE_IN_OUT;

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

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn tiles_sit_on_whole_points_like_the_real_windows() {
        let a = rect(4.0 - 4592.6724 + 4592.0, 32.0, 859.0, 1081.0);
        let b = rect(a.origin.x + 861.0, 32.0, 861.0, 1081.0);
        assert_eq!(whole(a).origin.x, 3.0);
        assert_eq!(
            whole(b).origin.x - whole(a).origin.x,
            861.0,
            "one rounding for the strip"
        );
        assert_eq!(whole(a).size, a.size);
        assert_eq!(
            whole_point(CGPoint::new(-861.3276, 0.4)),
            CGPoint::new(-861.0, 0.0)
        );
        assert_eq!(
            whole(rect(10.0, 20.0, 100.0, 50.0)),
            rect(10.0, 20.0, 100.0, 50.0)
        );
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
        // The shadow caster sits half a step behind its picture.
        assert!(-(deepest as f64) - 0.5 > BACKDROP_Z);
    }

    #[test]
    fn the_bar_is_drawn_over_every_tile_and_the_desktop_under_them() {
        let deepest_strip_tile = -64.0;
        assert!(BAR_Z > 0.0);
        assert!(
            BACKDROP_Z < deepest_strip_tile,
            "the desktop is under every tile"
        );
    }

    #[test]
    fn the_shadow_mask_surrounds_the_window_by_one_reach() {
        let size = CGSize::new(859.0, 1081.0);
        let frame = shadow_mask_frame(size);
        assert_eq!(frame.origin.x, -SHADOW_REACH);
        assert_eq!(frame.origin.y, -SHADOW_REACH);
        assert_eq!(frame.size.width, 859.0 + 2.0 * SHADOW_REACH);
        assert_eq!(frame.size.height, 1081.0 + 2.0 * SHADOW_REACH);
    }

    /// Three radii plus the offset is where a gaussian ramp is visually spent.
    #[test]
    fn the_mask_reaches_further_than_either_shadow_does() {
        for style in [UNFOCUSED_SHADOW, FOCUSED_SHADOW] {
            assert!(SHADOW_REACH > style.radius * 3.0 + style.offset_y);
        }
    }

    #[test]
    fn the_shadows_fall_downward() {
        // Flipped view: positive y is down the screen.
        assert!(UNFOCUSED_SHADOW.offset_y > 0.0);
        assert!(FOCUSED_SHADOW.offset_y > 0.0);
    }

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

    /// What `bounce_animation` hands Core Animation is `Bounce::track`, key for key and curve for
    /// curve, from rest and pushed mid-way, so the domain's tests pin what is drawn.
    #[test]
    fn the_bounce_installed_is_its_track() {
        let wall = CGPoint::new(-72.0, 0.0);
        let plays = Duration::from_millis(350);
        let first = Bounce::from_rest(wall, plays);
        for bounce in [first, first.pushed(Duration::from_millis(90), wall, plays)] {
            let track = bounce.track(GroupKey::STILL, 7.0);
            let animation = bounce_animation(&bounce, GroupKey::STILL, 7.0);
            assert_eq!(
                animation.keyPath().map(|k| k.to_string()),
                Some("position".into())
            );
            let values = animation.values().expect("values");
            let drawn: Vec<CGPoint> = values
                .iter()
                .map(|value| {
                    let value = value.downcast_ref::<NSValue>().expect("an NSValue");
                    // SAFETY: every value was made from a point.
                    unsafe { value.pointValue() }
                })
                .collect();
            let key_times: Vec<f64> = animation
                .keyTimes()
                .expect("key times")
                .iter()
                .map(|t| t.doubleValue())
                .collect();
            let keys: Vec<(f64, Value)> =
                key_times.into_iter().zip(drawn.into_iter().map(Value::Point)).collect();
            assert_eq!(keys, track.keys());
            let curves: Vec<[f32; 4]> = animation
                .timingFunctions()
                .expect("timing functions")
                .iter()
                .map(|f| control_points(&f))
                .collect();
            let expected: Vec<[f32; 4]> =
                track.curves().iter().map(|c| bezier_points(*c)).collect();
            assert_eq!(curves, expected);
            assert!(animation.isAdditive());
            assert_eq!(
                (animation.beginTime(), animation.duration()),
                (7.0, track.seconds())
            );
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
        layer
            .animationKeys()
            .map(|keys| keys.iter().map(|key| key.to_string()).collect())
            .unwrap_or_default()
    }

    fn playing(layer: &CALayer, key: &str) -> Playing {
        // SAFETY: a read of the layer's own animation.
        let animation = unsafe { layer.animationForKey(&NSString::from_str(key)) };
        Playing::of(&animation.expect("an animation under the key"))
    }

    /// A container riding its movement from 5.0, a bounce from 5.1 and a nudge from 5.2.
    fn accented_container() -> Retained<CALayer> {
        let layer = CALayer::layer();
        let timing = Timing {
            begin: 5.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let wall = CGPoint::new(-72.0, 0.0);
        let bounce = Bounce::from_rest(wall, Duration::from_millis(350));
        add_movement(&layer, &position_animation(CGPoint::new(0.0, 0.0), wall, timing));
        layer.addAnimation_forKey(
            &bounce_animation(&bounce, GroupKey::STILL, 5.1),
            Some(&NSString::from_str(BOUNCE_ANIMATION_KEY)),
        );
        let nudge = Timing {
            begin: 5.2,
            seconds: 0.455,
            ..timing
        };
        layer.addAnimation_forKey(
            &nudge_animation(CGPoint::new(-576.0, 0.0), nudge),
            Some(&NSString::from_str(NUDGE_ANIMATION_KEY)),
        );
        layer
    }

    fn new_leg(begin: f64) -> Retained<CABasicAnimation> {
        let leg = Timing {
            begin,
            seconds: 0.4,
            curve: MOTION_CURVE,
        };
        position_animation(CGPoint::new(-30.0, 0.0), CGPoint::new(-861.0, 0.0), leg)
    }

    fn close(a: Playing, b: Playing) -> bool {
        (a.begin - b.begin).abs() < 1e-9
            && (a.offset - b.offset).abs() < 1e-9
            && (a.active - b.active).abs() < 1e-9
    }

    /// A retarget gives a container a new leg, which begins after the bounce and the nudge riding
    /// it and so would hide them until it ends. They begin again with it, each as far into its own
    /// time as it had got and for what is left of it, so they go on where they were.
    #[test]
    fn after_a_retarget_a_containers_keys_are_its_movement_then_its_accents() {
        let layer = accented_container();
        add_movement(&layer, &new_leg(5.3));
        assert_eq!(
            animation_keys(&layer),
            [
                GROUP_ANIMATION_KEY,
                BOUNCE_ANIMATION_KEY,
                NUDGE_ANIMATION_KEY
            ]
        );
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

        add_movement(&layer, &new_leg(5.4));
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

    /// What `add_movement` answers: a layer's animations sort by begin time, not by the order they
    /// were added, so a movement added plainly goes after accents that began before it.
    #[test]
    fn core_animation_orders_a_layers_animations_by_begin_time() {
        let layer = accented_container();
        assert_eq!(
            animation_keys(&layer),
            [
                GROUP_ANIMATION_KEY,
                BOUNCE_ANIMATION_KEY,
                NUDGE_ANIMATION_KEY
            ]
        );
        layer.addAnimation_forKey(&new_leg(5.3), Some(&NSString::from_str(GROUP_ANIMATION_KEY)));
        assert_eq!(
            animation_keys(&layer),
            [
                BOUNCE_ANIMATION_KEY,
                NUDGE_ANIMATION_KEY,
                GROUP_ANIMATION_KEY
            ]
        );
        layer.addAnimation_forKey(&new_leg(5.15), Some(&NSString::from_str(GROUP_ANIMATION_KEY)));
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
        let ends = [
            rect(868.0, 32.0, 856.0, 1081.0),
            rect(4.0, 32.0, 856.0, 1081.0),
        ];
        let holes: Vec<CGRect> = ends.iter().map(|end| in_view(hole_in(*end), 32.0)).collect();
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
        let container = CALayer::layer();
        let mut tile = new_tile(&container);
        tile.crop_grid = Some(new_crop_grid(&tile.picture));
        let picture = CGSize::new(856.0, 1081.0);
        tile.crop_of = Some(picture);
        tile.region = rect(0.5, 0.0, 0.5, 1.0);
        let frame = CGSize::new(600.0, 1081.0);
        layout_crop_grid(&tile, frame);
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop_pieces(picture, frame)) {
            assert_eq!(layer.contentsRect(), within(tile.region, piece.contents));
            assert!(
                layer.contentsRect().origin.x >= 0.5,
                "{:?}",
                layer.contentsRect()
            );
        }
        let timing = Timing {
            begin: 5.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let to = CGSize::new(300.0, 1081.0);
        animate_crop_grid(&tile, frame, to, timing);
        let ends = crop_pieces(picture, frame).into_iter().zip(crop_pieces(picture, to));
        for (layer, (a, b)) in grid.pieces.iter().zip(ends) {
            // SAFETY: a read of the layer's own animation.
            let animation =
                unsafe { layer.animationForKey(&NSString::from_str("rini.piece.contents")) }
                    .expect("the contents ride the resize");
            let animation = animation.downcast::<CABasicAnimation>().expect("a basic animation");
            let value = |v: Option<Retained<objc2::runtime::AnyObject>>| {
                let v = v.expect("a value");
                // SAFETY: the value was made from a rect.
                unsafe { v.downcast_ref::<NSValue>().expect("an NSValue").rectValue() }
            };
            assert_eq!(value(animation.fromValue()), within(tile.region, a.contents));
            assert_eq!(value(animation.toValue()), within(tile.region, b.contents));
        }
        tile.region = WHOLE;
        layout_crop_grid(&tile, frame);
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop_pieces(picture, frame)) {
            assert_eq!(
                layer.contentsRect(),
                piece.contents,
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
        let frame = CGSize::new(2.0, 2.0);
        let region = test_region();
        set_tile_content(&mut tile, &region, ContentMode::Crop, frame, 2.0);
        assert_eq!(tile.region, region.image.unit_rect());
        assert_eq!(tile.region, rect(0.5, 0.0, 0.5, 1.0));
        let grid = tile.crop_grid.as_ref().unwrap();
        for (layer, piece) in grid.pieces.iter().zip(crop_pieces(frame, frame)) {
            assert_eq!(layer.contentsRect(), within(tile.region, piece.contents));
        }
        set_tile_content(&mut tile, &test_picture(frame), ContentMode::Crop, frame, 2.0);
        assert_eq!(tile.region, WHOLE);
    }

    /// A baked picture's corners are clipped to the window's radius, and only the picture is: the
    /// shadow caster behind it is never clipped. Without a radius the picture draws whole.
    #[test]
    fn a_tiles_picture_clips_to_its_corners_and_its_shadow_does_not() {
        let container = CALayer::layer();
        let tile = new_tile(&container);
        assert!(!tile.picture.masksToBounds(), "whole by default");
        clip_corners(&tile.picture, Some(12.0));
        assert_eq!(tile.picture.cornerRadius(), 12.0);
        assert!(tile.picture.masksToBounds());
        assert!(!tile.shadow.masksToBounds());
        assert_eq!(tile.shadow.cornerRadius(), 0.0);
        clip_corners(&tile.picture, None);
        assert_eq!(tile.picture.cornerRadius(), 0.0);
        assert!(!tile.picture.masksToBounds());
    }

    /// The ring rides beside the picture, not in it, so a corner clip on the picture leaves the
    /// outline, which lies outside the window, whole.
    #[test]
    fn a_corner_clip_leaves_the_hairline_whole() {
        let container = CALayer::layer();
        let tile = new_tile(&container);
        clip_corners(&tile.picture, Some(12.0));
        let parent = |layer: &CALayer| layer.superlayer().map(|parent| Retained::as_ptr(&parent));
        assert_eq!(parent(&tile.ring), Some(&*container as *const CALayer));
        assert!(!tile.ring.masksToBounds());
        assert_eq!(parent(&tile.outline), Some(&*tile.ring as *const CALayer));
    }

    /// A pooled tile placed at a new size wears its hairline at that size, rounded to its window's
    /// radius, and in front of its picture.
    #[test]
    fn the_hairline_is_laid_at_the_tiles_frame() {
        let mut tile = strip_tile(rect(4.0, 32.0, 1720.0, 1081.0));
        tile.radius = Some(16.0);
        let frame = rect(4.0, 32.0, 859.0, 1081.0);
        place_tile(&tile, frame, -2.0);
        assert_eq!(tile.ring.frame(), frame);
        assert_eq!(tile.ring.borderWidth(), RING_PT);
        assert_eq!(tile.ring.cornerRadius(), 16.0);
        assert_eq!(outline_bounds(&tile), rect(-0.25, -0.25, 859.5, 1081.5));
        assert!(tile.ring.zPosition() > tile.picture.zPosition());
        assert!(tile.ring.zPosition() < tile.picture.zPosition() + 0.25, "under a companion");

        place_tile(&tile, rect(4.0, 32.0, 12.0, 400.0), -2.0);
        assert_eq!(tile.ring.cornerRadius(), 6.0, "clamped to half the shorter side");
    }

    /// An entrance grows from no width: the ring's bounds ride the tile's leg, and its corner and
    /// the outline's path go from the zero-width shape to the slot's on the same timing.
    #[test]
    fn a_resize_carries_the_hairline_on_the_tiles_timing() {
        let timing = Timing {
            begin: 100.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let (from, to) = (rect(4.0, 32.0, 0.0, 1081.0), rect(4.0, 32.0, 859.0, 1081.0));
        let mut tile = strip_tile(to);
        animate_tile_resize(&mut tile, from, to, timing);
        let (start, end, begin) = animation_value(&tile.ring, "rini.tile.size", rect_of);
        assert_eq!((start.size, end.size, begin), (from.size, to.size, 100.0));

        let corner = basic(&tile.ring, "rini.tile.ring.corner");
        let number = |value: Option<Retained<objc2::runtime::AnyObject>>| {
            value.expect("a value").downcast_ref::<NSNumber>().expect("a number").doubleValue()
        };
        assert_eq!(
            (number(corner.fromValue()), number(corner.toValue()), corner.beginTime()),
            (0.0, 10.0, 100.0)
        );

        let path = basic(&tile.outline, "rini.tile.outline.path");
        assert_eq!(path_bounds(path.fromValue()), rect(-0.25, -0.25, 0.5, 1081.5));
        assert_eq!(path_bounds(path.toValue()), rect(-0.25, -0.25, 859.5, 1081.5));
        assert_eq!(path.beginTime(), 100.0);
    }

    /// The ring's white is the focused window's or not, and with it the shadow's depth.
    #[test]
    fn the_rings_white_follows_focus() {
        let tile = strip_tile(rect(4.0, 32.0, 859.0, 1081.0));
        let alpha = |tile: &Tile| {
            objc2_core_graphics::CGColor::alpha(tile.ring.borderColor().as_deref())
        };
        show_focus(&tile, true);
        assert_eq!(alpha(&tile), ring_alpha(true));
        assert_eq!(tile.shadow.shadowOpacity(), FOCUSED_SHADOW.opacity);
        show_focus(&tile, false);
        assert_eq!(alpha(&tile), ring_alpha(false));
        assert_eq!(tile.shadow.shadowOpacity(), UNFOCUSED_SHADOW.opacity);
    }

    /// A resize animates the shadow's silhouette and its mask's ring as paths. Handed over as
    /// pointers they failed objc2's encoding check, so a debug build panicked on the first resize.
    #[test]
    fn a_resize_hands_its_paths_to_core_animation_as_objects() {
        let timing = Timing {
            begin: 100.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let (from, to) = (CGSize::new(1200.0, 1081.0), CGSize::new(861.0, 1081.0));
        let origin = CGPoint::new(0.0, 0.0);
        let animation = path_animation(
            "shadowPath",
            &silhouette_path(from, origin),
            &silhouette_path(to, origin),
            timing,
        );
        assert_eq!(path_bounds(animation.fromValue()).size, from);
        assert_eq!(path_bounds(animation.toValue()).size, to);
    }

    /// The bounding box of a path an animation was given.
    fn path_bounds(value: Option<Retained<objc2::runtime::AnyObject>>) -> CGRect {
        let value = value.expect("a path");
        // SAFETY: the value was made from a CGPath, which is what it is read back as.
        let path: &objc2_core_graphics::CGPath =
            unsafe { &*(Retained::as_ptr(&value) as *const objc2_core_graphics::CGPath) };
        objc2_core_graphics::CGPath::bounding_box(Some(path))
    }

    fn outline_bounds(tile: &Tile) -> CGRect {
        objc2_core_graphics::CGPath::bounding_box(tile.outline.path().as_deref())
    }

    fn basic(layer: &CALayer, key: &str) -> Retained<CABasicAnimation> {
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

    fn point_of(value: &NSValue) -> CGPoint {
        // SAFETY: the value was made from a point.
        unsafe { value.pointValue() }
    }

    fn rect_of(value: &NSValue) -> CGRect {
        // SAFETY: the value was made from a rect.
        unsafe { value.rectValue() }
    }

    /// The strip's container, model at -861: a tile at 865 in it ends at 4 on screen.
    const STRIP_AT: CGPoint = CGPoint { x: -861.0, y: 0.0 };

    fn strip_tile(frame: CGRect) -> Tile {
        let container = CALayer::layer();
        let tile = new_tile(&container);
        place_tile(&tile, frame, 0.0);
        tile
    }

    /// The measured case, a tile 861 wide over a window 860 wide, riding its container: it is
    /// moved inside the container at once, its shadow, mask, silhouette and hairline with it.
    #[test]
    fn a_tile_riding_its_container_snaps_onto_its_window_at_once() {
        let mut tile = strip_tile(rect(865.0, 32.0, 861.0, 1081.0));
        let real = rect(4.0, 32.0, 860.0, 1081.0);
        snap_onto(&mut tile, STRIP_AT, real, 100.0);
        let snapped = rect(865.0, 32.0, 860.0, 1081.0);
        assert_eq!(tile.picture.frame(), snapped);
        assert_eq!(tile.shadow.frame(), snapped);
        assert_eq!(tile.ring.frame(), snapped);
        assert_eq!(outline_bounds(&tile), rect(-0.25, -0.25, 860.5, 1081.5));
        assert_eq!(tile.shadow_mask.frame(), shadow_mask_frame(snapped.size));
        let silhouette = tile.shadow.shadowPath().expect("a silhouette");
        assert_eq!(
            objc2_core_graphics::CGPath::bounding_box(Some(&silhouette)).size,
            snapped.size
        );
        assert!(tile.picture.animationKeys().is_none_or(|keys| keys.count() == 0));
    }

    /// A tile on a move of its own goes on along it to the window's frame: the same begin and
    /// start, a new end.
    #[test]
    fn a_tile_on_its_own_leg_snaps_on_the_same_timing() {
        let from = rect(1726.0, 32.0, 861.0, 1081.0);
        let to = rect(865.0, 32.0, 861.0, 1081.0);
        let timing = Timing {
            begin: 100.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let mut tile = strip_tile(to);
        animate_tile_leg(&mut tile, from, to, timing);
        snap_onto(&mut tile, STRIP_AT, rect(5.0, 32.0, 861.0, 1081.0), 100.2);
        let snapped = rect(866.0, 32.0, 861.0, 1081.0);
        assert_eq!(tile.picture.frame(), snapped);
        for layer in tile.layers() {
            assert_eq!(
                animation_value(layer, TILE_ANIMATION_KEY, point_of),
                (from.origin, snapped.origin, 100.0)
            );
        }
        assert_eq!(tile.leg, Some((from, snapped, timing)));

        let resized_from = rect(1726.0, 32.0, 1200.0, 1081.0);
        let mut tile = strip_tile(to);
        animate_tile_leg(&mut tile, resized_from, to, timing);
        let real = rect(4.0, 32.0, 860.0, 1081.0);
        snap_onto(&mut tile, STRIP_AT, real, 100.2);
        for layer in tile.layers() {
            let (start, end, begin) = animation_value(layer, "rini.tile.size", rect_of);
            assert_eq!(
                (start.size, end.size, begin),
                (resized_from.size, real.size, 100.0)
            );
        }
        let path = basic(&tile.outline, "rini.tile.outline.path");
        assert_eq!(path_bounds(path.toValue()).size, CGSize::new(860.5, 1081.5));
    }

    /// Once its leg has played, the tile's model moves and nothing is installed again.
    #[test]
    fn a_tile_whose_leg_has_ended_snaps_by_its_model() {
        let from = rect(1726.0, 32.0, 861.0, 1081.0);
        let to = rect(865.0, 32.0, 861.0, 1081.0);
        let timing = Timing {
            begin: 100.0,
            seconds: 0.35,
            curve: MOTION_CURVE,
        };
        let mut tile = strip_tile(to);
        animate_tile_leg(&mut tile, from, to, timing);
        snap_onto(&mut tile, STRIP_AT, rect(5.0, 32.0, 861.0, 1081.0), 100.5);
        assert_eq!(tile.picture.frame().origin, CGPoint::new(866.0, 32.0));
        assert_eq!(
            animation_value(&tile.picture, TILE_ANIMATION_KEY, point_of).1,
            to.origin,
            "the leg that played is left as it was"
        );
    }
}
