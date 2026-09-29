//! One display's bar window and its layer tree.
//!
//! An `NSPanel` with `NonactivatingPanel`, so a click on the bar never activates rini and takes
//! focus from the window the bar is about. Made once per display and kept, because making a window
//! costs about 112ms against 14ms to order one in, and ordered in and out rather than faded,
//! because an alpha-0 window still takes clicks. The layer tree is described in "Drawing" in
//! `src/bar/docs/README.md`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBackingStoreType, NSColor,
    NSEvent, NSPanel, NSScreen, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGError, CGImage};
use objc2_foundation::{NSNumber, NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, kCAFillModeBackwards,
    kCAGravityLeft, kCAMediaTimingFunctionEaseInEaseOut, kCAMediaTimingFunctionEaseOut,
};
use rini_skylight_sys::{G_CONNECTION, SLSSetWindowBackgroundBlurRadius};
use rustc_hash::FxHashMap as HashMap;
use tracing::warn;

use crate::animation::platform::overlay::set_contents;
use crate::bar::domain::extras::Kind;
use crate::bar::domain::layout::{self, Piece, Scene, Target};
use crate::bar::domain::model::DisplayBar;
use crate::bar::domain::motion::{self, FADE_SECONDS, Fade, SLIDE_SECONDS};
use crate::bar::domain::palette::{self, Colour};
use crate::bar::domain::pieces::{self, Context};
use crate::bar::domain::placement::{self, RULE_WIDTH};
use crate::bar::domain::style;
use crate::bar::platform::menu_extras::{Extra, Extras};
use crate::bar::platform::text::{Picture, Text};
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::screen::primary_display_height;

/// Above the flight overlay (18), so a flight never pictures the bar; below notification banners
/// (21) and the menu bar (24), so the menu bar revealed from the top edge draws over it.
const LEVEL: isize = 20;

/// Pixels per point, whatever the display. Drawing a 1x display at 2x costs memory and nothing
/// else; drawing a 2x display at 1x is soft. The switcher and the flight overlay draw at 2.0 for
/// the same reason.
pub const SCALE: f64 = 2.0;

const FADE_KEY: &str = "rini.bar.fade";
const SLIDE_KEY: &str = "rini.bar.slide";

/// Where a click on the bar goes, by what was under it.
pub type OnClick = Rc<dyn Fn(Target)>;

struct ViewIvars {
    /// As last drawn, for the hit test. Empty while the bar is ordered out.
    scene: RefCell<Scene>,
    on_click: OnClick,
}

define_class!(
    /// Top-left origin, so the layer tree and a click are in the coordinates `domain::layout` uses.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniBarView"]
    #[ivars = ViewIvars]
    struct BarView;

    impl BarView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The panel never becomes key, so without this a first click would only be taken as a
        /// request to become key.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let target = self.ivars().scene.borrow().hit(point.x);
            if let Some(target) = target {
                (self.ivars().on_click)(target);
            }
        }
    }
);

define_class!(
    /// AppKit keeps a window out of the menu-bar band by moving it down, which put the bar 32pt below
    /// the top edge, over the windows. The bar's frame is taken as given.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniBarPanel"]
    struct BarWindow;

    impl BarWindow {
        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain_frame_rect(&self, frame: CGRect, _screen: Option<&NSScreen>) -> CGRect {
            frame
        }
    }
);

/// What a piece is drawn with.
enum Look<'a> {
    Text(Rc<Picture>),
    Extra(&'a Extra),
    Rule(Colour),
}

impl Look<'_> {
    fn ink(&self) -> f64 {
        match self {
            Look::Text(picture) => picture.sheet.ink.size.width,
            Look::Extra(extra) => extra.ink.1 - extra.ink.0,
            Look::Rule(_) => RULE_WIDTH,
        }
    }
}

/// What a layer shows, compared before it is set so an unchanged layer is not touched.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shows {
    /// A picture, by its address. The layer holding it keeps it alive, so no other picture can have
    /// that address while it is shown.
    Picture(usize),
    Fill(Colour),
}

struct Drawn {
    layer: Retained<CALayer>,
    shows: Option<Shows>,
    frame: CGRect,
    shown: bool,
}

/// One display's bar, kept for the life of the process once made.
pub struct BarPanel {
    window: Retained<BarWindow>,
    view: Retained<BarView>,
    root: Retained<CALayer>,
    /// One layer per piece, reused across draws and hidden while its piece is not drawn.
    layers: HashMap<Piece, Drawn>,
    /// What the tray is drawn through. It clips the tray's pictures, and its bounds slide them into
    /// the chevron.
    tray: Retained<CALayer>,
    tray_drawn: Option<(CGRect, bool)>,
    underline: Retained<CALayer>,
    underline_drawn: Option<CGRect>,
    /// Where the bar was last put, in CoreGraphics coordinates.
    strip: Option<CGRect>,
    visible: bool,
}

impl BarPanel {
    pub fn new(mtm: MainThreadMarker, on_click: OnClick) -> Option<Self> {
        // A placeholder frame: `place` puts it on its display.
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(100.0, layout::HEIGHT));
        let window: Retained<BarWindow> = unsafe {
            msg_send![
                BarWindow::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setIgnoresMouseEvents(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        // A panel otherwise goes whenever rini deactivates, and with rini whenever it is hidden.
        window.setHidesOnDeactivate(false);
        window.setCanHide(false);
        window.setLevel(LEVEL);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenNone,
        );
        if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
            window.setAppearance(Some(&dark));
        }
        // SAFETY: plain values into SkyLight, for a window this process owns.
        let blurred = unsafe {
            SLSSetWindowBackgroundBlurRadius(
                *G_CONNECTION,
                window.windowNumber() as u32,
                palette::BAR_GROUND_BLUR,
            )
        };
        if blurred != CGError::Success {
            warn!(error = ?blurred, "could not blur the bar's ground");
        }

        let view = BarView::alloc(mtm).set_ivars(ViewIvars {
            scene: RefCell::default(),
            on_click,
        });
        let view: Retained<BarView> = unsafe { msg_send![super(view), initWithFrame: frame] };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));

        let root = view.layer()?;
        root.setContentsScale(SCALE);
        root.setBackgroundColor(Some(&cg_colour(palette::N1, palette::BAR_GROUND_ALPHA)));

        let tray = sublayer(&root);
        tray.setMasksToBounds(true);
        tray.setHidden(true);
        let underline = sublayer(&root);
        underline.setBackgroundColor(Some(&cg_colour(style::UNDERLINE, 1.0)));
        underline.setHidden(true);

        Some(Self {
            window,
            view,
            root,
            layers: HashMap::default(),
            tray,
            tray_drawn: None,
            underline,
            underline_drawn: None,
            strip: None,
            visible: false,
        })
    }

    /// Puts the bar across the top of `display`, in CoreGraphics coordinates. False while it has
    /// never had a frame to draw in.
    pub fn place(&mut self, display: CGRect) -> bool {
        let strip = placement::strip(display);
        if self.strip != Some(strip)
            && let Some(cocoa) =
                CoordinateConverter::from_height(primary_display_height()).convert_rect(strip)
        {
            self.window.setFrame_display(cocoa, false);
            let bounds = CGRect::new(CGPoint::new(0.0, 0.0), strip.size);
            self.view.setFrame(bounds);
            self.root.setFrame(bounds);
            self.strip = Some(strip);
        }
        self.strip.is_some()
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.window.orderFrontRegardless();
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.orderOut(None);
            self.visible = false;
            self.view.ivars().scene.take();
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Draws `bar`, setting only what changed. Runs inside the caller's transaction.
    pub fn draw(&mut self, bar: &DisplayBar, context: Context, extras: &Extras, text: &mut Text) {
        let width = self.strip.map_or(0.0, |strip| strip.size.width);
        let kinds: Vec<Kind> = extras.items.iter().map(|extra| extra.kind).collect();
        let right = pieces::right(&kinds);
        let mut looks: HashMap<Piece, Look> = HashMap::default();
        for piece in pieces::pieces(bar, context.fold, &right) {
            let look = match piece {
                Piece::PlaceDivider | Piece::ClockDivider => {
                    Some(Look::Rule(style::colour(piece, bar)))
                }
                Piece::Vital(_) | Piece::Tray(_) => pieces::extra(piece, &kinds)
                    .and_then(|index| extras.items.get(index))
                    .map(Look::Extra),
                _ => pieces::label(piece, bar, context)
                    .and_then(|label| text.picture(&label))
                    .map(Look::Text),
            };
            if let Some(look) = look {
                looks.insert(piece, look);
            }
        }
        let scene = layout::lay_out(width, bar, context.fold, &right, |piece| {
            looks.get(&piece).map(Look::ink)
        });

        let placed: Vec<(Piece, &Look, CGRect)> = scene
            .pieces
            .iter()
            .filter_map(|(piece, span)| {
                let look = looks.get(piece)?;
                let frame = match look {
                    Look::Text(picture) => placement::text_frame(
                        &picture.sheet,
                        *span,
                        placement::anchor(*piece),
                        SCALE,
                    ),
                    Look::Extra(extra) => {
                        placement::extra_frame(picture_size(extra), extra.ink.0, *span, SCALE)
                    }
                    Look::Rule(_) => placement::rule_frame(*span, SCALE),
                };
                Some((*piece, look, frame))
            })
            .collect();

        let tray_left = placed
            .iter()
            .filter(|(piece, ..)| matches!(piece, Piece::Tray(_)))
            .map(|(.., frame)| frame.origin.x)
            .reduce(f64::min);
        let tray = tray_left.zip(scene.span(Piece::Chevron)).map(|(left, chevron)| {
            (placement::tray_window(left, chevron, SCALE), context.tray_open)
        });
        self.put_tray(tray);
        self.put_underline(scene.underline.map(|span| placement::underline_frame(span, SCALE)));

        let mut drawn = HashSet::new();
        for (piece, look, frame) in placed {
            let frame = match (piece, tray) {
                (Piece::Tray(_), Some((window, _))) => CGRect::new(
                    CGPoint::new(frame.origin.x - window.origin.x, frame.origin.y),
                    frame.size,
                ),
                _ => frame,
            };
            self.put(piece, look, frame);
            drawn.insert(piece);
        }
        for (piece, drawn_piece) in self.layers.iter_mut() {
            if drawn_piece.shown && !drawn.contains(piece) {
                drawn_piece.layer.setHidden(true);
                drawn_piece.layer.removeAnimationForKey(&NSString::from_str(FADE_KEY));
                drawn_piece.layer.setOpacity(1.0);
                drawn_piece.shown = false;
            }
        }
        self.view.ivars().scene.replace(scene);
    }

    fn put(&mut self, piece: Piece, look: &Look, frame: CGRect) {
        let parent = if matches!(piece, Piece::Tray(_)) {
            &self.tray
        } else {
            &self.root
        };
        let drawn = self.layers.entry(piece).or_insert_with(|| {
            let layer = sublayer(parent);
            // Left, and as tall as its picture, so a picture is never scaled and a cut title is cut
            // rather than squeezed.
            layer.setContentsGravity(unsafe { kCAGravityLeft });
            layer.setMasksToBounds(true);
            Drawn {
                layer,
                shows: None,
                frame: CGRect::default(),
                shown: true,
            }
        });
        let (shows, image, scale) = match look {
            Look::Text(picture) => (
                Shows::Picture(address(&picture.image)),
                Some(&*picture.image),
                SCALE,
            ),
            Look::Extra(extra) => (
                Shows::Picture(address(&extra.image)),
                Some(&*extra.image),
                extra.scale,
            ),
            Look::Rule(colour) => (Shows::Fill(*colour), None, SCALE),
        };
        if drawn.shows != Some(shows) {
            match image {
                Some(image) => {
                    set_contents(&drawn.layer, image);
                    drawn.layer.setContentsScale(scale);
                }
                None => {
                    if let Shows::Fill(colour) = shows {
                        drawn.layer.setBackgroundColor(Some(&cg_colour(colour, 1.0)));
                    }
                }
            }
            drawn.shows = Some(shows);
        }
        if drawn.frame != frame {
            drawn.layer.setFrame(frame);
            drawn.frame = frame;
        }
        if !drawn.shown {
            drawn.layer.setHidden(false);
            drawn.shown = true;
        }
    }

    fn put_tray(&mut self, tray: Option<(CGRect, bool)>) {
        if self.tray_drawn == tray {
            return;
        }
        match tray {
            Some((window, open)) => {
                self.tray.setFrame(window);
                self.tray.setBounds(placement::tray_bounds(open, window));
                self.tray.setHidden(false);
            }
            None => self.tray.setHidden(true),
        }
        self.tray_drawn = tray;
    }

    fn put_underline(&mut self, underline: Option<CGRect>) {
        if self.underline_drawn == underline {
            return;
        }
        match underline {
            Some(frame) => {
                self.underline.setFrame(frame);
                self.underline.setHidden(false);
            }
            None => self.underline.setHidden(true),
        }
        self.underline_drawn = underline;
    }

    /// Fades the glyphs past the fold, staggered from `now` on the media clock. Returns how many.
    pub fn fade(&self, fade: Fade, now: f64) -> usize {
        let tail = motion::tail(
            self.layers.iter().filter(|(_, drawn)| drawn.shown).map(|(piece, _)| *piece),
        );
        let key = NSString::from_str(FADE_KEY);
        let to = fade.to();
        for (piece, delay) in tail.iter().zip(motion::delays(fade, tail.len())) {
            if let Some(drawn) = self.layers.get(piece) {
                let from = fade.from(fading(&drawn.layer, &key));
                drawn.layer.setOpacity(to as f32);
                drawn
                    .layer
                    .addAnimation_forKey(&fade_animation(from, to, now + delay), Some(&key));
            }
        }
        tail.len()
    }

    /// Slides the tray to open or closed from wherever it is showing now. The bounds have already
    /// been set by `draw`; this carries the picture there.
    pub fn slide(&self, open: bool) {
        let Some((window, _)) = self.tray_drawn else {
            return;
        };
        let to = placement::tray_bounds(open, window);
        // SAFETY: `presentationLayer` returns a read-only copy of the layer.
        let from = unsafe { self.tray.presentationLayer() }
            .map(|shown| shown.bounds())
            .unwrap_or_else(|| placement::tray_bounds(!open, window));
        let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("bounds")));
        // SAFETY: an NSValue holding a CGRect is what Core Animation expects for "bounds".
        unsafe {
            animation.setFromValue(Some(&NSValue::valueWithRect(from)));
            animation.setToValue(Some(&NSValue::valueWithRect(to)));
        }
        animation.setDuration(SLIDE_SECONDS);
        animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
            kCAMediaTimingFunctionEaseOut
        })));
        self.tray.addAnimation_forKey(&animation, Some(&NSString::from_str(SLIDE_KEY)));
    }
}

/// The opacity on screen of a layer whose fade is still running or waiting its turn, and `None` for
/// one at rest: a glyph just shown still presents its last committed opacity, 1, and would not fade.
fn fading(layer: &CALayer, key: &NSString) -> Option<f64> {
    // SAFETY: a lookup by key; `presentationLayer` returns a read-only copy of the layer.
    unsafe {
        layer.animationForKey(key)?;
        layer.presentationLayer().map(|shown| shown.opacity() as f64)
    }
}

fn fade_animation(from: f64, to: f64, begin: f64) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
    // SAFETY: an NSNumber is what Core Animation expects for "opacity".
    unsafe {
        animation.setFromValue(Some(&NSNumber::numberWithDouble(from)));
        animation.setToValue(Some(&NSNumber::numberWithDouble(to)));
    }
    animation.setDuration(FADE_SECONDS);
    animation.setBeginTime(begin);
    // Held at `from` until its turn in the stagger comes.
    animation.setFillMode(unsafe { kCAFillModeBackwards });
    animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
        kCAMediaTimingFunctionEaseInEaseOut
    })));
    animation
}

fn sublayer(parent: &CALayer) -> Retained<CALayer> {
    let layer = CALayer::layer();
    layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
    layer.setContentsScale(SCALE);
    parent.addSublayer(&layer);
    layer
}

fn picture_size(extra: &Extra) -> CGSize {
    CGSize::new(
        CGImage::width(Some(&extra.image)) as f64 / extra.scale,
        CGImage::height(Some(&extra.image)) as f64 / extra.scale,
    )
}

fn address(image: &CGImage) -> usize {
    image as *const CGImage as usize
}

fn cg_colour(colour: Colour, alpha: f64) -> Retained<CGColor> {
    let (red, green, blue) = colour.rgb();
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha).CGColor()
}
