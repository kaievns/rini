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
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, MainThreadMarker};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSDictionary;
use objc2_quartz_core::{CALayer, CATransaction};

use crate::actor::app::WindowId;
use crate::sys::cgs_window::CgsWindow;
use crate::ui::window_snapshot::WindowSnapshot;

/// Window level for the overlay. Managed windows all sit at CG layer 0, so anything above that
/// covers them. `NSPopUpMenuWindowLevel` is 101, which is what `mission_control.rs` already uses,
/// and it stays below the assistive and cursor levels so nothing accessibility-related is hidden.
const OVERLAY_LEVEL: i32 = 101;

/// One window's picture inside the overlay, and where it should be drawn.
pub struct OverlayTile {
    pub window: WindowId,
    /// Where the window starts, in the overlay's coordinate space.
    pub from: CGRect,
    /// Where the window ends up, in the overlay's coordinate space.
    pub to: CGRect,
    pub snapshot: WindowSnapshot,
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
    window: CgsWindow,
    /// Kept alive for as long as the overlay exists: dropping the CAContext unbinds the layer tree
    /// and the overlay goes blank while still being composited.
    _layer_context: Option<Retained<AnyObject>>,
    root: Retained<CALayer>,
    tile_layers: HashMap<WindowId, Retained<CALayer>>,
    frame: CGRect,
    scale: f64,
    visible: bool,
}

impl WorkspaceOverlay {
    /// Creates the overlay once, ordered in but fully transparent.
    ///
    /// `frame` must be the display's USABLE frame in top-left coordinates, i.e. excluding the menu
    /// bar strip, so the user's bar stays visible during an animation.
    pub fn new(frame: CGRect, scale: f64, _mtm: MainThreadMarker) -> Option<Self> {
        let root = CALayer::layer();
        // Top-left origin, matching how rini reasons about display and window frames everywhere
        // else. Without this every tile would be positioned upside down.
        root.setGeometryFlipped(true);
        root.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        root.setContentsScale(scale);

        let window = CgsWindow::new(frame).ok()?;
        let _ = window.set_resolution(scale);
        // Opaque: the overlay's whole job is to hide the real windows being repositioned underneath.
        let _ = window.set_opacity(true);
        let _ = window.set_level(OVERLAY_LEVEL);
        // Ordered in immediately and left that way. Alpha is the show/hide mechanism because
        // ordering costs a frame each way and alpha costs nothing.
        let _ = window.set_alpha(0.0);
        let _ = window.order_above(None);

        let layer_context = Self::host_layer(&window, &root);

        Some(Self {
            window,
            _layer_context: layer_context,
            root,
            tile_layers: HashMap::new(),
            frame,
            scale,
            visible: false,
        })
    }

    /// Binds a Core Animation layer tree into a raw window server window, via a remote CAContext.
    /// Same mechanism `mission_control.rs` uses.
    fn host_layer(window: &CgsWindow, root: &CALayer) -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"CAContext")?;
        let options = NSDictionary::<AnyObject, AnyObject>::new();
        unsafe {
            let raw: *mut AnyObject = msg_send![class, remoteContextWithOptions: &*options];
            let context = Retained::retain_autoreleased(raw)?;
            let _: () = msg_send![&*context, setLayer: root];
            window.bind_layer_context(Retained::as_ptr(&context).cast_mut().cast()).ok()?;
            CATransaction::flush();
            Some(context)
        }
    }

    pub fn frame(&self) -> CGRect {
        self.frame
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Repoints the overlay at a different display, or the same one after a resolution change.
    /// Cheaper than recreating it, which would mean paying the ~112ms window creation cost again.
    pub fn set_frame(&mut self, frame: CGRect, scale: f64) {
        if self.frame == frame && (self.scale - scale).abs() < f64::EPSILON {
            return;
        }
        self.frame = frame;
        self.scale = scale;
        let _ = self.window.set_shape(frame);
        let _ = self.window.set_resolution(scale);
        self.root.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
        self.root.setContentsScale(scale);
    }

    /// Installs the tiles for one animation and draws frame zero.
    ///
    /// Layers are reused across animations where the window is the same, since creating a `CALayer`
    /// and handing it a bitmap is the expensive part. Tiles absent from `tiles` are removed.
    pub fn set_tiles(&mut self, tiles: &[OverlayTile]) {
        // Batched so no half-built frame is ever presented.
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        let mut keep = Vec::with_capacity(tiles.len());
        for tile in tiles {
            keep.push(tile.window);
            let layer = self.tile_layers.entry(tile.window).or_insert_with(|| {
                let layer = CALayer::layer();
                layer.setGeometryFlipped(true);
                // Nearest-neighbour would shimmer while moving; the bitmap is already at device
                // scale so the filter only matters during the sub-pixel steps of the slide.
                // SAFETY: these are Core Animation's own filter-name string constants.
                unsafe {
                    layer.setMagnificationFilter(objc2_quartz_core::kCAFilterLinear);
                    layer.setMinificationFilter(objc2_quartz_core::kCAFilterLinear);
                }
                layer.setMasksToBounds(true);
                self.root.addSublayer(&layer);
                layer
            });
            layer.setContentsScale(self.scale);
            unsafe {
                let img: *const objc2_core_graphics::CGImage = &*tile.snapshot.image;
                let _: () = msg_send![&**layer, setContents: img];
            }
            layer.setFrame(tile.from);
            layer.setHidden(false);
        }

        // Anything not in this animation must stop being drawn, or the previous switch's windows
        // linger as ghosts behind the current one.
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
    /// The single transaction is the whole point: it is what makes tearing between windows
    /// impossible, rather than merely unlikely as it was with per-window Accessibility writes.
    pub fn draw_frame(&self, tiles: &[OverlayTile], t: f64) {
        let eased = ease_out_cubic(t);
        CATransaction::begin();
        // Implicit animations must be off. Core Animation would otherwise add its own quarter-second
        // ease to every frame we set, so our interpolation would fight a second one and the result
        // would lag behind the input by a fixed amount.
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
        let _ = self.window.set_alpha(1.0);
        // Ordering above nothing raises it over every other window without naming one, which avoids
        // needing the ordering privilege that yabai's scripting addition exists to provide.
        let _ = self.window.order_above(None);
        self.visible = true;
    }

    /// Hides the overlay, revealing the real windows already sitting at their final frames.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        let _ = self.window.set_alpha(0.0);
        self.visible = false;
    }

    /// Frees the bitmaps without destroying the overlay, so an idle rini is not holding tens of MB
    /// of window pictures. Each tile is a full-resolution image.
    pub fn release_tiles(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (_, layer) in self.tile_layers.drain() {
            layer.removeFromSuperlayer();
        }
        CATransaction::commit();
    }
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
