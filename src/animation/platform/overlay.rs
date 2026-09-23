//! The animation overlay: one rini-owned opaque window holding a picture of every animating window.
//! Real windows are placed at their final frames once, underneath it; tiles ride containers, one
//! per rigid piece of the flight. See "The overlay engine" in `src/animation/docs/animation-smoothness.md` and
//! the capture measurements in `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::{NSArray, NSNumber, NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CATransaction, kCAMediaTimingFunctionEaseInEaseOut,
};

pub use crate::animation::domain::motion::easing::{
    BOUNCE_TURN, CubicBezier, MOTION_CURVE, bounce_displacement, ease,
};
pub(crate) use crate::animation::domain::motion::plan::{
    AnimationTarget, animation_targets, bounce_carries,
};
use crate::animation::domain::motion::plan::{
    Banding, FlightPlan, GroupKey, Member, PlanDelta, group_relative, stale_overlay_layers,
};
pub use crate::animation::domain::motion::tile::{
    ContentMode, CropPiece, DressingAction, content_mode, crop_pieces, dressing_rebuild_allowed,
    lerp_rect, placeholder_mode, resize_in_flight,
};
use crate::animation::domain::motion::z_group::{StackGroup, container_z};
use crate::animation::platform::edge_dressing::{boundary_layout, tile_corner_radius};
use crate::animation::platform::window_snapshot::{SnapshotImage, WindowSnapshot};
use crate::displays::domain::screen::CoordinateConverter;
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
    pub snapshot: WindowSnapshot,
    /// In the other z-order band from the tiled windows. See `crate::animation::domain::motion::z_group`.
    pub floating: bool,
    /// The window server's front-to-back position, 0 frontmost; `None` when unreported.
    pub server_order: Option<usize>,
    /// Front-to-back position in the overlay, 0 frontmost; derived once per flight by the engine.
    pub depth: usize,
    /// A border window riding the window it traces: a quarter step in front of it, no shadow.
    pub companion: bool,
    /// Whether this window holds (or is about to hold) focus, which deepens its shadow.
    pub focused: bool,
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
    fn companion(&self) -> bool {
        self.companion
    }
}

impl OverlayTile {
    /// The zPosition this tile draws at; a companion's quarter step stays clear of the half step
    /// the shadow casters sit behind.
    pub(crate) fn z(&self) -> f64 {
        -(self.depth as f64) + if self.companion { 0.25 } else { 0.0 }
    }
}

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
/// pops at the lift. See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
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

/// `MOTION_CURVE` in Core Animation form, so `ease` and the render server run one curve.
fn motion_timing() -> Retained<CAMediaTimingFunction> {
    let c = MOTION_CURVE;
    CAMediaTimingFunction::functionWithControlPoints(
        c.x1 as f32,
        c.y1 as f32,
        c.x2 as f32,
        c.y2 as f32,
    )
}

/// An explicit begin on the media clock plus a length. Every animation of one leg shares one
/// `Timing`, so a leg re-installed with it continues on the same curve instead of restarting.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Timing {
    begin: f64,
    seconds: f64,
}

impl Timing {
    fn starting_now(duration: Duration) -> Self {
        Timing {
            begin: objc2_quartz_core::CACurrentMediaTime(),
            seconds: duration.as_secs_f64(),
        }
    }

    fn apply(&self, animation: &CABasicAnimation) {
        animation.setTimingFunction(Some(&motion_timing()));
        animation.setDuration(self.seconds);
        animation.setBeginTime(self.begin);
    }

    /// When the leg ends, on the wall clock.
    fn ends_at(&self) -> Instant {
        let left = self.begin + self.seconds - objc2_quartz_core::CACurrentMediaTime();
        Instant::now() + Duration::from_secs_f64(left.max(0.0))
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

/// One key per container bounce, separate from the movement's so the two compose.
const BOUNCE_ANIMATION_KEY: &str = "rini.group.bounce";

/// An additive position animation out to `overshoot` and back, so it rides a movement in flight
/// and leaves the model position alone.
fn bounce_animation(overshoot: CGPoint, timing: Timing) -> Retained<CAKeyframeAnimation> {
    let animation =
        CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    let rest = CGPoint::new(0.0, 0.0);
    // SAFETY: NSValues holding CGPoints are the value type Core Animation expects for "position".
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
        motion_timing(),
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
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: an NSValue holding a CGRect is the value type Core Animation expects here.
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithRect(from)));
        animation.setToValue(Some(&NSValue::valueWithRect(to)));
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
    // SAFETY: a CGPath is the value type Core Animation expects for path-valued key paths, and
    // the animation retains what it is given.
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

/// One window's picture, and a caster behind it carrying only the shadow, masked to a ring outside
/// the tile. See "A layer shadow covers the whole layer" in `src/animation/docs/capture-overlay-research.md`.
struct Tile {
    picture: Retained<CALayer>,
    shadow: Retained<CALayer>,
    /// Persistent, so a resize can animate its path; a swapped-in mask cannot be animated.
    shadow_mask: Retained<objc2_quartz_core::CAShapeLayer>,
    /// Hairline sublayers with their index in [`crate::animation::platform::edge_dressing::DressingLayout`] order
    /// (strips 0-3, corners 4-7).
    dressing: Vec<(usize, Retained<CALayer>)>,
    /// The four crop pieces, created on first crop-drawn animation and pooled with the tile.
    crop_grid: Option<CropGrid>,
    /// Size in points of the picture the crop pieces show; `None` when stretching.
    crop_of: Option<CGSize>,
    /// When the installed resize animation ends; a mismatched hairline is deferred until then.
    resize_until: Option<Instant>,
    /// The resize leg the tile is riding, so a picture swap re-installs it on the same timing.
    resize_leg: Option<(CGRect, CGRect, Timing)>,
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
    /// The real desktop, drawn behind everything and held still.
    backdrop: Retained<CALayer>,
    /// The bar, redrawn on top with its own alpha since the overlay covers the real one. See "The
    /// bar has to be captured on its own" in `src/animation/docs/capture-overlay-research.md`.
    bar: Retained<CALayer>,
    /// Whether the bar has ever been drawn, so a skipped capture keeps it rather than hiding it.
    bar_drawn: bool,
    tile_layers: HashMap<WindowId, Tile>,
    /// Display frame in CoreGraphics coordinates.
    frame: CGRect,
    scale: f64,
    visible: bool,
    mtm: MainThreadMarker,
}

impl TileOverlay {
    /// Creates the overlay once, ordered in but fully transparent. `frame` must be the display's
    /// full bounds in CoreGraphics coordinates. See "Level and coverage" in `src/animation/docs/capture-overlay-research.md`.
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
        // Opaque, so an undrawn gap is black rather than AppKit's grey slab.
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
                initWithFrame: CGRect::new(CGPoint::new(0.0, 0.0), frame.size)
            ]
        };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));

        let root = view.layer()?;
        root.setContentsScale(scale);

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

    /// Draws the bar on top of the moving tiles; `strip` is its rect in the overlay's coordinates.
    /// A failed capture keeps the previous picture rather than hiding the bar.
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
            None => self.bar.setHidden(!self.bar_drawn),
        }
        commit_now();
    }

    /// Sets the still image drawn behind the moving tiles, at its own covered size so it stays in
    /// register with the real desktop. A failed capture keeps the previous picture.
    pub fn set_backdrop(&mut self, snapshot: Option<&WindowSnapshot>) {
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
        commit_now();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Repoints the overlay at a different display, or the same one after a resolution change.
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

    /// Swaps a fresh picture into a tile mid-flight, leaving its movement alone. `remaining` is what
    /// is left of the flight, for a crop-drawn tile whose grid has to be re-keyed to the new picture.
    pub fn set_tile_picture(
        &mut self,
        window: WindowId,
        snapshot: &WindowSnapshot,
        remaining: Option<Duration>,
    ) {
        let scale = self.scale;
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
            let covered = snapshot.coverage.covered;
            entry.crop_of = Some(CGSize::new(covered.0, covered.1));
            let final_rect = entry.picture.frame();
            layout_crop_grid(entry, final_rect.size);
            // Re-installed on the same timing when riding a leg, so only the pixels cut.
            if remaining.is_some_and(|left| !left.is_zero()) {
                rekey = Some(match entry.resize_leg {
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
            // "Window borders during animations" in `src/animation/docs/animation-smoothness.md`.
            set_layer_contents(&entry.picture, snapshot);
        }
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
        dressing: &crate::animation::platform::edge_dressing::EdgeDressing,
    ) {
        let scale = self.scale;
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let size = entry.picture.bounds().size;
        let resizing = resize_in_flight(entry.resize_until, Instant::now());
        apply_edge_dressing(entry, Some(dressing), size, scale, true, resizing);
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
        let focused = if banding.floating_in_front {
            StackGroup::Floating
        } else {
            StackGroup::Tiled
        };
        for (key, layer) in &self.containers {
            let z = match key {
                GroupKey::Floating => container_z(StackGroup::Floating, focused),
                key => {
                    let index = banding
                        .group_order
                        .iter()
                        .position(|k| k == key)
                        .unwrap_or(banding.group_order.len());
                    container_z(StackGroup::Tiled, focused) - index as f64 * 0.25
                }
            };
            layer.setZPosition(z);
        }
        for (window, tile) in &self.tile_layers {
            let Some(&within) = banding.within.get(window) else {
                continue;
            };
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
        let tiles: Vec<(WindowId, Option<GroupKey>)> =
            self.tile_layers.iter().map(|(window, tile)| (*window, tile.key)).collect();
        let containers: Vec<GroupKey> = self.containers.keys().copied().collect();
        let (stale_tiles, empty_containers) = stale_overlay_layers(&tiles, keep, &containers);

        for window in stale_tiles {
            if let Some(entry) = self.tile_layers.remove(&window) {
                entry.picture.removeFromSuperlayer();
                entry.shadow.removeFromSuperlayer();
            }
        }
        for key in empty_containers {
            if let Some(layer) = self.containers.remove(&key) {
                layer.removeFromSuperlayer();
            }
        }
    }

    /// Hands a composed flight to Core Animation in one transaction: exactly the movements
    /// `animation_targets` names. A zero duration lands everything with no animation.
    pub(crate) fn fly(&mut self, plan: &FlightPlan, duration: Duration) {
        let timing = Timing::starting_now(duration);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for target in animation_targets(plan) {
            match target {
                AnimationTarget::Container { key, from, to } => {
                    let Some(layer) = self.containers.get(&key) else {
                        continue;
                    };
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

    /// Nudges the containers by `overshoot` and back, additively, on top of any movement in flight.
    /// See "Edge bounce" in `src/animation/docs/animation-smoothness.md`.
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
        reparent(&entry.picture, &parent);
        reparent(&entry.shadow, &parent);
        entry.key = key;
        entry.companion = tile.companion;
        entry.picture.setContentsScale(self.scale);
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
        // Tile layers are pooled, so every per-window property is written on each install.
        let style = tile_shadow_style(tile.focused);
        entry.shadow.setShadowOpacity(style.opacity);
        entry.shadow.setShadowRadius(style.radius);
        entry.shadow.setShadowOffset(CGSize::new(0.0, style.offset_y));
        place_tile(entry, whole(at), tile.z());
        entry.picture.setHidden(false);
        entry.shadow.setHidden(tile.companion);
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
        layer.setPosition(position);
        layer
    }

    /// Whether every container and picture is presented within half a point of its model: the
    /// render server runs behind the actor's clock. See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
    pub fn settled(&self) -> bool {
        let close = |a: CGPoint, b: CGPoint| (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5;
        let same_size = |a: CGSize, b: CGSize| {
            (a.width - b.width).abs() < 0.5 && (a.height - b.height).abs() < 0.5
        };
        // SAFETY: `presentationLayer` returns a read-only copy of the layer.
        let containers = self.containers.values().all(|layer| {
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

    /// Presented position of every container: what `merge_plans` retargets from.
    pub(crate) fn presented_positions(&self) -> HashMap<GroupKey, CGPoint> {
        self.containers
            .keys()
            .map(|key| (*key, self.presented_container_position(Some(*key))))
            .collect()
    }

    /// Applies one merged pass to a flight in progress in one transaction, reading every presented
    /// position before writing. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
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

        for &(key, install) in &delta.new_groups {
            let container = self.reset_container(key);
            container.setPosition(whole_point(install));
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
            reparent(&entry.picture, &container);
            reparent(&entry.shadow, &container);
            entry.key = Some(to_key);
            let z = entry.picture.zPosition();
            place_tile(entry, whole(frame), z);
            placed.push(window);
        }

        for &(key, to) in &delta.retargeted_groups {
            let Some(layer) = self.containers.get(&key) else {
                continue;
            };
            let from = presented.get(&key).copied().unwrap_or(layer.position());
            let to = whole_point(to);
            layer.setPosition(to);
            if !duration.is_zero() {
                let animation = position_animation(from, to, timing);
                layer.addAnimation_forKey(
                    &animation,
                    Some(&NSString::from_str(GROUP_ANIMATION_KEY)),
                );
            }
        }
        for &(key, install) in &delta.new_groups {
            let Some(layer) = self.containers.get(&key) else {
                continue;
            };
            let to = whole_point(plan.positions.get(&key).copied().unwrap_or(install));
            let install = whole_point(install);
            layer.setPosition(to);
            if !duration.is_zero() && !install.same_as(to) {
                let animation = position_animation(install, to, timing);
                layer.addAnimation_forKey(
                    &animation,
                    Some(&NSString::from_str(GROUP_ANIMATION_KEY)),
                );
            }
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
        let timing = Timing::starting_now(duration);
        // Proportional tolerance, not equality: a sub-tolerance re-fit rides the plain move.
        if !crate::animation::platform::window_snapshot::is_a_resize(from.size, to.size) {
            entry.resize_leg = None;
            // addAnimation copies, so one instance serves picture and shadow.
            let animation = position_animation(from.origin, to.origin, timing);
            let key = NSString::from_str(TILE_ANIMATION_KEY);
            entry.picture.addAnimation_forKey(&animation, Some(&key));
            entry.shadow.addAnimation_forKey(&animation, Some(&key));
            return;
        }
        self.animate_tile_resize(window, from, to, timing);
    }

    /// The resize: every layer of the tile rides its own pair of endpoint geometries on the shared
    /// curve. See "Resizes through the overlay" in `src/animation/docs/animation-smoothness.md`.
    fn animate_tile_resize(&mut self, window: WindowId, from: CGRect, to: CGRect, timing: Timing) {
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        entry.resize_until = Some(timing.ends_at());
        entry.resize_leg = Some((from, to, timing));

        // Same key prefix as the plain move, or a move retargeted into a resize leaves its old
        // position animation fighting this one.
        animate_layer_frame(&entry.picture, from, to, timing, "rini.tile");
        animate_layer_frame(&entry.shadow, from, to, timing, "rini.tile");

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

        if let (Some(from_layout), Some(to_layout)) =
            (boundary_layout(from.size), boundary_layout(to.size))
        {
            let rect_at = |layout: &crate::animation::platform::edge_dressing::DressingLayout,
                           index: usize| {
                if index < 4 {
                    layout.strips[index]
                } else {
                    layout.corners[index - 4]
                }
            };
            for (index, layer) in &entry.dressing {
                let a = rect_at(&from_layout, *index);
                let b = rect_at(&to_layout, *index);
                layer.setFrame(b);
                animate_layer_frame(layer, a, b, timing, "rini.dressing");
            }
        }
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
        if !self.visible {
            return;
        }
        self.window.setAlphaValue(0.0);
        self.visible = false;
    }

    /// Frees the tile contents without destroying the overlay.
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

/// The bar's picture at the strip's origin, at the size it covers, never stretched. See "The bar
/// has to be captured on its own" in `src/animation/docs/capture-overlay-research.md`.
fn bar_frame(strip: CGRect, covered: CGSize) -> CGRect {
    CGRect::new(strip.origin, covered)
}

/// The harvested image for one piece, in `DressingLayout` order: four strips, then four corners.
fn dressing_image(
    dressing: &crate::animation::platform::edge_dressing::EdgeDressing,
    index: usize,
) -> Option<&CFRetained<objc2_core_graphics::CGImage>> {
    if index < 4 {
        dressing.strips[index].as_ref()
    } else {
        dressing.corners[index - 4].as_ref()
    }
}

/// Every harvested piece, whatever `size`, so a fresh harvest swaps in place. See "Window borders
/// during animations" in `src/animation/docs/animation-smoothness.md`.
fn dressing_piece_indices(
    dressing: &crate::animation::platform::edge_dressing::EdgeDressing,
    size: CGSize,
) -> Vec<usize> {
    if boundary_layout(size).is_none() {
        return Vec::new();
    }
    (0..8).filter(|index| dressing_image(dressing, *index).is_some()).collect()
}

/// Dresses a tile with its window's harvested hairline as sublayers of the picture, or strips it
/// bare.
fn apply_edge_dressing(
    tile: &mut Tile,
    dressing: Option<&crate::animation::platform::edge_dressing::EdgeDressing>,
    size: CGSize,
    scale: f64,
    swap_in_place: bool,
    resize_in_flight: bool,
) {
    // A matching harvest swaps pixels into the existing layers so resize animations riding them
    // survive; a mismatch is deferred while a resize rides. Installs always rebuild (pooled layers).
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
        // Above the crop pieces, which sit at the default 0.
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

/// A tile: the picture, plus a caster behind it holding the shadow. No `masksToBounds`: it would
/// clip the layer's own shadow, and contents cannot spill anyway.
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

/// Puts both of a tile's layers at `frame`, with the caster half a step behind the picture. The
/// shadow's shape is rebuilt only when the size changes.
fn place_tile(tile: &Tile, frame: CGRect, z: f64) {
    let resized = tile.shadow.bounds().size != frame.size;
    tile.picture.setFrame(frame);
    tile.picture.setZPosition(z);
    tile.shadow.setFrame(frame);
    tile.shadow.setZPosition(z - 0.5);
    if resized {
        set_tile_shadow(&tile.shadow, &tile.shadow_mask, frame.size);
        layout_crop_grid(tile, frame.size);
    }
}

/// The window's rounded-rect silhouette: the shadow's shape and the mask's hole. Size and radius
/// are floored above zero, or Core Graphics emits a plain rect and path animations cut.
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

/// The ring: the whole mask minus the window's shape, wound even-odd so the inside is the hole.
/// Always one rect plus one rounded rect, so two ring paths interpolate.
fn ring_path(size: CGSize) -> CFRetained<objc2_core_graphics::CGMutablePath> {
    // Floored for the same reason as `silhouette_path`.
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
    snapshot: &WindowSnapshot,
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
    let (Some(grid), Some(picture)) = (&entry.crop_grid, entry.crop_of) else {
        return;
    };
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

/// Hands a snapshot to a layer as its contents; Core Animation takes a `CGImage` or an `IOSurface`
/// directly.
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

/// The primary display's bottom edge, which AppKit's bottom-left coordinates are anchored to.
fn primary_display_height() -> f64 {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    bounds.origin.y + bounds.size.height
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_clock_and_the_render_server_run_one_curve() {
        for step in 0..=1000 {
            let s = step as f64 / 1000.0;
            let (x, y) = MOTION_CURVE.at(s);
            assert!(
                (ease(x) - y).abs() < 1e-6,
                "the curves diverge at s={s}: ease({x})={} y={y}",
                ease(x)
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
        use crate::animation::domain::motion::z_group::{StackGroup, tile_depth};
        let deepest = tile_depth(None, false, StackGroup::Floating, StackGroup::Tiled);
        let shallowest = tile_depth(Some(0), true, StackGroup::Tiled, StackGroup::Tiled);
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

    #[test]
    fn the_dressing_piece_set_does_not_depend_on_the_tile_size() {
        use crate::animation::platform::edge_dressing::EdgeDressing;
        use crate::animation::platform::window_snapshot::test_bitmap;
        let piece = || Some(test_bitmap());
        let dressing = EdgeDressing {
            strips: [piece(), piece(), piece(), piece()],
            corners: [piece(), piece(), piece(), piece()],
        };
        let at_zero = dressing_piece_indices(&dressing, CGSize::new(0.0, 1081.0));
        let at_full = dressing_piece_indices(&dressing, CGSize::new(859.0, 1081.0));
        assert_eq!(
            at_zero,
            at_full,
            "a tile dressed at (0, h) wears {} pieces, at (w, h) {}",
            at_zero.len(),
            at_full.len()
        );
    }

    fn full_dressing() -> crate::animation::platform::edge_dressing::EdgeDressing {
        use crate::animation::platform::window_snapshot::test_bitmap;
        let piece = || Some(test_bitmap());
        crate::animation::platform::edge_dressing::EdgeDressing {
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

    #[test]
    fn the_piece_set_is_constant_over_random_sizes() {
        let dressing = full_dressing();
        let reference = dressing_piece_indices(&dressing, CGSize::new(859.0, 1081.0));
        assert_eq!(reference.len(), 8);
        // Seeded LCG.
        let mut state: u64 = 0x5eed_d2e5_5100;
        let mut next = move |n: u64| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 11) % n
        };
        for _ in 0..500 {
            let w = next(1729) as f64;
            let h = 1.0 + next(1081) as f64;
            let size = CGSize::new(w, h);
            assert_eq!(
                dressing_piece_indices(&dressing, size),
                reference,
                "size {size:?}"
            );
        }
    }
}
