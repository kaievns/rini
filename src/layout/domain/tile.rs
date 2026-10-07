//! A tiled window and its slot, the place the strip lays out for it: the window inset by the
//! border it wears, so a border takes its width out of the window and the strip is laid out the same
//! with borders on or off. Every value the strip reads is a slot's, and every frame it hands out is
//! a window's; this is the one conversion between them. See "A border takes its width out of the
//! window" in `src/layout/docs/strip.md`.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::layout::WindowLayoutConstraints;

/// The window laid out in `slot` under a border `border` wide on every side.
pub fn window_in(slot: CGRect, border: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(slot.origin.x + border, slot.origin.y + border),
        CGSize::new(
            (slot.size.width - 2.0 * border).max(0.0),
            (slot.size.height - 2.0 * border).max(0.0),
        ),
    )
}

/// The slot of a window at `frame` under a border `border` wide.
pub fn slot_of(frame: CGRect, border: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.origin.x - border, frame.origin.y - border),
        CGSize::new(frame.size.width + 2.0 * border, frame.size.height + 2.0 * border),
    )
}

/// The limits of a window's slot: each of the window's own, its minimum, lock and maximum, with
/// the border on both sides. A limit the window does not report stays unreported.
pub fn slot_limits(window: WindowLayoutConstraints, border: f64) -> WindowLayoutConstraints {
    let window = window.normalized();
    let with = |limit: f64| if limit > 0.0 { limit + 2.0 * border } else { limit };
    WindowLayoutConstraints {
        is_resizable: window.is_resizable,
        locked_width: with(window.locked_width),
        locked_height: with(window.locked_height),
        min_width: with(window.min_width),
        min_height: with(window.min_height),
        max_width: with(window.max_width),
        max_height: with(window.max_height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect { CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)) }

    #[test]
    fn a_slot_and_its_window_convert_both_ways() {
        let slot = rect(600.0, 3.0, 882.0, 1111.0);
        let window = window_in(slot, 4.0);
        assert_eq!(window, rect(604.0, 7.0, 874.0, 1103.0));
        assert_eq!(slot_of(window, 4.0), slot);
        assert_eq!(window_in(rect(0.0, 0.0, 6.0, 400.0), 4.0).size.width, 0.0, "never inside out");
    }

    #[test]
    fn a_zero_border_changes_nothing() {
        let frame = rect(600.0, 3.0, 882.0, 1111.0);
        assert_eq!(window_in(frame, 0.0), frame);
        assert_eq!(slot_of(frame, 0.0), frame);
        let limits = WindowLayoutConstraints { min_width: 400.0, max_height: 900.0, ..Default::default() };
        assert_eq!(slot_limits(limits, 0.0), limits.normalized());
    }

    #[test]
    fn a_slot_holds_the_windows_limits_and_its_border() {
        let limits = WindowLayoutConstraints {
            is_resizable: true,
            locked_width: 700.0,
            locked_height: 0.0,
            min_width: 400.0,
            min_height: 300.0,
            max_width: 0.0,
            max_height: 900.0,
        };
        let slot = slot_limits(limits, 4.0);
        assert_eq!((slot.locked_width, slot.min_width, slot.min_height, slot.max_height), (708.0, 408.0, 308.0, 908.0));
        assert_eq!((slot.locked_height, slot.max_width), (0.0, 0.0), "no lock and no maximum stay none");
    }
}
