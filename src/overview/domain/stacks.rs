//! The columns of a row that collapse into a stack at either edge of the display.
//!
//! The switcher's stacks turned on their side. Walking out from the pivot column, the first column
//! that crosses an edge and every one beyond it stack there. The top card is the nearest of them, at
//! its own size, `EDGE_INSET` inside the edge; each card behind is as wide as it, sits `CARD_STEP`
//! further out and `CARD_INSET` shorter at top and bottom, and only `DRAWN_CARDS` are drawn. A column the top card
//! overlaps joins the stack as its new top card, until none does. The pivot never stacks.

use rini_geometry::space::{OverlayRect, Point, Size};

/// Between the top card's outer edge and the display's edge.
pub const EDGE_INSET: f64 = 16.0;
/// How much further toward the edge each card behind the top one sits.
pub const CARD_STEP: f64 = 8.0;
/// How much shorter each card behind sits, at its top and at its bottom.
pub const CARD_INSET: f64 = 12.0;
/// The cards a stack draws: its top card and two behind. Three read as a stack; more would thin
/// each to a sliver.
pub const DRAWN_CARDS: usize = 3;

/// Which edge of the display a stack is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
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
    /// Where the card is drawn. A card past `DRAWN_CARDS` sits where the deepest drawn one does.
    pub frame: OverlayRect,
}

impl Card {
    pub fn drawn(&self) -> bool { self.depth < DRAWN_CARDS }
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
    let mut members: Vec<usize> = outward[first..].to_vec();
    let mut top = top_card(columns[members[0]], edges, side);
    for &next in outward[..first].iter().rev() {
        let overlaps = match side {
            Side::Left => columns[next].min_x() < top.max_x(),
            Side::Right => columns[next].max_x() > top.min_x(),
        };
        if !overlaps {
            break;
        }
        members.insert(0, next);
        top = top_card(columns[next], edges, side);
    }
    members
        .into_iter()
        .enumerate()
        .map(|(depth, column)| Card {
            column,
            depth,
            frame: behind(top, depth.min(DRAWN_CARDS - 1), side),
        })
        .collect()
}

/// The top card: `column` at its own size, its outer edge `EDGE_INSET` inside the edge.
fn top_card(column: OverlayRect, edges: Edges, side: Side) -> OverlayRect {
    let x = match side {
        Side::Left => edges.left + EDGE_INSET,
        Side::Right => edges.right - EDGE_INSET - column.width(),
    };
    OverlayRect::new(Point::new(x, column.min_y()), column.size)
}

/// The card `depth` behind `top`: `CARD_STEP` further out each, `CARD_INSET` shorter at each end.
fn behind(top: OverlayRect, depth: usize, side: Side) -> OverlayRect {
    let k = depth as f64;
    let out = match side {
        Side::Left => -CARD_STEP * k,
        Side::Right => CARD_STEP * k,
    };
    let height = (top.height() - 2.0 * CARD_INSET * k).max(0.0);
    OverlayRect::new(
        Point::new(top.min_x() + out, top.center().y - height / 2.0),
        Size::new(top.width(), height),
    )
}

#[cfg(test)]
mod tests {
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
        assert_eq!(right[0].column, first_crossing - 1, "{right:?}");
        let order: Vec<usize> = right.iter().map(|card| card.column).collect();
        assert_eq!(order, (first_crossing - 1..13).collect::<Vec<_>>());
        let top = right[0].frame;
        assert!((top.max_x() - (EDGES.right - EDGE_INSET)).abs() < 1e-9);
        assert_eq!(
            top.size, columns[right[0].column].size,
            "the top card keeps its size"
        );
        for card in right {
            let k = card.depth.min(DRAWN_CARDS - 1) as f64;
            assert!((card.frame.max_x() - (top.max_x() + CARD_STEP * k)).abs() < 1e-9);
            assert!((card.frame.min_y() - (top.min_y() + CARD_INSET * k)).abs() < 1e-9);
            assert!((card.frame.max_y() - (top.max_y() - CARD_INSET * k)).abs() < 1e-9);
            assert_eq!(card.drawn(), card.depth < 3);
        }
        assert_eq!(right.iter().filter(|card| card.drawn()).count(), 3);
    }

    #[test]
    fn a_column_overlapping_the_top_card_joins_the_stack() {
        // The fourth column crosses the right edge; the third, inside it, reaches past where the
        // top card goes, so it joins and is the top card.
        let columns = vec![
            column(700.0, 300.0),
            column(1010.0, 300.0),
            column(1320.0, 300.0),
            column(1630.0, 300.0),
        ];
        let stacks = edge_stacks(&columns, 0, EDGES);
        let order: Vec<usize> = stacks.right.iter().map(|card| card.column).collect();
        assert_eq!(order, vec![2, 3]);
        assert!(
            columns[1].max_x() <= stacks.right[0].frame.min_x(),
            "the next one inward clears the top card, so it stays in the row"
        );
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

    #[test]
    fn the_left_edge_mirrors_the_right() {
        let columns = strip(13, 180.0, 4.0, 11);
        let stacks = edge_stacks(&columns, 11, EDGES);
        assert!(stacks.right.is_empty());
        let top = stacks.left[0].frame;
        assert!((top.min_x() - (EDGES.left + EDGE_INSET)).abs() < 1e-9);
        assert!(stacks.left.windows(2).all(|pair| pair[0].column == pair[1].column + 1));
        let deepest = stacks.left[2].frame;
        assert!((deepest.min_x() - (top.min_x() - 2.0 * CARD_STEP)).abs() < 1e-9);
        assert_eq!(stacks.left.last().unwrap().column, 0);
    }

    #[test]
    fn an_undrawn_card_holds_nothing() {
        let columns = strip(13, 180.0, 4.0, 1);
        let stacks = edge_stacks(&columns, 1, EDGES);
        let undrawn: Vec<&Card> = stacks.right.iter().filter(|card| !card.drawn()).collect();
        assert!(!undrawn.is_empty());
        let deepest = stacks.right[DRAWN_CARDS - 1].frame;
        assert!(undrawn.iter().all(|card| card.frame == deepest));
    }
}
