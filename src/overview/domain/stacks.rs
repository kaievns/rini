//! The columns of a row that collapse into a stack at either edge of the display.
//!
//! The switcher's stacks turned on their side: the card's stack (`chrome::domain::card::STACK`)
//! toward each edge. Walking out from the pivot column, the first column that crosses an edge and
//! every one beyond it stack there. The top card is the nearest of them, at its own size,
//! `EDGE_INSET` inside the edge, and the thumb beside it still in the row is drawn over it rather
//! than joining it, the pivot among them: a column that joined left a gap as wide as itself between
//! the row and its stack. Where that thumb would leave a gap even so, the card moves in to reach
//! `TUCK` under it; where the card would reach past that thumb's inner edge, it moves out to it, so
//! it never shows in the gap inside that thumb. Each card behind is as wide as it, a step further out and shorter at top and
//! bottom, and only the stack's cards are drawn. The pivot never stacks.

use rini_geometry::space::{OverlayRect, Point};

use crate::chrome::domain::card::{STACK, Toward};

/// Between the top card's outer edge and the display's edge.
pub const EDGE_INSET: f64 = 16.0;
/// How far the thumb beside a stack in its row reaches over the stack's top card at least, at most
/// half the card. First value for Kai to tune.
pub const TUCK: f64 = 24.0;

/// Which edge of the display a stack is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// The way its cards behind go: out toward its edge.
    fn toward(self) -> Toward {
        match self {
            Side::Left => Toward::Left,
            Side::Right => Toward::Right,
        }
    }
}

/// The horizontal extent stacks form at the edges of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edges {
    pub left: f64,
    pub right: f64,
}

/// A column drawn as a card of an edge stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card {
    /// The column's index in its row.
    pub column: usize,
    /// How many cards are in front of it: 0 for the top card.
    pub depth: usize,
    /// Where the card is drawn. A card past the stack's last drawn sits where that one does.
    pub frame: OverlayRect,
}

impl Card {
    pub fn drawn(&self) -> bool { self.depth < STACK.cards }
}

/// Each edge's stack, its top card first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stacks {
    pub left: Vec<Card>,
    pub right: Vec<Card>,
}

impl Stacks {
    /// The stack holding `column` and its card there, if one does.
    pub fn card(&self, column: usize) -> Option<(Side, Card)> {
        let find = |cards: &[Card]| cards.iter().find(|card| card.column == column).copied();
        find(&self.left)
            .map(|card| (Side::Left, card))
            .or_else(|| find(&self.right).map(|card| (Side::Right, card)))
    }
}

/// The stacks `columns`, laid out left to right, form at `edges`, walking out from `pivot`.
pub fn edge_stacks(columns: &[OverlayRect], pivot: usize, edges: Edges) -> Stacks {
    if pivot >= columns.len() {
        return Stacks::default();
    }
    Stacks {
        left: stack(columns, pivot, edges, Side::Left),
        right: stack(columns, pivot, edges, Side::Right),
    }
}

fn stack(columns: &[OverlayRect], pivot: usize, edges: Edges, side: Side) -> Vec<Card> {
    let outward: Vec<usize> = match side {
        Side::Left => (0..pivot).rev().collect(),
        Side::Right => (pivot + 1..columns.len()).collect(),
    };
    let crosses = |rect: &OverlayRect| match side {
        Side::Left => rect.min_x() < edges.left,
        Side::Right => rect.max_x() > edges.right,
    };
    let Some(first) = outward.iter().position(|&column| crosses(&columns[column])) else {
        return Vec::new();
    };
    let nearest = first.checked_sub(1).map_or(pivot, |inward| outward[inward]);
    let top = top_card(columns[outward[first]], columns[nearest], edges, side);
    outward[first..]
        .iter()
        .enumerate()
        .map(|(depth, &column)| Card {
            column,
            depth,
            frame: STACK.behind(top, depth, side.toward()),
        })
        .collect()
}

/// The top card: `column` at its own size, its outer edge `EDGE_INSET` inside the edge, moved in
/// where that would leave less than `TUCK` of it under `nearest`, the thumb beside it in the row,
/// and out where its inner edge would reach past `nearest`'s, past the edge if it must.
fn top_card(column: OverlayRect, nearest: OverlayRect, edges: Edges, side: Side) -> OverlayRect {
    let tuck = TUCK.min(column.width() / 2.0);
    let x = match side {
        Side::Left => (edges.left + EDGE_INSET)
            .max(nearest.min_x() + tuck - column.width())
            .min(nearest.max_x() - column.width()),
        Side::Right => (edges.right - EDGE_INSET - column.width())
            .min(nearest.max_x() - tuck)
            .max(nearest.min_x()),
    };
    OverlayRect::new(Point::new(x, column.min_y()), column.size)
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::Size;

    use super::*;

    const EDGES: Edges = Edges { left: 0.0, right: 1728.0 };

    fn column(x: f64, width: f64) -> OverlayRect {
        OverlayRect::new(Point::new(x, 100.0), Size::new(width, 240.0))
    }

    /// Columns `width` wide and `gap` apart, the `pivot`th centred on 864.
    fn strip(count: usize, width: f64, gap: f64, pivot: usize) -> Vec<OverlayRect> {
        let start = 864.0 - width / 2.0 - pivot as f64 * (width + gap);
        (0..count).map(|i| column(start + i as f64 * (width + gap), width)).collect()
    }

    #[test]
    fn columns_inside_the_edges_never_stack() {
        let columns = strip(5, 180.0, 4.0, 2);
        assert_eq!(edge_stacks(&columns, 2, EDGES), Stacks::default());
    }

    #[test]
    fn columns_past_an_edge_stack_there_nearest_on_top_three_drawn() {
        let columns = strip(13, 180.0, 4.0, 1);
        let stacks = edge_stacks(&columns, 1, EDGES);
        assert!(stacks.left.is_empty());
        let right = &stacks.right;
        let first_crossing = (2..13).find(|&c| columns[c].max_x() > EDGES.right).unwrap();
        assert_eq!(right[0].column, first_crossing, "{right:?}");
        let order: Vec<usize> = right.iter().map(|card| card.column).collect();
        assert_eq!(order, (first_crossing..13).collect::<Vec<_>>());
        let top = right[0].frame;
        assert!((top.max_x() - (EDGES.right - EDGE_INSET)).abs() < 1e-9);
        assert_eq!(
            top.size, columns[right[0].column].size,
            "the top card keeps its size"
        );
        for card in right {
            assert_eq!(
                card.frame,
                STACK.behind(top, card.depth, Toward::Right),
                "the card's stack"
            );
            let k = card.depth.min(2) as f64;
            assert!((card.frame.max_x() - (top.max_x() + 8.0 * k)).abs() < 1e-9);
            assert!((card.frame.min_y() - (top.min_y() + 12.0 * k)).abs() < 1e-9);
            assert!((card.frame.max_y() - (top.max_y() - 12.0 * k)).abs() < 1e-9);
            assert_eq!(card.drawn(), card.depth < 3);
        }
        assert_eq!(right.iter().filter(|card| card.drawn()).count(), 3);
    }

    /// The thumb beside a stack stays in its row and is drawn over the stack's top card, which
    /// stays at the edge: joining it to the stack left a gap as wide as it between the row and the
    /// stack.
    #[test]
    fn the_thumb_beside_a_stack_overlaps_its_top_card_leaving_no_gap() {
        let columns = vec![
            column(700.0, 300.0),
            column(1010.0, 300.0),
            column(1320.0, 300.0),
            column(1630.0, 300.0),
        ];
        let stacks = edge_stacks(&columns, 0, EDGES);
        let order: Vec<usize> = stacks.right.iter().map(|card| card.column).collect();
        assert_eq!(order, vec![3]);
        let top = stacks.right[0].frame;
        assert!(
            (top.max_x() - (EDGES.right - EDGE_INSET)).abs() < 1e-9,
            "{top:?}"
        );
        assert!(
            columns[2].max_x() - top.min_x() >= TUCK,
            "the third column reaches over the top card"
        );
    }

    /// A top card that at the edge would leave a gap to the thumb beside it moves in to reach under
    /// it, on either side.
    #[test]
    fn a_top_card_moves_in_to_reach_under_the_thumb_beside_it() {
        let columns = vec![
            column(-150.0, 200.0),
            column(260.0, 400.0),
            column(700.0, 300.0),
            column(1100.0, 400.0),
            column(1540.0, 200.0),
        ];
        let stacks = edge_stacks(&columns, 2, EDGES);
        let (left, right) = (stacks.left[0], stacks.right[0]);
        assert_eq!((left.column, right.column), (0, 4));
        assert!((right.frame.min_x() - (columns[3].max_x() - TUCK)).abs() < 1e-9);
        assert!((left.frame.max_x() - (columns[1].min_x() + TUCK)).abs() < 1e-9);
        assert!(
            right.frame.max_x() < EDGES.right - EDGE_INSET,
            "moved in from the edge"
        );
        assert!(
            left.frame.min_x() > EDGES.left + EDGE_INSET,
            "moved in from the edge"
        );
    }

    /// A wide column stacked beside a narrow thumb sits no further in than that thumb's inner
    /// edge, past the display's edge if it must, so it never shows in the gap inside the thumb, and
    /// still reaches under it; on either side. Depth 0.3 on 1728pt: a half-width thumb beside a
    /// full-width column.
    #[test]
    fn a_wide_top_card_reaches_no_further_in_than_the_narrow_thumb_beside_it() {
        let columns = vec![
            column(-418.0, 518.0),
            column(105.0, 259.0),
            column(378.0, 236.0),
            column(628.0, 472.0),
            column(1114.0, 236.0),
            column(1364.0, 259.0),
            column(1628.0, 518.0),
        ];
        let stacks = edge_stacks(&columns, 3, EDGES);
        let (left, right) = (stacks.left[0], stacks.right[0]);
        assert_eq!((left.column, right.column), (0, 6));
        let (near_left, near_right) = (columns[1], columns[5]);
        assert!(
            right.frame.min_x() >= near_right.min_x() - 1e-9,
            "{right:?} shows left of {near_right:?}"
        );
        assert!(
            left.frame.max_x() <= near_left.max_x() + 1e-9,
            "{left:?} shows right of {near_left:?}"
        );
        assert!(near_right.max_x() - right.frame.min_x() >= TUCK, "{right:?}");
        assert!(left.frame.max_x() - near_left.min_x() >= TUCK, "{left:?}");
        assert_eq!(right.frame.size, columns[6].size, "the top card keeps its size");
    }

    #[test]
    fn the_current_column_never_stacks() {
        let columns = vec![
            column(-400.0, 300.0),
            column(-90.0, 1500.0),
            column(1420.0, 600.0),
        ];
        let stacks = edge_stacks(&columns, 1, EDGES);
        assert_eq!(stacks.card(1), None);
        assert_eq!(stacks.card(0).map(|(side, _)| side), Some(Side::Left));
        assert_eq!(stacks.card(2).map(|(side, _)| side), Some(Side::Right));
    }

    /// A top card beside the current column reaches under it at the edge rather than sitting past
    /// it: the row, the current column among it, is drawn over its stacks, so nothing covers it.
    #[test]
    fn a_top_card_beside_the_current_column_reaches_under_it() {
        // Full-width columns at depth 0.5: 848pt wide, 8pt apart, the pivot at 440..1288.
        let columns = strip(3, 848.0, 8.0, 1);
        let stacks = edge_stacks(&columns, 1, EDGES);
        let pivot = columns[1];
        let left = stacks.left[0].frame;
        let right = stacks.right[0].frame;
        assert!(
            (left.min_x() - (EDGES.left + EDGE_INSET)).abs() < 1e-9,
            "{left:?}"
        );
        assert!(
            (right.max_x() - (EDGES.right - EDGE_INSET)).abs() < 1e-9,
            "{right:?}"
        );
        assert!(left.max_x() - pivot.min_x() >= TUCK, "{left:?}");
        assert!(pivot.max_x() - right.min_x() >= TUCK, "{right:?}");
        assert_eq!(left.size, columns[0].size, "the top card keeps its size");
        assert_eq!(right.size, columns[2].size, "the top card keeps its size");
    }

    #[test]
    fn the_left_edge_mirrors_the_right() {
        let columns = strip(13, 180.0, 4.0, 11);
        let stacks = edge_stacks(&columns, 11, EDGES);
        assert!(stacks.right.is_empty());
        let top = stacks.left[0].frame;
        assert!((top.min_x() - (EDGES.left + EDGE_INSET)).abs() < 1e-9);
        assert!(stacks.left.windows(2).all(|pair| pair[0].column == pair[1].column + 1));
        let deepest = stacks.left[2].frame;
        assert!((deepest.min_x() - (top.min_x() - 2.0 * STACK.step)).abs() < 1e-9);
        assert_eq!(stacks.left.last().unwrap().column, 0);
    }

    #[test]
    fn an_undrawn_card_holds_nothing() {
        let columns = strip(13, 180.0, 4.0, 1);
        let stacks = edge_stacks(&columns, 1, EDGES);
        let undrawn: Vec<&Card> = stacks.right.iter().filter(|card| !card.drawn()).collect();
        assert!(!undrawn.is_empty());
        let deepest = stacks.right[STACK.cards - 1].frame;
        assert!(undrawn.iter().all(|card| card.frame == deepest));
    }
}
