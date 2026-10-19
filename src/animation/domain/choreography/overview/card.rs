//! A thumb of the overview drawn as the switcher's card (`chrome::domain::card`): rounded to the
//! card's corner on screen, its bands round that corner, and the selected one ringed at the card's
//! selection over the card's wash, as the switcher draws its selected card (`specs/overview.md`,
//! "What it shows").
//!
//! Every value is eased on screen from the window's own to the card's by how far the overview is
//! held (`Node::Held`), then divided by the zoom it is drawn under: at rest a thumb is its window, so
//! frame zero and the landing are the screen, and held it is the card at every depth.

use rini_core::ids::WindowId;
use rini_geometry::space::{Respace, TileSize};

use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{Channel, Node, Score, Value};
use crate::chrome::domain::card;
use crate::decorations::domain::shape::{Lay, Laying};

/// How far apart a thumb's dress is sampled along the zoom: two 120Hz frames, Core Animation's
/// straight line between two within a tenth of a point of the dress on screen
/// (`src/overview/docs/README.md`, "Thumbs are the switcher's cards").
pub const DRESS_STEP: f64 = 1.0 / 60.0;

/// The card's wash under the selected thumb, in the strip's points: how much of it shows, from none
/// to all, how far out of the thumb it reaches and the corner it rounds to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wash {
    pub share: f64,
    pub out: f64,
    pub corner: f64,
}

/// A thumb as it is drawn: its pieces and bands laid, and the wash under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dress {
    pub laying: Laying,
    pub wash: Wash,
}

/// How a thumb whose window lays it `own` on screen is dressed `held` of the way into the card under
/// a zoom by `zoom`: every corner but the selection's the card's, the border as wide as the window's,
/// and the ring as wide as the window's on any other thumb; on the `selected` one the ring at least
/// the card's ring and laid on the card's selection, its outer edge `SELECTION_OUTSET` out at
/// `SELECTION_CORNER`, over the card's wash.
pub fn dress(own: Laying, selected: bool, held: f64, zoom: f64) -> Dress {
    let share = held.clamp(0.0, 1.0);
    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    let ease = |from: f64, to: f64| (from + (to - from) * share) / zoom;
    let ring = if selected {
        let ringed = own.ring.width > 0.0;
        let own_ring = own.ring.width - own.border.width;
        let width = if ringed {
            own.border.width + own_ring.max(card::RING)
        } else {
            0.0
        };
        Lay {
            width: ease(own.ring.width, width),
            corner: ease(own.ring.corner, (card::SELECTION_CORNER - width).max(0.0)),
            out: ease(0.0, (card::SELECTION_OUTSET - width).max(0.0)),
        }
    } else {
        Lay {
            width: ease(own.ring.width, own.ring.width),
            corner: ease(own.ring.corner, card::CORNER),
            out: 0.0,
        }
    };
    Dress {
        laying: Laying {
            corner: ease(own.corner, card::CORNER),
            ring,
            border: Lay {
                width: ease(own.border.width, own.border.width),
                corner: ease(own.border.corner, card::CORNER),
                out: 0.0,
            },
        },
        wash: Wash {
            share: if selected { share } else { 0.0 },
            out: card::SELECTION_OUTSET / zoom,
            corner: card::SELECTION_CORNER / zoom,
        },
    }
}

/// When `window`'s thumb dresses along the overview's zoom on `score`: from the first begin to the
/// last end of the tracks on the zoom, on how far it is held and on the thumb itself that still play
/// as the latest of them begins, if the zoom or the held share is among them. None when neither
/// plays then: the thumb is dressed as their models have it.
pub fn dressing(score: &Score, window: WindowId) -> Option<(f64, f64)> {
    let tracks: Vec<_> = [Node::Strip, Node::Held, Node::Tile(window)]
        .into_iter()
        .filter_map(|node| score.track(node, Channel::Base))
        .filter(|track| track.seconds() > 0.0)
        .collect();
    let latest = tracks.iter().map(|track| track.begin()).fold(f64::NEG_INFINITY, f64::max);
    let playing: Vec<_> = tracks.into_iter().filter(|track| track.ends() > latest).collect();
    if !playing.iter().any(|track| track.node() != Node::Tile(window)) {
        return None;
    }
    let begin = playing.iter().map(|track| track.begin()).fold(f64::INFINITY, f64::min);
    let end = playing.iter().map(|track| track.ends()).fold(f64::NEG_INFINITY, f64::max);
    Some((begin, end))
}

/// `window`'s thumb dressed `DRESS_STEP` apart from `begin` to `end` on `score`, the last at `end`: its
/// size there, and its dress at how far the overview is held and the zoom it is drawn under then.
/// None for a thumb `score` has not placed.
pub fn dressed_over(
    score: &Score,
    window: WindowId,
    own: Laying,
    selected: bool,
    (begin, end): (f64, f64),
) -> Vec<(TileSize, Dress)> {
    let floats = floats(score, window);
    frames((begin, end))
        .map(|at| {
            let Some(Value::Rect(frame)) = score.sample(Node::Tile(window), at) else {
                return None;
            };
            let zoom = score.sample(Node::Strip, at).filter(|_| !floats);
            let dress = drawn(own, selected, (score.sample(Node::Held, at), zoom));
            Some((frame.size.respace(), dress))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// `window`'s thumb dressed as `score` draws it once its tracks end.
pub fn dressed(score: &Score, window: WindowId, own: Laying, selected: bool) -> Dress {
    let zoom = score.model(Node::Strip).filter(|_| !floats(score, window));
    drawn(own, selected, (score.model(Node::Held), zoom))
}

/// How wide a band `width` wide on screen is drawn under the strip's zoom on `score`: once its
/// tracks end, and while the zoom has a track, from its begin to its end `DRESS_STEP` apart, the
/// last at its end. An empty workspace's outline keeps its width on screen so.
pub fn outlined(score: &Score, width: f64) -> (f64, Option<Zoomed>) {
    let under = |zoom: Option<Value>| match zoom {
        Some(Value::Zoom(zoom)) if zoom.scale.is_finite() && zoom.scale > 0.0 => width / zoom.scale,
        _ => width,
    };
    let zoomed = score
        .track(Node::Strip, Channel::Base)
        .filter(|track| track.seconds() > 0.0)
        .map(|track| {
            let (begin, end) = (track.begin(), track.ends());
            let widths =
                frames((begin, end)).map(|at| under(score.sample(Node::Strip, at))).collect();
            Zoomed { begin, end, widths }
        });
    (under(score.model(Node::Strip)), zoomed)
}

/// A value drawn along the zoom: from `begin` to `end`, `DRESS_STEP` apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Zoomed {
    pub begin: f64,
    pub end: f64,
    pub widths: Vec<f64>,
}

/// Each moment from `begin` to `end` `DRESS_STEP` apart, evenly, the last at `end`.
fn frames((begin, end): (f64, f64)) -> impl Iterator<Item = f64> {
    let frames = ((end - begin) / DRESS_STEP).ceil().max(1.0) as usize;
    (0..=frames).map(move |frame| begin + (end - begin) * frame as f64 / frames as f64)
}

/// Whether `window` rides the floating container, which no zoom draws.
fn floats(score: &Score, window: WindowId) -> bool {
    score.parent(window) == Some(GroupKey::Floating)
}

/// A thumb's dress at a `held` share, none with no share, under a `zoom`, none with no zoom.
fn drawn(own: Laying, selected: bool, (held, zoom): (Option<Value>, Option<Value>)) -> Dress {
    let held = match held {
        Some(Value::Number(held)) => held,
        _ => 0.0,
    };
    let zoom = match zoom {
        Some(Value::Zoom(zoom)) => zoom.scale,
        _ => 1.0,
    };
    dress(own, selected, held, zoom)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::OverlayRect;
    use rini_geometry::to_overlay_space;

    use super::*;
    use crate::animation::domain::choreography::overview::{Motion, at_rest, held, pose, toward};
    use crate::animation::domain::motion::score::{Edit, Track};
    use crate::overview::domain::fixture::{Col, display, overview, row, strip, w};
    use crate::overview::domain::scene::{Scene, lay_out};

    const PRESS: f64 = 0.35;

    /// Kai's: a 10pt corner, a 1pt ring and no border.
    fn own() -> Laying {
        let edge = |width: f64| Lay { width, corner: 10.0, out: 0.0 };
        Laying {
            corner: 10.0,
            ring: edge(1.0),
            border: edge(0.0),
        }
    }

    fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

    /// At rest a thumb is its window, the selected one too, its wash showing none: frame zero and
    /// the landing are the screen.
    #[test]
    fn at_rest_a_thumb_is_its_window() {
        for selected in [false, true] {
            let at_rest = dress(own(), selected, 0.0, 1.0);
            assert_eq!(at_rest.laying, own());
            assert_eq!(at_rest.wash.share, 0.0);
        }
    }

    /// Held at any depth, every thumb rounds to the card's corner on screen, its bands round it and
    /// keep their window's width on screen, and only the selected one wears the wash.
    #[test]
    fn held_every_thumb_is_the_card_on_screen_at_any_depth() {
        for zoom in [0.22, 0.3, 0.5] {
            let held = dress(own(), false, 1.0, zoom);
            let on_screen = |value: f64| value * zoom;
            assert!(close(on_screen(held.laying.corner), card::CORNER), "{zoom}");
            assert!(close(on_screen(held.laying.ring.corner), card::CORNER));
            assert!(close(on_screen(held.laying.border.corner), card::CORNER));
            assert!(close(on_screen(held.laying.ring.width), 1.0));
            assert_eq!(held.laying.ring.out, 0.0);
            assert_eq!(held.wash.share, 0.0);
        }
    }

    /// Held, the selected thumb is ringed as the switcher rings its selected card: the ring at least
    /// the card's ring wide, its outer edge the selection's outset out at the selection's corner,
    /// over the card's wash reaching as far; a ring wider than that outset hugs the thumb.
    #[test]
    fn held_the_selected_thumb_wears_the_card_s_selection() {
        let zoom = 0.3;
        let held = dress(own(), true, 1.0, zoom);
        let ring = held.laying.ring;
        let on_screen = |value: f64| value * zoom;
        assert!(close(on_screen(ring.width), card::RING));
        assert!(close(on_screen(ring.out + ring.width), card::SELECTION_OUTSET));
        assert!(close(
            on_screen(ring.corner + ring.width),
            card::SELECTION_CORNER
        ));
        assert!(close(on_screen(held.laying.corner), card::CORNER));
        assert_eq!(held.wash.share, 1.0);
        assert!(close(on_screen(held.wash.out), card::SELECTION_OUTSET));
        assert!(close(on_screen(held.wash.corner), card::SELECTION_CORNER));
        let wide = Laying {
            ring: Lay { width: 6.0, ..own().ring },
            ..own()
        };
        let ring = dress(wide, true, 1.0, zoom).laying.ring;
        assert!(close(on_screen(ring.width), 6.0) && ring.out == 0.0, "{ring:?}");
        let bare = Laying {
            ring: Lay { width: 0.0, ..own().ring },
            ..own()
        };
        assert_eq!(
            dress(bare, true, 1.0, zoom).laying.ring.width,
            0.0,
            "no ring, none"
        );
    }

    fn scene() -> (Scene, Vec<(WindowId, OverlayRect)>) {
        let columns = [
            Col { width: 800.0, windows: &[1] },
            Col { width: 800.0, windows: &[2] },
            Col { width: 800.0, windows: &[3] },
        ];
        let input = overview(vec![row(strip(&columns, 0.0), Some(2))], 0);
        let shown = input.rows[0]
            .columns
            .iter()
            .flat_map(|column| &column.windows)
            .map(|laid| (laid.window, to_overlay_space(laid.frame, display())))
            .collect();
        (lay_out(&input), shown)
    }

    fn played(score: &Score, edits: &[Edit]) -> Score {
        let mut score = score.clone();
        for edit in edits {
            score.apply(edit);
        }
        score
    }

    /// The zoom out from frame zero and the zoom back in: what each plays from.
    fn zooms() -> (Score, Score, f64) {
        let (scene, shown) = scene();
        let rest = pose(&at_rest(&scene, &shown));
        let slide = Motion::Slide { seconds: PRESS };
        let out = played(&rest, &toward(&rest, &held(&scene), 100.0, slide));
        let back = played(&out, &toward(&out, &at_rest(&scene, &shown), 101.0, slide));
        (out, back, scene.scale)
    }

    /// Through the zoom out the selected thumb's corner eases on screen from its window's to the
    /// card's, never past either, and its wash from none to all, each on the zoom's own clock, so
    /// nothing jumps at frame zero or at the hold; the zoom back in eases them back to the window's
    /// at the landing.
    #[test]
    fn the_corner_and_the_wash_ease_with_the_zoom_from_the_window_to_the_card_and_back() {
        let (out, back, depth) = zooms();
        for (score, from, to) in [(&out, 0.0, 1.0), (&back, 1.0, 0.0)] {
            let span = dressing(score, w(2)).expect("dressed along the zoom");
            let zoom = score.track(Node::Strip, Channel::Base).expect("a zoom");
            assert_eq!(span, (zoom.begin(), zoom.ends()));
            let frames = dressed_over(score, w(2), own(), true, span);
            assert_eq!(
                frames.len(),
                (PRESS / DRESS_STEP).ceil() as usize + 1,
                "two frames apart"
            );
            let scale = |at: f64| match score.sample(Node::Strip, at) {
                Some(Value::Zoom(zoom)) => zoom.scale,
                other => panic!("{other:?}"),
            };
            let step = (span.1 - span.0) / (frames.len() - 1) as f64;
            let corners: Vec<f64> = frames
                .iter()
                .enumerate()
                .map(|(frame, (_, dress))| {
                    dress.laying.corner * scale(span.0 + step * frame as f64)
                })
                .collect();
            let washes: Vec<f64> = frames.iter().map(|(_, dress)| dress.wash.share).collect();
            let ease = |share: f64| 10.0 + (card::CORNER - 10.0) * share;
            assert!(close(corners[0], ease(from)) && close(washes[0], from));
            let last = corners.len() - 1;
            assert!(close(corners[last], ease(to)) && close(washes[last], to));
            let rising = to > from;
            for pair in corners.windows(2).chain(washes.windows(2)) {
                assert!(if rising {
                    pair[1] >= pair[0] - 1e-9
                } else {
                    pair[1] <= pair[0] + 1e-9
                });
            }
        }
        let held = dressed(&out, w(2), own(), true);
        assert!(close(held.laying.corner * depth, card::CORNER));
        let landed = dressed(&back, w(2), own(), true);
        assert_eq!(landed.laying, own());
    }

    /// A press while the zoom plays dresses its thumb from the zoom's begin to the later of their
    /// ends; once the zoom has ended a press on a thumb dresses nothing along it.
    #[test]
    fn a_thumb_dresses_along_the_zoom_only_while_the_zoom_plays() {
        let (out, _, _) = zooms();
        let zoom = out.track(Node::Strip, Channel::Base).expect("a zoom").clone();
        let model = out.model(Node::Tile(w(1))).expect("placed");
        let pressed = |at: f64| {
            let mut score = out.clone();
            score.apply(&Edit::Add(Track::between(
                Node::Tile(w(1)),
                Channel::Base,
                model,
                model,
                at,
                PRESS,
                crate::animation::domain::motion::score::Curve::Linear,
            )));
            dressing(&score, w(1))
        };
        assert_eq!(pressed(100.1), Some((zoom.begin(), 100.1 + PRESS)));
        assert_eq!(pressed(zoom.ends() + 0.5), None);
        assert_eq!(dressing(&Score::default(), w(1)), None);
    }
}
