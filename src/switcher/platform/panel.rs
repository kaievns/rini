//! The switcher's popup: one panel, a row of tiles, a highlight.
//!
//! Its own window rather than the animation overlay's, for four reasons each sufficient on its own.
//! The overlay is opaque black across the WHOLE display and is only ever shown with a captured desktop
//! behind it; it sets `ignoresMouseEvents(true)`, which a switcher eventually must not; it sits at
//! level 18 under a layer tree keyed by flight groups; and it is shown by fading its alpha, which a
//! window that accepts clicks cannot do — an alpha-0 window still hit-tests, so the overlay's trick
//! would leave a permanent invisible click-eating rectangle wherever the strip sits.
//!
//! An `NSPanel` with `NonactivatingPanel`, not a plain `NSWindow`. rini runs as an Accessory
//! application, so a window that takes a click ACTIVATES rini and deactivates the application being
//! switched away from — inverting the one thing a switcher exists to do.
//!
//! Created once and kept, because creating a window costs about 112ms against 14ms to order one in.
//! Shown by ordering in and out rather than by alpha, for the hit-testing reason above.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::NSString;
use objc2_quartz_core::{CALayer, CATextLayer};
use tracing::debug;

use crate::displays::domain::screen::CoordinateConverter;
use crate::switcher::domain::layout::{Metrics, Strip, lay_out};

/// Above the animation overlay's 18, so a switch opened mid-flight is not drawn behind the tiles it is
/// offering.
const PANEL_LEVEL: isize = 21;

const CORNER: f64 = 14.0;
const TILE_CORNER: f64 = 6.0;

define_class!(
    /// Top-left origin, so the layer tree agrees with the geometry in `domain::layout`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniSwitcherView"]
    struct SwitcherView;

    impl SwitcherView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// One row to draw.
pub struct Row {
    pub title: String,
    pub app_name: String,
    pub is_minimized: bool,
}

/// The popup, alive for the lifetime of the process and ordered in only while a switch is open.
pub struct SwitcherPanel {
    window: Retained<NSPanel>,
    view: Retained<SwitcherView>,
    /// One layer per visible row, reused across opens. Rebuilt only when the row count changes, so
    /// stepping the selection moves a highlight rather than tearing down a layer tree.
    tiles: Vec<Retained<CALayer>>,
    captions: Vec<Retained<CATextLayer>>,
    highlight: Retained<CALayer>,
    metrics: Metrics,
    visible: bool,
    scale: f64,
}

impl SwitcherPanel {
    pub fn new(mtm: MainThreadMarker) -> Option<Self> {
        // A placeholder frame: every open recomputes it from the screen it is showing on.
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(600.0, 200.0));
        let window: Retained<NSPanel> = unsafe {
            msg_send![
                NSPanel::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };

        window.setOpaque(false);
        let clear = NSColor::clearColor();
        window.setBackgroundColor(Some(&clear));
        window.setHasShadow(true);
        // Clicks come later, and until they do the panel must not be able to take one: rini is an
        // Accessory application, so a click would activate it and deactivate whatever is being
        // switched away from.
        window.setIgnoresMouseEvents(true);
        window.setLevel(PANEL_LEVEL);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenNone,
        );

        let view: Retained<SwitcherView> =
            unsafe { msg_send![SwitcherView::alloc(mtm), initWithFrame: frame] };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));

        let root = view.layer()?;
        let scale = backing_scale();
        root.setContentsScale(scale);
        root.setCornerRadius(CORNER);
        root.setMasksToBounds(true);
        root.setBackgroundColor(Some(
            &NSColor::colorWithSRGBRed_green_blue_alpha(0.11, 0.11, 0.13, 0.94).CGColor(),
        ));

        // Under the tiles, so a tile's picture is never hidden by its own highlight.
        let highlight = CALayer::layer();
        highlight.setAnchorPoint(CGPoint::new(0.0, 0.0));
        highlight.setContentsScale(scale);
        highlight.setCornerRadius(TILE_CORNER + 3.0);
        highlight.setZPosition(0.0);
        highlight.setHidden(true);
        highlight.setBackgroundColor(Some(
            &NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.22).CGColor(),
        ));
        root.addSublayer(&highlight);

        Some(Self {
            window,
            view,
            tiles: Vec::new(),
            captions: Vec::new(),
            highlight,
            metrics: Metrics::default(),
            visible: false,
            scale,
        })
    }

    /// Show the switch, or move the highlight if it is already showing.
    ///
    /// `screen` is the display to centre on, in CoreGraphics coordinates.
    pub fn show(&mut self, rows: &[Row], selected: usize, screen: CGRect) {
        let Some(strip) = lay_out(rows.len(), selected, screen, self.metrics) else {
            self.hide();
            return;
        };

        let converter = CoordinateConverter::from_height(primary_display_height());
        let Some(cocoa) = converter.convert_rect(strip.panel) else {
            debug!("switcher panel has no cocoa frame; not showing");
            return;
        };
        self.window.setFrame_display(cocoa, false);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), strip.panel.size);
        self.view.setFrame(bounds);
        if let Some(root) = self.view.layer() {
            root.setFrame(bounds);
        }

        self.rebuild_rows(rows.len());
        self.place(&strip, rows, selected);

        if !self.visible {
            self.window.orderFrontRegardless();
            self.visible = true;
        }
    }

    /// Order the panel out. Ordering rather than fading: an alpha-0 window still hit-tests, and this
    /// one will accept clicks.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.window.orderOut(None);
        self.visible = false;
    }

    /// Match the layer count to the row count, reusing what is already there.
    ///
    /// Rebuilt only on a change, so stepping the selection moves a highlight instead of tearing down
    /// and rebuilding a layer tree on every keypress.
    fn rebuild_rows(&mut self, count: usize) {
        if self.tiles.len() == count {
            return;
        }
        let Some(root) = self.view.layer() else {
            return;
        };
        for tile in self.tiles.drain(..) {
            tile.removeFromSuperlayer();
        }
        for caption in self.captions.drain(..) {
            caption.removeFromSuperlayer();
        }
        for _ in 0..count {
            let tile = CALayer::layer();
            tile.setAnchorPoint(CGPoint::new(0.0, 0.0));
            tile.setContentsScale(self.scale);
            tile.setCornerRadius(TILE_CORNER);
            tile.setMasksToBounds(true);
            tile.setZPosition(1.0);
            // A placeholder until the snapshot cache is read: a flat slab, so a row with no picture
            // still reads as a row rather than as a hole.
            tile.setBackgroundColor(Some(
                &NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.08).CGColor(),
            ));
            root.addSublayer(&tile);
            self.tiles.push(tile);

            let caption = CATextLayer::layer();
            caption.setAnchorPoint(CGPoint::new(0.0, 0.0));
            caption.setContentsScale(self.scale);
            caption.setZPosition(2.0);
            unsafe {
                caption.setFontSize(11.0);
                caption.setAlignmentMode(objc2_quartz_core::kCAAlignmentCenter);
                caption.setTruncationMode(objc2_quartz_core::kCATruncationEnd);
                caption.setForegroundColor(Some(&NSColor::whiteColor().CGColor()));
                // No `setFont`. CATextLayer's `font` is a CFTypeRef taking a name, a CGFont or a
                // CTFont, and none of the three bridges from an NSFont without a cast this file would
                // be the only place in the tree to need. The default face at this size is legible, and
                // a font is a thing to choose once the panel is worth polishing.
            }
            root.addSublayer(&caption);
            self.captions.push(caption);
        }
        debug!(count, "switcher panel rebuilt its rows");
    }

    fn place(&self, strip: &Strip, rows: &[Row], selected: usize) {
        for (index, rect) in strip.rows.iter().enumerate() {
            let Some(tile) = self.tiles.get(index) else { continue };
            let Some(caption) = self.captions.get(index) else {
                continue;
            };
            let Some(row) = rows.get(index) else { continue };

            let picture = CGRect::new(rect.origin, self.metrics.tile);
            tile.setFrame(picture);
            tile.setOpacity(if row.is_minimized { 0.45 } else { 1.0 });

            caption.setFrame(CGRect::new(
                CGPoint::new(rect.origin.x, rect.origin.y + self.metrics.tile.height + 4.0),
                CGSize::new(self.metrics.tile.width, self.metrics.caption - 6.0),
            ));
            let text = caption_text(row);
            unsafe {
                caption.setString(Some(&*NSString::from_str(&text)));
            }
        }

        match strip.rows.get(selected) {
            Some(rect) => {
                self.highlight.setHidden(false);
                self.highlight.setFrame(CGRect::new(
                    CGPoint::new(rect.origin.x - 4.0, rect.origin.y - 4.0),
                    CGSize::new(self.metrics.tile.width + 8.0, self.metrics.tile.height + 8.0),
                ));
            }
            None => self.highlight.setHidden(true),
        }
    }
}

/// Two lines: the application, then the window's own title.
///
/// The application first because it is the coarse thing the eye lands on, and a window title is often
/// a file path whose useful end is truncated away.
fn caption_text(row: &Row) -> String {
    let title = row.title.trim();
    if title.is_empty() {
        return row.app_name.clone();
    }
    format!("{}\n{}", row.app_name, title)
}

fn primary_display_height() -> f64 {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    bounds.origin.y + bounds.size.height
}

/// The backing scale to draw at.
///
/// 2.0 rather than a per-display read, matching what the animation path already assumes
/// (`publish_animation_display_for` hardcodes it). Drawing a retina panel at 1.0 is soft; drawing a
/// 1x panel at 2.0 costs memory and nothing else, so the safe direction is up.
fn backing_scale() -> f64 {
    2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(app: &str, title: &str) -> Row {
        Row {
            title: title.to_owned(),
            app_name: app.to_owned(),
            is_minimized: false,
        }
    }

    #[test]
    fn a_caption_names_the_application_then_the_window() {
        assert_eq!(
            caption_text(&row("Ghostty", "~/projects/rini")),
            "Ghostty\n~/projects/rini"
        );
    }

    /// A window with no title — which happens while one is still opening — must not draw a blank line
    /// under the tile.
    #[test]
    fn a_window_with_no_title_shows_just_the_application() {
        assert_eq!(caption_text(&row("Slack", "")), "Slack");
        assert_eq!(caption_text(&row("Slack", "   ")), "Slack");
    }
}
