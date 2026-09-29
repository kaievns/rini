//! What each piece of a bar says, which pieces a bar has to measure, which menu extra stands behind
//! a piece, what a click on a region asks for, and the time it says.

use std::time::{Duration, Instant};

use super::extras::{Kind, Vital};
use super::format::{clock_label, date_label, until_next_minute};
use super::glyphs;
use super::layout::{Fold, Piece, Right, Target};
use super::model::{Action, DisplayBar};
use super::palette::Colour;
use super::style::{self, CHEVRON_CLOSED, CHEVRON_OPEN, FOCUS_DOT, Face};

/// The wall clock, as much of it as the bar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    /// From Sunday = 0, as `tm_wday` counts.
    pub weekday: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// The clock as last read, what the bar last showed of it, and when its minute turns over.
///
/// Read on every wake, not only the minute's: any wake can be the first past the boundary.
#[derive(Clone, Copy, Debug)]
pub struct Timepiece {
    read: Clock,
    shown: Option<Clock>,
    turns: Instant,
}

impl Timepiece {
    /// `clock`, read at `at` and `seconds` into its minute.
    pub fn new((clock, seconds): (Clock, f64), at: Instant) -> Self {
        Self {
            read: clock,
            shown: None,
            turns: at + until_next_minute(seconds),
        }
    }

    /// A fresh reading, as `new` takes one. What the bar shows stays noted.
    pub fn read(&mut self, reading: (Clock, f64), at: Instant) {
        *self = Self {
            shown: self.shown,
            ..Self::new(reading, at)
        };
    }

    /// The clock to draw, noted as shown.
    pub fn show(&mut self) -> Clock {
        self.shown = Some(self.read);
        self.read
    }

    /// Whether the bar shows a minute other than the one last read.
    pub fn behind(&self) -> bool {
        self.shown.is_some_and(|shown| shown != self.read)
    }

    /// From `now` until the minute last read turns over, measured from when it was read, so a wake
    /// that took a while still lands on the boundary. Zero once it has passed.
    pub fn until_turn(&self, now: Instant) -> Duration {
        self.turns.saturating_duration_since(now)
    }
}

/// What a text piece says and how it is set.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub face: Face,
    pub colour: Colour,
    /// Drawn in a stand-in face when `face` is not installed. `None` draws `text` itself.
    pub stand_in: Option<String>,
}

/// What a bar says beyond its model.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub fold: &'a Fold,
    pub clock: Clock,
    pub tray_open: bool,
}

/// The words `piece` draws, or `None` for a piece that is not text or has nothing to say.
pub fn label(piece: Piece, bar: &DisplayBar, context: Context) -> Option<Label> {
    let focus = bar.focus.as_ref();
    let titled = focus.filter(|focus| !focus.title.is_empty());
    let (text, stand_in) = match piece {
        Piece::Numeral(row) => (bar.rows.get(row).map(|_| (row + 1).to_string())?, None),
        Piece::Glyph(index) => {
            let app = &bar.glyphs.get(index)?.app;
            (glyphs::token(app).to_string(), Some(initial(app)))
        }
        Piece::More => (context.fold.more.clone()?, None),
        Piece::FocusApp => (focus?.app.clone(), None),
        Piece::FocusDot => {
            titled?;
            (FOCUS_DOT.to_string(), None)
        }
        Piece::FocusTitle => (titled?.title.clone(), None),
        Piece::Time => (clock_label(context.clock.hour, context.clock.minute), None),
        Piece::Date => (date_label(context.clock.weekday, context.clock.day), None),
        Piece::Chevron => {
            let chevron = if context.tray_open {
                CHEVRON_OPEN
            } else {
                CHEVRON_CLOSED
            };
            (chevron.to_string(), None)
        }
        Piece::PlaceDivider | Piece::ClockDivider | Piece::Vital(_) | Piece::Tray(_) => {
            return None;
        }
    };
    Some(Label {
        text,
        face: style::face(piece, bar),
        colour: style::colour(piece, bar),
        stand_in,
    })
}

/// An application's first letter, which is what stands for its mark without the glyph font.
fn initial(app: &str) -> String {
    app.chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_default()
}

/// Every piece `lay_out` can ask the ink of for `bar`, so each is measured once before it runs.
pub fn pieces(bar: &DisplayBar, fold: &Fold, right: &Right) -> Vec<Piece> {
    let mut out: Vec<Piece> = (0..bar.rows.len()).map(Piece::Numeral).collect();
    out.extend((0..fold.shown.min(bar.glyphs.len())).map(Piece::Glyph));
    if fold.more.is_some() {
        out.push(Piece::More);
    }
    if let Some(focus) = &bar.focus {
        out.extend([Piece::PlaceDivider, Piece::FocusApp]);
        if !focus.title.is_empty() {
            out.extend([Piece::FocusDot, Piece::FocusTitle]);
        }
    }
    out.extend((0..right.tray).map(Piece::Tray));
    if right.tray > 0 {
        out.push(Piece::Chevron);
    }
    out.extend(right.vitals.iter().map(|vital| Piece::Vital(*vital)));
    out.extend([Piece::ClockDivider, Piece::Date, Piece::Time]);
    out
}

/// The right zone for the extras drawn, given each one's kind in menu-bar order.
pub fn right(kinds: &[Kind]) -> Right {
    Right {
        tray: kinds.iter().filter(|kind| **kind == Kind::Tray).count(),
        vitals: Vital::ORDER
            .into_iter()
            .filter(|vital| kinds.contains(&Kind::Vital(*vital)))
            .collect(),
    }
}

/// Which extra, by its index in `kinds`, a vital or tray piece is the picture of.
pub fn extra(piece: Piece, kinds: &[Kind]) -> Option<usize> {
    match piece {
        Piece::Vital(vital) => kinds.iter().position(|kind| *kind == Kind::Vital(vital)),
        Piece::Tray(index) => kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| **kind == Kind::Tray)
            .nth(index)
            .map(|(at, _)| at),
        _ => None,
    }
}

/// What a click is for, once it is known which display's bar took it.
#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// Something only the window manager can do.
    Ask(Action),
    /// Fold or unfold the glyphs, on every display.
    Fold,
    /// Open or close the tray, on every display.
    Tray,
}

pub fn click(target: Target, display: &str) -> Click {
    match target {
        Target::Workspace(index) => Click::Ask(Action::ShowWorkspace {
            display: display.to_string(),
            index,
        }),
        Target::Window(window) => Click::Ask(Action::Focus(window)),
        Target::Fold => Click::Fold,
        Target::Tray => Click::Tray,
    }
}

#[cfg(test)]
mod tests {
    use rini_core::ids::WindowId;

    use super::*;
    use crate::bar::domain::layout::fold;
    use crate::bar::domain::model::{FocusLabel, Glyph, Row};

    fn bar() -> DisplayBar {
        DisplayBar {
            uuid: "d".into(),
            screen: 1,
            rows: vec![Row::Occupied, Row::Shown, Row::Empty],
            glyphs: ["Code", "Ghostty", "Zen", "Slack", "Mail", "Notes", "Music"]
                .iter()
                .zip(1..)
                .map(|(app, n)| Glyph {
                    app: app.to_string(),
                    window: WindowId::new(n, n as u32),
                    lit: n == 1,
                })
                .collect(),
            focus: Some(FocusLabel {
                app: "Code".into(),
                title: "lib.rs — rini".into(),
            }),
        }
    }

    const CLOCK: Clock = Clock {
        weekday: 2,
        day: 29,
        hour: 7,
        minute: 5,
    };

    fn says(piece: Piece, bar: &DisplayBar, fold: &Fold, tray_open: bool) -> Option<String> {
        label(piece, bar, Context { fold, clock: CLOCK, tray_open }).map(|label| label.text)
    }

    /// Numerals count from one, as the workspaces are named.
    #[test]
    fn a_numeral_says_its_workspace_counted_from_one() {
        let bar = bar();
        let folded = fold(bar.glyphs.len(), false);
        assert_eq!(
            says(Piece::Numeral(0), &bar, &folded, true).as_deref(),
            Some("1")
        );
        assert_eq!(
            says(Piece::Numeral(2), &bar, &folded, true).as_deref(),
            Some("3")
        );
        assert_eq!(says(Piece::Numeral(3), &bar, &folded, true), None);
    }

    /// A glyph is its application's ligature token, with its first letter to stand in for it.
    #[test]
    fn a_glyph_is_a_token_with_an_initial_to_stand_in() {
        let bar = bar();
        let folded = fold(bar.glyphs.len(), false);
        let label = label(Piece::Glyph(0), &bar, Context {
            fold: &folded,
            clock: CLOCK,
            tray_open: true,
        })
        .unwrap();
        assert_eq!(label.text, ":code:");
        assert_eq!(label.stand_in.as_deref(), Some("C"));
        assert_eq!(label.face, style::face(Piece::Glyph(0), &bar));
        assert_eq!(label.colour, style::colour(Piece::Glyph(0), &bar));
    }

    #[test]
    fn a_lower_case_application_stands_in_upper_case() {
        assert_eq!(initial("zoom.us"), "Z");
        assert_eq!(initial(""), "");
    }

    #[test]
    fn the_fold_indicator_says_what_the_fold_says() {
        let bar = bar();
        assert_eq!(
            says(Piece::More, &bar, &fold(7, false), true).as_deref(),
            Some("+2")
        );
        assert_eq!(
            says(Piece::More, &bar, &fold(7, true), true).as_deref(),
            Some("\u{2212}")
        );
        assert_eq!(says(Piece::More, &bar, &fold(3, false), true), None);
    }

    #[test]
    fn the_focus_zone_says_the_application_then_the_title() {
        let bar = bar();
        let folded = fold(7, false);
        assert_eq!(
            says(Piece::FocusApp, &bar, &folded, true).as_deref(),
            Some("Code")
        );
        assert_eq!(
            says(Piece::FocusDot, &bar, &folded, true).as_deref(),
            Some(FOCUS_DOT)
        );
        assert_eq!(
            says(Piece::FocusTitle, &bar, &folded, true).as_deref(),
            Some("lib.rs — rini")
        );
    }

    /// No title, no dot: the dot separates two things.
    #[test]
    fn an_untitled_window_has_no_dot() {
        let mut bar = bar();
        bar.focus.as_mut().unwrap().title.clear();
        let folded = fold(7, false);
        assert_eq!(says(Piece::FocusDot, &bar, &folded, true), None);
        assert_eq!(says(Piece::FocusTitle, &bar, &folded, true), None);
        assert!(!pieces(&bar, &folded, &Right::default()).contains(&Piece::FocusDot));
    }

    #[test]
    fn the_clock_says_the_time_and_the_date() {
        let bar = bar();
        let folded = fold(7, false);
        assert_eq!(says(Piece::Time, &bar, &folded, true).as_deref(), Some("07:05"));
        assert_eq!(
            says(Piece::Date, &bar, &folded, true).as_deref(),
            Some("Tue 29th")
        );
    }

    /// Open, the chevron points the way the icons go on closing; closed, the way they will open.
    #[test]
    fn the_chevron_points_the_way_the_tray_will_go() {
        let bar = bar();
        let folded = fold(7, false);
        assert_eq!(
            says(Piece::Chevron, &bar, &folded, true).as_deref(),
            Some(CHEVRON_OPEN)
        );
        assert_eq!(
            says(Piece::Chevron, &bar, &folded, false).as_deref(),
            Some(CHEVRON_CLOSED)
        );
    }

    #[test]
    fn rules_and_pictures_have_no_words() {
        let bar = bar();
        let folded = fold(7, false);
        for piece in [
            Piece::PlaceDivider,
            Piece::ClockDivider,
            Piece::Vital(Vital::WiFi),
            Piece::Tray(0),
        ] {
            assert_eq!(says(piece, &bar, &folded, true), None, "{piece:?}");
        }
    }

    /// Everything `lay_out` can ask about is measured, and nothing it cannot: folded glyphs are
    /// left out, as is a chevron with no tray.
    #[test]
    fn the_pieces_measured_are_the_ones_a_bar_can_draw() {
        let bar = bar();
        let right = Right {
            tray: 2,
            vitals: vec![Vital::WiFi, Vital::Battery],
        };
        let measured = pieces(&bar, &fold(7, false), &right);
        assert_eq!(measured, vec![
            Piece::Numeral(0),
            Piece::Numeral(1),
            Piece::Numeral(2),
            Piece::Glyph(0),
            Piece::Glyph(1),
            Piece::Glyph(2),
            Piece::Glyph(3),
            Piece::Glyph(4),
            Piece::More,
            Piece::PlaceDivider,
            Piece::FocusApp,
            Piece::FocusDot,
            Piece::FocusTitle,
            Piece::Tray(0),
            Piece::Tray(1),
            Piece::Chevron,
            Piece::Vital(Vital::WiFi),
            Piece::Vital(Vital::Battery),
            Piece::ClockDivider,
            Piece::Date,
            Piece::Time,
        ]);
        let unfolded = pieces(&bar, &fold(7, true), &Right::default());
        assert!(unfolded.contains(&Piece::Glyph(6)));
        assert!(!unfolded.contains(&Piece::Chevron));
    }

    /// Menu-bar order for the tray; the vitals always go Wi-Fi, sound, battery, whatever order
    /// macOS lists them in.
    #[test]
    fn the_right_zone_comes_from_the_extras_kinds() {
        let kinds = [
            Kind::Tray,
            Kind::Vital(Vital::Battery),
            Kind::Tray,
            Kind::Vital(Vital::WiFi),
            Kind::Tray,
        ];
        assert_eq!(right(&kinds), Right {
            tray: 3,
            vitals: vec![Vital::WiFi, Vital::Battery]
        });
        assert_eq!(right(&[]), Right::default());
    }

    #[test]
    fn a_tray_or_vital_piece_finds_its_extra() {
        let kinds = [
            Kind::Tray,
            Kind::Vital(Vital::Battery),
            Kind::Tray,
            Kind::Vital(Vital::WiFi),
        ];
        assert_eq!(extra(Piece::Tray(0), &kinds), Some(0));
        assert_eq!(extra(Piece::Tray(1), &kinds), Some(2));
        assert_eq!(extra(Piece::Tray(2), &kinds), None);
        assert_eq!(extra(Piece::Vital(Vital::WiFi), &kinds), Some(3));
        assert_eq!(extra(Piece::Vital(Vital::Sound), &kinds), None);
        assert_eq!(extra(Piece::Time, &kinds), None);
    }

    /// A numeral shows its workspace on the display it was clicked on, not the focused one.
    #[test]
    fn a_numeral_asks_for_its_workspace_on_its_own_display() {
        assert_eq!(
            click(Target::Workspace(2), "B5E7"),
            Click::Ask(Action::ShowWorkspace {
                display: "B5E7".into(),
                index: 2
            })
        );
    }

    #[test]
    fn a_glyph_asks_for_its_window() {
        let window = WindowId::new(804, 4);
        assert_eq!(
            click(Target::Window(window), "d"),
            Click::Ask(Action::Focus(window))
        );
    }

    /// The fold and the tray are the bar's own, and never reach the window manager.
    #[test]
    fn the_fold_and_the_tray_stay_on_the_bar() {
        assert_eq!(click(Target::Fold, "d"), Click::Fold);
        assert_eq!(click(Target::Tray, "d"), Click::Tray);
    }

    const NEXT: Clock = Clock { minute: 6, ..CLOCK };

    /// A menu extra's picture wakes the bar just past the boundary. The time moves on then, not a
    /// minute later when the minute's sleep, re-aimed from that wake, fires.
    #[test]
    fn a_wake_that_is_not_the_minute_s_still_moves_the_time_on() {
        let start = Instant::now();
        let mut time = Timepiece::new((CLOCK, 50.0), start);
        assert_eq!(time.show(), CLOCK);

        time.read((NEXT, 0.5), start + Duration::from_secs_f64(10.5));
        assert!(time.behind());
        assert_eq!(time.show(), NEXT);
        assert!(!time.behind());
    }

    /// A bar that comes back up after a stretch with no bars, and so no minute's sleep, is drawn
    /// with the time its wake read rather than the one from before the stretch.
    #[test]
    fn a_bar_coming_back_shows_the_time_it_woke_to() {
        let start = Instant::now();
        let mut time = Timepiece::new((CLOCK, 0.0), start);
        assert_eq!(time.show(), CLOCK);

        let later = Clock { hour: 9, ..CLOCK };
        time.read((later, 12.0), start + Duration::from_secs(7200));
        assert_eq!(time.show(), later);
    }

    /// A wake inside the same minute changes nothing on the bar, and nothing is behind before the
    /// first draw.
    #[test]
    fn only_a_new_minute_is_behind() {
        let start = Instant::now();
        let mut time = Timepiece::new((CLOCK, 10.0), start);
        time.read((NEXT, 0.0), start);
        assert!(!time.behind(), "nothing shown yet");

        time.show();
        time.read((NEXT, 30.0), start + Duration::from_secs(30));
        assert!(!time.behind());
    }

    /// The wait is measured from the reading, so a wake that took a while still lands on the
    /// boundary, and one that ran past it asks for none.
    #[test]
    fn the_next_turn_is_timed_from_the_reading() {
        let start = Instant::now();
        let time = Timepiece::new((CLOCK, 15.25), start);
        assert_eq!(time.until_turn(start), Duration::from_secs_f64(44.75));
        assert_eq!(
            time.until_turn(start + Duration::from_secs(10)),
            Duration::from_secs_f64(34.75)
        );
        assert_eq!(time.until_turn(start + Duration::from_secs(45)), Duration::ZERO);
    }
}
