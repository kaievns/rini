//! The card: a picture of a window drawn as a thing of its own, as the switcher draws one per row of
//! its popup and the overview one per thumb, the ring the selected one wears, the stack a run of them
//! collapses into, and what darkens whatever is behind them. One record for both features, so their
//! look cannot drift (`specs/switcher.md`, "The popup"; `specs/overview.md`, "What it shows").
//!
//! In points on screen: the overview, which draws under a zoom, divides by it.

use rini_geometry::space::{Point, Rect, Size};

use super::palette::{Colour, N0};

/// A card's corner radius, and a departure from the design system: `--radius-control` is 5px, a
/// button's radius. A card is a picture of a window, and macOS rounds every window, so a
/// square-cornered card reads as a screenshot of a window rather than as a window.
pub const CORNER: f64 = 11.0;

/// Between two cards side by side.
pub const GAP: f64 = 14.0;

/// The ring around the selected card. A whole outline says "this is the one" about a card; the
/// elevation law's edge bar for an active list row says "this is the current line".
pub const RING: f64 = 2.0;

/// What darkens whatever is behind the cards: the deepest step on the spine, two below the content
/// plane, because the cards float over content rather than being content.
pub const BACKDROP: Colour = N0;

/// How much `BACKDROP` darkens. The one value here that is a judgement rather than a token: the
/// system has none for an overlay's translucency. Chosen against screenshots of 0.35, 0.50 and 0.65
/// over one desktop as "the native switcher, a step darker" (`src/switcher/docs/README.md`).
pub const BACKDROP_ALPHA: f64 = 0.40;

/// A stack of cards: its top card, and the cards behind it each a step further the way the stack
/// goes and shorter across it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stack {
    /// How much further each card behind sits than the card in front of it.
    pub step: f64,
    /// How much shorter each card behind sits across the stack, at each end. Wider than `CORNER`, or
    /// a card behind shows through the rounded corner of the card in front.
    pub inset: f64,
    /// How many are drawn: the top card and two behind. Three read as a stack; more would thin each
    /// to a sliver.
    pub cards: usize,
}

/// The one stack both features draw.
pub const STACK: Stack = Stack {
    step: 8.0,
    inset: 12.0,
    cards: 3,
};

/// Which way a stack's cards behind its top one go: up, as the switcher stacks an application's
/// windows, or out toward the edge a strip's columns collapse at, as the overview does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Up,
    Left,
    Right,
}

impl Stack {
    /// The card `k` behind `front`: `step` further `toward` for each, `inset` shorter across at each
    /// end, about the same centre. A card past the last one drawn sits where that one does.
    pub fn behind<U>(&self, front: Rect<U>, k: usize, toward: Toward) -> Rect<U> {
        let k = k.min(self.cards.saturating_sub(1)) as f64;
        let (along, across) = (self.step * k, self.inset * k);
        let centre = front.center();
        match toward {
            Toward::Up => {
                let width = (front.width() - 2.0 * across).max(0.0);
                Rect::new(
                    Point::new(centre.x - width / 2.0, front.min_y() - along),
                    Size::new(width, front.height()),
                )
            }
            Toward::Left | Toward::Right => {
                let out = if toward == Toward::Left {
                    -along
                } else {
                    along
                };
                let height = (front.height() - 2.0 * across).max(0.0);
                Rect::new(
                    Point::new(front.min_x() + out, centre.y - height / 2.0),
                    Size::new(front.width(), height),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::OverlayRect;

    use super::*;

    fn front() -> OverlayRect {
        OverlayRect::new(Point::new(100.0, 200.0), Size::new(300.0, 160.0))
    }

    #[test]
    fn a_card_behind_steps_up_and_narrows_on_each_side() {
        let behind = STACK.behind(front(), 2, Toward::Up);
        assert_eq!(behind.min_y(), 200.0 - 2.0 * STACK.step);
        assert_eq!(behind.min_x(), 100.0 + 2.0 * STACK.inset);
        assert_eq!(behind.max_x(), 400.0 - 2.0 * STACK.inset);
        assert_eq!(behind.height(), 160.0);
    }

    #[test]
    fn a_card_behind_steps_out_and_shortens_at_each_end_the_left_mirroring_the_right() {
        let right = STACK.behind(front(), 1, Toward::Right);
        let left = STACK.behind(front(), 1, Toward::Left);
        assert_eq!(right.min_x(), 100.0 + STACK.step);
        assert_eq!(left.min_x(), 100.0 - STACK.step);
        for card in [right, left] {
            assert_eq!(card.min_y(), 200.0 + STACK.inset);
            assert_eq!(card.max_y(), 360.0 - STACK.inset);
            assert_eq!(card.width(), 300.0);
        }
    }

    #[test]
    fn the_top_card_is_the_front_and_one_past_the_last_drawn_sits_on_it() {
        assert_eq!(STACK.behind(front(), 0, Toward::Right), front());
        let deepest = STACK.behind(front(), STACK.cards - 1, Toward::Right);
        assert_eq!(STACK.behind(front(), STACK.cards + 4, Toward::Right), deepest);
    }

    /// A card behind narrower than the corner would show through the front card's rounded corner.
    #[test]
    fn the_inset_is_wider_than_the_corner() { const { assert!(STACK.inset > CORNER) }; }
}
