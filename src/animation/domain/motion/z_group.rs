//! Windows come forward in SETS, and there are two kinds of set.
//!
//! The strip is one. A scrolling workspace is a single surface, so its windows belong together in
//! front-to-back order as well as in position: focusing any window on the strip brings the whole strip
//! in front of everything not on it.
//!
//! An APPLICATION is the other. Focusing one window of a multi-window application brings that
//! application's windows forward together, which is what macOS itself does — raising a window activates
//! its application, and activating an application raises its windows as a set. A zoom call is a
//! meeting window and its controls; an editor with a modal is two windows. Focusing one off-strip window
//! therefore lifts ITS application in front of the strip, and leaves every other application where it
//! was.
//!
//! What macOS does NOT have is the first notion. It raises the one window that was clicked, which leaves
//! a window from another application sandwiched between two columns that sit side by side on screen — so
//! one half of a 50/50 pair is in front of it and the other half behind.
//!
//! So a flight draws its windows in three bands, `Band`, and the strip is always the middle one. See
//! "Which windows come forward together" in `specs/animation.md`.
//!
//! The strip rule decides two different things: which containers the animation overlay draws in front
//! (`container_z`), and which real windows have to be raised to put the order back (`regroup_tiled`).

use rini_core::ids::WindowId;

/// Which kind of window this is: on the strip or off it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StackGroup {
    /// Part of the tiled surface, which moves as one.
    Tiled,
    /// Off the strip: floating, or otherwise not part of the scrolling surface.
    Floating,
}

/// Where a window is drawn for one flight, relative to the strip.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Band {
    /// Off the strip and in front of it: the application gaining focus, and any off-strip window the
    /// window server already has in front of the strip. Only occupied while focus is off the strip.
    Lifted,
    /// The strip.
    Strip,
    /// Off the strip and behind it.
    Behind,
}

/// Room for every window of one band before the next band starts, so no member of the band behind can
/// ever be drawn in front of a member of the band in front.
///
/// Public because the overlay derives its backdrop depth from it: the deepest possible tile is just
/// short of three strides, and the backdrop has to sit behind THAT, not behind some smaller constant.
/// The floating tiles used to land at zPosition about -(1<<20) while the backdrop sat at -10000, so
/// every floating tile was drawn behind the desktop picture — present in every composition and visible
/// in none.
pub const GROUP_STRIDE: usize = 1 << 20;

/// Inside the lifted band, the application gaining focus takes the front half and every other lifted
/// window the back half. The server has not raised the application yet when a flight starts, so its
/// own order can still put another application's window between two of its windows.
const APP_SPAN: usize = GROUP_STRIDE / 2;

/// The deepest depth `stack` can produce: the unreported-window fallback of the band behind the strip.
pub const MAX_TILE_DEPTH: usize = 3 * GROUP_STRIDE - 1;

/// What the z rule needs to know about one window.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Stacked {
    pub window: WindowId,
    pub group: StackGroup,
    /// The window server's front-to-back position, 0 frontmost; `None` when unreported.
    pub server_order: Option<usize>,
}

/// Where one window is drawn.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Placement {
    pub band: Band,
    /// Front-to-back position inside its band, 0 frontmost.
    pub within: usize,
    /// Front-to-back position across the whole flight, 0 frontmost: the band's offset plus `within`.
    pub depth: usize,
}

/// Whether the window gaining focus is off the strip, which is the only time anything is lifted.
///
/// A focus the flight does not draw counts as on the strip: nothing it would lift is known.
pub fn focus_is_off_strip(windows: &[Stacked], focus: Option<WindowId>) -> bool {
    focus.is_some_and(|focus| {
        windows.iter().any(|w| w.window == focus && w.group == StackGroup::Floating)
    })
}

/// Every window's placement, in the order `windows` gives them.
///
/// The strip is the middle band. Off-strip windows go behind it, except while focus is off the strip:
/// then the focused window's whole APPLICATION is lifted in front of it, and so is any other off-strip
/// window the server already has in front of the strip — which is where it stays when the flight lands,
/// because macOS raises only the application gaining focus.
///
/// Within a band the window server's own order is kept, since that is right for windows that really do
/// overlap. A window the server did not report sorts to the back of its own band rather than the back
/// of everything: a tile drawn too far back inside its band is invisible, while one drawn in the wrong
/// band is the bug this exists to prevent.
pub fn stack(windows: &[Stacked], focus: Option<WindowId>) -> Vec<Placement> {
    let off_strip = focus_is_off_strip(windows, focus);
    let strip_front = windows
        .iter()
        .filter(|w| w.group == StackGroup::Tiled)
        .filter_map(|w| w.server_order)
        .min();
    windows
        .iter()
        .map(|w| {
            let focused_app = focus.is_some_and(|focus| focus.pid == w.window.pid);
            let band = match w.group {
                StackGroup::Tiled => Band::Strip,
                StackGroup::Floating if !off_strip => Band::Behind,
                StackGroup::Floating => {
                    let in_front_of_strip =
                        w.server_order.zip(strip_front).is_some_and(|(order, front)| order < front);
                    if focused_app || in_front_of_strip {
                        Band::Lifted
                    } else {
                        Band::Behind
                    }
                }
            };
            let within = if focus == Some(w.window) {
                0
            } else if band == Band::Lifted {
                if focused_app {
                    order_in(w.server_order, APP_SPAN - 1)
                } else {
                    APP_SPAN + order_in(w.server_order, APP_SPAN - 1)
                }
            } else {
                order_in(w.server_order, GROUP_STRIDE - 1)
            };
            let depth = band_offset(band, off_strip) * GROUP_STRIDE + within;
            Placement { band, within, depth }
        })
        .collect()
}

/// A band's zPosition among its siblings in the overlay. With each tile at `-within` inside its
/// container, `container_z - within` is `-depth`, so containers band the way `stack` does. See "The
/// overlay engine" in `src/animation/docs/animation-smoothness.md`.
pub fn container_z(band: Band, focus_off_strip: bool) -> f64 {
    -((band_offset(band, focus_off_strip) * GROUP_STRIDE) as f64)
}

/// How many strides in front of a band are occupied. The lifted band is only ever occupied while focus
/// is off the strip, so with focus on the strip the strip leads and nothing is spent on an empty band.
fn band_offset(band: Band, focus_off_strip: bool) -> usize {
    match (band, focus_off_strip) {
        (Band::Lifted, _) => 0,
        (Band::Strip, false) => 0,
        (Band::Strip, true) => 1,
        (Band::Behind, false) => 1,
        (Band::Behind, true) => 2,
    }
}

/// One past the server's order, so 0 stays free for the window gaining focus; `cap` for a window the
/// server did not report. Saturating: the server's order is untrusted input, and `usize::MAX + 1` is a
/// debug-build abort for a value that only needed to mean "the back of the band".
fn order_in(server_order: Option<usize>, cap: usize) -> usize {
    server_order.map(|order| order.saturating_add(1)).unwrap_or(cap).min(cap)
}

/// Whether the real window order breaks the rule, given the groups front to back.
///
/// Broken means a floating window is INSIDE the strip: something on the strip in front of it and
/// something on the strip behind it. The strip is one group and cannot have a hole in it.
///
/// A floating window in front of the whole strip is not broken, it is the point of floating. That
/// distinction is the fix for a window vanishing the moment it opened: macOS raises a new window, so
/// it is frontmost with every strip window behind it, and the older rule — "broken as soon as
/// something off the strip sits in front of something on it" — called that broken and raised the strip
/// back over a window the user had just asked for.
///
/// Checked before doing anything about it, because putting the order back costs one Accessibility
/// raise per window on screen, and a click landing on an order that is already grouped should cost
/// nothing.
pub fn tiled_is_behind(front_to_back: &[StackGroup]) -> bool {
    let Some(floating) = front_to_back.iter().position(|group| *group == StackGroup::Floating)
    else {
        return false;
    };
    let strip_in_front = front_to_back[..floating].contains(&StackGroup::Tiled);
    let strip_behind = front_to_back[floating..].contains(&StackGroup::Tiled);
    strip_in_front && strip_behind
}

/// The windows to raise to put the strip back in front, back to front.
///
/// Empty when the order already obeys the rule, which is the common case: putting it back costs one
/// Accessibility raise per window on screen, so it is only worth doing when something is actually wrong.
///
/// Back to front because everything raised in one sequence ends up in front of everything not raised, and
/// within the sequence the last one raised is the frontmost. The windows that are NOT on the strip are left
/// out entirely rather than raised first: their order relative to each other is not this rule's business,
/// and leaving them alone is what puts them behind.
pub fn regroup_tiled<T: Copy>(front_to_back: &[(T, StackGroup)]) -> Vec<T> {
    let groups: Vec<StackGroup> = front_to_back.iter().map(|(_, group)| *group).collect();
    if !tiled_is_behind(&groups) {
        return Vec::new();
    }
    front_to_back
        .iter()
        .rev()
        .filter(|(_, group)| *group == StackGroup::Tiled)
        .map(|(window, _)| *window)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::StackGroup::{Floating, Tiled};
    use super::*;

    /// The measured order after clicking the left half of a 50/50 pair: the clicked terminal, then the
    /// floating Settings window, then the rest of the strip. Every strip window has to be raised, and the
    /// clicked one has to end up last so it stays in front.
    #[test]
    fn regrouping_raises_the_whole_strip_back_to_front() {
        let order = [(90, Tiled), (5830, Floating), (89, Tiled), (91, Tiled)];
        assert_eq!(regroup_tiled(&order), vec![91, 89, 90]);
    }

    #[test]
    fn an_order_that_already_obeys_the_rule_is_left_alone() {
        assert!(regroup_tiled(&[(90, Tiled), (91, Tiled), (5830, Floating)]).is_empty());
        assert!(regroup_tiled(&[(90, Tiled), (91, Tiled)]).is_empty());
        assert!(regroup_tiled::<i32>(&[]).is_empty());
    }

    /// The floating windows are left out rather than raised first. Raising them in the same sequence would
    /// leave their order against the strip up to whichever app answered first.
    ///
    /// The order has to be genuinely broken for there to be anything to raise, so the strip is on both
    /// sides of the floating pair here.
    #[test]
    fn regrouping_never_raises_a_window_off_the_strip() {
        let order = [(90, Tiled), (5830, Floating), (1350, Floating), (91, Tiled)];
        assert_eq!(regroup_tiled(&order), vec![91, 90]);
    }

    /// The measured case: clicking the left half of a 50/50 pair left the floating Settings window between
    /// the two terminals, in front of one and behind the other.
    ///
    /// This test used to assert `[Floating, Tiled]` was broken too, generalising from the measured
    /// sandwich to "a floating window in front of ANY strip window". That generalisation was the bug:
    /// a newly opened window is frontmost with the whole strip behind it, so it matched, and the strip
    /// was raised back over a window the user had just opened.
    #[test]
    fn a_floating_window_inside_the_strip_breaks_the_rule() {
        assert!(tiled_is_behind(&[Tiled, Floating, Tiled, Tiled]));
        assert!(tiled_is_behind(&[Tiled, Tiled, Floating, Tiled]));
    }

    #[test]
    fn the_whole_strip_in_front_of_the_floating_windows_is_the_rule_kept() {
        assert!(!tiled_is_behind(&[Tiled, Tiled, Tiled, Floating, Floating]));
        assert!(!tiled_is_behind(&[Tiled, Floating]));
    }

    #[test]
    fn an_order_with_only_one_kind_of_window_is_never_broken() {
        assert!(!tiled_is_behind(&[Tiled, Tiled, Tiled]));
        assert!(!tiled_is_behind(&[Floating, Floating]));
        assert!(!tiled_is_behind(&[]));
    }

    fn wid(pid: i32, idx: u32) -> WindowId {
        WindowId::new(pid, idx)
    }

    fn strip(window: WindowId, order: Option<usize>) -> Stacked {
        Stacked {
            window,
            group: Tiled,
            server_order: order,
        }
    }

    fn off(window: WindowId, order: Option<usize>) -> Stacked {
        Stacked {
            window,
            group: Floating,
            server_order: order,
        }
    }

    fn placed(windows: &[Stacked], focus: WindowId) -> Vec<Placement> {
        stack(windows, Some(focus))
    }

    /// Focusing either half of a 50/50 pair has to lift BOTH of them over the floating window, which is
    /// the whole point: they sit side by side on screen and cannot be on opposite sides of it. Even a
    /// floating window the server had in front of the strip goes behind it.
    #[test]
    fn focusing_one_strip_window_puts_the_whole_strip_in_front() {
        let (left, right, settings) = (wid(10, 1), wid(11, 2), wid(12, 3));
        let windows = [
            strip(left, Some(2)),
            strip(right, Some(3)),
            off(settings, Some(0)),
        ];
        let p = placed(&windows, left);

        assert_eq!(p[0].depth, 0, "the focused window leads");
        assert!(p[0].depth < p[1].depth, "then its partner");
        assert!(p[1].depth < p[2].depth, "and the floating window behind both");
        assert_eq!(p[2].band, Band::Behind);
    }

    /// The reported bug. Switching to a floating 1Password window drew zoom, another off-strip window,
    /// in front of the strip for the length of the flight — and the screen landed with zoom BEHIND the
    /// strip, because macOS raises only the application gaining focus.
    ///
    /// The server order as the flight started: the strip in front, zoom and 1Password behind it.
    #[test]
    fn focusing_one_application_does_not_lift_another() {
        let (column, onepassword, zoom) = (wid(10, 1), wid(20, 2), wid(30, 3));
        let windows = [
            strip(column, Some(0)),
            off(onepassword, Some(5)),
            off(zoom, Some(4)),
        ];
        let p = placed(&windows, onepassword);

        assert_eq!(p[1].band, Band::Lifted);
        assert_eq!(p[0].band, Band::Strip);
        assert_eq!(p[2].band, Band::Behind, "zoom stays behind the strip");
        assert!(p[1].depth < p[0].depth && p[0].depth < p[2].depth);
    }

    /// The use case the rule is for: a zoom call is a meeting window and its controls, and focusing
    /// either one brings both in front of the strip — including a window the server still has behind it.
    #[test]
    fn focusing_one_window_lifts_its_whole_application() {
        let (column, meeting, controls) = (wid(10, 1), wid(30, 2), wid(30, 3));
        let windows = [
            strip(column, Some(0)),
            off(meeting, Some(1)),
            off(controls, Some(2)),
        ];
        let p = placed(&windows, controls);

        assert_eq!(p[1].band, Band::Lifted, "the meeting comes with its controls");
        assert_eq!(p[2].band, Band::Lifted);
        assert_eq!(p[2].depth, 0, "the focused one leads its application");
        assert!(p[1].depth < p[0].depth, "and both are in front of the strip");
    }

    /// An off-strip window already in front of the strip stays there: macOS raises the application
    /// gaining focus over it, and does not push it behind the strip. It lands between the two.
    #[test]
    fn a_window_already_in_front_of_the_strip_stays_in_front_of_it() {
        let (column, zoom, onepassword) = (wid(10, 1), wid(30, 2), wid(20, 3));
        let windows = [
            off(zoom, Some(0)),
            strip(column, Some(1)),
            off(onepassword, Some(4)),
        ];
        let p = placed(&windows, onepassword);

        assert_eq!(p[0].band, Band::Lifted);
        assert!(p[2].depth < p[0].depth, "1Password in front of zoom");
        assert!(p[0].depth < p[1].depth, "and zoom still in front of the strip");
    }

    /// The server has not raised the application yet when the flight starts, so another application's
    /// window can sit between two of its windows. The application gaining focus leads its band whatever
    /// the server's order says, because that is the order it lands in.
    #[test]
    fn the_focused_application_leads_the_lifted_band() {
        let (column, zoom, meeting, controls) = (wid(10, 1), wid(30, 2), wid(40, 3), wid(40, 4));
        let windows = [
            off(zoom, Some(0)),
            strip(column, Some(1)),
            off(meeting, Some(2)),
            off(controls, Some(7)),
        ];
        let p = placed(&windows, meeting);

        assert!(p[3].depth < p[0].depth, "its controls in front of zoom");
    }

    #[test]
    fn within_a_band_the_window_servers_order_is_kept() {
        let (focus, a, b, c) = (wid(10, 1), wid(11, 2), wid(12, 3), wid(13, 4));
        let windows = [
            strip(focus, Some(0)),
            strip(a, Some(1)),
            strip(b, Some(17)),
            off(c, Some(0)),
        ];
        let p = placed(&windows, focus);
        assert!(p[1].depth < p[2].depth);
    }

    /// A window the server did not report must not fall out of its band: behind its own kind, still in
    /// front of the band that is meant to be behind.
    #[test]
    fn an_unreported_window_stays_inside_its_own_band() {
        let (focus, known, unknown, floating) = (wid(10, 1), wid(11, 2), wid(12, 3), wid(13, 4));
        let windows = [
            strip(focus, Some(0)),
            strip(known, Some(50)),
            strip(unknown, None),
            off(floating, Some(0)),
        ];
        let p = placed(&windows, focus);
        assert!(
            p[1].depth < p[2].depth,
            "behind the windows the server did report"
        );
        assert!(p[2].depth < p[3].depth, "but still in front of the band behind");
    }

    /// The stride has to outrun any plausible window count, or a deep window in one band would wrap past
    /// a shallow one in the next and the banding would silently invert.
    #[test]
    fn no_window_count_can_make_the_bands_overlap() {
        let (column, deep, onepassword, zoom) = (wid(10, 1), wid(11, 2), wid(20, 3), wid(30, 4));
        let windows = [
            strip(column, Some(0)),
            strip(deep, Some(usize::MAX)),
            off(onepassword, Some(usize::MAX)),
            off(zoom, Some(1)),
        ];
        let p = placed(&windows, onepassword);
        assert!(
            p[1].depth < p[3].depth,
            "the deepest strip window beats the band behind"
        );
        for placement in &p {
            assert!(placement.depth <= MAX_TILE_DEPTH);
        }
    }

    /// `container_z - within` is `-depth` for every band and either focus, which is the invariant the
    /// overlay relies on to put a tile inside its container.
    #[test]
    fn containers_band_exactly_as_windows_do() {
        let (column, onepassword, zoom, settings) =
            (wid(10, 1), wid(20, 2), wid(30, 3), wid(40, 4));
        let windows = [
            off(settings, Some(0)),
            strip(column, Some(1)),
            off(onepassword, Some(5)),
            off(zoom, Some(4)),
        ];
        for focus in [column, onepassword] {
            let off_strip = focus_is_off_strip(&windows, Some(focus));
            for p in placed(&windows, focus) {
                assert_eq!(
                    container_z(p.band, off_strip) - p.within as f64,
                    -(p.depth as f64),
                    "{p:?} with {focus:?} focused"
                );
            }
        }
    }

    /// With focus on the strip nothing is lifted, so the strip leads at zero and nothing is spent on an
    /// empty band in front of it.
    #[test]
    fn with_focus_on_the_strip_the_strip_leads_at_zero() {
        assert_eq!(container_z(Band::Strip, false), 0.0);
        assert_eq!(container_z(Band::Behind, false), -(GROUP_STRIDE as f64));
        assert_eq!(container_z(Band::Lifted, true), 0.0);
        assert_eq!(container_z(Band::Strip, true), -(GROUP_STRIDE as f64));
        assert_eq!(container_z(Band::Behind, true), -(2.0 * GROUP_STRIDE as f64));
    }

    /// A focus the flight does not draw lifts nothing: there is no application to lift.
    #[test]
    fn a_focus_outside_the_flight_lifts_nothing() {
        let (column, zoom) = (wid(10, 1), wid(30, 2));
        let windows = [strip(column, Some(1)), off(zoom, Some(0))];
        let p = stack(&windows, Some(wid(99, 9)));
        assert_eq!(p[1].band, Band::Behind);
        assert!(!focus_is_off_strip(&windows, Some(wid(99, 9))));
        assert!(!focus_is_off_strip(&windows, None));
    }

    /// A floating window in FRONT of the whole strip is the wanted state, not a broken order.
    ///
    /// This is what a window that has just opened looks like: macOS raises it, so it is frontmost and
    /// every strip window is behind it. Calling that broken raises the strip over a window the user
    /// just asked for, and it disappears behind the columns a moment after appearing.
    #[test]
    fn a_floating_window_in_front_of_the_whole_strip_is_not_broken() {
        assert!(!tiled_is_behind(&[Floating, Tiled, Tiled]));
        assert!(regroup_tiled(&[(5830, Floating), (90, Tiled), (91, Tiled)]).is_empty());
    }

    /// Two floating windows in front of the strip: still the wanted state.
    #[test]
    fn several_floating_windows_in_front_of_the_strip_are_not_broken() {
        assert!(!tiled_is_behind(&[Floating, Floating, Tiled]));
    }

    /// The order this rule exists for: a floating window BETWEEN two strip windows. The strip is one
    /// group and cannot have something else inside it.
    #[test]
    fn a_floating_window_between_two_strip_windows_is_broken() {
        assert!(tiled_is_behind(&[Tiled, Floating, Tiled]));
    }

    /// Only floating windows, with no strip at all, is nothing to judge.
    #[test]
    fn an_order_with_no_strip_window_is_not_broken() {
        assert!(!tiled_is_behind(&[Floating, Floating]));
    }
}
