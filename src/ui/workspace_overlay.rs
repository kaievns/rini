//! The animation overlay: one rini-owned window holding a picture of every animating window.
//!
//! Replaces per-frame Accessibility writes. The old path wrote `AXPosition` to every animating
//! window on every frame, and since each write is a synchronous request into a different process
//! that answers at its own speed, the windows never landed together. That is the cross-app tear,
//! measured at 100 to 150px between neighbouring windows mid-scroll.
//!
//! Here nothing real moves during the animation. Real windows are placed at their final frames once,
//! underneath an opaque overlay, and what the eye follows is a layer per window inside that overlay.
//! Every layer is repositioned in a single Core Animation transaction, so they cannot tear against
//! each other by construction.
//!
//! Design constraints, all measured. See `docs/capture-overlay-research.md`.
//!
//! - **Size to the usable display frame, not the whole screen.** sketchybar sits at CG layer -20,
//!   below normal windows, and is visible only because nothing occupies the menu bar strip. A
//!   full-screen overlay covers the user's bar and flickers it on every switch. Managed windows all
//!   sit at layer 0 inside the usable frame, so leaving that strip alone costs no coverage.
//! - **Toggle alpha, never order in and out.** Ordering costs about 14ms each way, a full frame at
//!   60fps. An alpha toggle costs 0.36ms. So the overlay is created once and stays ordered in at
//!   alpha 0 forever. Alpha works because rini owns this window; on foreign windows it does nothing.
//! - **Never transform a foreign window.** `SLSSetWindowTransform` returns success on someone else's
//!   window and is silently discarded, and `SLSMoveWindow` even updates `SLSGetWindowBounds` while
//!   the window does not move. Transforms only work on windows we created, which is what this is.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPopUpMenuWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_quartz_core::{CALayer, CATransaction};

use crate::actor::app::WindowId;
use crate::sys::screen::CoordinateConverter;
use crate::ui::window_snapshot::{SnapshotImage, WindowSnapshot};

/// Window level for the overlay. Managed windows all sit at CG layer 0, so anything above that
/// covers them. `NSPopUpMenuWindowLevel` is 101, which `mission_control.rs` already uses, and it
/// stays below the assistive and cursor levels so nothing accessibility-related is hidden.
const OVERLAY_LEVEL: isize = NSPopUpMenuWindowLevel as isize;

define_class!(
    /// An `NSView` with a top-left origin.
    ///
    /// Everything in rini reasons about window frames in CoreGraphics coordinates, which put the
    /// origin at the top left. AppKit puts it at the bottom left, and `setGeometryFlipped(true)` on a
    /// VIEW-BACKED layer does not change that: AppKit owns that layer and manages its geometry, so the
    /// flag was silently ineffective and every child was positioned in bottom-left space while the
    /// arithmetic assumed top-left.
    ///
    /// That was measurable rather than theoretical: the bar layer, positioned at (0, 0), rendered along
    /// the BOTTOM of the screen. Overriding `isFlipped` on the view is the sanctioned way to get a
    /// top-left system, and it makes the whole layer tree agree with the rest of the codebase.
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

/// One window's picture at a fixed position on the canvas.
///
/// Canvas coordinates, not screen coordinates. The canvas holds every window across every workspace
/// laid out side by side and stacked, and an animation moves the CANVAS rather than the windows on it.
/// That is what makes a jump across the strip, or from workspace 1 to workspace 4, scroll past
/// everything in between instead of cutting straight to the destination.
pub struct CanvasTile {
    pub window: WindowId,
    /// Fixed position on the canvas. Never interpolated.
    pub frame: CGRect,
    pub snapshot: WindowSnapshot,
    /// Front-to-back position on screen, 0 being frontmost.
    pub depth: usize,
}

/// One window's picture inside the overlay, and where it should be drawn.
pub struct OverlayTile {
    pub window: WindowId,
    /// Where the window starts, in the overlay's coordinate space.
    pub from: CGRect,
    /// Where the window ends up, in the overlay's coordinate space.
    pub to: CGRect,
    pub snapshot: WindowSnapshot,
    /// Front-to-back position on screen, 0 being frontmost.
    ///
    /// Without this the tiles stack in whatever order the layout happened to list them, so the window
    /// that is really in front can be drawn behind. The overlay then drops and the real front window
    /// appears to jump forward, which is what made a switch end with a visible pop.
    pub depth: usize,
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

/// Ease-out cubic. Fast at the start and settling at the end, which reads as the strip being flicked
/// rather than dragged, and matches what niri does.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

pub struct WorkspaceOverlay {
    window: Retained<NSWindow>,
    /// The layer every tile is added to. Owned by the window's content view, which is layer-backed,
    /// so AppKit presents it on the GPU with no manual rasterisation.
    root: Retained<CALayer>,
    /// Holds every tile at its fixed canvas position. Moving THIS layer is the animation: one
    /// transform per frame regardless of how many windows are on screen, and the tiles cannot drift
    /// against each other because they are siblings under one parent that moves as a unit.
    canvas: Retained<CALayer>,
    /// The real desktop, drawn behind everything and held still while the canvas moves.
    ///
    /// The overlay has to be opaque so the real windows being repositioned underneath stay hidden,
    /// which meant the gaps between and around strips were a flat colour. That flat area appearing and
    /// disappearing reads as flickering, and on a workspace holding one half-width window it was half
    /// the screen. Showing the actual desktop there makes those gaps look like the desktop, because
    /// they are.
    backdrop: Retained<CALayer>,
    /// The bar, redrawn on top and held still while the canvas moves beneath it.
    ///
    /// The overlay spans the whole display so the desktop fits at its true size and strips can travel
    /// through the full height. That covers the user's bar, so it is drawn back on top rather than
    /// left hidden for the duration of every animation.
    foreground: Retained<CALayer>,
    tile_layers: HashMap<WindowId, Retained<CALayer>>,
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
    /// `frame` must be the display's FULL bounds in CoreGraphics (top-left origin) coordinates. Sizing
    /// it to the usable frame instead forced the captured desktop to be squashed into a shorter box,
    /// and left a vertical animation invisible in the strip beneath the bar. The bar is drawn back on
    /// top by `set_foreground` so covering it costs nothing.
    ///
    /// Built on `NSWindow` rather than a raw window server window. A raw window needs its layer tree
    /// bound through `SLSSetWindowLayerContext`, which fails with `kCGErrorFailure` here, leaving only
    /// a manual `renderInContext` fallback. That fallback has to rasterise every tile's `IOSurface`
    /// into CPU memory on every frame, which measured at over 800MB resident for one animation. A
    /// layer-backed `NSWindow` composites on the GPU instead, which is both correct and free.
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
        // Without this the canvas between strips renders as a bare grey slab, which reads as a glitch
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

        // Behind the canvas, and never moved: the desktop does not scroll with the workspaces.
        //
        // Deliberately NOT geometryFlipped. That property flips a layer's own contents as well as its
        // children's coordinates, so setting it here drew the captured desktop upside down, which put
        // the menu bar strip along the bottom of the screen and made a dark wallpaper look black.
        let backdrop = CALayer::layer();
        backdrop.setAnchorPoint(CGPoint::new(0.0, 0.0));
        backdrop.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        backdrop.setContentsScale(scale);
        backdrop.setZPosition(-10_000.0);
        root.addSublayer(&backdrop);

        // Above the canvas, and never moved: the bar does not scroll with the workspaces. Not
        // geometryFlipped, for the same reason as the backdrop.
        let foreground = CALayer::layer();
        foreground.setAnchorPoint(CGPoint::new(0.0, 0.0));
        foreground.setContentsScale(scale);
        foreground.setZPosition(10_000.0);
        foreground.setHidden(true);
        root.addSublayer(&foreground);

        let canvas = CALayer::layer();
        // Anchored at its top-left so setting the position translates the children directly, with no
        // half-size offset to reason about.
        canvas.setAnchorPoint(CGPoint::new(0.0, 0.0));
        canvas.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        canvas.setPosition(CGPoint::new(0.0, 0.0));
        root.addSublayer(&canvas);

        window.orderFrontRegardless();

        Some(Self {
            window,
            root,
            backdrop,
            foreground,
            canvas,
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

    /// Sets the still image drawn ON TOP of the moving canvas, for the bar.
    ///
    /// Positioned from the image's own covered size at the top of the display, because that is where
    /// a menu bar strip lives. A failed capture leaves whatever was there rather than hiding it, so
    /// the bar does not blink.
    ///
    /// The layer is not geometryFlipped, so its contents draw the right way up while its position is
    /// still interpreted in the flipped root's top-left space.
    pub fn set_foreground(&mut self, snapshot: Option<&WindowSnapshot>) {
        let Some(snapshot) = snapshot else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let (covered_w, covered_h) = snapshot.coverage.covered;
        self.foreground.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(covered_w, covered_h),
        ));
        self.foreground.setContentsScale(self.scale);
        set_layer_contents(&self.foreground, snapshot);
        self.foreground.setHidden(false);
        CATransaction::commit();
    }

    /// Sets the still image drawn behind the moving canvas.
    ///
    /// Positioned from the image's own covered size rather than stretched to the overlay. The desktop
    /// spans the FULL display while the overlay covers only the usable frame, so drawing it to the
    /// overlay's bounds squashed it by the menu bar inset and left it visibly out of register with the
    /// real desktop. The difference in height IS that inset, so the image is placed that far above the
    /// overlay's top edge at its true size.
    ///
    /// A failed capture keeps whatever was there before. Hiding it instead made the desktop blink in
    /// and out whenever one capture did not land.
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

    /// Installs the canvas for one animation: every tile at a fixed position.
    ///
    /// Tiles are reused across animations for the same window, since handing a layer a bitmap is the
    /// expensive part. Anything absent is removed, or the previous animation's windows linger.
    pub fn set_canvas(&mut self, tiles: &[CanvasTile]) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        let mut keep = Vec::with_capacity(tiles.len());
        for tile in tiles {
            keep.push(tile.window);
            let layer = self.tile_layers.entry(tile.window).or_insert_with(|| {
                let layer = CALayer::layer();
                layer.setMasksToBounds(true);
                // SAFETY: Core Animation's own filter-name constants.
                unsafe {
                    layer.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
                    layer.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
                }
                self.canvas.addSublayer(&layer);
                layer
            });
            layer.setContentsScale(self.scale);
            set_layer_contents(layer, &tile.snapshot);
            layer.setFrame(tile.frame);
            layer.setZPosition(-(tile.depth as f64));
            layer.setHidden(false);
        }

        let stale: Vec<WindowId> =
            self.tile_layers.keys().copied().filter(|w| !keep.contains(w)).collect();
        for window in stale {
            if let Some(layer) = self.tile_layers.remove(&window) {
                layer.removeFromSuperlayer();
            }
        }

        CATransaction::commit();
    }

    /// Moves the canvas so that `offset` in canvas coordinates sits at the overlay's top-left.
    ///
    /// This is the entire animation: one property on one layer. Because every tile is a child of the
    /// canvas, they move together exactly, which is what makes relative drift between windows
    /// impossible rather than merely unlikely.
    pub fn set_canvas_offset(&mut self, offset: CGPoint) {
        CATransaction::begin();
        // Implicit animations off, or Core Animation adds its own ease on top of ours and the result
        // lags the input by a fixed amount.
        CATransaction::setDisableActions(true);
        self.canvas.setPosition(CGPoint::new(-offset.x, -offset.y));
        CATransaction::commit();
    }

    /// Installs the tiles for one animation and draws frame zero.
    ///
    /// Layers are reused across animations for the same window, since creating a `CALayer` and giving
    /// it a bitmap is the expensive part. Tiles absent from `tiles` are removed, or the previous
    /// switch's windows linger as ghosts behind the current one.
    pub fn set_tiles(&mut self, tiles: &[OverlayTile]) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        let mut keep = Vec::with_capacity(tiles.len());
        for tile in tiles {
            keep.push(tile.window);
            let layer = self.tile_layers.entry(tile.window).or_insert_with(|| {
                let layer = CALayer::layer();
                layer.setMasksToBounds(true);
                // SAFETY: Core Animation's own filter-name constants.
                unsafe {
                    layer.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
                    layer.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
                }
                self.root.addSublayer(&layer);
                layer
            });
            layer.setContentsScale(self.scale);
            set_layer_contents(layer, &tile.snapshot);
            layer.setFrame(tile.from);
            // Negated so a smaller depth, meaning nearer the front, draws on top.
            layer.setZPosition(-(tile.depth as f64));
            layer.setHidden(false);
        }

        let stale: Vec<WindowId> =
            self.tile_layers.keys().copied().filter(|w| !keep.contains(w)).collect();
        for window in stale {
            if let Some(layer) = self.tile_layers.remove(&window) {
                layer.removeFromSuperlayer();
            }
        }

        CATransaction::commit();
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
            if let Some(layer) = self.tile_layers.get(&tile.window) {
                layer.setFrame(lerp_rect(tile.from, tile.to, eased));
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
        for (_, layer) in self.tile_layers.drain() {
            layer.removeFromSuperlayer();
        }
        CATransaction::commit();
        let _ = self.mtm;
    }
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

    #[test]
    fn easing_clamps_out_of_range_input() {
        // A time-based driver can hand over t slightly outside 0..1 when a frame is late, and an
        // unclamped cubic would overshoot the target position.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
    }
}