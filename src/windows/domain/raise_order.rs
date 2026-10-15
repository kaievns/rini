//! What order windows are raised in, which ones are not worth raising, and when the strip has to be
//! raised back in front of the windows off it (`regroup_tiled`).
//!
//! Raising is last-wins: the window server puts each raised window in front of the ones raised
//! before it. So the order of a raise list IS the z-order it produces, and every rule here is about
//! preserving an order that was decided upstream.

use std::hash::Hash;

use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;

/// Drop the windows a scrolling layout has parked off the strip.
///
/// A parked column sits just past the screen edge, leaving a 1pt sliver — macOS refuses to place a
/// window entirely outside every display. Raising a sliver costs an Accessibility round-trip and
/// shows nothing, and a sliver raised after the on-screen windows sits in FRONT of them.
///
/// An earlier attempt sorted parked windows to the front instead, on the theory that last-wins would
/// leave them behind. It flickered: the first entry of a raise list is not just the first raise, it
/// is the PRIMARY window, used for make-key, the `is_standard` check and the activation wait. Sorting
/// a parked window into that slot made macOS activate an invisible window and then raise the real one.
///
/// If EVERY window is parked the list is kept as it is. An empty raise list takes a different path in
/// the caller — it can skip the raise and the focus entirely — and the caller asked for these windows
/// for a reason, such as a workspace switch whose layout has not been applied yet.
pub fn drop_parked(windows: Vec<WindowId>, is_parked: impl Fn(WindowId) -> bool) -> Vec<WindowId> {
    if windows.iter().all(|window| is_parked(*window)) {
        return windows;
    }
    windows.into_iter().filter(|window| !is_parked(*window)).collect()
}

/// Put the strip regroup at the front in its own order, and keep everything else behind it.
///
/// The regroup is the whole strip lifted over a floating window that got in front of it. Its order
/// is load-bearing — back to front, focused last — so it leads, and whatever else the layout asked
/// for follows without being duplicated.
pub fn lead_with_regroup(raise: Vec<WindowId>, regroup: &[WindowId]) -> Vec<WindowId> {
    if regroup.is_empty() {
        return raise;
    }
    let mut ordered = regroup.to_vec();
    ordered.extend(raise.into_iter().filter(|window| !regroup.contains(window)));
    ordered
}

/// Windows of one application, raised in order by its own Accessibility thread. A window of another
/// application never joins one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppBatch {
    pid: pid_t,
    windows: Vec<WindowId>,
}

impl AppBatch {
    pub fn new(first: WindowId) -> Self {
        Self {
            pid: first.pid,
            windows: vec![first],
        }
    }

    /// Adds `window` last, or answers false and leaves the batch as it was when another application
    /// owns it.
    pub fn push(&mut self, window: WindowId) -> bool {
        let joins = window.pid == self.pid;
        if joins {
            self.windows.push(window);
        }
        joins
    }

    pub fn pid(&self) -> pid_t { self.pid }

    pub fn windows(&self) -> &[WindowId] { &self.windows }

    pub fn into_windows(self) -> Vec<WindowId> { self.windows }

    #[cfg(test)]
    pub fn of(windows: &[WindowId]) -> Self {
        let (&first, rest) = windows.split_first().expect("a batch has a window");
        let mut batch = Self::new(first);
        for &window in rest {
            assert!(batch.push(window), "{window:?} is another application's");
        }
        batch
    }
}

/// Batch the raise list by application and space, keeping the order the list arrived in.
///
/// One batch per `(pid, space)` because each application raises its own windows through its own
/// Accessibility thread, and a batch crossing spaces would ask one app to raise windows the user
/// cannot see.
///
/// The batches come back in the order their first window appeared. This is the point of the
/// function: the caller has already decided an order — `lead_with_regroup` put the strip first — and
/// grouping through a hash map would hand the batches back in hash order and throw that away.
pub fn group_by_app_and_space<S: Eq + Hash + Copy>(
    windows: &[WindowId],
    space_of: impl Fn(WindowId) -> Option<S>,
) -> Vec<AppBatch> {
    let mut batches: Vec<AppBatch> = Vec::new();
    let mut seen: HashMap<(pid_t, Option<S>), usize> = HashMap::default();
    for &window in windows {
        let key = (window.pid, space_of(window));
        match seen.get(&key) {
            Some(&index) => {
                batches[index].push(window);
            }
            None => {
                seen.insert(key, batches.len());
                batches.push(AppBatch::new(window));
            }
        }
    }
    batches
}

/// Which kind of window this is: on the strip or off it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StackGroup {
    /// Part of the tiled surface, which moves as one.
    Tiled,
    /// Off the strip: floating, or otherwise not part of the scrolling surface.
    Floating,
}

impl StackGroup {
    pub fn of(floating: bool) -> Self {
        if floating {
            StackGroup::Floating
        } else {
            StackGroup::Tiled
        }
    }
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
    use StackGroup::{Floating, Tiled};

    use super::*;

    fn window(pid: pid_t, idx: u32) -> WindowId { WindowId::new(pid, idx) }

    #[test]
    fn parked_windows_are_dropped() {
        let windows = vec![window(1, 1), window(1, 2), window(1, 3)];
        let parked = [window(1, 2)];
        let kept = drop_parked(windows, |w| parked.contains(&w));
        assert_eq!(kept, [window(1, 1), window(1, 3)]);
    }

    /// The guard: an empty raise list is a different path in the caller, so a list that is entirely
    /// parked is passed through rather than emptied.
    #[test]
    fn a_list_that_is_entirely_parked_is_kept_as_it_is() {
        let windows = vec![window(1, 1), window(1, 2)];
        assert_eq!(drop_parked(windows.clone(), |_| true), windows);
    }

    #[test]
    fn dropping_parked_windows_preserves_the_order_of_the_rest() {
        let windows = vec![window(1, 4), window(1, 1), window(1, 3), window(1, 2)];
        let parked = [window(1, 3)];
        let kept = drop_parked(windows, |w| parked.contains(&w));
        assert_eq!(kept, [window(1, 4), window(1, 1), window(1, 2)]);
    }

    #[test]
    fn the_regroup_leads_and_the_rest_follows() {
        let raise = vec![window(1, 9), window(1, 2)];
        let regroup = [window(1, 1), window(1, 2), window(1, 3)];
        assert_eq!(
            lead_with_regroup(raise, &regroup),
            vec![window(1, 1), window(1, 2), window(1, 3), window(1, 9)],
            "the regroup keeps its own order, and window 2 is not raised twice"
        );
    }

    #[test]
    fn an_empty_regroup_leaves_the_raise_list_alone() {
        let raise = vec![window(1, 9), window(1, 2)];
        assert_eq!(lead_with_regroup(raise.clone(), &[]), raise);
    }

    #[test]
    fn windows_of_one_app_on_one_space_are_one_batch() {
        let windows = [window(1, 1), window(1, 2)];
        let batches = group_by_app_and_space(&windows, |_| Some(7));
        assert_eq!(batches, [AppBatch::of(&[window(1, 1), window(1, 2)])]);
    }

    #[test]
    fn one_app_on_two_spaces_is_two_batches() {
        let windows = [window(1, 1), window(1, 2)];
        let batches = group_by_app_and_space(&windows, |w| Some(w.idx.get()));
        assert_eq!(batches, [
            AppBatch::new(window(1, 1)),
            AppBatch::new(window(1, 2))
        ]);
    }

    /// A window whose space is unknown batches with the other unknowns rather than with everything.
    #[test]
    fn an_unknown_space_is_its_own_key() {
        let windows = [window(1, 1), window(1, 2), window(1, 3)];
        let batches = group_by_app_and_space(&windows, |w| (w.idx.get() != 2).then_some(7));
        let known = AppBatch::of(&[window(1, 1), window(1, 3)]);
        assert_eq!(batches, [known, AppBatch::new(window(1, 2))]);
    }

    /// The reason this is a function. Raising is last-wins, so the caller's order IS the z-order it
    /// asked for. Grouping through a hash map returned the batches in hash order, which silently
    /// undid the strip regroup that `lead_with_regroup` had just produced.
    #[test]
    fn the_batches_come_back_in_the_order_their_first_window_appeared() {
        let windows: Vec<WindowId> = (1..=8).map(|pid| window(pid, 1)).collect();
        let batches = group_by_app_and_space(&windows, |_| Some(7));
        let leaders: Vec<pid_t> = batches.iter().map(|batch| batch.windows()[0].pid).collect();
        assert_eq!(leaders, (1..=8).collect::<Vec<pid_t>>());
    }

    #[test]
    fn windows_of_two_apps_on_one_space_are_a_batch_each() {
        let windows = [window(1, 1), window(2, 1), window(1, 2), window(2, 2)];
        let batches = group_by_app_and_space(&windows, |_| Some(7));
        assert_eq!(batches, [
            AppBatch::of(&[window(1, 1), window(1, 2)]),
            AppBatch::of(&[window(2, 1), window(2, 2)])
        ]);
        assert!(batches.iter().all(|batch| batch.windows().iter().all(|w| w.pid == batch.pid())));
    }

    #[test]
    fn another_apps_window_does_not_join_a_batch() {
        let mut batch = AppBatch::new(window(1, 1));
        assert!(!batch.push(window(2, 1)));
        assert!(batch.push(window(1, 2)));
        assert_eq!(batch.pid(), 1);
        assert_eq!(batch.into_windows(), [window(1, 1), window(1, 2)]);
    }

    #[test]
    fn nothing_to_batch_is_no_batches() {
        assert!(group_by_app_and_space(&[], |_| Some(7)).is_empty());
    }

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
