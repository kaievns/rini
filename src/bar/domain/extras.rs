//! Which of macOS's menu-bar extras the bar draws, where, how much of each picture is icon, and when
//! they are pictured.
//!
//! The bar draws the system's own status items rather than glyphs of its own: the Wi-Fi fan, the
//! speaker and the battery are Control Center's, and third-party extras are their apps' own icons.
//! Every one is hosted in the Control Center process as a window at the status level, named by its
//! module (`WiFi`, `Battery`) or anonymously (`Item-0`). All of them are pictured in one capture a
//! tick, cut apart here. See `src/bar/docs/menu-extras.md`.

use std::hash::{DefaultHasher, Hasher};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_geometry::CGRectExt;

/// The three extras drawn as vitals, beside the clock, rather than in the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vital {
    WiFi,
    Sound,
    Battery,
}

impl Vital {
    /// Left to right, as drawn.
    pub const ORDER: [Vital; 3] = [Vital::WiFi, Vital::Sound, Vital::Battery];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vital(Vital),
    Tray,
    Skip,
}

/// The process every real menu extra lives in. An entry from any other owner is not one: after a
/// display replug the enumeration carried `LinkedNotesUIService`, a 268pt service window.
pub const HOST: &str = "Control Center";

/// Modules the bar already draws another way, or that a key reaches: the clock is drawn as text, and
/// Control Center's own button opens what the vitals show.
const SKIP: [&str; 7] = [
    "Clock",
    "BentoBox-0",
    "Now Playing",
    "NowPlaying",
    "PharosSystems.SecurePrint.JobManagement",
    "com.displaylink.DisplayLinkUserAgent",
    "",
];

/// Widest extra drawn, in points. A sanity guard, not a filter on text extras, which are wanted:
/// Outlook draws its next event as text, measured at 141pt and 170pt.
pub const MAX_WIDTH: f64 = 300.0;

/// A status-level window as the window server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusWindow {
    pub owner: String,
    /// `kCGWindowName`: the module, a bundle id, or `Item-0`.
    pub name: String,
    /// Left edge on the menu bar, which is the order the extras are drawn in.
    pub x: f64,
    /// Top edge: the display's top while its menu bar shows, the window's height above it while hidden.
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn kind(window: &StatusWindow) -> Kind {
    if window.owner != HOST || window.width > MAX_WIDTH {
        return Kind::Skip;
    }
    match window.name.as_str() {
        "WiFi" => Kind::Vital(Vital::WiFi),
        "Sound" => Kind::Vital(Vital::Sound),
        "Battery" => Kind::Vital(Vital::Battery),
        name if SKIP.contains(&name) || name.starts_with("com.apple.") => Kind::Skip,
        _ => Kind::Tray,
    }
}

fn is_anonymous(name: &str) -> bool {
    name.strip_prefix("Item-").is_some_and(|rest| rest.parse::<u32>().is_ok())
}

fn is_bundle_id(name: &str) -> bool {
    name.contains('.')
}

/// The extras to draw, in menu-bar order left to right, each with its index in `windows`.
///
/// macOS can list the same third-party extras twice, once by bundle id and once as `Item-0`: 28
/// entries on one session, nine named and the same nine again anonymously. Only one block is kept,
/// and it is the anonymous one, because a capture by bundle id came back blank for Docker and
/// 1Password where the anonymous twin drew. The larger block wins if they ever differ, since losing
/// icons is the worse failure.
pub fn select(windows: &[StatusWindow]) -> Vec<(usize, Kind)> {
    let tray = |window: &&StatusWindow| kind(window) == Kind::Tray;
    let anonymous = windows.iter().filter(tray).filter(|w| is_anonymous(&w.name)).count();
    let named = windows.iter().filter(tray).filter(|w| is_bundle_id(&w.name)).count();
    let drop_named = anonymous > 0 && anonymous >= named;
    let drop_anonymous = !drop_named && named > 0;

    let mut out: Vec<(usize, Kind)> = windows
        .iter()
        .enumerate()
        .filter_map(|(index, window)| {
            let kind = kind(window);
            let duplicate = kind == Kind::Tray
                && ((drop_named && is_bundle_id(&window.name))
                    || (drop_anonymous && is_anonymous(&window.name)));
            (kind != Kind::Skip && !duplicate).then_some((index, kind))
        })
        .collect();
    out.sort_by(|a, b| windows[a.0].x.total_cmp(&windows[b.0].x));
    out
}

/// Opacity below which a pixel is not ink. The pictures have a transparent ground, and an icon's
/// anti-aliased edge fades well below this.
pub const INK_ALPHA: u8 = 24;

/// The first and last column holding ink, from each column's greatest alpha. `None` for a picture
/// with nothing in it.
///
/// This is what spaces the icons evenly. An extra's picture is its icon plus a margin, and the margin
/// is not the same from one extra to the next: 25.5pt to 39pt measured, and Apple's audio/video pill
/// carries 39.5pt of ink in a 62pt picture. Spaced by picture the row is uneven; spaced by ink it is
/// even.
pub fn ink_columns(column_alpha: &[u8]) -> Option<(usize, usize)> {
    let first = column_alpha.iter().position(|&alpha| alpha > INK_ALPHA)?;
    let last = column_alpha.iter().rposition(|&alpha| alpha > INK_ALPHA)?;
    Some((first, last))
}

/// Where the ink starts and ends, in points from the picture's left edge, at `scale` pixels a point.
pub fn ink_span(column_alpha: &[u8], scale: f64) -> Option<(f64, f64)> {
    let (first, last) = ink_columns(column_alpha)?;
    Some((first as f64 / scale, (last + 1) as f64 / scale))
}

/// Whether a status window is on the menu bar of the display with these bounds: its centre is within
/// the display's width, and it is in the band along the display's top edge, from its own height above
/// the edge (the menu bar hidden) down to the edge (shown). A display stacked above or below another
/// shares its x range, so x alone would take the other's extras too.
pub fn on_display(window: &StatusWindow, display: CGRect) -> bool {
    let centre = window.x + window.width / 2.0;
    let top = display.origin.y;
    display.origin.x <= centre
        && centre < display.origin.x + display.size.width
        && top - window.height <= window.y
        && window.y <= top
}

/// A rectangle of whole pixels, from a picture's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl PixelRect {
    pub fn rect(&self) -> CGRect {
        CGRect::new(
            CGPoint::new(self.x as f64, self.y as f64),
            CGSize::new(self.width as f64, self.height as f64),
        )
    }
}

/// One capture of several status windows, which is a picture of the union of their bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Composite {
    union: CGRect,
    /// Pixels per point.
    pub scale: f64,
    width: usize,
    height: usize,
}

impl Composite {
    /// `None` unless a picture of `picture` pixels is that union at 1x or finer. A failed capture can
    /// come back as an empty picture rather than none.
    pub fn new(bounds: &[CGRect], picture: (usize, usize)) -> Option<Composite> {
        let union = bounds.iter().copied().reduce(|union, rect| union.union(&rect))?;
        if union.size.width <= 0.0 {
            return None;
        }
        let (width, height) = picture;
        let scale = width as f64 / union.size.width;
        let fits = scale >= 1.0 && (union.size.height * scale - height as f64).abs() <= 1.0;
        fits.then_some(Composite { union, scale, width, height })
    }

    /// Where the window at `bounds` is in the picture. Edges round to whole pixels, so neighbours
    /// share theirs rather than overlap or leave a gap. `None` when none of it is in the picture.
    pub fn crop(&self, bounds: CGRect) -> Option<PixelRect> {
        let pixel = |points: f64, origin: f64, limit: usize| {
            (((points - origin) * self.scale).round().max(0.0) as usize).min(limit)
        };
        let x0 = pixel(bounds.origin.x, self.union.origin.x, self.width);
        let x1 = pixel(bounds.origin.x + bounds.size.width, self.union.origin.x, self.width);
        let y0 = pixel(bounds.origin.y, self.union.origin.y, self.height);
        let y1 = pixel(bounds.origin.y + bounds.size.height, self.union.origin.y, self.height);
        (x1 > x0 && y1 > y0).then_some(PixelRect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
    }
}

/// `rect`'s rows of an RGBA8 picture laid out `stride` bytes a row, top row first.
fn rows(pixels: &[u8], stride: usize, rect: PixelRect) -> impl Iterator<Item = &[u8]> {
    let span = rect.x * 4..(rect.x + rect.width) * 4;
    pixels.chunks(stride).skip(rect.y).take(rect.height).filter_map(move |row| row.get(span.clone()))
}

/// Each column's greatest alpha within `rect`, alpha being every pixel's last byte.
pub fn column_alpha(pixels: &[u8], stride: usize, rect: PixelRect) -> Vec<u8> {
    let mut out = vec![0; rect.width];
    for row in rows(pixels, stride, rect) {
        for (alpha, pixel) in out.iter_mut().zip(row.chunks_exact(4)) {
            *alpha = (*alpha).max(pixel[3]);
        }
    }
    out
}

/// A hash of `rect`'s pixels, which is how a picture that changed is told from one that did not.
pub fn fingerprint(pixels: &[u8], stride: usize, rect: PixelRect) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write_usize(rect.width);
    hasher.write_usize(rect.height);
    for row in rows(pixels, stride, rect) {
        hasher.write(row);
    }
    hasher.finish()
}

/// What a sent extra is compared by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    pub window: u32,
    pub kind: Kind,
    pub hash: u64,
}

/// Whether the extras differ from the ones last sent: another window, kind or order, or a picture
/// that changed. Nothing sent yet is a change.
pub fn changed(last: Option<&[Stamp]>, now: &[Stamp]) -> bool {
    last != Some(now)
}

/// For each extra now, the index of the one last sent with the same stamp, whose picture it is sent
/// with again. The bar sets a layer's contents only when its picture is another one, so a fresh copy
/// of the same pixels would redraw it.
pub fn reused(last: &[Stamp], now: &[Stamp]) -> Vec<Option<usize>> {
    now.iter().map(|stamp| last.iter().position(|sent| sent == stamp)).collect()
}

/// How often the extras are pictured. sketchybar pictured each one once a second; one batched capture
/// a second costs WindowServer about 0.8ms.
pub const TICK: Duration = Duration::from_secs(1);

/// What the bar tells the thread that pictures the extras.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Pause(bool),
    Refresh,
}

/// When the extras are next pictured.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    paused: bool,
    due: Instant,
}

impl Schedule {
    /// Running, with the first picture due at once.
    pub fn new(now: Instant) -> Schedule {
        Schedule { paused: false, due: now }
    }

    /// How long to wait for a command before picturing. `None` while paused: wait for one.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        (!self.paused).then(|| self.due.saturating_duration_since(now))
    }

    pub fn on(&mut self, command: Command, now: Instant) {
        match command {
            Command::Pause(paused) => self.paused = paused,
            Command::Refresh => self.due = now,
        }
    }

    /// Whether to picture now. Taking one puts the next a tick away.
    pub fn take(&mut self, now: Instant) -> bool {
        if self.paused || now < self.due {
            return false;
        }
        self.due = now + TICK;
        true
    }

    /// Whether a pass taken may go on to capture, given the pause as it stands now rather than as the
    /// last command read said. A pause that landed since calls the capture off and holds from here,
    /// and the picture stays due for when it ends.
    pub fn may_capture(&mut self, paused: bool, now: Instant) -> bool {
        if paused {
            self.paused = true;
            self.due = now;
        }
        !paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the built-in display's hidden menu bar.
    fn status(name: &str, x: f64) -> StatusWindow {
        StatusWindow { owner: HOST.into(), name: name.into(), x, y: -33.0, width: 38.0, height: 33.0 }
    }

    fn names(windows: &[StatusWindow]) -> Vec<(&str, Kind)> {
        select(windows).into_iter().map(|(index, kind)| (windows[index].name.as_str(), kind)).collect()
    }

    /// The enumeration this machine gave, right to left as the window server lists it.
    fn session() -> Vec<StatusWindow> {
        vec![
            status("Clock", 1680.0),
            status("BentoBox-0", 1640.0),
            status("Sound", 1600.0),
            status("WiFi", 1560.0),
            status("Battery", 1520.0),
            status("Item-0", 1380.0),
            status("Item-0", 1346.0),
            status("AudioVideoModule", 1100.0),
        ]
    }

    #[test]
    fn the_vitals_are_picked_out_by_name() {
        assert_eq!(kind(&status("WiFi", 0.0)), Kind::Vital(Vital::WiFi));
        assert_eq!(kind(&status("Sound", 0.0)), Kind::Vital(Vital::Sound));
        assert_eq!(kind(&status("Battery", 0.0)), Kind::Vital(Vital::Battery));
    }

    /// The clock is drawn as text and Control Center's own button duplicates the vitals.
    #[test]
    fn what_the_bar_draws_another_way_is_skipped() {
        assert_eq!(kind(&status("Clock", 0.0)), Kind::Skip);
        assert_eq!(kind(&status("BentoBox-0", 0.0)), Kind::Skip);
        assert_eq!(kind(&status("com.apple.Spotlight", 0.0)), Kind::Skip);
    }

    /// Only Control Center hosts real extras.
    #[test]
    fn a_window_from_another_owner_is_not_an_extra() {
        let mut window = status("Window(4)", 0.0);
        window.owner = "LinkedNotesUIService".into();
        assert_eq!(kind(&window), Kind::Skip);
    }

    /// Outlook's event text is an extra and is drawn; something 300pt wide is not one.
    #[test]
    fn a_text_extra_is_kept_and_a_huge_one_is_not() {
        let mut outlook = status("Item-0", 0.0);
        outlook.width = 170.0;
        assert_eq!(kind(&outlook), Kind::Tray);
        outlook.width = 320.0;
        assert_eq!(kind(&outlook), Kind::Skip);
    }

    /// Drawn left to right, as the menu bar has them.
    #[test]
    fn extras_come_out_in_menu_bar_order() {
        assert_eq!(
            names(&session()),
            vec![
                ("AudioVideoModule", Kind::Tray),
                ("Item-0", Kind::Tray),
                ("Item-0", Kind::Tray),
                ("Battery", Kind::Vital(Vital::Battery)),
                ("WiFi", Kind::Vital(Vital::WiFi)),
                ("Sound", Kind::Vital(Vital::Sound)),
            ]
        );
    }

    /// With both blocks listed, the anonymous one is kept: it is the one whose captures draw.
    #[test]
    fn the_anonymous_twin_block_wins() {
        let windows = vec![
            status("com.electron.dockerdesktop", 100.0),
            status("com.agilebits.onepassword7", 140.0),
            status("Item-0", 200.0),
            status("Item-0", 240.0),
        ];
        let kept: Vec<f64> = select(&windows).iter().map(|(index, _)| windows[*index].x).collect();
        assert_eq!(kept, vec![200.0, 240.0]);
    }

    /// If one block is short, the bigger one is kept.
    #[test]
    fn the_larger_block_wins_a_mismatch() {
        let windows = vec![
            status("com.microsoft.Outlook", 100.0),
            status("us.zoom.xos", 140.0),
            status("Item-0", 200.0),
        ];
        let kept: Vec<&str> =
            select(&windows).iter().map(|(index, _)| windows[*index].name.as_str()).collect();
        assert_eq!(kept, vec!["com.microsoft.Outlook", "us.zoom.xos"]);
    }

    /// Named extras alone are all kept: some sessions resolve every name.
    #[test]
    fn a_session_of_named_extras_keeps_them() {
        let windows = vec![status("com.amazon.ACME", 100.0), status("us.zoom.xos", 140.0)];
        assert_eq!(select(&windows).len(), 2);
    }

    #[test]
    fn ink_is_the_span_of_opaque_columns() {
        assert_eq!(ink_columns(&[0, 0, 30, 255, 0, 200, 10, 0]), Some((2, 5)));
        assert_eq!(ink_columns(&[255]), Some((0, 0)));
    }

    /// A faint edge is not ink, and a blank picture has none.
    #[test]
    fn a_blank_picture_has_no_ink() {
        assert_eq!(ink_columns(&[0, INK_ALPHA, 3, 0]), None);
        assert_eq!(ink_columns(&[]), None);
    }

    /// The span ends at the far edge of the last inked column, in points.
    #[test]
    fn ink_is_measured_in_points() {
        assert_eq!(ink_span(&[0, 0, 30, 255, 0, 200, 10, 0], 2.0), Some((1.0, 3.0)));
        assert_eq!(ink_span(&[255], 1.0), Some((0.0, 1.0)));
        assert_eq!(ink_span(&[0, 0], 2.0), None);
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    /// With two displays side by side, each extra belongs to the display under its centre.
    #[test]
    fn an_extra_is_on_the_display_under_its_centre() {
        let builtin = rect(0.0, 0.0, 1728.0, 1117.0);
        let external = rect(1728.0, -300.0, 2560.0, 1440.0);
        let mut window = status("WiFi", 1700.0);
        assert!(on_display(&window, builtin));
        assert!(!on_display(&window, external));
        window.x = 1710.0;
        window.y = -333.0;
        assert!(!on_display(&window, builtin), "centre at 1729");
        assert!(on_display(&window, external));
    }

    fn at(x: f64, y: f64) -> StatusWindow {
        StatusWindow { y, ..status("WiFi", x) }
    }

    /// The external display stacked above the built-in, wider on both sides.
    fn stacked() -> (CGRect, CGRect) {
        (rect(0.0, 0.0, 1728.0, 1117.0), rect(-670.0, -1692.0, 3008.0, 1692.0))
    }

    /// Stacked, the two share an x range, so an extra belongs to the display whose top edge it is on.
    #[test]
    fn a_stacked_display_keeps_only_the_extras_on_its_own_top_edge() {
        let (builtin, external) = stacked();
        assert!(on_display(&at(2200.0, -1725.0), external));
        assert!(!on_display(&at(2200.0, -1725.0), builtin), "right of the built-in");
        assert!(on_display(&at(1500.0, -33.0), builtin));
        assert!(!on_display(&at(1500.0, -33.0), external), "under the external, at the built-in's top");
        assert!(on_display(&at(500.0, -1725.0), external));
        assert!(!on_display(&at(500.0, -1725.0), builtin), "over the built-in, at the external's top");
    }

    /// Hidden a window's height above the top edge, shown at it, and anywhere between as it slides.
    #[test]
    fn an_extra_is_on_its_display_hidden_shown_or_sliding() {
        let (builtin, external) = stacked();
        assert!(on_display(&at(1500.0, -33.0), builtin), "hidden");
        assert!(on_display(&at(1500.0, 0.0), builtin), "shown");
        assert!(on_display(&at(1500.0, -16.0), builtin), "sliding");
        assert!(!on_display(&at(1500.0, -34.0), builtin));
        assert!(!on_display(&at(1500.0, 1.0), builtin));
        assert!(on_display(&at(1500.0, -1692.0), external), "shown");
    }

    /// The hidden menu bar's items at 2x: 14 windows, 33pt tall, at y = -33.
    fn composite() -> Composite {
        let bounds = [rect(1100.0, -33.0, 62.0, 33.0), rect(1680.0, -33.0, 48.0, 33.0)];
        Composite::new(&bounds, (1256, 66)).expect("the union at 2x")
    }

    #[test]
    fn the_scale_is_the_pictures_width_over_the_unions() {
        assert_eq!(composite().scale, 2.0);
        let bounds = [rect(0.0, 0.0, 38.0, 24.0)];
        assert_eq!(Composite::new(&bounds, (38, 24)).map(|c| c.scale), Some(1.0));
    }

    /// A capture that failed can still hand back an image, empty or not the union's shape.
    #[test]
    fn a_picture_that_is_not_the_union_is_refused() {
        let bounds = [rect(1100.0, -33.0, 628.0, 33.0)];
        assert_eq!(Composite::new(&bounds, (0, 0)), None);
        assert_eq!(Composite::new(&bounds, (1, 1)), None);
        assert_eq!(Composite::new(&bounds, (1256, 40)), None, "not 33pt at 2x");
        assert_eq!(Composite::new(&bounds, (1256, 67)).map(|c| c.scale), Some(2.0), "a pixel over");
        assert_eq!(Composite::new(&[], (1256, 66)), None);
        assert_eq!(Composite::new(&[rect(0.0, 0.0, 0.0, 33.0)], (0, 66)), None);
    }

    #[test]
    fn each_window_is_cut_out_of_the_union_at_its_offset() {
        let composite = composite();
        assert_eq!(
            composite.crop(rect(1100.0, -33.0, 62.0, 33.0)),
            Some(PixelRect { x: 0, y: 0, width: 124, height: 66 })
        );
        assert_eq!(
            composite.crop(rect(1680.0, -33.0, 48.0, 33.0)),
            Some(PixelRect { x: 1160, y: 0, width: 96, height: 66 })
        );
    }

    /// At a scale that puts edges between pixels, neighbours share the rounded edge.
    #[test]
    fn neighbours_share_a_rounded_edge() {
        let bounds = [rect(0.0, 0.0, 10.25, 20.0), rect(10.25, 0.0, 9.75, 20.0)];
        let composite = Composite::new(&bounds, (40, 40)).expect("the union at 2x");
        let left = composite.crop(bounds[0]).unwrap();
        let right = composite.crop(bounds[1]).unwrap();
        assert_eq!(left.x + left.width, right.x);
        assert_eq!(right.x + right.width, 40);
    }

    /// Nothing is read past the picture's edge, and a window outside it has no crop.
    #[test]
    fn a_crop_stays_inside_the_picture() {
        let composite = composite();
        assert_eq!(
            composite.crop(rect(1700.0, -40.0, 60.0, 40.0)),
            Some(PixelRect { x: 1200, y: 0, width: 56, height: 66 })
        );
        assert_eq!(composite.crop(rect(900.0, -33.0, 40.0, 33.0)), None);
        assert_eq!(composite.crop(rect(1200.0, -33.0, 0.0, 33.0)), None);
    }

    /// A 4 x 2 RGBA picture with 4 bytes of padding a row, alpha given per pixel.
    fn pixels(alpha: [[u8; 4]; 2]) -> (Vec<u8>, usize) {
        let stride = 4 * 4 + 4;
        let mut out = vec![0xEE; stride * 2];
        for (y, row) in alpha.iter().enumerate() {
            for (x, &a) in row.iter().enumerate() {
                out[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[a / 2, a / 3, a / 4, a]);
            }
        }
        (out, stride)
    }

    #[test]
    fn a_column_is_as_opaque_as_its_most_opaque_pixel() {
        let (bytes, stride) = pixels([[0, 10, 200, 0], [5, 90, 30, 0]]);
        let whole = PixelRect { x: 0, y: 0, width: 4, height: 2 };
        assert_eq!(column_alpha(&bytes, stride, whole), vec![5, 90, 200, 0]);
        let inner = PixelRect { x: 1, y: 1, width: 2, height: 1 };
        assert_eq!(column_alpha(&bytes, stride, inner), vec![90, 30]);
    }

    /// The row padding is not part of the picture, and neither is anything outside the crop.
    #[test]
    fn a_fingerprint_is_of_the_crop_alone() {
        let (bytes, stride) = pixels([[0, 10, 200, 0], [5, 90, 30, 0]]);
        let left = PixelRect { x: 0, y: 0, width: 2, height: 2 };
        let mut other = bytes.clone();
        other[3 * 4 + 3] = 255;
        other[4 * 4] = 0;
        assert_eq!(fingerprint(&bytes, stride, left), fingerprint(&other, stride, left));
        other[4 + 3] = 11;
        assert_ne!(fingerprint(&bytes, stride, left), fingerprint(&other, stride, left));
        let wide = PixelRect { x: 0, y: 0, width: 4, height: 1 };
        let tall = PixelRect { x: 0, y: 0, width: 2, height: 2 };
        let blank = vec![0; stride * 2];
        assert_ne!(fingerprint(&blank, stride, wide), fingerprint(&blank, stride, tall));
    }

    fn stamp(window: u32, kind: Kind, hash: u64) -> Stamp {
        Stamp { window, kind, hash }
    }

    #[test]
    fn the_first_pictures_are_always_sent() {
        assert!(changed(None, &[]));
        assert!(changed(None, &[stamp(7, Kind::Tray, 1)]));
    }

    /// Only a change is sent: a picture, the set of extras, their kinds or their order.
    #[test]
    fn only_a_change_is_sent() {
        let last = [stamp(7, Kind::Tray, 1), stamp(9, Kind::Vital(Vital::WiFi), 2)];
        assert!(!changed(Some(&last), &last));
        assert!(changed(Some(&last), &[last[0], stamp(9, Kind::Vital(Vital::WiFi), 3)]));
        assert!(changed(Some(&last), &[last[0]]));
        assert!(changed(Some(&last), &[last[1], last[0]]));
        assert!(changed(Some(&last), &[last[0], stamp(9, Kind::Tray, 2)]));
        assert!(changed(Some(&last), &[last[0], stamp(10, Kind::Vital(Vital::WiFi), 2)]));
    }

    /// An extra is sent with the picture last sent for it unless its window, kind or pixels changed,
    /// wherever it now is in the row.
    #[test]
    fn an_unchanged_extra_keeps_the_picture_last_sent() {
        let last = [stamp(7, Kind::Tray, 1), stamp(9, Kind::Vital(Vital::WiFi), 2)];
        let now = [
            stamp(9, Kind::Vital(Vital::WiFi), 2),
            stamp(7, Kind::Tray, 5),
            stamp(11, Kind::Tray, 1),
            stamp(9, Kind::Tray, 2),
        ];
        assert_eq!(reused(&last, &now), vec![Some(1), None, None, None]);
        assert_eq!(reused(&last, &last), vec![Some(0), Some(1)]);
        assert_eq!(reused(&[], &last), vec![None, None]);
    }

    #[test]
    fn the_extras_are_pictured_at_once_and_then_every_tick() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert_eq!(schedule.wait(start), Some(Duration::ZERO));
        assert!(schedule.take(start));
        assert!(!schedule.take(start));
        let half = start + TICK / 2;
        assert_eq!(schedule.wait(half), Some(TICK / 2));
        assert!(!schedule.take(half));
        assert!(schedule.take(start + TICK));
    }

    /// While paused the thread waits for a command rather than a tick, and pictures nothing.
    #[test]
    fn nothing_is_pictured_while_paused() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.on(Command::Pause(true), start);
        assert_eq!(schedule.wait(start), None);
        assert!(!schedule.take(start + TICK * 5));
        schedule.on(Command::Refresh, start + TICK * 5);
        assert!(!schedule.take(start + TICK * 5));
        schedule.on(Command::Pause(false), start + TICK * 6);
        assert!(schedule.take(start + TICK * 6), "the picture that fell due while paused");
    }

    /// A flight that starts between a pass's listing and its capture calls the capture off. The thread
    /// then waits for commands, and the picture is taken as soon as the pause ends.
    #[test]
    fn a_pause_that_lands_after_the_listing_stops_the_capture() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert!(schedule.take(start));
        let listed = start + TICK / 100;
        assert!(!schedule.may_capture(true, listed));
        assert_eq!(schedule.wait(listed), None);
        assert!(!schedule.take(start + TICK * 3));
        schedule.on(Command::Pause(true), listed);
        schedule.on(Command::Pause(false), start + TICK / 2);
        assert!(schedule.take(start + TICK / 2), "the pass called off, at once");
    }

    #[test]
    fn with_no_pause_the_capture_goes_ahead_and_keeps_its_tick() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert!(schedule.take(start));
        assert!(schedule.may_capture(false, start));
        assert!(!schedule.take(start + TICK / 2));
        assert!(schedule.take(start + TICK));
    }

    #[test]
    fn a_refresh_pictures_now_and_restarts_the_tick() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert!(schedule.take(start));
        let soon = start + TICK / 4;
        schedule.on(Command::Refresh, soon);
        assert_eq!(schedule.wait(soon), Some(Duration::ZERO));
        assert!(schedule.take(soon));
        assert!(!schedule.take(start + TICK));
        assert!(schedule.take(soon + TICK));
    }
}
