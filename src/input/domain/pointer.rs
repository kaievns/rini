//! When the pointer is worth looking at, and when the last answer still stands.
//!
//! The session tap sees every mouse move the hardware produces. Two rules keep that from becoming
//! a window-server query per event, and both used to live inside the tap's callback where nothing
//! could reach them — in the file with the worst code-to-test ratio in the tree.

use rini_core::ids::WindowServerId;

/// Whether a mouse move is far enough from the last one to process.
///
/// `now` and `last` are Core Graphics event timestamps, which are monotonic nanoseconds. The first
/// move of a session has no predecessor and is always admitted.
///
/// Saturating on purpose: a timestamp going backwards would otherwise wrap to an enormous interval
/// and admit everything. Core Graphics is monotonic, so this is belt rather than braces, but the
/// failure it prevents is "every hardware move becomes a window-server query".
pub fn admits_move(last: Option<u64>, now: u64, min_interval: u64) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_sub(last) >= min_interval,
    }
}

/// The last window the pointer was resolved to, and the event hint it was resolved from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PointerCache {
    /// Whether anything has been cached yet.
    pub valid: bool,
    /// The `CGEvent` window hint the cached answer came from.
    pub hint: Option<WindowServerId>,
    /// What that hint resolved to. `None` is a real answer: the pointer was over no window.
    pub resolved: Option<WindowServerId>,
}

/// Whether the cached pointer window still answers for this event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerWindow {
    /// Reuse this, which may be "over no window".
    Cached(Option<WindowServerId>),
    /// Ask the window server.
    NeedsLookup,
}

/// Reuse the cached answer only when the event carries the SAME non-empty hint.
///
/// A `CGEvent`'s window hint is stable while the pointer stays in one window, so an unchanged hint
/// means an unchanged answer. An ABSENT hint proves nothing: the pointer can cross windows without
/// one appearing, so the cache cannot be trusted and the lookup has to run.
pub fn pointer_window(cache: PointerCache, hint: Option<WindowServerId>) -> PointerWindow {
    if cache.valid && hint.is_some() && cache.hint == hint {
        PointerWindow::Cached(cache.resolved)
    } else {
        PointerWindow::NeedsLookup
    }
}

/// Whether the tap should ask for keyboard events at all.
///
/// Only when something would act on them: a disable hotkey, or any bound hotkey. An active tap sits
/// in the delivery path — the window server holds each matching event until the callback answers —
/// so asking for keys nobody is listening for puts rini between the user and every keystroke for
/// nothing.
pub fn wants_keyboard_events(has_disable_hotkey: bool, bound_hotkeys: usize) -> bool {
    has_disable_hotkey || bound_hotkeys > 0
}

/// Whether the tap should ask for mouse-move events.
///
/// Needs both halves of focus-follows-mouse: the setting, and the runtime flag that suppresses it
/// during a drag or an animation. Event processing being off overrules both.
pub fn wants_mouse_move_events(
    event_processing_enabled: bool,
    focus_follows_mouse_configured: bool,
    focus_follows_mouse_enabled: bool,
) -> bool {
    event_processing_enabled && focus_follows_mouse_configured && focus_follows_mouse_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: u64 = 8_000_000; // 8ms, about 120Hz

    #[test]
    fn the_first_move_of_a_session_is_always_admitted() {
        assert!(admits_move(None, 0, INTERVAL));
        assert!(admits_move(None, 12_345, INTERVAL));
    }

    #[test]
    fn a_move_inside_the_interval_is_dropped_and_one_at_the_boundary_is_not() {
        assert!(!admits_move(Some(1_000), 1_000 + INTERVAL - 1, INTERVAL));
        assert!(admits_move(Some(1_000), 1_000 + INTERVAL, INTERVAL));
    }

    /// A timestamp going backwards must not wrap into an enormous interval and admit everything.
    #[test]
    fn a_timestamp_going_backwards_is_not_admitted() {
        assert!(!admits_move(Some(1_000_000), 5, INTERVAL));
    }

    #[test]
    fn a_zero_interval_admits_everything() {
        assert!(admits_move(Some(1_000), 1_000, 0));
    }

    fn cached(hint: u32, resolved: Option<u32>) -> PointerCache {
        PointerCache {
            valid: true,
            hint: Some(WindowServerId::new(hint)),
            resolved: resolved.map(WindowServerId::new),
        }
    }

    #[test]
    fn the_same_hint_reuses_the_cached_window() {
        let cache = cached(7, Some(42));
        assert_eq!(
            pointer_window(cache, Some(WindowServerId::new(7))),
            PointerWindow::Cached(Some(WindowServerId::new(42)))
        );
    }

    /// "Over no window" is a real cached answer, not a cache miss.
    #[test]
    fn a_cached_answer_of_no_window_is_still_an_answer() {
        assert_eq!(
            pointer_window(cached(7, None), Some(WindowServerId::new(7))),
            PointerWindow::Cached(None)
        );
    }

    #[test]
    fn a_different_hint_needs_a_lookup() {
        assert_eq!(
            pointer_window(cached(7, Some(42)), Some(WindowServerId::new(8))),
            PointerWindow::NeedsLookup
        );
    }

    /// The rule that keeps this correct rather than merely fast. An absent hint proves nothing: the
    /// pointer can cross windows without one appearing, so the cache cannot be trusted.
    #[test]
    fn an_absent_hint_needs_a_lookup_even_with_a_valid_cache() {
        assert_eq!(
            pointer_window(cached(7, Some(42)), None),
            PointerWindow::NeedsLookup
        );
    }

    #[test]
    fn an_empty_cache_needs_a_lookup() {
        assert_eq!(
            pointer_window(PointerCache::default(), Some(WindowServerId::new(7))),
            PointerWindow::NeedsLookup
        );
    }

    /// The rule that keeps rini out of the keyboard path when nothing is bound.
    #[test]
    fn keyboard_events_are_only_wanted_when_something_would_act_on_them() {
        assert!(
            !wants_keyboard_events(false, 0),
            "no bindings, no reason to see keys"
        );
        assert!(wants_keyboard_events(false, 1));
        assert!(
            wants_keyboard_events(true, 0),
            "the disable hotkey is reason enough"
        );
    }

    #[test]
    fn mouse_moves_need_the_setting_and_the_runtime_flag_and_processing() {
        assert!(wants_mouse_move_events(true, true, true));
        assert!(
            !wants_mouse_move_events(false, true, true),
            "processing off overrules both"
        );
        assert!(!wants_mouse_move_events(true, false, true), "not configured");
        assert!(
            !wants_mouse_move_events(true, true, false),
            "suppressed at runtime, during a drag or an animation"
        );
    }
}
