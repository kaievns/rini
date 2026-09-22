//! What order windows are raised in, and which ones are not worth raising.
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
) -> Vec<Vec<WindowId>> {
    let mut batches: Vec<Vec<WindowId>> = Vec::new();
    let mut seen: HashMap<(pid_t, Option<S>), usize> = HashMap::default();
    for &window in windows {
        let key = (window.pid, space_of(window));
        match seen.get(&key) {
            Some(&index) => batches[index].push(window),
            None => {
                seen.insert(key, batches.len());
                batches.push(vec![window]);
            }
        }
    }
    batches
}

#[cfg(test)]
mod tests {
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
        assert_eq!(batches, [vec![window(1, 1), window(1, 2)]]);
    }

    #[test]
    fn one_app_on_two_spaces_is_two_batches() {
        let windows = [window(1, 1), window(1, 2)];
        let batches = group_by_app_and_space(&windows, |w| Some(w.idx.get()));
        assert_eq!(batches, vec![vec![window(1, 1)], vec![window(1, 2)]]);
    }

    /// A window whose space is unknown batches with the other unknowns rather than with everything.
    #[test]
    fn an_unknown_space_is_its_own_key() {
        let windows = [window(1, 1), window(1, 2), window(1, 3)];
        let batches = group_by_app_and_space(&windows, |w| (w.idx.get() != 2).then_some(7));
        let known = vec![window(1, 1), window(1, 3)];
        assert_eq!(batches, [known, vec![window(1, 2)]]);
    }

    /// The reason this is a function. Raising is last-wins, so the caller's order IS the z-order it
    /// asked for. Grouping through a hash map returned the batches in hash order, which silently
    /// undid the strip regroup that `lead_with_regroup` had just produced.
    #[test]
    fn the_batches_come_back_in_the_order_their_first_window_appeared() {
        let windows: Vec<WindowId> = (1..=8).map(|pid| window(pid, 1)).collect();
        let batches = group_by_app_and_space(&windows, |_| Some(7));
        let leaders: Vec<pid_t> = batches.iter().map(|batch| batch[0].pid).collect();
        assert_eq!(leaders, (1..=8).collect::<Vec<pid_t>>());
    }

    #[test]
    fn nothing_to_batch_is_no_batches() {
        assert!(group_by_app_and_space(&[], |_| Some(7)).is_empty());
    }
}
