//! How one tile fits its picture to the frame it is drawn in, and how its frame interpolates.
//! Sizes and rects only; the layers that realise these live in the overlay.

use std::time::Instant;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::fit::{fits_frame, is_a_resize, outgrows};

/// How a tile's picture is fitted to the frame it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentMode {
    /// Scaled to fill the frame. Right for a movement whose picture matches the frame's shape.
    Stretch,
    /// Cropped or revealed: the picture draws at native 1:1 scale, and the size change is absorbed
    /// by a seam a band in from the trailing edges — content never stretches, the moving edge
    /// swallows or reveals it, which is how a real resize reads. The trailing band, carrying the
    /// window's rounded corners and border, rides the moving edge intact. See "Resizes through the
    /// overlay" in `docs/animation-smoothness.md`.
    Crop,
}

/// Crop for a resize, and for any picture that no longer matches the frame it starts in; stretched
/// only when picture and frames agree, where stretching is exact.
pub fn content_mode(covered: (f64, f64), from: CGSize, to: CGSize) -> ContentMode {
    if is_a_resize(from, to)
        || !fits_frame(covered, (from.width, from.height))
    {
        placeholder_mode(covered, to)
    } else {
        ContentMode::Stretch
    }
}

/// How a tile fits a picture that may not cover its destination: the ordinary crop when it can,
/// stretched when it cannot. The stretch is the placeholder of a grow whose reveal has not landed;
/// every mode fills the whole frame, so a placeholder never shows the backdrop. A top-left crop
/// that left the growth undrawn was tried and read as a hole (see "A grow holds, then reveals"
/// in `docs/animation-smoothness.md`).
pub fn placeholder_mode(covered: (f64, f64), to: CGSize) -> ContentMode {
    if outgrows(covered, to) {
        ContentMode::Stretch
    } else {
        ContentMode::Crop
    }
}

/// What a fresh hairline does to the layers a tile already wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DressingAction {
    /// Same piece set: new pixels into the existing layers, riding any installed animation.
    SwapInPlace,
    /// Different piece set: drop the ring and build it at the model size.
    Rebuild,
    /// Different piece set while a resize is animating: leave the worn ring alone.
    Defer,
}

/// Whether a harvest may rebuild a tile's ring now. See "A fresh picture or hairline swaps in
/// place" in `docs/animation-smoothness.md`.
pub fn dressing_rebuild_allowed(resize_in_flight: bool, worn_matches: bool) -> DressingAction {
    if worn_matches {
        DressingAction::SwapInPlace
    } else if resize_in_flight {
        DressingAction::Defer
    } else {
        DressingAction::Rebuild
    }
}

/// Whether a tile's resize animation, ending at `resize_until`, is still riding at `now`.
pub fn resize_in_flight(resize_until: Option<Instant>, now: Instant) -> bool {
    resize_until.is_some_and(|until| now < until)
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
/// cropped at all (`placeholder_mode`), so this only covers the frames a fitting picture passes
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
/// Interpolates a rect. Separated out and tested because getting this wrong produces an animation
/// that looks almost right, which is much harder to debug than one that is obviously broken.
pub fn lerp_rect(from: CGRect, to: CGRect, t: f64) -> CGRect {
    let l = |a: f64, b: f64| a + (b - a) * t;
    CGRect::new(
        CGPoint::new(l(from.origin.x, to.origin.x), l(from.origin.y, to.origin.y)),
        CGSize::new(l(from.size.width, to.size.width), l(from.size.height, to.size.height)),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// T9 (1.5) of `.kiro/specs/flight-render-stability/bugfix.md`. A harvest with a different
    /// piece set lands while the tile's resize is animating: rebuilding puts the ring at the
    /// destination size over a picture still travelling. Asserts the fixed `Defer`; unfixed
    /// rebuilds.
    #[test]
    fn a_mismatched_dressing_is_deferred_while_a_resize_is_in_flight() {
        let action = dressing_rebuild_allowed(true, false);
        assert_eq!(action, DressingAction::Defer, "hairline rebuilt mid-resize: {action:?}");
    }

    #[test]
    fn dressing_rebuild_allowed_full_table() {
        assert_eq!(dressing_rebuild_allowed(false, true), DressingAction::SwapInPlace);
        assert_eq!(dressing_rebuild_allowed(true, true), DressingAction::SwapInPlace);
        assert_eq!(dressing_rebuild_allowed(false, false), DressingAction::Rebuild);
        assert_eq!(dressing_rebuild_allowed(true, false), DressingAction::Defer);
    }

    #[test]
    fn a_resize_is_in_flight_until_its_end_instant() {
        let now = Instant::now();
        assert!(resize_in_flight(Some(now + Duration::from_millis(100)), now));
        assert!(!resize_in_flight(Some(now - Duration::from_millis(1)), now));
        assert!(!resize_in_flight(Some(now), now), "the end instant itself is over");
        assert!(!resize_in_flight(None, now));
    }

    /// Exhaustive over worn × available 8-bit piece sets and both resize flags, matched the way
    /// `apply_edge_dressing` matches them: a rebuild never lands while a resize is in flight.
    #[test]
    fn a_resize_in_flight_never_rebuilds_the_hairline() {
        for worn_bits in 0u32..256 {
            for available_bits in 0u32..256 {
                let worn: Vec<usize> = (0..8).filter(|i| worn_bits & (1 << i) != 0).collect();
                let available: Vec<usize> =
                    (0..8).filter(|i| available_bits & (1 << i) != 0).collect();
                let worn_matches = !worn.is_empty() && worn == available;
                for resize_in_flight in [false, true] {
                    let action = dressing_rebuild_allowed(resize_in_flight, worn_matches);
                    if resize_in_flight {
                        assert_ne!(
                            action,
                            DressingAction::Rebuild,
                            "worn {worn:?} available {available:?}"
                        );
                    }
                    if worn_matches {
                        assert_eq!(action, DressingAction::SwapInPlace);
                    } else if !resize_in_flight {
                        assert_eq!(action, DressingAction::Rebuild);
                    }
                }
            }
        }
    }

    /// P-3.7 of `.kiro/specs/flight-render-stability/bugfix.md`. Observed: a harvest whose piece
    /// set matches the worn set swaps pixels in place whether or not a resize is in flight, over
    /// every non-empty piece set, matched the way `apply_edge_dressing` matches them.
    #[test]
    fn a_matching_piece_set_swaps_in_place_in_or_out_of_a_resize() {
        for bits in 1u32..256 {
            let worn: Vec<usize> = (0..8).filter(|i| bits & (1 << i) != 0).collect();
            let available = worn.clone();
            let worn_matches = !worn.is_empty() && worn == available;
            assert!(worn_matches, "pieces {worn:?}");
            for resize_in_flight in [false, true] {
                assert_eq!(
                    dressing_rebuild_allowed(resize_in_flight, worn_matches),
                    DressingAction::SwapInPlace,
                    "pieces {worn:?} resize_in_flight={resize_in_flight}"
                );
            }
        }
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
        let got = lerp_rect(rect(0.0, 0.0, 100.0, 100.0), rect(100.0, 200.0, 200.0, 300.0), 0.5);
        assert_eq!(got, rect(50.0, 100.0, 150.0, 200.0));
    }

    #[test]
    fn lerp_handles_a_negative_origin() {
        // Off-strip windows legitimately sit at negative x, so this is the common case rather than
        // an edge case: the measured strip had columns at x = -1680.
        let got = lerp_rect(rect(-1680.0, 32.0, 859.0, 1081.0), rect(0.0, 32.0, 859.0, 1081.0), 0.5);
        assert_eq!(got.origin.x, -840.0);
        assert_eq!(got.origin.y, 32.0);
    }

    #[test]
    fn a_matching_movement_stretches_and_everything_else_crops() {
        let col = CGSize::new(859.0, 1081.0);
        let picture = (859.0, 1081.0);
        assert_eq!(content_mode(picture, col, col), ContentMode::Stretch);
        assert_eq!(
            content_mode((918.0, 1081.0), CGSize::new(918.0, 1081.0), CGSize::new(917.0, 1081.0)),
            ContentMode::Stretch
        );
        // A horizontal grow past the picture is the placeholder: it stretches until the reveal
        // lands. A vertical shrink crops.
        assert_eq!(
            content_mode(picture, col, CGSize::new(1720.0, 1081.0)),
            ContentMode::Stretch
        );
        assert_eq!(content_mode(picture, col, CGSize::new(859.0, 540.0)), ContentMode::Crop);
        // A stale narrow picture in a wider frame is the placeholder case too: the reveal fills it.
        assert_eq!(content_mode((572.0, 1081.0), col, col), ContentMode::Stretch);
        assert_eq!(content_mode((918.0, 1081.0), col, col), ContentMode::Crop, "a wider one crops");
    }

    /// A picture that cannot cover its destination is stretched over it; one that covers it takes
    /// the ordinary crop. Either way the whole frame is drawn.
    #[test]
    fn a_placeholder_never_shows_backdrop() {
        let picture = (859.0, 1081.0);
        assert_eq!(placeholder_mode(picture, CGSize::new(1147.0, 1081.0)), ContentMode::Stretch);
        assert_eq!(placeholder_mode(picture, CGSize::new(859.0, 1300.0)), ContentMode::Stretch);
        assert_eq!(placeholder_mode(picture, CGSize::new(859.0, 1081.0)), ContentMode::Crop);
        assert_eq!(placeholder_mode(picture, CGSize::new(572.0, 1081.0)), ContentMode::Crop);
        // Both modes fill the frame: the match is exhaustive, so a mode that left part of the
        // frame undrawn would have to be added here to compile.
        for w in (1..=40).map(|i| i as f64 * 50.0) {
            for h in (1..=30).map(|i| i as f64 * 50.0) {
                let to = CGSize::new(w, h);
                match placeholder_mode(picture, to) {
                    ContentMode::Stretch => {
                        assert!(outgrows(picture, to), "{to:?}")
                    }
                    ContentMode::Crop => {
                        assert!(!outgrows(picture, to), "{to:?}")
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
        assert_eq!(body.frame.size.width, 1720.0 - 40.0, "stretched across the grown lead");
        // The trailing band is still the picture's own trailing 40pt at 1:1.
        let band = &pieces[1];
        assert!((band.contents.size.width * picture.width - 40.0).abs() < 1e-9);
        assert_eq!(band.frame.size.width, 40.0);
        // And the pieces still tile the frame exactly.
        let area: f64 =
            pieces.iter().map(|p| p.frame.size.width * p.frame.size.height).sum();
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
        assert!((title.contents.origin.y).abs() < 1e-9, "showing the picture's top");
        assert_eq!(title.frame.size.height, 40.0);
        let body = &pieces[2];
        assert_eq!(body.frame.origin.y, 40.0, "body starts at the seam below the title");
        // Bottom-anchored: the contents reach the picture's bottom edge exactly.
        let content_bottom =
            (body.contents.origin.y + body.contents.size.height) * picture.height;
        assert!((content_bottom - 1081.0).abs() < 1e-9, "contents reach the picture's bottom");
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
}
