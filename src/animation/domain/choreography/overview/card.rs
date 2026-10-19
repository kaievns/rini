//! A thumb of the overview drawn as the switcher's card (`chrome::domain::card`): rounded to the
//! card's corner on screen, or to a wider corner its picture keeps, its bands round that corner, and
//! the selected one ringed at the card's selection over the card's wash, as the switcher draws its
//! selected card (`specs/overview.md`, "What it shows").
//!
//! Every value is eased on screen from the window's own to the card's by how far the overview is
//! held (`Node::Held`), then divided by the zoom it is drawn under: at rest a thumb is its window,
//! so frame zero and the landing are the screen, and held it is the card at every depth.

use rini_core::ids::WindowId;
use rini_geometry::space::{Respace, Size, TileSize};
use rini_geometry::stretch::is_a_resize;

use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{Channel, Node, SAMPLE_STEP, Score, Value};
use crate::chrome::domain::card;
use crate::decorations::domain::shape::{Lay, Laying};

/// How far apart a thumb's dress is sampled along the zoom: two 120Hz frames, Core Animation's
/// straight line between two within a tenth of a point of the dress on screen; one a thumb's own
/// track resizes is sampled every frame (`SAMPLE_STEP`), so its pieces keep to the frame that track
/// plays (`src/overview/docs/README.md`, "Thumbs are the switcher's cards").
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

/// A thumb as its window has it: how the window lays it on screen, whether its picture keeps the
/// window's own corner, which no clip rounds tighter, and whether it is the selected one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub own: Laying,
    pub keeps_corner: bool,
    pub selected: bool,
}

/// How a thumb is dressed `held` of the way into the card under a zoom by `zoom`: every corner but
/// the selection's the card's, or the picture's own on screen where it keeps a wider one, the
/// border as wide as the window's, and the ring as wide as the window's on any other thumb; on the
/// selected one the ring at least the card's ring and laid on the card's selection, its outer edge
/// `SELECTION_OUTSET` out at `SELECTION_CORNER` as far past the thumb's corner as the card's is
/// past `CORNER`, over the card's wash reaching as far as the ring's outer edge.
pub fn dress(thumb: Thumb, held: f64, zoom: f64) -> Dress {
    let Thumb { own, keeps_corner, selected } = thumb;
    let share = held.clamp(0.0, 1.0);
    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    let ease = |from: f64, to: f64| (from + (to - from) * share) / zoom;
    let corner = if keeps_corner {
        card::CORNER.max(own.corner * zoom)
    } else {
        card::CORNER
    };
    let selection_corner = card::SELECTION_CORNER + (corner - card::CORNER);
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
            corner: ease(own.ring.corner, (selection_corner - width).max(0.0)),
            out: ease(0.0, (card::SELECTION_OUTSET - width).max(0.0)),
        }
    } else {
        Lay {
            width: ease(own.ring.width, own.ring.width),
            corner: ease(own.ring.corner, corner),
            out: 0.0,
        }
    };
    Dress {
        laying: Laying {
            corner: ease(own.corner, corner),
            ring,
            border: Lay {
                width: ease(own.border.width, own.border.width),
                corner: ease(own.border.corner, corner),
                out: 0.0,
            },
        },
        wash: Wash {
            share: if selected { share } else { 0.0 },
            out: ease(0.0, card::SELECTION_OUTSET),
            corner: ease(own.corner, selection_corner),
        },
    }
}

/// When `window`'s thumb dresses along the overview's zoom on `score` at media time `now`: from the
/// first begin to the last end of the tracks on the zoom, on how far it is held and on the thumb
/// itself that still play at `now`, or as the latest of them begins if that is later, if the zoom
/// or the held share is among them. None when neither plays then: the thumb is dressed as their
/// models have it.
pub fn dressing(score: &Score, window: WindowId, now: f64) -> Option<(f64, f64)> {
    let tracks: Vec<_> = [Node::Strip, Node::Held, Node::Tile(window)]
        .into_iter()
        .filter_map(|node| score.track(node, Channel::Base))
        .filter(|track| track.seconds() > 0.0)
        .collect();
    let latest = tracks.iter().map(|track| track.begin()).fold(now, f64::max);
    let playing: Vec<_> = tracks.into_iter().filter(|track| track.ends() > latest).collect();
    if !playing.iter().any(|track| track.node() != Node::Tile(window)) {
        return None;
    }
    let begin = playing.iter().map(|track| track.begin()).fold(f64::INFINITY, f64::min);
    let end = playing.iter().map(|track| track.ends()).fold(f64::NEG_INFINITY, f64::max);
    Some((begin, end))
}

/// `window`'s thumb dressed `DRESS_STEP` apart from `begin` to `end` on `score`, `SAMPLE_STEP`
/// apart while its own track resizes it, the last at `end`: its size there, and its dress at how
/// far the overview is held and the zoom it is drawn under then. None for a thumb `score` has not
/// placed.
pub fn dressed_over(
    score: &Score,
    window: WindowId,
    thumb: Thumb,
    (begin, end): (f64, f64),
) -> Vec<(TileSize, Dress)> {
    let floats = floats(score, window);
    let resizes = score.track(Node::Tile(window), Channel::Base).is_some_and(|track| {
        let size = |value: Value| match value {
            Value::Rect(frame) => frame.size,
            _ => Size::zero(),
        };
        let overlaps = track.begin() < end && track.ends() > begin;
        overlaps
            && track
                .keys()
                .windows(2)
                .any(|pair| is_a_resize(size(pair[0].1), size(pair[1].1)))
    });
    let step = if resizes { SAMPLE_STEP } else { DRESS_STEP };
    frames((begin, end), step)
        .map(|at| {
            let Some(Value::Rect(frame)) = score.sample(Node::Tile(window), at) else {
                return None;
            };
            let zoom = score.sample(Node::Strip, at).filter(|_| !floats);
            let dress = drawn(thumb, (score.sample(Node::Held, at), zoom));
            Some((frame.size.respace(), dress))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// `window`'s thumb dressed as `score` draws it once its tracks end.
pub fn dressed(score: &Score, window: WindowId, thumb: Thumb) -> Dress {
    let zoom = score.model(Node::Strip).filter(|_| !floats(score, window));
    drawn(thumb, (score.model(Node::Held), zoom))
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
            let widths = frames((begin, end), DRESS_STEP)
                .map(|at| under(score.sample(Node::Strip, at)))
                .collect();
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

/// Each moment from `begin` to `end` at most `step` apart, evenly, the last at `end`.
fn frames((begin, end): (f64, f64), step: f64) -> impl Iterator<Item = f64> {
    let frames = ((end - begin) / step).ceil().max(1.0) as usize;
    (0..=frames).map(move |frame| begin + (end - begin) * frame as f64 / frames as f64)
}

/// Whether `window` rides the floating container, which no zoom draws.
fn floats(score: &Score, window: WindowId) -> bool {
    score.parent(window) == Some(GroupKey::Floating)
}

/// A thumb's dress at a `held` share, none with no share, under a `zoom`, none with no zoom.
fn drawn(thumb: Thumb, (held, zoom): (Option<Value>, Option<Value>)) -> Dress {
    let held = match held {
        Some(Value::Number(held)) => held,
        _ => 0.0,
    };
    let zoom = match zoom {
        Some(Value::Zoom(zoom)) => zoom.scale,
        _ => 1.0,
    };
    dress(thumb, held, zoom)
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

    /// `own`'s thumb, its picture keeping its window's corner as a SkyLight capture does.
    fn thumb(own: Laying, selected: bool) -> Thumb {
        Thumb {
            own,
            keeps_corner: true,
            selected,
        }
    }

    fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

    /// At rest a thumb is its window, the selected one too, its wash showing none: frame zero and
    /// the landing are the screen.
    #[test]
    fn at_rest_a_thumb_is_its_window() {
        for selected in [false, true] {
            let at_rest = dress(thumb(own(), selected), 0.0, 1.0);
            assert_eq!(at_rest.laying, own());
            assert_eq!(at_rest.wash.share, 0.0);
        }
    }

    /// Held at any depth, every thumb rounds to the card's corner on screen, its bands round it and
    /// keep their window's width on screen, and only the selected one wears the wash.
    #[test]
    fn held_every_thumb_is_the_card_on_screen_at_any_depth() {
        for zoom in [0.22, 0.3, 0.5] {
            let held = dress(thumb(own(), false), 1.0, zoom);
            let on_screen = |value: f64| value * zoom;
            assert!(close(on_screen(held.laying.corner), card::CORNER), "{zoom}");
            assert!(close(on_screen(held.laying.ring.corner), card::CORNER));
            assert!(close(on_screen(held.laying.border.corner), card::CORNER));
            assert!(close(on_screen(held.laying.ring.width), 1.0));
            assert_eq!(held.laying.ring.out, 0.0);
            assert_eq!(held.wash.share, 0.0);
        }
    }

    /// Held, the selected thumb is ringed as the switcher rings its selected card: the ring at
    /// least the card's ring wide, its outer edge the selection's outset out at the selection's
    /// corner, over the card's wash reaching as far; a ring wider than that outset hugs the thumb.
    #[test]
    fn held_the_selected_thumb_wears_the_card_s_selection() {
        let zoom = 0.3;
        let held = dress(thumb(own(), true), 1.0, zoom);
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
        let ring = dress(thumb(wide, true), 1.0, zoom).laying.ring;
        assert!(close(on_screen(ring.width), 6.0) && ring.out == 0.0, "{ring:?}");
        let bare = Laying {
            ring: Lay { width: 0.0, ..own().ring },
            ..own()
        };
        assert_eq!(
            dress(thumb(bare, true), 1.0, zoom).laying.ring.width,
            0.0,
            "no ring, none"
        );
    }

    /// A picture that keeps a corner wider on screen than the card's, as a 26pt window's SkyLight
    /// capture does at depth 0.5, rounds to that corner, its bands and the selection with it, since
    /// no clip rounds it tighter; clipped, it is the card's. At no share does a thumb round tighter
    /// on screen than the corner its picture keeps.
    #[test]
    fn a_picture_keeping_a_wider_corner_than_the_card_rounds_to_its_own() {
        let edge = |width: f64| Lay { width, corner: 26.0, out: 0.0 };
        let wide = Laying {
            corner: 26.0,
            ring: edge(1.0),
            border: edge(0.0),
        };
        let zoom = 0.5;
        let on_screen = |value: f64| value * zoom;
        let held = dress(thumb(wide, false), 1.0, zoom).laying;
        for corner in [held.corner, held.ring.corner, held.border.corner] {
            assert!(close(on_screen(corner), 13.0), "{held:?}");
        }
        let selected = dress(thumb(wide, true), 1.0, zoom);
        let ring = selected.laying.ring;
        let past = card::SELECTION_CORNER - card::CORNER;
        assert!(close(on_screen(ring.corner + ring.width), 13.0 + past));
        assert!(close(on_screen(selected.wash.corner), 13.0 + past));
        let clipped = Thumb {
            keeps_corner: false,
            ..thumb(wide, false)
        };
        assert!(close(
            on_screen(dress(clipped, 1.0, zoom).laying.corner),
            card::CORNER
        ));
        for step in 0..=20 {
            let share = f64::from(step) / 20.0;
            let zoom = 1.0 - (1.0 - 0.5) * share;
            let laying = dress(thumb(wide, false), share, zoom).laying;
            for corner in [laying.corner, laying.ring.corner, laying.border.corner] {
                assert!(corner * zoom >= 26.0 * zoom - 1e-9, "{share}: {laying:?}");
            }
        }
    }

    /// At every share the selected thumb's wash reaches no further out than its ring's outer edge,
    /// so no halo shows outside the ring while the zoom plays.
    #[test]
    fn the_wash_never_reaches_past_the_ring() {
        let wide = Laying {
            ring: Lay { width: 6.0, ..own().ring },
            ..own()
        };
        let bare = Laying {
            ring: Lay { width: 0.0, ..own().ring },
            ..own()
        };
        for laying in [own(), wide, bare] {
            for step in 0..=20 {
                let share = f64::from(step) / 20.0;
                let zoom = 1.0 - (1.0 - 0.3) * share;
                let dress = dress(thumb(laying, true), share, zoom);
                let ring = dress.laying.ring;
                assert!(
                    dress.wash.out <= ring.out + ring.width + 1e-9,
                    "{share}: {dress:?}"
                );
            }
        }
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
            let span = dressing(score, w(2), 0.0).expect("dressed along the zoom");
            let zoom = score.track(Node::Strip, Channel::Base).expect("a zoom");
            assert_eq!(span, (zoom.begin(), zoom.ends()));
            let frames = dressed_over(score, w(2), thumb(own(), true), span);
            assert_eq!(
                frames.len(),
                (PRESS / SAMPLE_STEP).ceil() as usize + 1,
                "a frame apart, as its own track resizes it"
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
            for (_, dress) in &frames {
                let ring = dress.laying.ring;
                assert!(dress.wash.out <= ring.out + ring.width + 1e-9, "{dress:?}");
            }
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
        let held = dressed(&out, w(2), thumb(own(), true));
        assert!(close(held.laying.corner * depth, card::CORNER));
        let landed = dressed(&back, w(2), thumb(own(), true));
        assert_eq!(landed.laying, own());
    }

    /// A press while the zoom plays dresses its thumb from the zoom's begin to the later of their
    /// ends, two frames apart while its size holds; once the zoom has ended, by a press's begin or
    /// by the time given, nothing dresses along it.
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
            score
        };
        let still = pressed(100.1);
        let span = dressing(&still, w(1), 0.0);
        assert_eq!(span, Some((zoom.begin(), 100.1 + PRESS)));
        let frames = dressed_over(&still, w(1), thumb(own(), false), span.expect("dressed"));
        let apart = ((100.1 + PRESS - zoom.begin()) / DRESS_STEP).ceil() as usize + 1;
        assert_eq!(
            frames.len(),
            apart,
            "two frames apart, as its own track holds its size"
        );
        assert_eq!(dressing(&pressed(zoom.ends() + 0.5), w(1), 0.0), None);
        assert_eq!(dressing(&Score::default(), w(1), 0.0), None);
        assert_eq!(dressing(&out, w(1), 100.1), Some((zoom.begin(), zoom.ends())));
        assert_eq!(
            dressing(&out, w(1), zoom.ends()),
            None,
            "the zoom has ended by then"
        );
    }
}
