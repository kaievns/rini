//! Where the switcher's cursor is, and what moves it.
//!
//! One owner for the selection, because three things move it — the trigger key repeating, the arrow
//! keys, and a mouse click — and any two of them keeping their own idea of "the selected row" is a
//! race that shows up as the popup highlighting one window and the release focusing another.
//!
//! Pure, so the wrap-around and the empty cases are settled here rather than in whichever of the three
//! callers is written last.

/// The live cursor over a switch list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    len: usize,
    index: usize,
}

impl Selection {
    /// A cursor over `len` rows starting at `index`, or `None` when there is nothing to select.
    ///
    /// An out-of-range start is clamped rather than refused: the list is rebuilt while a switch is
    /// open (a window can close under it) and a cursor past the end must land somewhere valid instead
    /// of ending the switch.
    pub fn new(len: usize, index: usize) -> Option<Self> {
        if len == 0 {
            return None;
        }
        Some(Self { len, index: index.min(len - 1) })
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Move by `delta` rows, wrapping at both ends.
    ///
    /// Wrapping in both directions is what makes holding the key usable: the native switcher never
    /// stops at the end, and a cursor that stuck there would make the last row feel like a dead key.
    pub fn step(&mut self, delta: isize) {
        if self.len == 0 {
            return;
        }
        let len = self.len as isize;
        // Two rem_euclid steps: the first keeps the delta inside one lap so a huge value cannot
        // overflow the addition, the second brings the sum back into range.
        let step = delta.rem_euclid(len);
        self.index = ((self.index as isize + step).rem_euclid(len)) as usize;
    }

    /// Point at a row directly, as a click does. Out-of-range is ignored rather than clamped: a click
    /// outside the rows is not a request to select the last one.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        self.index = index;
        true
    }

    /// Re-point the cursor at a list that has changed length, keeping the row it was on if that row
    /// still exists.
    ///
    /// `moved_to` is where the previously selected row now sits, if it is still in the list. A switch
    /// stays open while windows open and close, and the selection has to follow the WINDOW rather than
    /// the position — otherwise closing a window above the cursor silently retargets the commit.
    pub fn relocate(&mut self, len: usize, moved_to: Option<usize>) -> bool {
        if len == 0 {
            return false;
        }
        self.len = len;
        self.index = match moved_to {
            Some(index) => index.min(len - 1),
            None => self.index.min(len - 1),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_over_nothing_does_not_exist() {
        assert_eq!(Selection::new(0, 0), None);
    }

    #[test]
    fn stepping_forward_walks_the_list() {
        let mut cursor = Selection::new(3, 0).expect("cursor");
        cursor.step(1);
        assert_eq!(cursor.index(), 1);
        cursor.step(1);
        assert_eq!(cursor.index(), 2);
    }

    /// The native switcher never stops at the end, and a cursor that did would make the last row feel
    /// like a dead key.
    #[test]
    fn stepping_wraps_at_both_ends() {
        let mut cursor = Selection::new(3, 2).expect("cursor");
        cursor.step(1);
        assert_eq!(cursor.index(), 0, "past the end wraps to the start");

        cursor.step(-1);
        assert_eq!(cursor.index(), 2, "before the start wraps to the end");
    }

    #[test]
    fn a_list_of_one_is_its_own_neighbour() {
        let mut cursor = Selection::new(1, 0).expect("cursor");
        cursor.step(1);
        assert_eq!(cursor.index(), 0);
        cursor.step(-1);
        assert_eq!(cursor.index(), 0);
    }

    /// A held key repeats fast. A delta bigger than the list must not overflow the arithmetic or land
    /// outside it.
    #[test]
    fn a_delta_larger_than_the_list_still_lands_inside_it() {
        let mut cursor = Selection::new(3, 0).expect("cursor");
        cursor.step(7);
        assert_eq!(cursor.index(), 1);
        cursor.step(-7);
        assert_eq!(cursor.index(), 0);
        cursor.step(isize::MAX);
        assert!(cursor.index() < 3);
        cursor.step(isize::MIN);
        assert!(cursor.index() < 3);
    }

    #[test]
    fn a_click_points_at_a_row_directly() {
        let mut cursor = Selection::new(4, 0).expect("cursor");
        assert!(cursor.select(3));
        assert_eq!(cursor.index(), 3);
    }

    /// A click outside the rows is not a request to select the nearest one.
    #[test]
    fn a_click_outside_the_rows_is_refused() {
        let mut cursor = Selection::new(2, 0).expect("cursor");
        assert!(!cursor.select(5));
        assert_eq!(cursor.index(), 0, "and the cursor has not moved");
    }

    /// A window closing while the switch is open must not silently retarget the commit to whatever
    /// slid into that position.
    #[test]
    fn the_cursor_follows_its_row_when_the_list_changes() {
        let mut cursor = Selection::new(4, 2).expect("cursor");
        assert!(cursor.relocate(3, Some(1)));
        assert_eq!(cursor.index(), 1);
    }

    /// When the selected row has gone entirely, the cursor stays where it is rather than jumping.
    #[test]
    fn a_vanished_row_leaves_the_cursor_in_place() {
        let mut cursor = Selection::new(4, 2).expect("cursor");
        assert!(cursor.relocate(3, None));
        assert_eq!(cursor.index(), 2);
    }

    #[test]
    fn relocating_past_the_new_end_clamps() {
        let mut cursor = Selection::new(4, 3).expect("cursor");
        assert!(cursor.relocate(2, None));
        assert_eq!(cursor.index(), 1);
    }

    #[test]
    fn relocating_onto_an_empty_list_fails_rather_than_panicking() {
        let mut cursor = Selection::new(2, 1).expect("cursor");
        assert!(!cursor.relocate(0, None));
    }

    /// The list is rebuilt under a live switch, so a start index past the end has to land somewhere
    /// valid rather than ending the switch.
    #[test]
    fn an_out_of_range_start_is_clamped() {
        let cursor = Selection::new(2, 9).expect("cursor");
        assert_eq!(cursor.index(), 1);
    }
}
