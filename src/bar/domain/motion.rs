//! The bar's two movements, both asked for: the glyphs past the fold fade in and out, and the tray
//! slides into and out of its chevron. Everything else on the bar is a cut.

use std::time::Duration;

use super::layout::{MAX_GLYPHS, Piece};

/// One glyph's fade: 14 frames at 60Hz, the old bar's `FOLD_TICKS`.
pub const FADE_SECONDS: f64 = 14.0 / 60.0;
/// From one glyph's fade starting to the next one's.
pub const FADE_STAGGER: f64 = 0.035;
/// The tray's slide, eased out.
pub const SLIDE_SECONDS: f64 = 0.2;

/// Where the glyph fold is. One fold for every display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Folding {
    #[default]
    Folded,
    Unfolded,
    /// Fading the tail out, and laid out unfolded until that has run.
    Refolding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fade {
    In,
    Out,
}

impl Folding {
    /// Whether the glyphs are laid out unfolded.
    pub fn expanded(self) -> bool {
        self != Folding::Folded
    }

    /// A click on the fold indicator: where the fold goes, and the fade that takes it there. A
    /// click while the tail is fading out brings it back.
    pub fn clicked(self) -> (Folding, Fade) {
        match self {
            Folding::Folded | Folding::Refolding => (Folding::Unfolded, Fade::In),
            Folding::Unfolded => (Folding::Refolding, Fade::Out),
        }
    }

    /// The fade out has run.
    pub fn faded(self) -> Folding {
        match self {
            Folding::Refolding => Folding::Folded,
            other => other,
        }
    }
}

/// The glyphs past the fold among `pieces`, left to right.
pub fn tail(pieces: impl IntoIterator<Item = Piece>) -> Vec<Piece> {
    let mut indices: Vec<usize> = pieces
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Glyph(index) if index >= MAX_GLYPHS => Some(index),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    indices.into_iter().map(Piece::Glyph).collect()
}

/// When each of `count` glyphs, left to right, starts to fade, in seconds from the click. In runs
/// left to right, the list unrolling; out runs right to left, the list retracting.
pub fn delays(fade: Fade, count: usize) -> Vec<f64> {
    (0..count)
        .map(|index| {
            let step = match fade {
                Fade::In => index,
                Fade::Out => count - 1 - index,
            };
            step as f64 * FADE_STAGGER
        })
        .collect()
}

/// From the first glyph starting to fade to the last one finishing.
pub fn fade_length(count: usize) -> Duration {
    match count {
        0 => Duration::ZERO,
        _ => Duration::from_secs_f64((count - 1) as f64 * FADE_STAGGER + FADE_SECONDS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unfolding lays out unfolded at once; folding keeps the tail laid out until it has faded.
    #[test]
    fn a_click_unfolds_at_once_and_folds_after_the_fade() {
        let (unfolded, fade) = Folding::Folded.clicked();
        assert_eq!((unfolded, fade), (Folding::Unfolded, Fade::In));
        assert!(unfolded.expanded());

        let (refolding, fade) = unfolded.clicked();
        assert_eq!((refolding, fade), (Folding::Refolding, Fade::Out));
        assert!(refolding.expanded());
        assert_eq!(refolding.faded(), Folding::Folded);
        assert!(!Folding::Folded.expanded());
    }

    /// A second click while the tail fades out fades it back in, and the fade finishing then does
    /// not fold it.
    #[test]
    fn a_click_during_the_fade_out_brings_the_tail_back() {
        let (back, fade) = Folding::Refolding.clicked();
        assert_eq!((back, fade), (Folding::Unfolded, Fade::In));
        assert_eq!(back.faded(), Folding::Unfolded);
    }

    #[test]
    fn the_tail_is_the_glyphs_past_the_fold_left_to_right() {
        let pieces = [
            Piece::Glyph(7),
            Piece::Numeral(0),
            Piece::Glyph(4),
            Piece::Glyph(5),
            Piece::More,
            Piece::Glyph(6),
        ];
        assert_eq!(tail(pieces), vec![
            Piece::Glyph(5),
            Piece::Glyph(6),
            Piece::Glyph(7)
        ]);
        assert!(tail([Piece::Glyph(0), Piece::Glyph(4)]).is_empty());
    }

    /// 0.035s apart: in from the left, out from the right.
    #[test]
    fn the_fades_are_staggered_toward_the_way_the_list_moves() {
        let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-12);
        assert!(close(&delays(Fade::In, 3), &[0.0, 0.035, 0.07]));
        assert!(close(&delays(Fade::Out, 3), &[0.07, 0.035, 0.0]));
        assert!(delays(Fade::Out, 0).is_empty());
    }

    /// The last glyph starts two staggers in and takes 14 frames.
    #[test]
    fn a_fade_lasts_until_its_last_glyph_has_run() {
        assert_eq!(fade_length(0), Duration::ZERO);
        assert_eq!(fade_length(1), Duration::from_secs_f64(FADE_SECONDS));
        assert_eq!(fade_length(3), Duration::from_secs_f64(0.07 + FADE_SECONDS));
    }
}
