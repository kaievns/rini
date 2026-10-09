//! How one tile fits its picture to the frame it is drawn in, how its frame interpolates, and when
//! and how it snaps onto its window's real frame, as the score's edits. Sizes and rects only; the
//! layers that realise these live in the overlay.

use rini_core::ids::WindowId;
use rini_geometry::space::{
    ContentsRect, OverlayPoint, OverlayRect, Point, PointSpace, Rect, ScreenRect, ScreenSize, Size,
    TileRect, TileSize,
};

use super::plan::group_relative;
use super::score::{Channel, Edit, Node, Score, Value};
use crate::pictures::domain::coverage::fits_frame;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::watch::{END_SLACK, near};

/// How a tile's picture is fitted to the frame it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentMode {
    /// Scaled to fill the frame. Right only where scaling is exact: a picture matching the frame at
    /// both ends of a movement, and the stand-in's one flat pixel.
    Stretch,
    /// Cropped or revealed: the picture draws at native 1:1 scale against the edge of each axis
    /// that moves less (`Anchors`), and the size change is absorbed by a seam a band in from the
    /// moving edge, so the moving edge swallows or reveals content, which is how a real resize
    /// reads; where the picture ends, the window's ground. The band on the moving edge, carrying
    /// the window's rounded corners and border, rides it intact. See "Resizes through the overlay"
    /// in `src/animation/docs/animation-smoothness.md`.
    Crop,
}

/// Stretched only where stretching is exact: a picture fitting the frame it starts in and the one
/// it ends in, and the stand-in, whose one flat pixel claims to cover nothing; cropped otherwise,
/// a resize, a picture lagging one and a grow whose repainted picture has not landed alike, so
/// content is never stretched.
pub fn content_mode<U: PointSpace>(
    covered: ScreenSize,
    source: Route,
    from: Size<U>,
    to: Size<U>,
) -> ContentMode {
    if source == Route::StandIn || fits_frame(covered, from) && fits_frame(covered, to) {
        ContentMode::Stretch
    } else {
        ContentMode::Crop
    }
}

/// The trailing band preserved intact when a tile draws cropped, in points.
///
/// Past the roundest corner macOS draws here, 26pt, whose curve runs 37pt along each edge, so the
/// crop seam never cuts a corner square, and small against any real column so almost all of the
/// window is drawn 1:1.
const EDGE_BAND: f64 = 40.0;

/// The band that fits the frame being drawn into: shrinks with the frame, so a window growing in
/// from nothing starts with no band at all and gains it continuously — no seam pops mid-flight.
fn crop_band(frame: TileSize) -> f64 {
    EDGE_BAND.min(0.45 * frame.width.min(frame.height)).max(0.0)
}

/// The edge of one axis a cropped picture stands still against: the start (top, left) or the end
/// (bottom, right).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Start,
    End,
}

/// The edge each axis of a crop-drawn tile stands still against over one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Anchors {
    pub x: Anchor,
    pub y: Anchor,
}

impl Anchors {
    /// Per axis, the edge that moves less from `from` to `to`, the start on a tie: a top row
    /// growing down stands against its top, a bottom row growing up against its bottom, a window
    /// widening leftwards against its right. A pure move ties, and either anchor rides the tile.
    pub fn of_leg<U>(from: Rect<U>, to: Rect<U>) -> Anchors {
        let anchor = |start: f64, end: f64, start_to: f64, end_to: f64| {
            if (end_to - end).abs() + 1e-6 < (start_to - start).abs() {
                Anchor::End
            } else {
                Anchor::Start
            }
        };
        Anchors {
            x: anchor(from.min_x(), from.max_x(), to.min_x(), to.max_x()),
            y: anchor(from.min_y(), from.max_y(), to.min_y(), to.max_y()),
        }
    }
}

/// One piece of a crop-drawn tile: where it sits in the tile, and which part of the picture it
/// shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropPiece {
    pub frame: TileRect,
    pub contents: ContentsRect,
}

/// A crop-drawn tile laid out for one frame: the nine pieces of its picture, row by row from the
/// top-left, and the three spans of the tile its ground fills where the picture ends, a column
/// the tile's height and the row across either side of it, each empty where the picture covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub pieces: [CropPiece; 9],
    pub ground: [TileRect; 3],
}

/// One axis of a crop grid, in points: where a piece sits along the tile and how long it is, and
/// where its part of the picture starts and how long that is.
#[derive(Debug, Clone, Copy)]
struct Span {
    at: f64,
    len: f64,
    from: f64,
    shows: f64,
}

/// The near band, the middle and the far band of one axis, and the span of it the ground fills.
/// The bands show the picture's own edges; the middle shows the picture from the anchored band on,
/// as much as the frame has room for, and the seam it ends at sits a band in from the edge that
/// moves. Past the picture the middle shows all of it, and the ground fills from the middle to the
/// moving edge's band.
fn spans(picture: f64, frame: f64, band: f64, anchor: Anchor) -> ([Span; 3], (f64, f64)) {
    let room = (frame - 2.0 * band).max(0.0);
    let shows = room.min(picture - 2.0 * band);
    let gap = room - shows;
    let (middle, from, ground) = match anchor {
        Anchor::Start => (band, band, band + shows),
        Anchor::End => (band + gap, picture - band - shows, band),
    };
    let spans = [
        Span { at: 0.0, len: band, from: 0.0, shows: band },
        Span { at: middle, len: shows, from, shows },
        Span { at: band + room, len: band, from: picture - band, shows: band },
    ];
    (spans, (ground, gap))
}

/// The 3x3 crop grid for a picture drawn into `frame` and its ground: four corner pieces a
/// `crop_band` square, four edge pieces and the middle.
///
/// On each axis the content stands still against the anchored edge (`Anchors::of_leg`), the band
/// on the other edge rides the moving edge, and the reveal or the swallow happens at the seam one
/// band in from it. All four rounded corners are corner pieces, so they survive any frame.
///
/// Every piece is 1:1, its frame exactly as large as the picture region it shows, so nothing is
/// ever stretched. On an axis the picture does not cover, the gap between its middle and the moving
/// edge's band is the window's ground, never the picture's edge pixels drawn on. Pieces and ground
/// tile the frame without overlap, so a translucent window's ground is laid once wherever it shows.
/// A grid 1:1 and seamless at both ends of a leg stays so at every frame Core Animation
/// interpolates between them; with the picture at one end's size, as a resize flies, it is the grid
/// of each frame drawn.
pub fn crop(picture: TileSize, frame: TileSize, anchors: Anchors) -> Crop {
    let pw = picture.width.max(1.0);
    let ph = picture.height.max(1.0);
    let band = crop_band(frame).min(pw / 2.0).min(ph / 2.0);
    let (columns, (gx, gw)) = spans(pw, frame.width, band, anchors.x);
    let (rows, (gy, gh)) = spans(ph, frame.height, band, anchors.y);
    let pieces = std::array::from_fn(|index| {
        let (column, row) = (columns[index % 3], rows[index / 3]);
        CropPiece {
            frame: Rect::new(Point::new(column.at, row.at), Size::new(column.len, row.len)),
            contents: Rect::new(
                Point::new(column.from / pw, row.from / ph),
                Size::new(column.shows / pw, row.shows / ph),
            ),
        }
    });
    let span = |x: f64, y: f64, w: f64, h: f64| Rect::new(Point::new(x, y), Size::new(w, h));
    let ground = [
        span(gx, 0.0, gw, frame.height),
        span(0.0, gy, gx, gh),
        span(gx + gw, gy, frame.width - gx - gw, gh),
    ];
    Crop { pieces, ground }
}

/// A tile this close to its window agrees with it: under a quarter point nothing shows at the lift.
pub const SNAP_FROM: f64 = 0.25;

/// Where a tile ending at `model` goes when its window really landed at `real`: onto `real` when an
/// edge or a side is more than `SNAP_FROM` off and the window has landed (`END_SLACK`), nowhere
/// otherwise. A strip pans by a fractional vector and the window server rounds each window on its
/// own, so a tile of a rigid group can end a point from its window. See "Tiles snap to the real
/// frame" in `src/animation/docs/animation-smoothness.md`.
pub fn snap_to(model: ScreenRect, real: ScreenRect) -> Option<ScreenRect> {
    (near(model, real, END_SLACK) && !near(model, real, SNAP_FROM)).then_some(real)
}

/// The edits that move `window`'s tile onto `real`, its window's frame, at `at`: its model goes
/// there, and a track of its own still playing then is given the same timing and start ending
/// there, so it lands with its movement instead of popping at the lift. A tile riding its container
/// only moves inside it. None for a window the score has no tile of. Whether to snap is
/// `snap_to`'s, decided by the caller; this does not judge it again. See "Tiles snap to
/// the real frame" in `src/animation/docs/animation-smoothness.md`.
pub fn snap(score: &Score, window: WindowId, real: OverlayRect, at: f64) -> Vec<Edit> {
    let node = Node::Tile(window);
    if score.model(node).is_none() {
        return Vec::new();
    }
    let offset = match score.parent(window).and_then(|key| score.model(Node::Container(key))) {
        Some(Value::Point(position)) => position,
        _ => OverlayPoint::origin(),
    };
    let to = Value::Rect(group_relative(real, offset));
    let playing = score.track(node, Channel::Base).filter(|track| track.at(at).is_some());
    std::iter::once(Edit::Place(node, to))
        .chain(playing.map(|track| Edit::Add(track.ending_at(to))))
        .collect()
}

/// Interpolates a rect. Separated out and tested because getting this wrong produces an animation
/// that looks almost right, which is much harder to debug than one that is obviously broken.
pub fn lerp_rect<U>(from: Rect<U>, to: Rect<U>, t: f64) -> Rect<U> {
    let l = |a: f64, b: f64| a + (b - a) * t;
    Rect::new(
        Point::new(l(from.origin.x, to.origin.x), l(from.origin.y, to.origin.y)),
        Size::new(
            l(from.size.width, to.size.width),
            l(from.size.height, to.size.height),
        ),
    )
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Overlay, Respace};

    use super::*;
    use crate::pictures::domain::region::within;

    fn rect<U>(x: f64, y: f64, w: f64, h: f64) -> Rect<U> {
        Rect::new(Point::new(x, y), Size::new(w, h))
    }

    /// A tile's frame size on the overlay, as a track's ends carry it.
    fn drawn(w: f64, h: f64) -> Size<Overlay> {
        Size::new(w, h)
    }

    #[test]
    fn lerp_at_zero_is_the_start_frame() {
        let from: OverlayRect = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        let got = lerp_rect(from, to, 0.0);
        assert_eq!(got, from);
    }

    #[test]
    fn lerp_at_one_is_the_end_frame() {
        let from: OverlayRect = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        assert_eq!(lerp_rect(from, to, 1.0), to);
    }

    #[test]
    fn lerp_midpoint_is_halfway_on_every_axis() {
        let got: OverlayRect = lerp_rect(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(100.0, 200.0, 200.0, 300.0),
            0.5,
        );
        assert_eq!(got, rect(50.0, 100.0, 150.0, 200.0));
    }

    #[test]
    fn lerp_handles_a_negative_origin() {
        // Off-strip windows legitimately sit at negative x, so this is the common case rather than
        // an edge case: the measured strip had columns at x = -1680.
        let got: OverlayRect = lerp_rect(
            rect(-1680.0, 32.0, 859.0, 1081.0),
            rect(0.0, 32.0, 859.0, 1081.0),
            0.5,
        );
        assert_eq!(got.origin.x, -840.0);
        assert_eq!(got.origin.y, 32.0);
    }

    /// A picture matching the frame at both ends stretches, exactly; anything else crops, a grow
    /// past the picture and a stale picture included, so no content is stretched. The stand-in,
    /// covering nothing, stretches its flat pixel over any frame.
    #[test]
    fn only_a_picture_fitting_both_ends_and_the_stand_in_stretch() {
        let col = drawn(859.0, 1081.0);
        let picture = ScreenSize::new(859.0, 1081.0);
        let mode = |covered: ScreenSize, from, to| content_mode(covered, Route::SkyLight, from, to);
        assert_eq!(mode(picture, col, col), ContentMode::Stretch);
        let rounding = ScreenSize::new(918.0, 1081.0);
        assert_eq!(
            mode(rounding, drawn(918.0, 1081.0), drawn(917.0, 1081.0)),
            ContentMode::Stretch,
            "a point of rounding is no resize"
        );
        for (case, covered, from, to) in [
            ("a grow past the picture", picture, col, drawn(1720.0, 1081.0)),
            ("a shrink", picture, col, drawn(859.0, 540.0)),
            ("a stale narrow picture", ScreenSize::new(572.0, 1081.0), col, col),
            ("a stale wide picture", ScreenSize::new(918.0, 1081.0), col, col),
            ("the end's picture", ScreenSize::new(1720.0, 1081.0), col, drawn(1720.0, 1081.0)),
        ] {
            assert_eq!(mode(covered, from, to), ContentMode::Crop, "{case}");
        }
        let stand_in = |from, to| content_mode(ScreenSize::zero(), Route::StandIn, from, to);
        assert_eq!(stand_in(col, drawn(1720.0, 1081.0)), ContentMode::Stretch);
        assert_eq!(stand_in(drawn(40.0, 300.0), col), ContentMode::Stretch);
    }

    /// The legs of a resize in Kai's geometry: a column at x=4, 859 wide, its 2-stack's rows at y=3
    /// and 542, 535 tall, its 3-stack's at 3, 362 and 721, 355 tall, all 1074 tall maximized, and a
    /// wider column's bottom row; the anchors each axis takes.
    fn legs() -> [(&'static str, Rect<Overlay>, Rect<Overlay>, Anchors); 10] {
        let (top, end) = (Anchor::Start, Anchor::End);
        let leg = |case, from, to, x, y| (case, from, to, Anchors { x, y });
        let (column, wide) = (rect(4.0, 3.0, 859.0, 1074.0), rect(4.0, 3.0, 1721.0, 1074.0));
        let (upper, lower) = (rect(4.0, 3.0, 859.0, 535.0), rect(4.0, 542.0, 859.0, 535.0));
        let middle = rect(4.0, 362.0, 859.0, 355.0);
        let (parked, slot) = (rect(1726.0, 3.0, 859.0, 1074.0), rect(865.0, 3.0, 859.0, 1074.0));
        [
            leg("top row growing down", upper, column, top, top),
            leg("top row shrinking up", column, upper, top, top),
            leg("bottom row growing up", lower, column, top, end),
            leg("bottom row shrinking down", column, lower, top, end),
            leg("middle row, its bottom moving more", middle, lower, top, top),
            leg("middle row, its top moving more", middle, upper, top, end),
            leg("moving left as it widens", rect(866.0, 3.0, 859.0, 1074.0), wide, end, top),
            leg("widening right", column, wide, top, top),
            leg("widening as it shrinks up", column, rect(4.0, 542.0, 1148.0, 535.0), top, end),
            leg("a pure move", parked, slot, top, top),
        ]
    }

    #[test]
    fn each_axis_of_a_leg_anchors_to_the_edge_that_moves_less() {
        for (case, from, to, anchors) in legs() {
            assert_eq!(Anchors::of_leg(from, to), anchors, "{case}");
        }
    }

    fn close<U>(a: Rect<U>, b: Rect<U>, eps: f64) -> bool {
        [
            a.origin.x - b.origin.x,
            a.origin.y - b.origin.y,
            a.size.width - b.size.width,
            a.size.height - b.size.height,
        ]
        .iter()
        .all(|d| d.abs() < eps)
    }

    /// Where the picture's point `at` along one axis is drawn, from the tile's edge, by the three
    /// pieces of one row or column of a grid: `None` where no piece shows it.
    fn drawn_at(pieces: [&CropPiece; 3], picture: f64, at: f64, vertical: bool) -> Option<f64> {
        pieces.into_iter().find_map(|piece| {
            let (frame, contents) = (piece.frame, piece.contents);
            let (start, from, shows) = if vertical {
                (frame.origin.y, contents.origin.y * picture, contents.size.height * picture)
            } else {
                (frame.origin.x, contents.origin.x * picture, contents.size.width * picture)
            };
            (from - 1e-9 <= at && at < from + shows).then_some(start + at - from)
        })
    }

    /// Every piece 1:1, the nine and the ground tiling `frame` with no gap and no overlap, and the
    /// four corners band-square pieces showing the picture's own corners.
    fn assert_seamless(grid: &Crop, picture: TileSize, frame: TileSize, case: &str) {
        let eps = 1e-6;
        for (i, piece) in grid.pieces.iter().enumerate() {
            let (f, c) = (piece.frame, piece.contents);
            let across = c.size.width * picture.width - f.size.width;
            let down = c.size.height * picture.height - f.size.height;
            assert!(across.abs() < eps, "{case}: piece {i} stretches across");
            assert!(down.abs() < eps, "{case}: piece {i} stretches down");
        }
        let laid: Vec<TileRect> =
            grid.pieces.iter().map(|piece| piece.frame).chain(grid.ground).collect();
        let mut area = 0.0;
        for (i, f) in laid.iter().enumerate() {
            assert!(f.size.width >= -eps && f.size.height >= -eps, "{case}: {i} is {f:?}");
            area += f.size.width * f.size.height;
            for o in &laid[i + 1..] {
                let w = f.max_x().min(o.max_x()) - f.min_x().max(o.min_x());
                let h = f.max_y().min(o.max_y()) - f.min_y().max(o.min_y());
                assert!(w <= eps || h <= eps, "{case}: {f:?} overlaps {o:?}");
            }
        }
        assert!((area - frame.width * frame.height).abs() < 1e-3, "{case}: a gap at {frame:?}");
        let band = crop_band(frame).min(picture.width / 2.0).min(picture.height / 2.0);
        let at = |far: bool, side: f64| if far { (side - band) / side } else { 0.0 };
        let corners = [(0, false, false), (2, true, false), (6, false, true), (8, true, true)];
        for (i, right, bottom) in corners {
            let corner = grid.pieces[i];
            assert_eq!(corner.frame.size, TileSize::new(band, band), "{case}: corner {i}");
            let x = corner.contents.origin.x - at(right, picture.width);
            let y = corner.contents.origin.y - at(bottom, picture.height);
            let shows = corner.contents;
            assert!(x.abs() < eps && y.abs() < eps, "{case}: corner {i} shows {shows:?}");
        }
    }

    /// The area of `frame` the ground of `grid` fills.
    fn grounded(grid: &Crop) -> f64 {
        grid.ground.iter().map(|span| span.size.width * span.size.height).sum()
    }

    /// `picture` drawn over the leg `from` to `to` on `anchors`, at each quarter as Core Animation
    /// interpolates the endpoint grids: the grid is the one of the frame drawn, seamless and 1:1,
    /// the content stands still against the anchored edge of each axis, the band on the other edge
    /// rides the moving edge, and the ground fills exactly what the picture does not cover.
    fn assert_flies(picture: TileSize, from: Rect<Overlay>, to: Rect<Overlay>, anchors: Anchors) {
        let case = format!("{picture:?} over {from:?} to {to:?}");
        let grid = |frame: Rect<Overlay>| crop(picture, frame.size.respace(), anchors);
        let (first, last) = (grid(from), grid(to));
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let frame = lerp_rect(from, to, t);
            let drawn = grid(frame);
            for (i, piece) in drawn.pieces.iter().enumerate() {
                let frame = lerp_rect(first.pieces[i].frame, last.pieces[i].frame, t);
                let contents = lerp_rect(first.pieces[i].contents, last.pieces[i].contents, t);
                assert!(
                    close(piece.frame, frame, 1e-6) && close(piece.contents, contents, 1e-9),
                    "{case} at {t}: piece {i} is not interpolated"
                );
            }
            for (i, span) in drawn.ground.iter().enumerate() {
                let between = lerp_rect(first.ground[i], last.ground[i], t);
                assert!(close(*span, between, 1e-6), "{case} at {t}: ground {i} not interpolated");
            }
            assert_seamless(&drawn, picture, frame.size.respace(), &case);
            let (w, h) = (frame.size.width, frame.size.height);
            let bare = w * h - w.min(picture.width) * h.min(picture.height);
            assert!((grounded(&drawn) - bare).abs() < 1e-3, "{case} at {t}: ground off the bare");
            for (vertical, anchor, side, start, len) in [
                (false, anchors.x, picture.width, frame.origin.x, frame.size.width),
                (true, anchors.y, picture.height, frame.origin.y, frame.size.height),
            ] {
                let pieces = &drawn.pieces;
                let line = if vertical {
                    [&pieces[1], &pieces[4], &pieces[7]]
                } else {
                    [&pieces[3], &pieces[4], &pieces[5]]
                };
                let on_screen = |at: f64| {
                    start + drawn_at(line, side, at, vertical).unwrap_or_else(|| {
                        panic!("{case} at {t}: {at} not drawn")
                    })
                };
                let from_start = |depth: f64| on_screen(depth) - start;
                let from_end = |depth: f64| start + len - on_screen(side - depth);
                type Depth<'a> = &'a dyn Fn(f64) -> f64;
                let (anchored, moving): (Depth, Depth) = match anchor {
                    Anchor::Start => (&from_start, &from_end),
                    Anchor::End => (&from_end, &from_start),
                };
                for depth in [5.0, 50.0, 200.0] {
                    let off = anchored(depth) - depth;
                    assert!(off.abs() < 1e-6, "{case} at {t}: content {off} off its anchor");
                }
                let off = moving(5.0) - 5.0;
                assert!(off.abs() < 1e-6, "{case} at {t}: the band {off} off the moving edge");
            }
        }
    }

    /// A resize drawn with a picture covering both its ends: no ground anywhere.
    #[test]
    fn a_resize_stands_still_against_the_edge_that_moves_less_and_clips_at_the_moving_one() {
        for (_, from, to, anchors) in legs() {
            let picture = TileSize::new(
                from.size.width.max(to.size.width),
                from.size.height.max(to.size.height),
            );
            assert_flies(picture, from, to, anchors);
            assert_eq!(grounded(&crop(picture, from.size.respace(), anchors)), 0.0);
        }
    }

    /// A resize flown with the picture of either end, as one is: where the picture covers the frame
    /// the ground is empty, where it ends the ground fills the rest, and nothing stretches. A grow
    /// flying its before picture grounds its growth; a shrink flying it, none.
    #[test]
    fn a_resize_flown_with_either_ends_picture_grounds_only_where_the_picture_ends() {
        for (case, from, to, anchors) in legs() {
            for picture in [from.size.respace(), to.size.respace()] {
                assert_flies(picture, from, to, anchors);
            }
            let before = crop(from.size.respace(), to.size.respace(), anchors);
            let grows = to.size.width > from.size.width || to.size.height > from.size.height;
            assert_eq!(grounded(&before) > 0.0, grows, "{case}");
        }
    }

    /// A frame narrower than two bands still tiles 1:1 whichever way it is anchored.
    #[test]
    fn a_frame_narrower_than_two_bands_still_tiles_one_to_one() {
        let picture = TileSize::new(859.0, 1081.0);
        let frame = TileSize::new(20.0, 1081.0);
        for x in [Anchor::Start, Anchor::End] {
            for y in [Anchor::Start, Anchor::End] {
                let grid = crop(picture, frame, Anchors { x, y });
                assert_seamless(&grid, picture, frame, "20pt wide");
            }
        }
    }

    /// A picture narrower than two bands on an axis, a small spawn picture or a dialog, is all corner
    /// bands there, each half its side, with no middle, so no piece runs negative or overlaps.
    #[test]
    fn a_picture_narrower_than_two_bands_is_all_corners() {
        let frame = TileSize::new(859.0, 1081.0);
        for picture in [TileSize::new(60.0, 1081.0), TileSize::new(859.0, 60.0)] {
            for x in [Anchor::Start, Anchor::End] {
                for y in [Anchor::Start, Anchor::End] {
                    let grid = crop(picture, frame, Anchors { x, y });
                    assert_seamless(&grid, picture, frame, &format!("{picture:?} {x:?} {y:?}"));
                    assert_eq!(grid.pieces[0].frame.size, TileSize::new(30.0, 30.0), "half its side");
                }
            }
        }
    }

    /// Growing past the picture, the middle shows all of the picture's own content 1:1 — never
    /// stretched, never reaching past its edge, where a translucent window's near-transparent
    /// pixels painted the grow as a hole — the ground fills between it and the moving edge's band,
    /// and both bands stay 1:1 so the corners and hairline never distort.
    #[test]
    fn a_grow_past_the_picture_shows_all_of_it_and_grounds_the_rest() {
        let picture = TileSize::new(859.0, 1081.0);
        let frame = TileSize::new(1720.0, 1081.0);
        for (x, middle_at, ground_at) in [(Anchor::Start, 40.0, 819.0), (Anchor::End, 901.0, 40.0)]
        {
            let grid = crop(picture, frame, Anchors { x, y: Anchor::Start });
            let middle = &grid.pieces[4];
            assert!((middle.contents.origin.x * picture.width - 40.0).abs() < 1e-9);
            assert!((middle.contents.size.width * picture.width - (859.0 - 80.0)).abs() < 1e-9);
            assert_eq!(middle.frame.origin.x, middle_at, "{x:?}");
            assert_eq!(middle.frame.size.width, 859.0 - 80.0, "1:1, not stretched");
            let (left, right) = (&grid.pieces[3], &grid.pieces[5]);
            assert!((left.contents.size.width * picture.width - 40.0).abs() < 1e-9);
            assert_eq!(left.frame.size.width, 40.0);
            assert!((right.contents.origin.x * picture.width - (859.0 - 40.0)).abs() < 1e-9);
            assert!((right.contents.size.width * picture.width - 40.0).abs() < 1e-9);
            assert_eq!((right.frame.origin.x, right.frame.size.width), (1680.0, 40.0));
            let column = TileRect::new(Point::new(ground_at, 0.0), Size::new(861.0, 1081.0));
            assert_eq!(grid.ground[0], column, "{x:?}: the ground, the frame's height");
            assert_eq!(grounded(&grid), 861.0 * 1081.0, "{x:?}: no row grounded");
            assert_seamless(&grid, picture, frame, "a grow past the picture");
        }
    }

    /// A picture short of the frame on both axes, a missed hold maximizing from a stack: the ground
    /// column and row meet without overlap, so a translucent ground is laid once everywhere.
    #[test]
    fn a_picture_short_on_both_axes_is_grounded_once() {
        let picture = TileSize::new(859.0, 535.0);
        let frame = TileSize::new(1721.0, 1074.0);
        for x in [Anchor::Start, Anchor::End] {
            for y in [Anchor::Start, Anchor::End] {
                let grid = crop(picture, frame, Anchors { x, y });
                assert_seamless(&grid, picture, frame, "maximized from a stack");
                let bare = 1721.0 * 1074.0 - 859.0 * 535.0;
                assert!((grounded(&grid) - bare).abs() < 1e-6, "{x:?} {y:?}");
            }
        }
    }

    /// The band shrinks with the frame and vanishes at zero, so an entrance growing from nothing
    /// starts as a plain reveal and gains its band continuously — no seam pops in mid-flight.
    #[test]
    fn crop_band_fits_any_frame() {
        assert_eq!(crop_band(TileSize::new(859.0, 1081.0)), 40.0);
        assert!((crop_band(TileSize::new(60.0, 1081.0)) - 27.0).abs() < 1e-9);
        assert_eq!(crop_band(TileSize::new(0.0, 1081.0)), 0.0);
    }

    /// Every crop piece of a region picture stays inside the region, scaled to it, growing or
    /// shrinking: a piece reaching past the region would show the composite's next window.
    #[test]
    fn the_crop_grid_of_a_region_stays_in_the_region() {
        let region: ContentsRect = rect(0.125, 0.5, 0.25, 0.5);
        let picture = TileSize::new(856.0, 1081.0);
        for frame in [
            TileSize::new(856.0, 1081.0),
            TileSize::new(428.0, 1081.0),
            TileSize::new(856.0, 540.0),
            TileSize::new(1200.0, 1300.0),
        ] {
            let anchors = [Anchor::Start, Anchor::End]
                .into_iter()
                .flat_map(|x| [Anchor::Start, Anchor::End].map(|y| Anchors { x, y }));
            for piece in anchors.flat_map(|anchors| crop(picture, frame, anchors).pieces) {
                let whole = piece.contents;
                let cut = within(region, whole);
                let eps = 1e-12;
                assert!(cut.origin.x >= region.origin.x - eps, "{frame:?}: {cut:?}");
                assert!(cut.origin.y >= region.origin.y - eps, "{frame:?}: {cut:?}");
                assert!(cut.origin.x + cut.size.width <= region.origin.x + region.size.width + eps);
                assert!(
                    cut.origin.y + cut.size.height <= region.origin.y + region.size.height + eps
                );
                assert!((cut.size.width - whole.size.width * region.size.width).abs() < eps);
            }
        }
    }

    /// The measured case: a rigid pan ended a tile 861 wide against its window's 860.
    #[test]
    fn a_tile_a_point_off_its_window_snaps_onto_it() {
        let model: ScreenRect = rect(4.0, 32.0, 861.0, 1081.0);
        let real = rect(4.0, 32.0, 860.0, 1081.0);
        assert_eq!(snap_to(model, real), Some(real));
        let moved = rect(3.0, 32.0, 861.0, 1081.0);
        assert_eq!(snap_to(model, moved), Some(moved));
    }

    #[test]
    fn a_tile_snaps_only_past_a_quarter_point_and_up_to_twenty_points_off() {
        let model: ScreenRect = rect(4.0, 32.0, 861.0, 1081.0);
        let off = |d: f64| rect(4.0 + d, 32.0, 861.0, 1081.0);
        for d in [0.26, -0.26, 1.0, 19.5, 20.0, -20.0] {
            assert_eq!(snap_to(model, off(d)), Some(off(d)), "{d}");
        }
        for d in [0.0, 0.2, -0.24, 0.25, 20.5, -21.0, 400.0] {
            assert_eq!(snap_to(model, off(d)), None, "{d}");
        }
        assert_eq!(
            snap_to(model, rect(4.5, 32.0, 861.0, 1111.0)),
            None,
            "half a point across but 30 taller: judged by the furthest edge"
        );
    }

    mod snaps {
        use super::*;
        use crate::animation::domain::motion::easing::MOTION_CURVE;
        use crate::animation::domain::motion::plan::GroupKey;
        use crate::animation::domain::motion::score::{Curve, Track};
        use rini_geometry::space::GroupRect;

        const STRIP: GroupKey = GroupKey::STILL;

        fn window() -> WindowId { WindowId::new(1, 1) }

        /// The strip's container, model at -861, and a tile at `frame` in it: at 865 it ends at 4 on
        /// screen. `leg` is the tile's own track, when it has one.
        fn riding(frame: GroupRect, leg: Option<&Track>) -> Score {
            let mut score = Score::default();
            for edit in [
                Edit::Place(Node::Container(STRIP), Value::Point(Point::new(-861.0, 0.0))),
                Edit::Ride(window(), STRIP),
                Edit::Place(Node::Tile(window()), Value::Rect(frame)),
            ]
            .into_iter()
            .chain(leg.cloned().map(Edit::Add))
            {
                score.apply(&edit);
            }
            score
        }

        fn leg(from: GroupRect, to: GroupRect) -> Track {
            Track::between(
                Node::Tile(window()),
                Channel::Base,
                Value::Rect(from),
                Value::Rect(to),
                100.0,
                0.35,
                Curve::Bezier(MOTION_CURVE),
            )
        }

        /// The measured case, a tile 861 wide over a window 860 wide, riding its container: it is
        /// moved inside the container at once, and nothing plays.
        #[test]
        fn a_tile_riding_its_container_snaps_onto_its_window_at_once() {
            let score = riding(rect(865.0, 32.0, 861.0, 1081.0), None);
            let real: OverlayRect = rect(4.0, 32.0, 860.0, 1081.0);
            assert_eq!(snap(&score, window(), real, 100.0), [Edit::Place(
                Node::Tile(window()),
                Value::Rect(rect(865.0, 32.0, 860.0, 1081.0))
            )]);
        }

        /// A tile on a track of its own goes on along it to the window's frame: the same begin, start
        /// and curve, a new end, a resize's size with it.
        #[test]
        fn a_tile_on_its_own_leg_snaps_on_the_same_timing() {
            let to: GroupRect = rect(865.0, 32.0, 861.0, 1081.0);
            for from in [
                rect(1726.0, 32.0, 861.0, 1081.0),
                rect(1726.0, 32.0, 1200.0, 1081.0),
            ] {
                let leg = leg(from, to);
                let score = riding(to, Some(&leg));
                let real: OverlayRect = rect(4.0, 32.0, 860.0, 1081.0);
                let snapped = Value::Rect(rect(865.0, 32.0, 860.0, 1081.0));
                let edits = snap(&score, window(), real, 100.2);
                assert_eq!(edits, [
                    Edit::Place(Node::Tile(window()), snapped),
                    Edit::Add(leg.ending_at(snapped)),
                ]);
                let Edit::Add(track) = &edits[1] else { unreachable!() };
                assert_eq!(track.keys()[0], (0.0, Value::Rect(from)));
                assert_eq!((track.begin(), track.seconds()), (100.0, 0.35));
            }
        }

        /// Once its track has played, only the tile's model moves.
        #[test]
        fn a_tile_whose_leg_has_ended_snaps_by_its_model() {
            let to: GroupRect = rect(865.0, 32.0, 861.0, 1081.0);
            let leg = leg(rect(1726.0, 32.0, 861.0, 1081.0), to);
            let score = riding(to, Some(&leg));
            assert_eq!(snap(&score, window(), rect(5.0, 32.0, 861.0, 1081.0), 100.5), [
                Edit::Place(
                    Node::Tile(window()),
                    Value::Rect(rect(866.0, 32.0, 861.0, 1081.0))
                )
            ]);
            assert!(
                snap(&Score::default(), window(), rect(865.0, 32.0, 861.0, 1081.0), 100.5)
                    .is_empty(),
                "no tile, no snap"
            );
        }
    }
}
