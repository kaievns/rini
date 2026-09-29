//! Where everything on a bar goes, and what a click at a point asks for.
//!
//! **Measured in ink, not in boxes.** Each piece is placed by where its ink starts and ends, and the
//! gaps below are ink to ink. That is what the sketchybar bar this replaces did (it sized items by
//! their glyphs' path bounds), and it is why its spacing read as even although a "1" is narrower than
//! a "2" and every icon carries a different margin. The numbers were read off that bar's pixels on
//! 2026-09-29; see "Measured" in `src/bar/docs/README.md`.
//!
//! The platform measures each piece's ink and draws its picture so the ink lands on the span handed
//! back. Nothing here knows what a font is.

use rini_core::ids::WindowId;

use super::extras::Vital;
use super::model::{DisplayBar, Row};

/// The bar's height: exactly the band macOS keeps above windows on a display with a notch, so the bar
/// costs no window any height there.
pub const HEIGHT: f64 = 32.0;

/// Applications drawn for the shown workspace before the rest fold behind a count.
pub const MAX_GLYPHS: usize = 5;

/// Baselines, from the bar's top edge. The 14pt numerals sit a point higher than the 12-13pt text.
pub const NUMERAL_BASELINE: f64 = 20.0;
pub const TEXT_BASELINE: f64 = 21.0;
/// Application glyphs are marks, not letters, so they are centred on their own ink at this height.
pub const GLYPH_CENTRE: f64 = 15.75;
/// The hairlines between zones: top and height.
pub const HAIRLINE_TOP: f64 = 9.0;
pub const HAIRLINE_HEIGHT: f64 = 14.0;
/// The shown workspace's rule, flush with the bottom edge.
pub const UNDERLINE_HEIGHT: f64 = 2.0;

/// Screen edge to where the first numeral's padding starts.
const LEFT_EDGE: f64 = 9.5;
/// Between one workspace's group and the next.
const GROUP_GAP: f64 = 5.0;

/// Screen edge to the end of the time's ink.
const TIME_TO_EDGE: f64 = 27.0;
const DATE_TO_TIME: f64 = 16.0;
const DIVIDER_TO_DATE: f64 = 11.5;
const VITAL_TO_DIVIDER: f64 = 10.0;
/// Between the vitals, and either side of the chevron. The old bar measured 17.5 and 14 between the
/// vitals and 16 and 22 either side of the chevron; its padding could not be made even, this can.
const ICON_GAP: f64 = 16.0;
/// Between tray icons: the old bar's landing value, measured 12.5 to 17 at a 14.4 mean.
const TRAY_GAP: f64 = 15.0;
/// Least room kept between the end of the left zone and the start of the right.
const ZONE_CLEARANCE: f64 = 16.0;

/// Something drawn on the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Piece {
    /// A workspace, by index.
    Numeral(usize),
    /// One of the shown workspace's glyphs, by index into `DisplayBar::glyphs`.
    Glyph(usize),
    /// How many glyphs are folded away, or the way back.
    More,
    PlaceDivider,
    FocusApp,
    FocusDot,
    FocusTitle,
    /// A tray extra, by index, left to right.
    Tray(usize),
    Chevron,
    Vital(Vital),
    ClockDivider,
    Date,
    Time,
}

/// A horizontal run of ink, in points from the bar's left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub x0: f64,
    pub x1: f64,
}

impl Span {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    fn contains(&self, x: f64) -> bool {
        self.x0 <= x && x < self.x1
    }

    fn grown(&self, left: f64, right: f64) -> Span {
        Span { x0: self.x0 - left, x1: self.x1 + right }
    }
}

/// What a click on a region is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Workspace(usize),
    Window(WindowId),
    /// Fold or unfold the glyphs.
    Fold,
    /// Open or close the tray.
    Tray,
}

/// How many of the shown workspace's glyphs are drawn, and what the fold indicator says.
#[derive(Clone, Debug, PartialEq)]
pub struct Fold {
    pub shown: usize,
    pub more: Option<String>,
}

pub fn fold(glyphs: usize, expanded: bool) -> Fold {
    if glyphs <= MAX_GLYPHS {
        return Fold { shown: glyphs, more: None };
    }
    if expanded {
        // The minus sign, not a hyphen: it is as wide as the plus it replaces.
        Fold { shown: glyphs, more: Some("\u{2212}".into()) }
    } else {
        Fold { shown: MAX_GLYPHS, more: Some(format!("+{}", glyphs - MAX_GLYPHS)) }
    }
}

/// Everything placed on one bar.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Scene {
    /// Each drawn piece and the span its ink covers, left zone first.
    pub pieces: Vec<(Piece, Span)>,
    /// Under the shown workspace's numeral and its glyphs.
    pub underline: Option<Span>,
    /// Clickable regions, full bar height.
    pub hits: Vec<(Span, Target)>,
}

impl Scene {
    pub fn span(&self, piece: Piece) -> Option<Span> {
        self.pieces.iter().find(|(p, _)| *p == piece).map(|(_, span)| *span)
    }

    pub fn hit(&self, x: f64) -> Option<Target> {
        self.hits.iter().find(|(span, _)| span.contains(x)).map(|(_, target)| *target)
    }
}

/// What the right zone holds, which is the same on every display.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Right {
    /// Tray extras, left to right.
    pub tray: usize,
    /// The vitals that exist, in `Vital::ORDER`.
    pub vitals: Vec<Vital>,
}

/// Lays out one display's bar, `width` points wide.
///
/// `ink` answers how wide a piece's ink is, or `None` when it is not drawn. A piece with no ink is
/// left out, and the gaps close over it.
pub fn lay_out(
    width: f64,
    bar: &DisplayBar,
    fold: &Fold,
    right: &Right,
    ink: impl Fn(Piece) -> Option<f64>,
) -> Scene {
    let mut scene = Scene::default();
    lay_out_right(width, right, &ink, &mut scene);
    let limit = scene.pieces.iter().map(|(_, span)| span.x0).fold(width, f64::min) - ZONE_CLEARANCE;
    lay_out_left(bar, fold, &ink, limit, &mut scene);
    scene.pieces.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0));
    scene
}

/// Padding either side of a left-zone piece. The gap between two neighbours is the first's right
/// padding and the second's left, plus `GROUP_GAP` after each workspace.
fn pads(piece: Piece, bar: &DisplayBar) -> (f64, f64) {
    match piece {
        Piece::Numeral(_) => (8.5, 8.5),
        Piece::Glyph(index) if bar.glyphs.get(index).is_some_and(|glyph| glyph.lit) => (8.0, 9.0),
        Piece::Glyph(_) => (2.5, 2.5),
        Piece::More => (0.0, 8.0),
        Piece::PlaceDivider => (4.5, 5.0),
        Piece::FocusApp => (9.0, 8.0),
        Piece::FocusDot => (8.0, 3.0),
        Piece::FocusTitle => (3.0, 8.0),
        _ => (0.0, 0.0),
    }
}

fn lay_out_left(
    bar: &DisplayBar,
    fold: &Fold,
    ink: &impl Fn(Piece) -> Option<f64>,
    limit: f64,
    scene: &mut Scene,
) {
    let mut cursor = LEFT_EDGE;
    let place = |piece: Piece, scene: &mut Scene, cursor: &mut f64| -> Option<Span> {
        let width = ink(piece)?;
        let (left, right) = pads(piece, bar);
        let span = Span { x0: *cursor + left, x1: *cursor + left + width };
        *cursor = span.x1 + right;
        scene.pieces.push((piece, span));
        Some(span)
    };

    for (index, row) in bar.rows.iter().enumerate() {
        let Some(numeral) = place(Piece::Numeral(index), scene, &mut cursor) else {
            continue;
        };
        let (left, right) = pads(Piece::Numeral(index), bar);
        scene.hits.push((numeral.grown(left, right), Target::Workspace(index)));

        if *row == Row::Shown {
            let start = numeral.x0 - left;
            for (glyph_index, glyph) in bar.glyphs.iter().enumerate().take(fold.shown) {
                if let Some(span) = place(Piece::Glyph(glyph_index), scene, &mut cursor) {
                    let (left, right) = pads(Piece::Glyph(glyph_index), bar);
                    scene.hits.push((span.grown(left, right), Target::Window(glyph.window)));
                }
            }
            if fold.more.is_some()
                && let Some(span) = place(Piece::More, scene, &mut cursor)
            {
                scene.hits.push((span.grown(0.0, 8.0), Target::Fold));
            }
            scene.underline = Some(Span { x0: start, x1: cursor });
        }
        cursor += GROUP_GAP;
    }

    if bar.focus.is_none() {
        return;
    }
    place(Piece::PlaceDivider, scene, &mut cursor);
    place(Piece::FocusApp, scene, &mut cursor);
    if bar.focus.as_ref().is_some_and(|focus| !focus.title.is_empty()) {
        place(Piece::FocusDot, scene, &mut cursor);
        if let Some(title) = place(Piece::FocusTitle, scene, &mut cursor)
            && title.x1 > limit
        {
            // Cut at the right zone rather than drawn under it.
            let last = scene.pieces.last_mut().expect("the title was just placed");
            last.1.x1 = limit.max(title.x0);
        }
    }
}

fn lay_out_right(
    width: f64,
    right: &Right,
    ink: &impl Fn(Piece) -> Option<f64>,
    scene: &mut Scene,
) {
    // Right to left, each piece's ink ending one gap short of the last one's start.
    let mut edge = width - TIME_TO_EDGE;
    let mut gap = 0.0;
    let mut place = |piece: Piece, next_gap: f64, scene: &mut Scene| -> Option<Span> {
        let width = ink(piece)?;
        let x1 = edge - gap;
        let span = Span { x0: x1 - width, x1 };
        edge = span.x0;
        gap = next_gap;
        scene.pieces.push((piece, span));
        Some(span)
    };

    place(Piece::Time, DATE_TO_TIME, scene);
    place(Piece::Date, DIVIDER_TO_DATE, scene);
    place(Piece::ClockDivider, VITAL_TO_DIVIDER, scene);
    for vital in right.vitals.iter().rev() {
        place(Piece::Vital(*vital), ICON_GAP, scene);
    }
    if right.tray == 0 {
        return;
    }
    if let Some(chevron) = place(Piece::Chevron, ICON_GAP, scene) {
        scene.hits.push((chevron.grown(8.0, 8.0), Target::Tray));
    }
    for index in (0..right.tray).rev() {
        place(Piece::Tray(index), TRAY_GAP, scene);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::domain::model::{FocusLabel, Glyph};

    const WIDTH: f64 = 1728.0;

    fn glyph(n: u32, lit: bool) -> Glyph {
        Glyph { app: format!("app{n}"), window: WindowId::new(n as i32, n), lit }
    }

    /// The bar the numbers were measured on: four workspaces with the second shown, four glyphs with
    /// the first lit, and a focused window.
    fn bar() -> DisplayBar {
        DisplayBar {
            uuid: "d".into(),
            screen: 1,
            rows: vec![Row::Occupied, Row::Shown, Row::Empty, Row::Empty],
            glyphs: vec![glyph(1, true), glyph(2, false), glyph(3, false), glyph(4, false)],
            focus: Some(FocusLabel { app: "Code".into(), title: "lib.rs — rini".into() }),
        }
    }

    fn right() -> Right {
        Right { tray: 3, vitals: Vital::ORDER.to_vec() }
    }

    /// Widths as they measured: a "1" is 5pt of ink and a "2" 6.5pt, glyphs 12 to 15.
    fn ink(piece: Piece) -> Option<f64> {
        Some(match piece {
            Piece::Numeral(0) => 5.0,
            Piece::Numeral(_) => 6.5,
            Piece::Glyph(0) => 14.0,
            Piece::Glyph(_) => 12.0,
            Piece::More => 12.0,
            Piece::PlaceDivider | Piece::ClockDivider => 1.0,
            Piece::FocusApp => 26.0,
            Piece::FocusDot => 2.0,
            Piece::FocusTitle => 90.0,
            Piece::Time => 36.5,
            Piece::Date => 60.0,
            Piece::Vital(Vital::Battery) => 25.5,
            Piece::Vital(_) => 17.0,
            Piece::Chevron => 5.5,
            Piece::Tray(_) => 16.0,
        })
    }

    fn scene() -> Scene {
        lay_out(WIDTH, &bar(), &fold(4, false), &right(), ink)
    }

    fn gap(scene: &Scene, a: Piece, b: Piece) -> f64 {
        scene.span(b).unwrap().x0 - scene.span(a).unwrap().x1
    }

    /// The left zone's rhythm, against the old bar: 18 from the edge, 22 between numerals, 16.5 from
    /// the shown numeral to the lit glyph, 11.5 then 5 between glyphs.
    #[test]
    fn the_left_zone_keeps_the_measured_rhythm() {
        let scene = scene();
        assert_eq!(scene.span(Piece::Numeral(0)).unwrap().x0, 18.0);
        assert_eq!(gap(&scene, Piece::Numeral(0), Piece::Numeral(1)), 22.0);
        assert_eq!(gap(&scene, Piece::Numeral(1), Piece::Glyph(0)), 16.5);
        assert_eq!(gap(&scene, Piece::Glyph(0), Piece::Glyph(1)), 11.5);
        assert_eq!(gap(&scene, Piece::Glyph(1), Piece::Glyph(2)), 5.0);
        assert_eq!(gap(&scene, Piece::Glyph(3), Piece::Numeral(2)), 16.0);
        assert_eq!(gap(&scene, Piece::Numeral(2), Piece::Numeral(3)), 22.0);
    }

    /// Divider, application, dot and title: 18, 14, 16, 6.
    #[test]
    fn the_focus_zone_follows_the_workspaces() {
        let scene = scene();
        assert_eq!(gap(&scene, Piece::Numeral(3), Piece::PlaceDivider), 18.0);
        assert_eq!(gap(&scene, Piece::PlaceDivider, Piece::FocusApp), 14.0);
        assert_eq!(gap(&scene, Piece::FocusApp, Piece::FocusDot), 16.0);
        assert_eq!(gap(&scene, Piece::FocusDot, Piece::FocusTitle), 6.0);
    }

    /// The right zone from the edge in: 27, 16, 11.5, 10, then an even 16 between the vitals and
    /// either side of the chevron, and 15 through the tray.
    #[test]
    fn the_right_zone_keeps_the_measured_rhythm() {
        let scene = scene();
        assert_eq!(WIDTH - scene.span(Piece::Time).unwrap().x1, 27.0);
        assert_eq!(gap(&scene, Piece::Date, Piece::Time), 16.0);
        assert_eq!(gap(&scene, Piece::ClockDivider, Piece::Date), 11.5);
        assert_eq!(gap(&scene, Piece::Vital(Vital::Battery), Piece::ClockDivider), 10.0);
        assert_eq!(gap(&scene, Piece::Vital(Vital::Sound), Piece::Vital(Vital::Battery)), 16.0);
        assert_eq!(gap(&scene, Piece::Vital(Vital::WiFi), Piece::Vital(Vital::Sound)), 16.0);
        assert_eq!(gap(&scene, Piece::Chevron, Piece::Vital(Vital::WiFi)), 16.0);
        assert_eq!(gap(&scene, Piece::Tray(2), Piece::Chevron), 16.0);
        assert_eq!(gap(&scene, Piece::Tray(0), Piece::Tray(1)), 15.0);
    }

    /// The rule runs under the shown numeral and its glyphs, padding included, and stops there.
    #[test]
    fn the_underline_spans_the_shown_group() {
        let scene = scene();
        let underline = scene.underline.unwrap();
        assert_eq!(underline.x0, scene.span(Piece::Numeral(1)).unwrap().x0 - 8.5);
        assert_eq!(underline.x1, scene.span(Piece::Glyph(3)).unwrap().x1 + 2.5);
    }

    #[test]
    fn a_click_on_a_numeral_asks_for_its_workspace() {
        let scene = scene();
        let numeral = scene.span(Piece::Numeral(2)).unwrap();
        assert_eq!(scene.hit(numeral.x0 + 1.0), Some(Target::Workspace(2)));
        // Its padding counts: the box is where sketchybar's item was.
        assert_eq!(scene.hit(numeral.x0 - 7.0), Some(Target::Workspace(2)));
    }

    #[test]
    fn a_click_on_a_glyph_asks_for_its_window() {
        let scene = scene();
        let glyph = scene.span(Piece::Glyph(1)).unwrap();
        assert_eq!(scene.hit(glyph.x0 + 1.0), Some(Target::Window(WindowId::new(2, 2))));
    }

    #[test]
    fn a_click_on_the_chevron_toggles_the_tray() {
        let scene = scene();
        let chevron = scene.span(Piece::Chevron).unwrap();
        assert_eq!(scene.hit(chevron.x0 + 1.0), Some(Target::Tray));
    }

    /// The gaps between groups, the title, the clock and the tray icons are not buttons.
    #[test]
    fn a_click_on_nothing_asks_for_nothing() {
        let scene = scene();
        assert_eq!(scene.hit(scene.span(Piece::FocusTitle).unwrap().x0 + 5.0), None);
        assert_eq!(scene.hit(scene.span(Piece::Time).unwrap().x0 + 5.0), None);
        assert_eq!(scene.hit(scene.span(Piece::Tray(0)).unwrap().x0 + 5.0), None);
        assert_eq!(scene.hit(2.0), None);
    }

    /// Past five the tail folds behind a count, and unfolded there is a way back.
    #[test]
    fn a_long_glyph_list_folds() {
        assert_eq!(fold(4, false), Fold { shown: 4, more: None });
        assert_eq!(fold(8, false), Fold { shown: 5, more: Some("+3".into()) });
        assert_eq!(fold(8, true), Fold { shown: 8, more: Some("\u{2212}".into()) });
    }

    #[test]
    fn the_fold_indicator_is_clickable() {
        let mut bar = bar();
        bar.glyphs = (1..=8).map(|n| glyph(n, n == 1)).collect();
        let scene = lay_out(WIDTH, &bar, &fold(8, false), &right(), ink);
        assert!(scene.span(Piece::Glyph(5)).is_none());
        let more = scene.span(Piece::More).unwrap();
        assert_eq!(gap(&scene, Piece::Glyph(4), Piece::More), 2.5);
        assert_eq!(scene.hit(more.x0 + 1.0), Some(Target::Fold));
        assert_eq!(scene.underline.unwrap().x1, more.x1 + 8.0);
    }

    /// Without a focused window the divider and the focus zone are not drawn at all.
    #[test]
    fn no_focus_is_no_focus_zone() {
        let mut bar = bar();
        bar.focus = None;
        let scene = lay_out(WIDTH, &bar, &fold(4, false), &right(), ink);
        assert!(scene.span(Piece::PlaceDivider).is_none());
        assert!(scene.span(Piece::FocusApp).is_none());
    }

    /// A title that would run into the right zone is cut short of it.
    #[test]
    fn a_title_stops_short_of_the_right_zone() {
        let scene = lay_out(700.0, &bar(), &fold(4, false), &right(), |piece| match piece {
            Piece::FocusTitle => Some(600.0),
            other => ink(other),
        });
        let title = scene.span(Piece::FocusTitle).unwrap();
        let tray = scene.span(Piece::Tray(0)).unwrap();
        assert_eq!(title.x1, tray.x0 - ZONE_CLEARANCE);
    }

    /// No tray, no chevron, and the vitals close up; a missing vital closes up too.
    #[test]
    fn missing_extras_close_up() {
        let right = Right { tray: 0, vitals: vec![Vital::WiFi, Vital::Sound] };
        let scene = lay_out(WIDTH, &bar(), &fold(4, false), &right, ink);
        assert!(scene.span(Piece::Chevron).is_none());
        assert_eq!(gap(&scene, Piece::Vital(Vital::Sound), Piece::ClockDivider), 10.0);
    }

    /// Pieces come back left to right, whatever order they were placed in.
    #[test]
    fn pieces_are_in_drawing_order() {
        let scene = scene();
        assert!(scene.pieces.windows(2).all(|pair| pair[0].1.x0 <= pair[1].1.x0));
    }
}
