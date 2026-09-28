//! The switcher's popup: one panel, a row of tiles, a highlight.
//!
//! Its own window rather than the animation overlay's, for four reasons each sufficient on its own.
//! The overlay is opaque black across the WHOLE display and is only ever shown with a captured desktop
//! behind it; it sets `ignoresMouseEvents(true)`, which a switcher that takes clicks must not; it sits at
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

use std::cell::RefCell;
use std::rc::Rc;

use rustc_hash::FxHashMap as HashMap;

use objc2::DefinedClass;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameVibrantDark, NSBackingStoreType,
    NSColor, NSEvent, NSPanel, NSRunningApplication, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::NSString;
use objc2_quartz_core::{
    CALayer, CAMediaTimingFunction, CATextLayer, CATransaction, kCAMediaTimingFunctionEaseOut,
};
use tracing::debug;

use crate::animation::platform::overlay::set_layer_contents;
use crate::animation::platform::window_snapshot::WindowSnapshot;
use crate::displays::domain::screen::CoordinateConverter;
use crate::switcher::domain::layout::{Metrics, Strip, lay_out};
use crate::switcher::domain::motion::{GLIDE_SECONDS, glides};
use crate::windows::platform::app::NSRunningApplicationExt;

/// The pop-up menu level, which is where macOS draws its own switcher and its menus.
///
/// Was 21, chosen only to clear rini's animation overlay at 18. That is too low: an application is free
/// to keep a window at the floating (3), modal (8) or status (25) level, and at 21 the popup went under
/// anything at 25 or above. A switcher that can be covered is a switcher that cannot be read.
const PANEL_LEVEL: isize = 101;

// Colours and radii from the Okibi design system, dark theme. Named tokens rather than chosen values:
// "Nothing in a UI is a one-off colour; every surface and tone is a step."
//
// The panel is a floating surface, which the elevation law puts on the content plane with a hairline:
// "Bars/sidebars on --n1, content on --n2", and "Shadows are for floating things: windows, popovers".

/// `--n0`, the deepest step on the spine.
///
/// The tint over the blur. Two steps below the content plane on purpose: this panel floats OVER content
/// rather than being content, and the darkening has to be done with the COLOUR rather than with opacity,
/// because opacity is what the blur needs left over to be visible at all. `--n1` at 0.62 was the
/// previous answer and it read as an opaque slab.
const N0: (f64, f64, f64) = (0.059, 0.067, 0.075);
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

/// The panel's corner radius.
///
/// A DEPARTURE from the design system, asked for and worth recording. Its geometry tops out at
/// `--radius-card` 7px with the rule "corners stay crisp; only pills/circles fully round", which is
/// right for cards in a document. This is not a card: it is a floating macOS panel, and macOS's own
/// floating surfaces — Spotlight, the volume HUD, a popover — are rounded far more than 7px. Matching
/// the platform reads as correct here in a way that matching the document system does not.
const CORNER: f64 = 26.0;
/// The tiles' radius, and a departure for the same reason as the panel's.
///
/// `--radius-control` is 5px, which is a button's radius. A tile here is a picture of a window, and the
/// windows it is a picture of are themselves rounded at about 10px by macOS — so a square-cornered tile
/// reads as a screenshot of a window rather than as a window.
const TILE_CORNER: f64 = 11.0;
/// The ember ring around the selected row.
///
/// The elevation law's default for an active ROW in a list is a soft fill plus a 2px inset bar at its
/// left edge, which is what this was. A switcher row is not a list row: it is a focus target, and the
/// ember's own remit covers "focused borders" as well as active bars. A whole outline says "this is
/// the one" about a tile; an edge bar says "this is the current line" about a list.
const FOCUS_RING: f64 = 2.0;
/// The tint laid over the blurred backing.
///
/// Not a fill: the panel is blurred by an `NSVisualEffectView` behind this layer, and an opaque layer
/// on top would hide it entirely. So this is a wash that darkens the blur rather than replacing it —
/// the `HUDWindow` material is already dark, and this takes it the rest of the way.
///
/// The spec has no token for an overlay's translucency, so the number is a judgement. It has been 0.78
/// and 0.90 as flat FILLS, and 0.62 over the blur — where it was still high enough that the blur was
/// doing nothing visible, reported as the blur not working. A dark material under a 0.62 near-black wash
/// leaves about a tenth of the backdrop, which is indistinguishable from an opaque slab.
///
/// So the darkening moved to the colour: `--n0` instead of `--n1`, at an opacity low enough that the
/// blur is the thing you see. Same intent as the native switcher, a step darker.
const PANEL_ALPHA: f64 = 0.30;

/// The app icon badged into a tile's corner. Small enough to read as a cue rather than as content,
/// large enough to tell two apps apart at a glance.
const ICON: f64 = 38.0;
/// How far the badge sits inside the tile's corner, so it reads as on top of the picture rather than
/// as part of it.
const ICON_INSET: f64 = 7.0;

/// Where to send a click on a row: the window it was drawn for.
pub type OnPick = Rc<dyn Fn(rini_core::ids::WindowId)>;

/// Where the rows are and which window each one is, as last drawn, for the view's hit test.
#[derive(Default)]
struct Hits {
    strip: Option<Strip>,
    windows: Vec<rini_core::ids::WindowId>,
}

struct ViewIvars {
    hits: RefCell<Hits>,
    on_pick: OnPick,
}

define_class!(
    /// Top-left origin, so the layer tree agrees with the geometry in `domain::layout`, and a click lands
    /// in the same coordinates the rows were laid out in.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniSwitcherView"]
    #[ivars = ViewIvars]
    struct SwitcherView;

    impl SwitcherView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The panel is never the key window, so without this the first click would only be taken as
        /// a request to become key, and the row under it would need a second click.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// A click on a row picks that row's window. A click in a gap or the padding picks nothing,
        /// which `Strip::row_at` decides exactly rather than by nearest row.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let picked = {
                let hits = self.ivars().hits.borrow();
                hits.strip
                    .as_ref()
                    .and_then(|strip| strip.row_at(point))
                    .and_then(|row| hits.windows.get(row).copied())
            };
            if let Some(window) = picked {
                (self.ivars().on_pick)(window);
            }
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

/// What a draw does to the layers' geometry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    /// Put them where they belong, at once. The panel is appearing, or its rows were just rebuilt, so
    /// there is no previous position to travel from.
    Snap,
    /// Travel there. The strip is already up and only the selection and the scroll have moved.
    Glide,
    /// Leave the geometry untouched. This draw is filling in a picture that has arrived, and re-setting
    /// a frame would cut short a glide already running.
    Leave,
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
    /// The blurred backing. Held because it has to be resized with the panel.
    backing: Retained<NSVisualEffectView>,
    view: Retained<SwitcherView>,
    /// One layer per visible row, reused across opens. Rebuilt only when the row count changes, so
    /// stepping the selection moves a highlight rather than tearing down a layer tree.
    tiles: Vec<Retained<CALayer>>,
    captions: Vec<Retained<CATextLayer>>,
    /// One badge per row, in front of its tile.
    icons: Vec<Retained<CALayer>>,
    /// The `--ember-soft` wash, UNDER the tiles: it tints a row whose picture has not arrived yet.
    highlight: Retained<CALayer>,
    /// The ember ring, OVER the tiles. Its own layer because the wash cannot follow it there — a fill
    /// drawn over a tile would hide the picture it is marking.
    ring: Retained<CALayer>,
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
    pub fn new(mtm: MainThreadMarker, on_pick: OnPick) -> Option<Self> {
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
        // Takes clicks without activating rini, which `NonactivatingPanel` is for: rini is an Accessory
        // application, and activating it would deactivate whatever is being switched away from. And
        // never becomes the key window for one, or the keys typed next would go to the panel rather
        // than to the window the click just committed to.
        window.setIgnoresMouseEvents(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setLevel(PANEL_LEVEL);
        // FullScreenAuxiliary, NOT FullScreenNone. They sound like the same statement — this window is
        // never itself full screen — but FullScreenNone also means the window is never shown ON a full
        // screen space. With an application in native full screen, that made the popup impossible to
        // see: the switch worked and nothing appeared. Auxiliary is the one that says "not full screen
        // itself, but allowed to sit over one".
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        // Vibrant dark, pinned rather than inherited. The material's own colour comes from the
        // appearance, so on a machine in light mode an inherited appearance would render a LIGHT frosted
        // panel under a dark tint — the two fighting, and neither winning. Vibrancy is also what the
        // blur samples through.
        if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameVibrantDark }) {
            window.setAppearance(Some(&dark));
        }

        // A blurred backing rather than a flat translucent fill. `HUDWindow` is the material macOS uses
        // for exactly this kind of floating panel, and `BehindWindow` is what makes it sample the
        // desktop rather than its own siblings. `Active` so it stays blurred while rini is not the
        // frontmost application — which it never is, being an Accessory app, so the default
        // `FollowsWindowActiveState` would leave the material flat.
        let backing = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        backing.setMaterial(NSVisualEffectMaterial::HUDWindow);
        backing.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        backing.setState(NSVisualEffectState::Active);
        backing.setWantsLayer(true);
        if let Some(layer) = backing.layer() {
            // The blur has to be clipped to the same rounded rect as the panel, or its square corners
            // show through underneath the layer tree's rounded ones.
            layer.setCornerRadius(CORNER);
            layer.setMasksToBounds(true);
        }
        window.setContentView(Some(&backing));

        let view = SwitcherView::alloc(mtm).set_ivars(ViewIvars {
            hits: RefCell::new(Hits::default()),
            on_pick,
        });
        let view: Retained<SwitcherView> = unsafe { msg_send![super(view), initWithFrame: frame] };
        view.setWantsLayer(true);
        backing.addSubview(&view);

        let root = view.layer()?;
        let scale = backing_scale();
        root.setContentsScale(scale);
        root.setCornerRadius(CORNER);
        root.setMasksToBounds(true);
        root.setBackgroundColor(Some(&token(N0, PANEL_ALPHA)));
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

        // Over everything: the tiles are at 1.0, the badges at 1.5, the captions at 2.0. A ring drawn
        // under the tiles is clipped away by them on every side except where the 4pt margin shows, which
        // reads as a glow rather than as an outline.
        let ring = CALayer::layer();
        ring.setAnchorPoint(CGPoint::new(0.0, 0.0));
        ring.setContentsScale(scale);
        ring.setCornerRadius(TILE_CORNER + 3.0);
        ring.setZPosition(3.0);
        ring.setHidden(true);
        ring.setBorderWidth(FOCUS_RING);
        ring.setBorderColor(Some(&token(EMBER, 1.0)));
        root.addSublayer(&ring);

        Some(Self {
            window,
            backing,
            view,
            tiles: Vec::new(),
            captions: Vec::new(),
            icons: Vec::new(),
            highlight,
            ring,
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
        // Read BEFORE `rebuild_rows`, which is what makes the layers new and so makes their positions
        // meaningless to travel from.
        let showing = self.visible.then_some(self.tiles.len());

        self.window.setFrame_display(cocoa, false);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), strip.panel.size);
        self.backing.setFrame(bounds);
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
        let movement = if glides(showing, rows.len()) {
            Move::Glide
        } else {
            Move::Snap
        };
        self.place(&strip, rows, selected, movement);
        self.view.ivars().hits.replace(Hits {
            strip: Some(strip.clone()),
            windows: rows.iter().map(|row| row.window).collect(),
        });
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
        // Geometry untouched: a picture arriving mid-step must not cut short the glide already running.
        self.place(&last.strip, &last.rows, last.selected, Move::Leave);
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
        self.view.ivars().hits.take();
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

    fn place(&mut self, strip: &Strip, rows: &[Row], selected: usize, movement: Move) {
        // Resolved before the drawing loops, which borrow the layer vectors: caching an icon needs
        // `&mut self` and a loop cannot hold both.
        let badges: Vec<Option<Retained<objc2_core_graphics::CGImage>>> =
            rows.iter().map(|row| self.app_icon(row.window.pid)).collect();

        self.draw_contents(rows, &badges);
        if movement != Move::Leave {
            self.move_layers(strip, rows, selected, movement);
        }
    }

    /// What each row SHOWS. Never animated.
    ///
    /// Setting `contents` on a layer cross-fades over about a quarter of a second by default, and these
    /// layers are REUSED across switches: a tile that held the previous switch's window fades from that
    /// picture into this one. With the two most recent windows trading places between one switch and the
    /// next — which they do, because the list is ordered by focus — two adjacent tiles cross-fade into
    /// each other's pictures, and the strip looks like it is shuffling itself after it has already
    /// appeared. Reported as exactly that.
    fn draw_contents(
        &self,
        rows: &[Row],
        badges: &[Option<Retained<objc2_core_graphics::CGImage>>],
    ) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (index, row) in rows.iter().enumerate() {
            if let Some(tile) = self.tiles.get(index) {
                tile.setOpacity(if row.is_minimized { 0.45 } else { 1.0 });
                match self.pictures.get(&row.window) {
                    Some(snapshot) => {
                        // Scaled to fit inside the tile rather than cropped: a cropped thumbnail of a
                        // browser is a rectangle of text.
                        tile.setContentsGravity(unsafe {
                            objc2_quartz_core::kCAGravityResizeAspect
                        });
                        set_layer_contents(tile, snapshot);
                    }
                    // Left as the placeholder slab. A row with no picture still reads as a row.
                    None => unsafe { tile.setContents(None) },
                }
            }
            if let Some(badge) = self.icons.get(index) {
                match badges.get(index).and_then(|icon| icon.clone()) {
                    Some(image) => {
                        let raw: *const objc2_core_graphics::CGImage = &*image;
                        unsafe {
                            let _: () = msg_send![&**badge, setContents: raw];
                        }
                        badge.setHidden(false);
                    }
                    None => badge.setHidden(true),
                }
            }
            if let Some(caption) = self.captions.get(index) {
                let text = caption_text(row);
                unsafe {
                    caption.setString(Some(&*NSString::from_str(&text)));
                }
            }
        }
        CATransaction::commit();
    }

    /// Where each row SITS, and where the selection's ring sits.
    ///
    /// Animated when the strip is only moving, which is what makes the ring travel to the next window
    /// and the strip scroll under it rather than both teleporting. Ease-out, because the eye needs to
    /// see where the ring left from and does not care how it arrives.
    fn move_layers(&self, strip: &Strip, rows: &[Row], selected: usize, movement: Move) {
        CATransaction::begin();
        CATransaction::setDisableActions(movement == Move::Snap);
        if movement == Move::Glide {
            CATransaction::setAnimationDuration(GLIDE_SECONDS);
            let ease =
                CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionEaseOut });
            CATransaction::setAnimationTimingFunction(Some(&ease));
        }
        for (index, rect) in strip.rows.iter().enumerate() {
            if rows.get(index).is_none() {
                continue;
            }
            // The row's own width: each tile is as wide as its window is in proportion.
            if let Some(tile) = self.tiles.get(index) {
                tile.setFrame(CGRect::new(
                    rect.origin,
                    CGSize::new(rect.size.width, self.metrics.tile_height),
                ));
            }
            // Bottom-left of the picture: away from a window's own controls, which sit top-left, and
            // away from the caption below.
            if let Some(badge) = self.icons.get(index) {
                badge.setFrame(CGRect::new(
                    CGPoint::new(
                        rect.origin.x + ICON_INSET,
                        rect.origin.y + self.metrics.tile_height - ICON - ICON_INSET,
                    ),
                    CGSize::new(ICON, ICON),
                ));
            }
            // Exactly the caption band, so the text's own box has no slack to read as extra padding.
            if let Some(caption) = self.captions.get(index) {
                caption.setFrame(CGRect::new(
                    CGPoint::new(
                        rect.origin.x,
                        rect.origin.y + self.metrics.tile_height + self.metrics.caption_gap,
                    ),
                    CGSize::new(rect.size.width, self.metrics.caption),
                ));
            }
        }
        self.place_highlight(strip, selected);
        CATransaction::commit();
    }

    fn place_highlight(&self, strip: &Strip, selected: usize) {
        match strip.rows.get(selected) {
            Some(rect) => {
                let frame = CGRect::new(
                    CGPoint::new(rect.origin.x - 4.0, rect.origin.y - 4.0),
                    CGSize::new(rect.size.width + 8.0, self.metrics.tile_height + 8.0),
                );
                self.highlight.setHidden(false);
                self.highlight.setFrame(frame);
                self.ring.setHidden(false);
                self.ring.setFrame(frame);
            }
            None => {
                self.highlight.setHidden(true);
                self.ring.setHidden(true);
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
