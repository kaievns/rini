//! The switcher's popup: one panel, a row of tiles or stacks of them, a highlight.
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

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBackingStoreType, NSColor,
    NSEvent, NSGlassEffectView, NSGlassEffectViewStyle, NSPanel, NSRunningApplication, NSView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_quartz_core::{
    CALayer, CALayerContentsGravity, CAMediaTimingFunction, CATextLayer, CATransaction,
    kCAMediaTimingFunctionEaseOut,
};
use rini_runloop::turns;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::chrome::domain::{card, palette};
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::screen::primary_display_height;
use crate::pictures::platform::layer::{clear_contents, set_contents, set_layer_contents};
use crate::pictures::platform::picture::Picture;
use crate::pictures::platform::service::Pictures;
use crate::switcher::domain::entries::{BEHIND, CardBehind, Entry, cards_behind};
use crate::switcher::domain::layout::{Metrics, Strip, Tile, lay_out};
use crate::switcher::domain::motion::{GLIDE_SECONDS, glides};
use crate::windows::platform::app::{NSRunningApplicationExt, image_file_pixels};

/// The pop-up menu level, which is where macOS draws its own switcher and its menus.
///
/// Above every level an application may keep a window at, floating (3), modal (8) and status (25)
/// among them, and above rini's overlay at 18: a switcher that can be covered cannot be read.
const PANEL_LEVEL: isize = 101;

// Colours and radii from the Okibi design system, dark theme. Named tokens rather than chosen values:
// "Nothing in a UI is a one-off colour; every surface and tone is a step."
//
// The panel is a floating surface, which the elevation law puts on the content plane with a hairline:
// "Bars/sidebars on --n1, content on --n2", and "Shadows are for floating things: windows, popovers".

/// `--n3`, raised. A tile with no picture yet is a surface sitting on the panel.
const N3: (f64, f64, f64) = palette::N3.rgb();
/// `--n11`, primary text.
const N11: (f64, f64, f64) = palette::N11.rgb();
/// `--ember`. Owns selection and active indicators, on a budget of one or two appearances per screen —
/// here it is exactly one: the selected row.
const EMBER: (f64, f64, f64) = palette::EMBER.rgb();
/// `--ember-soft`, the specified fill for an active row.
const EMBER_SOFT: (f64, f64, f64) = palette::EMBER_SOFT.rgb();

/// The panel's corner radius.
///
/// A DEPARTURE from the design system, asked for and worth recording. Its geometry tops out at
/// `--radius-card` 7px with the rule "corners stay crisp; only pills/circles fully round", which is
/// right for cards in a document. This is not a card: it is a floating macOS panel, and macOS's own
/// floating surfaces — Spotlight, the volume HUD, a popover — are rounded far more than 7px. Matching
/// the platform reads as correct here in a way that matching the document system does not.
const CORNER: f64 = 26.0;
/// A tile's default hairline: `--n11` faint enough to read as the window's edge rather than as a frame.
/// Light rather than `--line`, because the tiles are mostly dark windows and a hairline darker than
/// them disappears.
const TILE_EDGE_ALPHA: f64 = 0.16;

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

/// One row to draw: a window, or an application's stack drawn from its top window.
#[derive(Clone)]
pub struct Row {
    /// The top window, which a click on the row commits.
    pub window: rini_core::ids::WindowId,
    /// The top window's size on screen, which decides how wide the row is.
    pub size: CGSize,
    /// The caption's second line.
    pub detail: String,
    pub app_name: String,
    /// An image configured to badge this row in place of its application's own icon.
    pub icon: Option<std::path::PathBuf>,
    /// The windows drawn as cards behind the top one, nearest first. Empty for a lone window.
    pub behind: Vec<rini_core::ids::WindowId>,
}

impl Row {
    pub fn of(entry: &Entry, icon: Option<std::path::PathBuf>) -> Self {
        let front = entry.front();
        Self {
            window: front.window,
            size: front.size,
            detail: entry.detail(),
            app_name: front.app_name.clone(),
            icon,
            behind: entry.behind().iter().map(|candidate| candidate.window).collect(),
        }
    }
}

/// Every window `rows` draw a card of, top cards and those behind, which are the pictures a switch
/// reads and warms; never a window a stack stands for but does not draw.
pub fn drawn_windows(rows: &[Row]) -> Vec<rini_core::ids::WindowId> {
    rows.iter()
        .flat_map(|row| std::iter::once(row.window).chain(row.behind.iter().copied()))
        .collect()
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
    /// The glass behind everything. Held because it has to be resized with the panel.
    backing: Retained<NSGlassEffectView>,
    view: Retained<SwitcherView>,
    /// One layer per visible row, reused across opens. Rebuilt only when the row count changes, so
    /// stepping the selection moves a highlight rather than tearing down a layer tree. A row's top
    /// card.
    tiles: Vec<Retained<CALayer>>,
    /// `BEHIND` cards per row, nearest first, behind its tile. Hidden past the row's depth.
    behind: Vec<Vec<Retained<CALayer>>>,
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
    /// Configured icon files already read, by path. `None` is cached too, so a missing file is warned
    /// about once rather than on every redraw.
    file_icons: HashMap<std::path::PathBuf, Option<Retained<objc2_core_graphics::CGImage>>>,
    /// Where the cards' pictures are read, at every draw. Nothing is kept here: a window's picture is
    /// whatever the cache holds of it now.
    pictures: Pictures,
}

impl SwitcherPanel {
    pub fn new(mtm: MainThreadMarker, on_pick: OnPick, pictures: Pictures) -> Option<Self> {
        // A placeholder frame: every open recomputes it from the screen it is showing on.
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(600.0, 200.0));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );

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

        // Dark, pinned rather than inherited, so a machine in light mode does not get a light glass
        // under a dark wash.
        // SAFETY: an immutable `NSString` constant AppKit exports.
        let dark_aqua = unsafe { NSAppearanceNameDarkAqua };
        if let Some(dark) = NSAppearance::appearanceNamed(dark_aqua) {
            window.setAppearance(Some(&dark));
        }

        // Clear Liquid Glass, which is what macOS's own switcher is made of: a light blur that keeps the
        // shape of what is behind it. The heavy `HUDWindow` blur averages a dark desktop to one flat
        // slab. The glass rounds its own corners, and
        // its `tintColor` shifts hue rather than brightness, so the darkening is a layer inside it.
        let backing = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        backing.setStyle(NSGlassEffectViewStyle::Clear);
        backing.setCornerRadius(CORNER);
        window.setContentView(Some(&backing));

        let view = SwitcherView::alloc(mtm).set_ivars(ViewIvars {
            hits: RefCell::new(Hits::default()),
            on_pick,
        });
        // SAFETY: `NSView`'s designated initialiser takes the frame and returns the receiver, whose
        // ivars are set above.
        let view: Retained<SwitcherView> = unsafe { msg_send![super(view), initWithFrame: frame] };
        view.setWantsLayer(true);
        backing.setContentView(Some(&view));

        let root = view.layer()?;
        let metrics = Metrics::default();
        let scale = backing_scale();
        root.setContentsScale(scale);
        root.setCornerRadius(CORNER);
        root.setMasksToBounds(true);
        // No hairline of its own: the glass draws its edge.
        root.setBackgroundColor(Some(&token(card::BACKDROP.rgb(), card::BACKDROP_ALPHA)));

        // Under the tiles, so a tile's picture is never hidden by its own highlight.
        let highlight = CALayer::layer();
        highlight.setAnchorPoint(CGPoint::new(0.0, 0.0));
        highlight.setContentsScale(scale);
        highlight.setCornerRadius(metrics.tile_corner + 3.0);
        highlight.setZPosition(0.0);
        highlight.setHidden(true);
        highlight.setBackgroundColor(Some(&token(EMBER_SOFT, 1.0)));
        root.addSublayer(&highlight);

        // Over everything: the cards behind are at 0.5 and 0.75, the tiles at 1.0, the badges at 1.5,
        // the captions at 2.0. A ring drawn under the tiles is clipped away by them on every side except
        // where the 4pt margin shows, which reads as a glow rather than as an outline.
        let ring = CALayer::layer();
        ring.setAnchorPoint(CGPoint::new(0.0, 0.0));
        ring.setContentsScale(scale);
        ring.setCornerRadius(metrics.tile_corner + 3.0);
        ring.setZPosition(3.0);
        ring.setHidden(true);
        // The ember's remit covers "focused borders" as well as active bars, and a switcher row is
        // a focus target rather than a list row.
        ring.setBorderWidth(card::RING);
        ring.setBorderColor(Some(&token(EMBER, 1.0)));
        root.addSublayer(&ring);

        Some(Self {
            window,
            backing,
            view,
            tiles: Vec::new(),
            behind: Vec::new(),
            captions: Vec::new(),
            icons: Vec::new(),
            highlight,
            ring,
            metrics,
            visible: false,
            scale,
            pictures,
            app_icons: HashMap::default(),
            file_icons: HashMap::default(),
            last: None,
        })
    }

    /// Show the switch, or move the highlight if it is already showing.
    ///
    /// `screen` is the display to centre on, in CoreGraphics coordinates.
    pub fn show(&mut self, rows: &[Row], selected: usize, screen: CGRect) {
        let tiles: Vec<Tile> = rows
            .iter()
            .map(|row| Tile {
                size: row.size,
                depth: row.behind.len(),
            })
            .collect();
        let Some(strip) = lay_out(&tiles, selected, screen, self.metrics) else {
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
        turns::changed("switcher.frame");
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
            turns::changed("switcher.show");
            self.visible = true;
        }
    }

    /// Draw the strip again with whatever is now known, if the panel is up.
    ///
    /// The pictures warmed on the open land after the popup has already appeared, so without this the
    /// first open of a switch would stay grey.
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

    /// Whether the last draw has a card of `window`.
    pub fn draws(&self, window: rini_core::ids::WindowId) -> bool {
        self.last
            .as_ref()
            .is_some_and(|last| drawn_windows(&last.rows).contains(&window))
    }

    /// Order the panel out. Ordering rather than fading: an alpha-0 window still hit-tests, and this
    /// one will accept clicks.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.window.orderOut(None);
        turns::changed("switcher.hide");
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
        for card in self.behind.drain(..).flatten() {
            card.removeFromSuperlayer();
        }
        for caption in self.captions.drain(..) {
            caption.removeFromSuperlayer();
        }
        for icon in self.icons.drain(..) {
            icon.removeFromSuperlayer();
        }
        for _ in 0..count {
            let tile = self.card(1.0);
            root.addSublayer(&tile);
            self.tiles.push(tile);

            let behind: Vec<Retained<CALayer>> = (1..=BEHIND)
                .map(|depth| {
                    let card = self.card(1.0 - 0.25 * depth as f64);
                    card.setHidden(true);
                    root.addSublayer(&card);
                    card
                })
                .collect();
            self.behind.push(behind);

            let caption = CATextLayer::layer();
            caption.setAnchorPoint(CGPoint::new(0.0, 0.0));
            caption.setContentsScale(self.scale);
            caption.setZPosition(2.0);
            caption.setFontSize(11.0);
            // SAFETY: both are immutable `NSString` constants Core Animation exports.
            let (alignment, truncation) = unsafe {
                (
                    objc2_quartz_core::kCAAlignmentCenter,
                    objc2_quartz_core::kCATruncationEnd,
                )
            };
            caption.setAlignmentMode(alignment);
            caption.setTruncationMode(truncation);
            // n11, primary text. Both lines of the caption share it: the contrast law asks for
            // n9 or brighter for anything that is reading matter rather than furniture, and one
            // text layer cannot carry two colours. Splitting the app name onto n10 would need a
            // second layer per row, which is a change to make when the panel is worth polishing.
            caption.setForegroundColor(Some(&token(N11, 1.0)));
            // No `setFont`. CATextLayer's `font` is a CFTypeRef taking a name, a CGFont or a
            // CTFont, and none of the three bridges from an NSFont without a cast this file would
            // be the only place in the tree to need. The default face at this size is legible, and
            // a font is a thing to choose once the panel is worth polishing.
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

    /// One card of a row: a picture of a window, or the placeholder until one arrives.
    fn card(&self, z: f64) -> Retained<CALayer> {
        let card = CALayer::layer();
        card.setAnchorPoint(CGPoint::new(0.0, 0.0));
        card.setContentsScale(self.scale);
        card.setCornerRadius(self.metrics.tile_corner);
        card.setMasksToBounds(true);
        card.setZPosition(z);
        // The raised plane, so a card with no picture yet reads as a surface rather than as a hole.
        card.setBackgroundColor(Some(&token(N3, 1.0)));
        // Drawn over the picture, so every card has an edge whatever its window looks like.
        card.setBorderWidth(1.0);
        card.setBorderColor(Some(&token(N11, TILE_EDGE_ALPHA)));
        card
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

    /// A configured icon file, read once and kept. A file that cannot be read falls back to the
    /// application's own icon, with one warning.
    fn file_icon(
        &mut self,
        path: &std::path::Path,
    ) -> Option<Retained<objc2_core_graphics::CGImage>> {
        if let Some(cached) = self.file_icons.get(path) {
            return cached.clone();
        }
        let icon = image_file_pixels(path, ICON * self.scale);
        if icon.is_none() {
            warn!(path = %path.display(), "switcher icon override is not a readable image; using the application's own");
        }
        self.file_icons.insert(path.to_path_buf(), icon.clone());
        icon
    }

    fn place(&mut self, strip: &Strip, rows: &[Row], selected: usize, movement: Move) {
        // Resolved before the drawing loops, which borrow the layer vectors: caching an icon needs
        // `&mut self` and a loop cannot hold both.
        let badges: Vec<Option<Retained<objc2_core_graphics::CGImage>>> = rows
            .iter()
            .map(|row| {
                row.icon
                    .as_deref()
                    .and_then(|path| self.file_icon(path))
                    .or_else(|| self.app_icon(row.window.pid))
            })
            .collect();

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
        // `usable`, not `get`: the cheap capture route returns a sliver for exactly the windows a
        // switcher exists to show. Age is no reason to refuse one.
        let pictures: HashMap<rini_core::ids::WindowId, Picture> =
            self.pictures.usable_many(&drawn_windows(rows)).into_iter().collect();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (index, row) in rows.iter().enumerate() {
            if let Some(tile) = self.tiles.get(index) {
                // Scaled to fit rather than cropped: a cropped thumbnail of a browser is a rectangle
                // of text.
                // SAFETY: an immutable `NSString` constant Core Animation exports.
                let fit = unsafe { objc2_quartz_core::kCAGravityResizeAspect };
                draw_picture(tile, pictures.get(&row.window), fit);
            }
            let slots = cards_behind(&row.behind);
            for (card, slot) in self.behind.get(index).into_iter().flatten().zip(slots) {
                draw_card_behind(card, slot, &pictures);
            }
            if let Some(badge) = self.icons.get(index) {
                match badges.get(index).and_then(|icon| icon.clone()) {
                    Some(image) => {
                        set_contents(badge, &image);
                        badge.setHidden(false);
                    }
                    None => badge.setHidden(true),
                }
            }
            if let Some(caption) = self.captions.get(index) {
                let text = caption_text(row);
                // SAFETY: an `NSString`, one of the two classes a text layer's `string` takes.
                unsafe { caption.setString(Some(&*NSString::from_str(&text))) };
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
            // SAFETY: an immutable `NSString` constant Core Animation exports.
            let ease_out = unsafe { kCAMediaTimingFunctionEaseOut };
            let ease = CAMediaTimingFunction::functionWithName(ease_out);
            CATransaction::setAnimationTimingFunction(Some(&ease));
        }
        for (index, rect) in strip.rows.iter().enumerate() {
            let Some(row) = rows.get(index) else {
                continue;
            };
            let cards = self.metrics.cards(*rect, row.behind.len());
            if let Some(tile) = self.tiles.get(index) {
                tile.setFrame(cards[0]);
            }
            for (card, frame) in self.behind.get(index).into_iter().flatten().zip(&cards[1..]) {
                card.setFrame(*frame);
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

/// Two lines: the application, then the window's own title or the stack's window count.
///
/// The application first because it is the coarse thing the eye lands on, and a window title is often
/// a file path whose useful end is truncated away.
fn caption_text(row: &Row) -> String {
    let detail = row.detail.trim();
    if detail.is_empty() {
        return row.app_name.clone();
    }
    format!("{}\n{}", row.app_name, detail)
}

fn draw_picture(card: &CALayer, picture: Option<&Picture>, gravity: &CALayerContentsGravity) {
    match picture {
        Some(picture) => {
            card.setContentsGravity(gravity);
            set_layer_contents(card, picture);
        }
        // Left as the placeholder slab. A card with no picture still reads as a card.
        None => clear_contents(card),
    }
}

/// A card behind a row's top card, filled and cropped: only its top edge shows, and a fitted picture
/// in a narrower card would leave that edge a band of placeholder. One past the row's depth is hidden
/// holding no picture, as nothing shows it.
fn draw_card_behind(
    card: &CALayer,
    slot: CardBehind,
    pictures: &HashMap<rini_core::ids::WindowId, Picture>,
) {
    match slot {
        CardBehind::Shows(window) => {
            card.setHidden(false);
            // SAFETY: an immutable `NSString` constant Core Animation exports.
            let fill = unsafe { objc2_quartz_core::kCAGravityResizeAspectFill };
            draw_picture(card, pictures.get(&window), fill);
        }
        CardBehind::Hidden => {
            clear_contents(card);
            card.setHidden(true);
        }
    }
}

/// The backing scale to draw at.
///
/// 2.0 rather than the display's own: the panel is built once and shown on whichever display a
/// switch is driven from. Drawing a retina panel at 1.0 is soft; drawing a 1x panel at 2.0 costs
/// memory and nothing else, so the safe direction is up.
fn backing_scale() -> f64 { 2.0 }

#[cfg(test)]
mod tests {
    use rini_core::ids::{SpaceId, WindowId};

    use super::*;
    use crate::switcher::domain::candidates::{Candidate, Scope};
    use crate::switcher::domain::entries::entries;

    fn row(app: &str, title: &str) -> Row {
        Row {
            window: WindowId::new(1, 1),
            size: CGSize::new(800.0, 600.0),
            detail: title.to_owned(),
            app_name: app.to_owned(),
            icon: None,
            behind: Vec::new(),
        }
    }

    /// A card behind shows its window's picture, and one past the row's depth is hidden letting go of
    /// the picture an earlier switch left on it.
    #[test]
    fn a_hidden_card_behind_holds_no_picture() {
        use rini_geometry::space::ScreenSize;

        use crate::pictures::platform::picture::test_picture;

        let window = WindowId::new(1, 2);
        let pictures: HashMap<WindowId, Picture> =
            [(window, test_picture(ScreenSize::new(800.0, 600.0)))].into_iter().collect();
        let card = CALayer::layer();
        draw_card_behind(&card, CardBehind::Shows(window), &pictures);
        assert!(!card.isHidden());
        // SAFETY: `contents` is only checked for presence, never used as any type.
        assert!(unsafe { card.contents() }.is_some(), "its window's picture");
        draw_card_behind(&card, CardBehind::Hidden, &pictures);
        assert!(card.isHidden());
        // SAFETY: as above, presence only.
        assert!(unsafe { card.contents() }.is_none(), "let go of");
    }

    /// A switch reads and warms the cards it draws: each row's top window, then those behind it.
    #[test]
    fn the_windows_drawn_are_each_rows_cards_in_order() {
        let lone = Row {
            window: WindowId::new(2, 1),
            ..row("Slack", "general")
        };
        let stack = Row {
            behind: vec![WindowId::new(1, 2), WindowId::new(1, 3)],
            ..row("Ghostty", "~")
        };
        assert_eq!(drawn_windows(&[stack, lone]), vec![
            WindowId::new(1, 1),
            WindowId::new(1, 2),
            WindowId::new(1, 3),
            WindowId::new(2, 1)
        ]);
    }

    fn ghostty(idx: u32, width: f64) -> Candidate {
        Candidate {
            window: WindowId::new(1, idx),
            space: SpaceId::new(1),
            workspace: slotmap::KeyData::from_ffi((1u64 << 32) | 1).into(),
            workspace_index: 0,
            size: CGSize::new(width, 600.0),
            title: format!("~/projects/{idx}"),
            app_name: "Ghostty".to_owned(),
        }
    }

    #[test]
    fn a_stack_caption_names_the_application_then_the_count() {
        let list = (1..=4).map(|idx| ghostty(idx, 800.0)).collect();
        let stack = &entries(list, Scope::Everything)[0];
        assert_eq!(caption_text(&Row::of(stack, None)), "Ghostty\n4 windows");
    }

    /// The row is drawn from its top window, which is also what a click on it commits.
    #[test]
    fn a_stack_row_is_drawn_from_its_top_window_with_two_behind() {
        let list = (1..=4).map(|idx| ghostty(idx, 400.0 * idx as f64)).collect();
        let row = Row::of(&entries(list, Scope::Everything)[0], None);
        assert_eq!(row.window, WindowId::new(1, 1));
        assert_eq!(row.size, CGSize::new(400.0, 600.0));
        assert_eq!(row.behind, vec![WindowId::new(1, 2), WindowId::new(1, 3)]);
    }

    #[test]
    fn a_window_row_is_captioned_by_its_title_and_has_nothing_behind() {
        let list = (1..=2).map(|idx| ghostty(idx, 800.0)).collect();
        let row = Row::of(&entries(list, Scope::App(1))[0], None);
        assert_eq!(caption_text(&row), "Ghostty\n~/projects/1");
        assert!(row.behind.is_empty());
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
