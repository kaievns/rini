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

use rustc_hash::FxHashMap as HashMap;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSRunningApplication, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::NSString;
use objc2_quartz_core::{CALayer, CATextLayer, CATransaction};
use tracing::debug;

use crate::animation::platform::overlay::set_layer_contents;
use crate::animation::platform::window_snapshot::WindowSnapshot;
use crate::displays::domain::screen::CoordinateConverter;
use crate::switcher::domain::layout::{Metrics, Strip, lay_out};
use crate::windows::platform::app::NSRunningApplicationExt;

/// Above the animation overlay's 18, so a switch opened mid-flight is not drawn behind the tiles it is
/// offering.
const PANEL_LEVEL: isize = 21;

// Colours and radii from the Okibi design system, dark theme. Named tokens rather than chosen values:
// "Nothing in a UI is a one-off colour; every surface and tone is a step."
//
// The panel is a floating surface, which the elevation law puts on the content plane with a hairline:
// "Bars/sidebars on --n1, content on --n2", and "Shadows are for floating things: windows, popovers".

/// `--n2`, the content plane. The panel is the working surface, and the law is that the working
/// surface is the brightest plane.
const N2: (f64, f64, f64) = (0.106, 0.118, 0.125);
/// `--n3`, raised. A tile with no picture yet is a surface sitting on the panel.
const N3: (f64, f64, f64) = (0.133, 0.145, 0.153);
/// `--line`, the hairline. "Borders are soft visible hairlines — never bright, never
/// darker-than-panel voids."
const LINE: (f64, f64, f64) = (0.184, 0.196, 0.208);
/// `--n11`, primary text.
const N11: (f64, f64, f64) = (0.882, 0.890, 0.898);
/// `--ember`. Owns selection and active indicators, on a budget of one or two appearances per screen —
/// here it is exactly one: the selected row.
const EMBER: (f64, f64, f64) = (1.0, 0.486, 0.314);
/// `--ember-soft`, the specified fill for an active row.
const EMBER_SOFT: (f64, f64, f64) = (0.247, 0.176, 0.157);

/// `--radius-card`, 7px. "Corners stay crisp; only pills/circles fully round" — so the panel gets the
/// card radius rather than something rounder.
const CORNER: f64 = 7.0;
/// `--radius-control`, 5px, for the smaller surfaces inside it.
const TILE_CORNER: f64 = 5.0;
/// The 2px inset bar an active row carries, per the elevation law.
const ACTIVE_BAR: f64 = 2.0;
/// The panel's fill opacity. The spec has no token for an overlay's translucency, so this is the one
/// value here that is a judgement rather than a token.
const PANEL_ALPHA: f64 = 0.78;

/// The app icon badged into a tile's corner. Small enough to read as a cue rather than as content,
/// large enough to tell two apps apart at a glance.
const ICON: f64 = 38.0;
/// How far the badge sits inside the tile's corner, so it reads as on top of the picture rather than
/// as part of it.
const ICON_INSET: f64 = 7.0;

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

/// A token as a `CGColor`, at `alpha`.
fn token(rgb: (f64, f64, f64), alpha: f64) -> Retained<objc2_core_graphics::CGColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(rgb.0, rgb.1, rgb.2, alpha).CGColor()
}

/// One row to draw.
#[derive(Clone)]
pub struct Row {
    pub window: rini_core::ids::WindowId,
    /// The window's size on screen, which decides how wide its tile is.
    pub size: CGSize,
    pub title: String,
    pub app_name: String,
    pub is_minimized: bool,
}

#[derive(Clone)]
struct LastDraw {
    strip: Strip,
    rows: Vec<Row>,
    selected: usize,
}

/// The popup, alive for the lifetime of the process and ordered in only while a switch is open.
pub struct SwitcherPanel {
    window: Retained<NSPanel>,
    view: Retained<SwitcherView>,
    /// One layer per visible row, reused across opens. Rebuilt only when the row count changes, so
    /// stepping the selection moves a highlight rather than tearing down a layer tree.
    tiles: Vec<Retained<CALayer>>,
    captions: Vec<Retained<CATextLayer>>,
    /// One badge per row, in front of its tile.
    icons: Vec<Retained<CALayer>>,
    highlight: Retained<CALayer>,
    active_bar: Retained<CALayer>,
    metrics: Metrics,
    visible: bool,
    scale: f64,
    /// What was last drawn, so a picture arriving after the popup is already up can be drawn without
    /// the reactor being asked to send the rows again.
    last: Option<LastDraw>,
    /// App icons already read, by pid.
    ///
    /// Cached because reading one goes out to the application bundle, and a switch redraws on every
    /// step. `None` is cached too: an application with no icon must not be asked again on each redraw.
    app_icons: HashMap<rini_core::ids::pid_t, Option<Retained<objc2_core_graphics::CGImage>>>,
    /// Pictures handed over by the animation engine, by window.
    ///
    /// Kept across opens: a picture that was good enough to draw last time is still better than a grey
    /// box, and the engine only ever sends more.
    pictures: HashMap<rini_core::ids::WindowId, WindowSnapshot>,
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
        root.setBackgroundColor(Some(&token(N2, PANEL_ALPHA)));
        root.setBorderWidth(1.0);
        root.setBorderColor(Some(&token(LINE, 1.0)));

        // Under the tiles, so a tile's picture is never hidden by its own highlight.
        let highlight = CALayer::layer();
        highlight.setAnchorPoint(CGPoint::new(0.0, 0.0));
        highlight.setContentsScale(scale);
        highlight.setCornerRadius(TILE_CORNER + 3.0);
        highlight.setZPosition(0.0);
        highlight.setHidden(true);
        highlight.setBackgroundColor(Some(&token(EMBER_SOFT, 1.0)));
        root.addSublayer(&highlight);

        // The 2px inset ember bar the elevation law pairs with an ember-soft fill. A separate layer so
        // it can sit at the selection's left edge whatever width that row is.
        let active_bar = CALayer::layer();
        active_bar.setAnchorPoint(CGPoint::new(0.0, 0.0));
        active_bar.setContentsScale(scale);
        active_bar.setZPosition(2.5);
        active_bar.setHidden(true);
        active_bar.setBackgroundColor(Some(&token(EMBER, 1.0)));
        root.addSublayer(&active_bar);

        Some(Self {
            window,
            view,
            tiles: Vec::new(),
            captions: Vec::new(),
            icons: Vec::new(),
            highlight,
            active_bar,
            metrics: Metrics::default(),
            visible: false,
            scale,
            pictures: HashMap::default(),
            app_icons: HashMap::default(),
            last: None,
        })
    }

    /// Show the switch, or move the highlight if it is already showing.
    ///
    /// `screen` is the display to centre on, in CoreGraphics coordinates.
    pub fn show(&mut self, rows: &[Row], selected: usize, screen: CGRect) {
        let sizes: Vec<CGSize> = rows.iter().map(|row| row.size).collect();
        let Some(strip) = lay_out(&sizes, selected, screen, self.metrics) else {
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
        // The root's frame and any freshly built row layers, for the same no-implicit-animation reason
        // as `place`: a panel that changes size between switches would otherwise slide into its new
        // bounds while its rows are already being drawn at the new ones.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(root) = self.view.layer() {
            root.setFrame(bounds);
        }
        self.rebuild_rows(rows.len());
        CATransaction::commit();
        self.place(&strip, rows, selected);
        self.last = Some(LastDraw {
            strip,
            rows: rows.to_vec(),
            selected,
        });

        if !self.visible {
            self.window.orderFrontRegardless();
            self.visible = true;
        }
    }

    /// Draw the strip again with whatever is now known, if the panel is up.
    ///
    /// The pictures arrive after the popup has already appeared — the engine is asked on the open and
    /// answers a moment later — so without this the first open of a switch would stay grey.
    pub fn redraw(&mut self) {
        if !self.visible {
            return;
        }
        let Some(last) = self.last.clone() else {
            return;
        };
        self.place(&last.strip, &last.rows, last.selected);
    }

    /// Take the pictures the animation engine holds, and redraw if the panel is up.
    pub fn set_pictures(&mut self, pictures: Vec<(rini_core::ids::WindowId, WindowSnapshot)>) {
        for (window, snapshot) in pictures {
            self.pictures.insert(window, snapshot);
        }
    }

    /// Whether a window already has a picture, so the caller knows what is worth warming.
    pub fn has_picture(&self, window: rini_core::ids::WindowId) -> bool {
        self.pictures.contains_key(&window)
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
        for icon in self.icons.drain(..) {
            icon.removeFromSuperlayer();
        }
        for _ in 0..count {
            let tile = CALayer::layer();
            tile.setAnchorPoint(CGPoint::new(0.0, 0.0));
            tile.setContentsScale(self.scale);
            tile.setCornerRadius(TILE_CORNER);
            tile.setMasksToBounds(true);
            tile.setZPosition(1.0);
            // The raised plane, so a row with no picture yet reads as a surface rather than as a hole.
            tile.setBackgroundColor(Some(&token(N3, 1.0)));
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
                // n11, primary text. Both lines of the caption share it: the contrast law asks for
                // n9 or brighter for anything that is reading matter rather than furniture, and one
                // text layer cannot carry two colours. Splitting the app name onto n10 would need a
                // second layer per row, which is a change to make when the panel is worth polishing.
                caption.setForegroundColor(Some(&token(N11, 1.0)));
                // No `setFont`. CATextLayer's `font` is a CFTypeRef taking a name, a CGFont or a
                // CTFont, and none of the three bridges from an NSFont without a cast this file would
                // be the only place in the tree to need. The default face at this size is legible, and
                // a font is a thing to choose once the panel is worth polishing.
            }
            root.addSublayer(&caption);
            self.captions.push(caption);

            let icon = CALayer::layer();
            icon.setAnchorPoint(CGPoint::new(0.0, 0.0));
            icon.setContentsScale(self.scale);
            // In front of the tile, which is at 1.0.
            icon.setZPosition(1.5);
            icon.setHidden(true);
            root.addSublayer(&icon);
            self.icons.push(icon);
        }
        debug!(count, "switcher panel rebuilt its rows");
    }

    /// The application's icon, read once and kept.
    ///
    /// `None` is cached as well as `Some`: an application with no icon, or one that quit between being
    /// listed and being drawn, must not be asked again on every redraw of the strip.
    fn app_icon(
        &mut self,
        pid: rini_core::ids::pid_t,
    ) -> Option<Retained<objc2_core_graphics::CGImage>> {
        if let Some(cached) = self.app_icons.get(&pid) {
            return cached.clone();
        }
        let icon = NSRunningApplication::with_process_id(pid)
            .and_then(|app| app.icon_image(ICON * self.scale));
        self.app_icons.insert(pid, icon.clone());
        icon
    }

    fn place(&mut self, strip: &Strip, rows: &[Row], selected: usize) {
        // Resolved before the drawing loop, which borrows the layer vectors: caching an icon needs
        // `&mut self` and the loop cannot hold both.
        let badges: Vec<Option<Retained<objc2_core_graphics::CGImage>>> =
            rows.iter().map(|row| self.app_icon(row.window.pid)).collect();

        // No implicit animations. Setting `contents` on a layer cross-fades over about a quarter of a
        // second by default, and these layers are REUSED across switches: a tile that held the previous
        // switch's window fades from that picture into this one. With the two most recent windows
        // trading places between one switch and the next — which they do, because the list is ordered
        // by focus — two adjacent tiles cross-fade into each other's pictures, and the strip looks like
        // it is shuffling itself after it has already appeared. Reported as exactly that.
        //
        // The same reason the overlay disables actions everywhere it touches a layer.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (index, rect) in strip.rows.iter().enumerate() {
            let Some(tile) = self.tiles.get(index) else { continue };
            let Some(caption) = self.captions.get(index) else {
                continue;
            };
            let Some(row) = rows.get(index) else { continue };

            // The row's own width: each tile is as wide as its window is in proportion.
            let picture = CGRect::new(
                rect.origin,
                CGSize::new(rect.size.width, self.metrics.tile_height),
            );
            tile.setFrame(picture);
            tile.setOpacity(if row.is_minimized { 0.45 } else { 1.0 });
            // Bottom-left of the picture: away from a window's own controls, which sit top-left, and
            // away from the caption below.
            if let Some(badge) = self.icons.get(index) {
                match badges.get(index).and_then(|icon| icon.clone()) {
                    Some(image) => {
                        badge.setFrame(CGRect::new(
                            CGPoint::new(
                                rect.origin.x + ICON_INSET,
                                rect.origin.y + self.metrics.tile_height - ICON - ICON_INSET,
                            ),
                            CGSize::new(ICON, ICON),
                        ));
                        let raw: *const objc2_core_graphics::CGImage = &*image;
                        unsafe {
                            let _: () = msg_send![&**badge, setContents: raw];
                        }
                        badge.setHidden(false);
                    }
                    None => badge.setHidden(true),
                }
            }

            match self.pictures.get(&row.window) {
                Some(snapshot) => {
                    // The window is wider than the tile, so the picture is scaled to fit inside it
                    // rather than cropped: a cropped thumbnail of a browser is a rectangle of text.
                    tile.setContentsGravity(unsafe { objc2_quartz_core::kCAGravityResizeAspect });
                    set_layer_contents(tile, snapshot);
                }
                // Left as the placeholder slab. A row with no picture still reads as a row.
                None => unsafe { tile.setContents(None) },
            }

            caption.setFrame(CGRect::new(
                CGPoint::new(rect.origin.x, rect.origin.y + self.metrics.tile_height + 4.0),
                CGSize::new(rect.size.width, self.metrics.caption - 6.0),
            ));
            let text = caption_text(row);
            unsafe {
                caption.setString(Some(&*NSString::from_str(&text)));
            }
        }

        self.place_highlight(strip, selected);
        CATransaction::commit();
    }

    fn place_highlight(&self, strip: &Strip, selected: usize) {
        match strip.rows.get(selected) {
            Some(rect) => {
                self.highlight.setHidden(false);
                self.highlight.setFrame(CGRect::new(
                    CGPoint::new(rect.origin.x - 4.0, rect.origin.y - 4.0),
                    CGSize::new(rect.size.width + 8.0, self.metrics.tile_height + 8.0),
                ));
                self.active_bar.setHidden(false);
                self.active_bar.setFrame(CGRect::new(
                    CGPoint::new(rect.origin.x - 4.0, rect.origin.y - 4.0),
                    CGSize::new(ACTIVE_BAR, self.metrics.tile_height + 8.0),
                ));
            }
            None => {
                self.highlight.setHidden(true);
                self.active_bar.setHidden(true);
            }
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
            window: rini_core::ids::WindowId::new(1, 1),
            size: CGSize::new(800.0, 600.0),
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
