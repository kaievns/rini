//! The animation overlay: one rini-owned window holding a picture of every animating window.
//!
//! Nothing real moves during an animation. Real windows are placed at their final frames once,
//! underneath an opaque overlay, and every layer inside it is repositioned in a single Core Animation
//! transaction, so windows cannot tear against each other the way per-frame `AXPosition` writes did.
//!
//! Design constraints, all measured, in `docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPopUpMenuWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::{NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CATransaction,
};

use crate::actor::app::WindowId;
use crate::sys::geometry::SameAs;
use crate::sys::screen::CoordinateConverter;
use crate::ui::edge_dressing::{self, dressing_layout, tile_corner_radius};
use crate::ui::window_snapshot::{SnapshotImage, WindowSnapshot};


/// Window level for the overlay. Managed windows all sit at CG layer 0, so anything above that
/// covers them. `NSPopUpMenuWindowLevel` is 101, which `mission_control.rs` already uses, and it
/// stays below the assistive and cursor levels so nothing accessibility-related is hidden.
const OVERLAY_LEVEL: isize = NSPopUpMenuWindowLevel as isize;

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
    /// Front-to-back position on screen, 0 being frontmost. Without it a tile can be drawn behind a
    /// window it is really in front of, and the handover pops.
    pub depth: usize,
    /// A border window riding the window it traces: drawn a quarter-step in front of its window's
    /// depth, and without a shadow, because the real border window casts none.
    pub companion: bool,
}

impl OverlayTile {
    /// The zPosition this tile draws at. A companion sits just in front of the window it traces —
    /// a quarter of a depth step, clear of the half-step the shadow casters sit behind.
    pub(crate) fn z(&self) -> f64 {
        -(self.depth as f64) + if self.companion { 0.25 } else { 0.0 }
    }
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
/// window pops. Fitted to a shadow read out of a ScreenCaptureKit capture that included one: 18% of black
/// 3.5pt out from the edge, 13% at 7pt, 6% at 14pt, 3.5% at 17pt, and the bottom reaching about twice as far
/// as the top. See "Shadows are never in the surface" in `docs/capture-overlay-research.md`.
const SHADOW_OPACITY: f32 = 0.4;
const SHADOW_RADIUS: f64 = 9.0;
/// Positive is downward: the tiles hang off a flipped view, so the layer's y axis points down.
const SHADOW_OFFSET_Y: f64 = 5.0;

/// How far the shadow can reach from a window's edge, which is what the mask has to leave room for. The
/// measured ramp is spent by 17pt; 40pt is generous enough that no blur is clipped at the edge of the ring.
const SHADOW_REACH: f64 = 40.0;

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

/// One key for every tile movement, so a retarget replaces the animation in flight rather than
/// stacking a second one on the same property.
const TILE_ANIMATION_KEY: &str = "rini.tile.move";

/// `ease_out_cubic` in Core Animation form — exactly, not approximately. Derivation in
/// `docs/animation-smoothness.md`; the identity is pinned by test.
fn ease_out_cubic_timing() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(1.0 / 3.0, 1.0, 2.0 / 3.0, 1.0)
}

/// An explicit position animation from `from` to `to`, in layer coordinates. The caller sets the
/// model to the destination; this carries the presentation there and is removed on completion,
/// revealing the model value — no snap-back, no completion delegate.
fn position_animation(from: CGPoint, to: CGPoint, seconds: f64) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    // SAFETY: an NSValue holding a CGPoint is the value type Core Animation expects for the
    // "position" key path.
    unsafe {
        animation.setFromValue(Some(&NSValue::valueWithPoint(from)));
        animation.setToValue(Some(&NSValue::valueWithPoint(to)));
    }
    animation.setTimingFunction(Some(&ease_out_cubic_timing()));
    animation.setDuration(seconds);
    animation
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
    /// The hairline sublayers riding the picture, replaced whenever the picture is re-dressed.
    dressing: Vec<Retained<CALayer>>,
}

pub struct WorkspaceOverlay {
    window: Retained<NSWindow>,
    /// The layer every tile is added to. Owned by the window's content view, which is layer-backed,
    /// so AppKit presents it on the GPU with no manual rasterisation.
    root: Retained<CALayer>,
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
        CATransaction::commit();
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
        CATransaction::commit();
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
    pub fn set_tile_picture(&mut self, window: WindowId, snapshot: &WindowSnapshot) {
        let scale = self.scale;
        let Some(entry) = self.tile_layers.get_mut(&window) else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        set_layer_contents(&entry.picture, snapshot);
        // A recapture can carry a fresh hairline too — the focus recapture is exactly the one
        // that changes it.
        if snapshot.dressing.is_some() {
            let size = entry.picture.bounds().size;
            apply_edge_dressing(entry, snapshot, size, scale);
        }
        CATransaction::commit();
    }

    /// Installs the tiles for one animation and draws frame zero.
    ///
    /// Layers are pooled per window, since handing one a bitmap is the expensive part. Anything absent
    /// is removed, or the previous switch's windows linger as ghosts.
    ///
    /// Pre-flight only: this places every tile at its START. A tile already animating must not pass
    /// through here — its model sits at the destination while Core Animation carries the
    /// presentation, and re-placing it at `from` would end the flight on the wrong frame. Mid-flight
    /// changes go through `retarget_tile` and `add_tile`.
    pub fn set_tiles(&mut self, tiles: &[OverlayTile]) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        let mut keep = Vec::with_capacity(tiles.len());
        for tile in tiles {
            keep.push(tile.window);
            self.install_tile(tile);
        }

        let stale: Vec<WindowId> =
            self.tile_layers.keys().copied().filter(|w| !keep.contains(w)).collect();
        for window in stale {
            if let Some(entry) = self.tile_layers.remove(&window) {
                entry.picture.removeFromSuperlayer();
                entry.shadow.removeFromSuperlayer();
            }
        }

        CATransaction::commit();
    }

    /// Installs one tile at its start position. Callers hold the transaction.
    fn install_tile(&mut self, tile: &OverlayTile) {
        let entry = self
            .tile_layers
            .entry(tile.window)
            .or_insert_with(|| new_tile(&self.root));
        reparent(&entry.picture, &self.root);
        reparent(&entry.shadow, &self.root);
        entry.picture.setContentsScale(self.scale);
        set_layer_contents(&entry.picture, &tile.snapshot);
        apply_edge_dressing(entry, &tile.snapshot, tile.from.size, self.scale);
        // Negated so a smaller depth, meaning nearer the front, draws on top.
        place_tile(entry, tile.from, tile.z());
        entry.picture.setHidden(false);
        // A border window casts no shadow, so its tile must not either.
        entry.shadow.setHidden(tile.companion);
    }

    /// Adds one tile to an animation already in flight and starts its movement.
    ///
    /// The reactor lays a layout out over several passes, so a window can join after the others
    /// have left. It gets the full duration from where it stands: joining at the group's current
    /// progress would snap it to a mid-flight position first, which is the worse artifact.
    pub fn add_tile(&mut self, tile: &OverlayTile, duration: Duration) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.install_tile(tile);
        self.animate_tile_movement(tile.window, tile.from, tile.to, tile.z(), duration);
        CATransaction::commit();
    }

    /// Hands every tile's movement to Core Animation, in ONE transaction.
    ///
    /// One commit, one timebase, one curve: the render server starts every animation on the same
    /// beat and interpolates them vsync-locked at the display's native refresh, so tiles cannot
    /// tear against each other and a busy actor thread cannot drop drawn frames. The model layers
    /// jump straight to their destinations; the animations carry the presentation and are removed
    /// on completion, revealing the model — the same handoff the whole overlay uses.
    pub fn animate_tiles(&mut self, tiles: &[OverlayTile], duration: Duration) {
        // Core Animation reads a zero duration as "use the default 0.25s".
        if duration.is_zero() {
            self.draw_frame(tiles, 1.0);
            return;
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for tile in tiles {
            if tile.from.same_as(tile.to) {
                continue;
            }
            self.animate_tile_movement(tile.window, tile.from, tile.to, tile.z(), duration);
        }
        CATransaction::commit();
    }

    /// Retargets one tile mid-flight: continues from wherever it is DRAWN right now to the new
    /// destination, over a fresh duration.
    ///
    /// The presentation tree is the truth about the current position — the model already sits at
    /// the old destination — and re-adding under the same key replaces the old animation, so the
    /// tile bends toward the new target instead of restarting. Same chaining pattern as the canvas.
    pub fn retarget_tile(&mut self, window: WindowId, to: CGRect, z: f64, duration: Duration) {
        let Some(entry) = self.tile_layers.get(&window) else { return };
        // SAFETY: `presentationLayer` returns a read-only copy of the layer as currently presented.
        let current = unsafe { entry.picture.presentationLayer() }
            .map(|presented| presented.position())
            .unwrap_or_else(|| entry.picture.position());
        let from = CGRect::new(current, to.size);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.animate_tile_movement(window, from, to, z, duration);
        CATransaction::commit();
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
        let Some(entry) = self.tile_layers.get(&window) else { return };
        place_tile(entry, to, z);
        // Anchor points are (0,0), so position is the frame origin; this path never changes a
        // tile's size. addAnimation copies, so one instance serves picture and shadow.
        let animation = position_animation(from.origin, to.origin, duration.as_secs_f64());
        let key = NSString::from_str(TILE_ANIMATION_KEY);
        entry.picture.addAnimation_forKey(&animation, Some(&key));
        entry.shadow.addAnimation_forKey(&animation, Some(&key));
    }

    /// Positions every tile for a given progress through the animation, in ONE transaction.
    ///
    /// The single transaction is the whole point: it makes tearing between windows impossible rather
    /// than merely unlikely, which is what per-window Accessibility writes could never achieve.
    pub fn draw_frame(&mut self, tiles: &[OverlayTile], t: f64) {
        let eased = ease_out_cubic(t);
        CATransaction::begin();
        // Implicit animations must be off. Core Animation would otherwise add its own quarter-second
        // ease to every frame, so our interpolation would fight a second one and lag behind.
        CATransaction::setDisableActions(true);
        for tile in tiles {
            if let Some(entry) = self.tile_layers.get(&tile.window) {
                place_tile(entry, lerp_rect(tile.from, tile.to, eased), tile.z());
            }
        }
        CATransaction::commit();
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
        CATransaction::commit();
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

/// Dresses a tile with its window's harvested hairline, or strips it bare.
///
/// The ring rides the picture as thin sublayers — four straight runs and four corner boxes, in
/// [`crate::ui::edge_dressing::DressingLayout`] order — so every movement animation carries it for
/// free. Replaced wholesale because tile layers are pooled: a tile must not wear another window's
/// edge, or a stale one after a recapture.
fn apply_edge_dressing(tile: &mut Tile, snapshot: &WindowSnapshot, size: CGSize, scale: f64) {
    for layer in tile.dressing.drain(..) {
        layer.removeFromSuperlayer();
    }
    let Some(dressing) = &snapshot.dressing else { return };
    let Some(layout) = dressing_layout(size, edge_dressing::RING_PT, edge_dressing::CORNER_RADIUS)
    else {
        return;
    };
    let pieces = dressing
        .strips
        .iter()
        .zip(layout.strips)
        .chain(dressing.corners.iter().zip(layout.corners));
    for (image, frame) in pieces {
        let Some(image) = image else { continue };
        if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
            continue;
        }
        let layer = CALayer::layer();
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setFrame(frame);
        layer.setContentsScale(scale);
        // SAFETY: a retained CGImage; Core Animation retains what it draws.
        unsafe {
            let raw: *const objc2_core_graphics::CGImage = &**image;
            let _: () = msg_send![&*layer, setContents: raw];
        }
        tile.picture.addSublayer(&layer);
        tile.dressing.push(layer);
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
    // caster has no contents and no background, so the shadow is all it ever draws.
    shadow.setShadowOpacity(SHADOW_OPACITY);
    shadow.setShadowRadius(SHADOW_RADIUS);
    shadow.setShadowOffset(CGSize::new(0.0, SHADOW_OFFSET_Y));
    container.addSublayer(&shadow);

    let picture = CALayer::layer();
    picture.setAnchorPoint(CGPoint::new(0.0, 0.0));
    // SAFETY: Core Animation's own filter-name constants.
    unsafe {
        picture.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
        picture.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
    }
    container.addSublayer(&picture);

    Tile { picture, shadow, dressing: Vec::new() }
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
        set_tile_shadow(&tile.shadow, frame.size);
    }
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
fn set_tile_shadow(layer: &CALayer, size: CGSize) {
    let radius = tile_corner_radius(size);
    let silhouette = CGRect::new(CGPoint::new(0.0, 0.0), size);
    // SAFETY: a null transform means the path is taken as given.
    let path = unsafe {
        objc2_core_graphics::CGPath::with_rounded_rect(silhouette, radius, radius, std::ptr::null())
    };
    layer.setShadowPath(Some(&path));

    let mask_frame = shadow_mask_frame(size);
    let mask = objc2_quartz_core::CAShapeLayer::layer();
    mask.setAnchorPoint(CGPoint::new(0.0, 0.0));
    mask.setFrame(mask_frame);
    // The ring: the whole mask, minus the window's own shape, wound so the inside is the hole.
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
    mask.setPath(Some(&ring));
    // SAFETY: Core Animation's own fill-rule constant, and a mask layer we just created and own.
    unsafe {
        mask.setFillRule(objc2_quartz_core::kCAFillRuleEvenOdd);
        layer.setMask(Some(&mask));
    }
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
    /// visible straight edge in it. The measured ramp is spent by 17pt.
    #[test]
    fn the_mask_reaches_further_than_the_shadow_does() {
        assert!(SHADOW_REACH > SHADOW_RADIUS * 2.0 + SHADOW_OFFSET_Y);
    }

    #[test]
    fn the_shadow_falls_downward() {
        // The tiles hang off a flipped view, so a positive y offset is down the screen. Getting the sign
        // wrong lights the window from below, which reads as wrong without being obviously wrong.
        assert!(SHADOW_OFFSET_Y > 0.0);
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
}