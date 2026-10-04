//! How one tile fits its picture to the frame it is drawn in, how its frame interpolates, and when
//! and how it snaps onto its window's real frame, as the score's edits. Sizes and rects only; the
//! layers that realise these live in the overlay.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::WindowId;

use rini_geometry::space::{FromCg, Overlay, ScreenRect, ScreenSize, Size};
use rini_geometry::stretch::is_a_resize;

use super::plan::group_relative;
use super::score::{Channel, Edit, Node, Score, Value};
use crate::pictures::domain::coverage::{fits_frame, outgrows};
use crate::pictures::domain::watch::{END_SLACK, near};

/// How a tile's picture is fitted to the frame it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentMode {
    /// Scaled to fill the frame. Right for a movement whose picture matches the frame's shape.
    Stretch,
    /// Cropped or revealed: the picture draws at native 1:1 scale, and the size change is absorbed
    /// by a seam a band in from the trailing edges — content never stretches, the moving edge
    /// swallows or reveals it, which is how a real resize reads. The trailing band, carrying the
    /// window's rounded corners and border, rides the moving edge intact. See "Resizes through the
    /// overlay" in `src/animation/docs/animation-smoothness.md`.
    Crop,
}

/// Crop for a resize, and for any picture that no longer matches the frame it starts in; stretched
/// only when picture and frames agree, where stretching is exact.
pub fn content_mode(covered: ScreenSize, from: CGSize, to: CGSize) -> ContentMode {
    if is_a_resize(from, to) || !fits_frame(covered, Size::<Overlay>::from_cg(from)) {
        stand_in_mode(covered, to)
    } else {
        ContentMode::Stretch
    }
}

/// How a tile fits a picture that may not cover its destination: the ordinary crop when it can,
/// stretched when it cannot. The stretch is the stand-in of a grow whose reveal has not landed;
/// every mode fills the whole frame, so a stand-in never shows the backdrop. A top-left crop
/// that left the growth undrawn was tried and read as a hole (see "A grow holds, then reveals"
/// in `src/transitions/docs/coordination.md`).
pub fn stand_in_mode(covered: ScreenSize, to: CGSize) -> ContentMode {
    if outgrows(covered, Size::<Overlay>::from_cg(to)) {
        ContentMode::Stretch
    } else {
        ContentMode::Crop
    }
}

/// The trailing band preserved intact when a tile draws cropped, in points.
///
/// Comfortably past the ~10pt corner radius so the crop seam never cuts a corner square, and small
/// against any real column so almost all of the window is drawn 1:1.
const EDGE_BAND: f64 = 40.0;

/// The band that fits the frame being drawn into: shrinks with the frame, so a window growing in
/// from nothing starts with no band at all and gains it continuously — no seam pops mid-flight.
fn crop_band(frame: CGSize) -> f64 {
    EDGE_BAND.min(0.45 * frame.width.min(frame.height)).max(0.0)
}

/// One piece of a crop-drawn tile: where it sits in the tile, and which part of the picture it
/// shows, in the picture's unit coordinates.
///
/// Every mapping is 1:1 — a piece's frame is exactly as large as the picture region it shows — so
/// nothing ever stretches. A region reaching past the picture's edge is deliberate: Core Animation
/// extends the edge pixels outward, which paints a grow with window-coloured pixels instead of a
/// hole.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropPiece {
    pub frame: CGRect,
    pub contents: CGRect,
}

/// The 2x2 crop grid for a picture drawn into `frame`, split one `crop_band` in from the RIGHT
/// edge and one down from the TOP.
///
/// The anchoring follows how each axis of a real resize reads:
/// - Horizontally, content anchors LEFT and the right edge swallows or reveals it — the right
///   band, carrying the window's right border and corners, rides the moving edge intact.
/// - Vertically, the TITLE BAR stays put and content anchors to the BOTTOM — the prompt of a
///   terminal rides the bottom edge — so the seam sits just below the title-bar band, and the
///   bottom-anchored body slides into or out of it.
///
/// The window's top corners live in the top band at 1:1, its bottom corners in the bottom-anchored
/// body, so all four rounded corners survive any frame the animation passes through.
///
/// Piece frames and contents are LINEAR in `frame` while the band is constant, which is what lets
/// a resize ride plain Core Animation interpolation between the endpoint grids. Every piece maps
/// 1:1 while the frame fits inside the picture. Growing PAST the picture, the seam region
/// stretches the picture's own content instead of reaching beyond its edge: `contentsRect` past
/// the edge extends the outermost pixels, near-transparent on a translucent window, so a grow
/// painted a hole to the backdrop. A picture that cannot cover its destination is not drawn
/// cropped at all (`stand_in_mode`), so this only covers the frames a fitting picture passes
/// through.
pub fn crop_pieces(picture: CGSize, frame: CGSize) -> [CropPiece; 4] {
    let pw = picture.width.max(1.0);
    let ph = picture.height.max(1.0);
    let band = crop_band(frame).min(pw).min(ph);
    let (bodyw, bodyh) = ((frame.width - band).max(0.0), (frame.height - band).max(0.0));
    // What the anchored regions may show of the picture: everything up to the seam, which
    // belongs to the bands. Capping is what keeps the seam continuous — the body must never
    // duplicate a band's content.
    let leadw = bodyw.min(pw - band);
    let tailh = bodyh.min(ph - band);
    let piece = |x: f64, y: f64, w: f64, h: f64, cx: f64, cy: f64, cw: f64, ch: f64| CropPiece {
        frame: CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)),
        contents: CGRect::new(CGPoint::new(cx / pw, cy / ph), CGSize::new(cw / pw, ch / ph)),
    };
    [
        // Title-bar band: the picture's top edge, pinned to the frame's top.
        piece(0.0, 0.0, bodyw, band, 0.0, 0.0, leadw, band),
        // Top-right corner: pinned top and right.
        piece(bodyw, 0.0, band, band, pw - band, 0.0, band, band),
        // Body: left-anchored horizontally, BOTTOM-anchored vertically — the picture's bottom
        // rows ride the moving bottom edge, and the seam below the title bar absorbs the change.
        piece(0.0, band, bodyw, bodyh, 0.0, ph - tailh, leadw, tailh),
        // Right band: the picture's right edge, riding the moving right edge, bottom-anchored to
        // stay row-continuous with the body.
        piece(bodyw, band, band, bodyh, pw - band, ph - tailh, band, tailh),
    ]
}

/// A tile this close to its window agrees with it: under a quarter point nothing shows at the lift.
pub const SNAP_FROM: f64 = 0.25;

/// Where a tile ending at `model` goes when its window really landed at `real`: onto `real` when an
/// edge or a side is more than `SNAP_FROM` off and the window has landed (`END_SLACK`), nowhere
/// otherwise. A strip pans by a fractional vector and the window server rounds each window on its
/// own, so a tile of a rigid group can end a point from its window. See "Tiles snap to the real
/// frame" in `src/animation/docs/animation-smoothness.md`.
pub fn snap_to(model: CGRect, real: CGRect) -> Option<CGRect> {
    let (at, landed) = (ScreenRect::from_cg(model), ScreenRect::from_cg(real));
    (near(at, landed, END_SLACK) && !near(at, landed, SNAP_FROM)).then_some(real)
}

/// The edits that move `window`'s tile onto `real`, its window's frame in the overlay's space, at
/// `at`: its model goes there, and a track of its own still playing then is given the same timing
/// and start ending there, so it lands with its movement instead of popping at the lift. A tile
/// riding its container only moves inside it. None for a window the score has no tile of. Whether
/// to snap is `snap_to`'s, decided by the caller; this does not judge it again. See "Tiles snap to
/// the real frame" in `src/animation/docs/animation-smoothness.md`.
pub fn snap(score: &Score, window: WindowId, real: CGRect, at: f64) -> Vec<Edit> {
    let node = Node::Tile(window);
    if score.model(node).is_none() {
        return Vec::new();
    }
    let offset = match score.parent(window).and_then(|key| score.model(Node::Container(key))) {
        Some(Value::Point(position)) => position,
        _ => CGPoint::new(0.0, 0.0),
    };
    let to = Value::Rect(group_relative(real, offset));
    let playing = score.track(node, Channel::Base).filter(|track| track.at(at).is_some());
    std::iter::once(Edit::Place(node, to))
        .chain(playing.map(|track| Edit::Add(track.ending_at(to))))
        .collect()
}

/// Interpolates a rect. Separated out and tested because getting this wrong produces an animation
/// that looks almost right, which is much harder to debug than one that is obviously broken.
pub fn lerp_rect(from: CGRect, to: CGRect, t: f64) -> CGRect {
    let l = |a: f64, b: f64| a + (b - a) * t;
    CGRect::new(
        CGPoint::new(l(from.origin.x, to.origin.x), l(from.origin.y, to.origin.y)),
        CGSize::new(
            l(from.size.width, to.size.width),
            l(from.size.height, to.size.height),
        ),
    )
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ContentsRect;

    use super::*;
    use crate::pictures::domain::region::within;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn lerp_at_zero_is_the_start_frame() {
        let from = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        let got = lerp_rect(from, to, 0.0);
        assert_eq!(got, from);
    }

    #[test]
    fn lerp_at_one_is_the_end_frame() {
        let from = rect(10.0, 20.0, 100.0, 200.0);
        let to = rect(50.0, 60.0, 300.0, 400.0);
        assert_eq!(lerp_rect(from, to, 1.0), to);
    }

    #[test]
    fn lerp_midpoint_is_halfway_on_every_axis() {
        let got = lerp_rect(
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
        let got = lerp_rect(
            rect(-1680.0, 32.0, 859.0, 1081.0),
            rect(0.0, 32.0, 859.0, 1081.0),
            0.5,
        );
        assert_eq!(got.origin.x, -840.0);
        assert_eq!(got.origin.y, 32.0);
    }

    #[test]
    fn a_matching_movement_stretches_and_everything_else_crops() {
        let col = CGSize::new(859.0, 1081.0);
        let picture = ScreenSize::new(859.0, 1081.0);
        assert_eq!(content_mode(picture, col, col), ContentMode::Stretch);
        assert_eq!(
            content_mode(
                ScreenSize::new(918.0, 1081.0),
                CGSize::new(918.0, 1081.0),
                CGSize::new(917.0, 1081.0)
            ),
            ContentMode::Stretch
        );
        // A horizontal grow past the picture is the stand-in: it stretches until the reveal
        // lands. A vertical shrink crops.
        assert_eq!(
            content_mode(picture, col, CGSize::new(1720.0, 1081.0)),
            ContentMode::Stretch
        );
        assert_eq!(
            content_mode(picture, col, CGSize::new(859.0, 540.0)),
            ContentMode::Crop
        );
        // A stale narrow picture in a wider frame is the stand-in case too: the reveal fills it.
        assert_eq!(content_mode(ScreenSize::new(572.0, 1081.0), col, col), ContentMode::Stretch);
        assert_eq!(
            content_mode(ScreenSize::new(918.0, 1081.0), col, col),
            ContentMode::Crop,
            "a wider one crops"
        );
    }

    /// A picture that cannot cover its destination is stretched over it; one that covers it takes
    /// the ordinary crop. Either way the whole frame is drawn.
    #[test]
    fn a_stand_in_never_shows_backdrop() {
        let picture = ScreenSize::new(859.0, 1081.0);
        assert_eq!(
            stand_in_mode(picture, CGSize::new(1147.0, 1081.0)),
            ContentMode::Stretch
        );
        assert_eq!(
            stand_in_mode(picture, CGSize::new(859.0, 1300.0)),
            ContentMode::Stretch
        );
        assert_eq!(
            stand_in_mode(picture, CGSize::new(859.0, 1081.0)),
            ContentMode::Crop
        );
        assert_eq!(
            stand_in_mode(picture, CGSize::new(572.0, 1081.0)),
            ContentMode::Crop
        );
        // Both modes fill the frame: the match is exhaustive, so a mode that left part of the
        // frame undrawn would have to be added here to compile.
        for w in (1..=40).map(|i| i as f64 * 50.0) {
            for h in (1..=30).map(|i| i as f64 * 50.0) {
                let to = CGSize::new(w, h);
                match stand_in_mode(picture, to) {
                    ContentMode::Stretch => {
                        assert!(outgrows(picture, Size::<Overlay>::from_cg(to)), "{to:?}")
                    }
                    ContentMode::Crop => {
                        assert!(!outgrows(picture, Size::<Overlay>::from_cg(to)), "{to:?}")
                    }
                }
            }
        }
    }

    /// Every crop piece maps 1:1 while the frame fits inside the picture — its frame exactly as
    /// large as the picture region it shows — and the four pieces always tile the frame with no
    /// gap and no overlap. Any violation is a stretch or a seam, which is exactly what this mode
    /// exists to rule out.
    #[test]
    fn crop_pieces_map_one_to_one_and_tile_the_frame() {
        let picture = CGSize::new(859.0, 1081.0);
        let frames = [
            CGSize::new(572.0, 1081.0), // shrink
            CGSize::new(859.0, 540.0),  // vertical
            CGSize::new(20.0, 1081.0),  // narrower than the band
        ];
        for frame in frames {
            let mut area = 0.0;
            for piece in crop_pieces(picture, frame) {
                assert!(
                    (piece.contents.size.width * picture.width - piece.frame.size.width).abs()
                        < 1e-9,
                    "a piece stretches horizontally at {frame:?}"
                );
                assert!(
                    (piece.contents.size.height * picture.height - piece.frame.size.height).abs()
                        < 1e-9,
                    "a piece stretches vertically at {frame:?}"
                );
                area += piece.frame.size.width * piece.frame.size.height;
            }
            assert!(
                (area - frame.width * frame.height).abs() < 1e-6,
                "gap or overlap at {frame:?}"
            );
        }
    }

    /// Growing past the picture, the leading region stretches the picture's own content — never
    /// reaches past its edge, where a translucent window's near-transparent pixels painted the
    /// grow as a hole — and the trailing band stays 1:1 so the corners and hairline never distort.
    #[test]
    fn a_grow_past_the_picture_stretches_the_lead_and_keeps_the_band_intact() {
        let picture = CGSize::new(859.0, 1081.0);
        let frame = CGSize::new(1720.0, 1081.0);
        let pieces = crop_pieces(picture, frame);
        let body = &pieces[0];
        // The body shows everything up to the trailing band and no further.
        assert!((body.contents.origin.x).abs() < 1e-9);
        assert!((body.contents.size.width * picture.width - (859.0 - 40.0)).abs() < 1e-9);
        assert_eq!(
            body.frame.size.width,
            1720.0 - 40.0,
            "stretched across the grown lead"
        );
        // The trailing band is still the picture's own trailing 40pt at 1:1.
        let band = &pieces[1];
        assert!((band.contents.size.width * picture.width - 40.0).abs() < 1e-9);
        assert_eq!(band.frame.size.width, 40.0);
        // And the pieces still tile the frame exactly.
        let area: f64 = pieces.iter().map(|p| p.frame.size.width * p.frame.size.height).sum();
        assert!((area - frame.width * frame.height).abs() < 1e-6);
    }

    /// The right band shows the picture's own right edge pinned to the frame's moving right edge,
    /// so the window's right border and corners ride it intact.
    #[test]
    fn crop_right_band_comes_from_the_pictures_right_edge() {
        let picture = CGSize::new(859.0, 1081.0);
        let pieces = crop_pieces(picture, CGSize::new(572.0, 1081.0));
        let band = &pieces[3];
        assert!((band.contents.origin.x * picture.width - (859.0 - 40.0)).abs() < 1e-9);
        assert_eq!(band.frame.origin.x, 572.0 - 40.0);
        assert_eq!(band.frame.size.width, 40.0);
    }

    /// A vertical resize anchors like the real thing: the title bar stays put at the top, the
    /// bottom content rides the moving bottom edge, and the seam sits just below the title-bar
    /// band. Cutting at the bottom instead read as the window sliding into a slot.
    #[test]
    fn a_vertical_resize_pins_the_title_bar_and_anchors_content_to_the_bottom() {
        let picture = CGSize::new(859.0, 1081.0);
        let frame = CGSize::new(859.0, 540.0);
        let pieces = crop_pieces(picture, frame);
        let title = &pieces[0];
        assert_eq!(title.frame.origin.y, 0.0, "title band pinned to the top");
        assert!(
            (title.contents.origin.y).abs() < 1e-9,
            "showing the picture's top"
        );
        assert_eq!(title.frame.size.height, 40.0);
        let body = &pieces[2];
        assert_eq!(
            body.frame.origin.y, 40.0,
            "body starts at the seam below the title"
        );
        // Bottom-anchored: the contents reach the picture's bottom edge exactly.
        let content_bottom = (body.contents.origin.y + body.contents.size.height) * picture.height;
        assert!(
            (content_bottom - 1081.0).abs() < 1e-9,
            "contents reach the picture's bottom"
        );
        assert_eq!(body.frame.size.height, 540.0 - 40.0);
    }

    /// The band shrinks with the frame and vanishes at zero, so an entrance growing from nothing
    /// starts as a plain reveal and gains its band continuously — no seam pops in mid-flight.
    #[test]
    fn crop_band_fits_any_frame() {
        assert_eq!(crop_band(CGSize::new(859.0, 1081.0)), 40.0);
        assert!((crop_band(CGSize::new(60.0, 1081.0)) - 27.0).abs() < 1e-9);
        assert_eq!(crop_band(CGSize::new(0.0, 1081.0)), 0.0);
    }

    /// Every crop piece of a region picture stays inside the region, scaled to it, growing or
    /// shrinking: a piece reaching past the region would show the composite's next window.
    #[test]
    fn the_crop_grid_of_a_region_stays_in_the_region() {
        let region = rect(0.125, 0.5, 0.25, 0.5);
        let picture = CGSize::new(856.0, 1081.0);
        for frame in [
            CGSize::new(856.0, 1081.0),
            CGSize::new(428.0, 1081.0),
            CGSize::new(856.0, 540.0),
            CGSize::new(1200.0, 1300.0),
        ] {
            for piece in crop_pieces(picture, frame) {
                let whole = piece.contents;
                let cut = within(ContentsRect::from_cg(region), ContentsRect::from_cg(whole));
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
        let model = rect(4.0, 32.0, 861.0, 1081.0);
        let real = rect(4.0, 32.0, 860.0, 1081.0);
        assert_eq!(snap_to(model, real), Some(real));
        let moved = rect(3.0, 32.0, 861.0, 1081.0);
        assert_eq!(snap_to(model, moved), Some(moved));
    }

    #[test]
    fn a_tile_snaps_only_past_a_quarter_point_and_up_to_twenty_points_off() {
        let model = rect(4.0, 32.0, 861.0, 1081.0);
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

        const STRIP: GroupKey = GroupKey::STILL;

        fn window() -> WindowId { WindowId::new(1, 1) }

        /// The strip's container, model at -861, and a tile at `frame` in it: at 865 it ends at 4 on
        /// screen. `leg` is the tile's own track, when it has one.
        fn riding(frame: CGRect, leg: Option<&Track>) -> Score {
            let mut score = Score::default();
            for edit in [
                Edit::Place(Node::Container(STRIP), Value::Point(CGPoint::new(-861.0, 0.0))),
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

        fn leg(from: CGRect, to: CGRect) -> Track {
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
            let real = rect(4.0, 32.0, 860.0, 1081.0);
            assert_eq!(snap(&score, window(), real, 100.0), [Edit::Place(
                Node::Tile(window()),
                Value::Rect(rect(865.0, 32.0, 860.0, 1081.0))
            )]);
        }

        /// A tile on a track of its own goes on along it to the window's frame: the same begin, start
        /// and curve, a new end, a resize's size with it.
        #[test]
        fn a_tile_on_its_own_leg_snaps_on_the_same_timing() {
            let to = rect(865.0, 32.0, 861.0, 1081.0);
            for from in [
                rect(1726.0, 32.0, 861.0, 1081.0),
                rect(1726.0, 32.0, 1200.0, 1081.0),
            ] {
                let leg = leg(from, to);
                let score = riding(to, Some(&leg));
                let real = rect(4.0, 32.0, 860.0, 1081.0);
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
            let to = rect(865.0, 32.0, 861.0, 1081.0);
            let leg = leg(rect(1726.0, 32.0, 861.0, 1081.0), to);
            let score = riding(to, Some(&leg));
            assert_eq!(snap(&score, window(), rect(5.0, 32.0, 861.0, 1081.0), 100.5), [
                Edit::Place(
                    Node::Tile(window()),
                    Value::Rect(rect(866.0, 32.0, 861.0, 1081.0))
                )
            ]);
            assert!(
                snap(&Score::default(), window(), to, 100.5).is_empty(),
                "no tile, no snap"
            );
        }
    }
}
