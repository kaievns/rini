//! The picture cache and its rules: what an incoming capture replaces, each window's focused look,
//! and what each client was last delivered. Generic over what it holds, so the rules are tested on
//! plain sizes.

use std::collections::HashMap;

use rini_core::ids::WindowId;

use crate::pictures::domain::coverage::{Coverage, should_replace};
use crate::pictures::domain::watch::{Client, Tag};

/// Anything the cache can hold and judge, so the replacement policy can be tested on plain sizes.
pub trait HasCoverage {
    fn coverage(&self) -> Coverage;
}

/// What a cache payload adds to the coverage rule: whether the one held carries over a capture that
/// would replace it, and whether it may be cached at all.
pub trait CarriesOver {
    /// Whether this payload is worth more than `incoming` even though the coverage rule would replace
    /// it.
    fn keeps_over(&self, _incoming: &Self) -> bool { false }
    /// Whether this payload may be cached at all.
    fn cacheable(&self) -> bool { true }
}

/// The rule behind a picture's `keeps_over`, on plain values: a picture carrying its window's blur is
/// kept over a capture of the window on its own at the same size.
pub fn keeps_blur(
    held: bool,
    incoming: bool,
    held_coverage: Coverage,
    incoming_coverage: Coverage,
) -> bool {
    held && !incoming && held_coverage.window == incoming_coverage.window
}

impl HasCoverage for Coverage {
    fn coverage(&self) -> Coverage { *self }
}

impl CarriesOver for Coverage {}

/// Pictures held per window, so a switch can composite without capturing anything synchronously, and
/// each window's last picture taken while it was focused. Keyed by [`WindowId`] because window server
/// ids are recycled when a window closes and reopens.
pub struct PictureCache<T> {
    entries: HashMap<WindowId, T>,
    focused: HashMap<WindowId, T>,
}

impl<T: HasCoverage> Default for PictureCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            focused: HashMap::new(),
        }
    }
}

impl<T: HasCoverage + CarriesOver> PictureCache<T> {
    pub fn new() -> Self { Self::default() }

    /// Stores a picture, subject to [`should_replace`] and to what the one held keeps over it
    /// ([`CarriesOver`]). True when its pixels are now the ones held.
    pub fn insert(&mut self, window: WindowId, picture: T) -> bool {
        if !picture.cacheable() {
            return false;
        }
        if let Some(existing) = self.entries.get(&window)
            && (!should_replace(Some(existing.coverage()), picture.coverage())
                || existing.keeps_over(&picture))
        {
            return false;
        }
        self.entries.insert(window, picture);
        true
    }

    pub fn get(&self, window: WindowId) -> Option<&T> { self.entries.get(&window) }

    /// The picture to actually draw, or `None` if what we hold is not worth drawing.
    pub fn usable(&self, window: WindowId) -> Option<&T> {
        self.entries.get(&window).filter(|s| s.coverage().is_usable())
    }

    /// Puts `with` in place of what is held of `window`, if what is held is still what `still` says:
    /// a copy made off the lock replaces only the picture it was made of, never a newer one.
    pub fn replace_if(&mut self, window: WindowId, still: impl FnOnce(&T) -> bool, with: T) -> bool {
        match self.entries.get_mut(&window) {
            Some(held) if still(held) => {
                *held = with;
                true
            }
            _ => false,
        }
    }

    /// A window that is gone: its picture and its focused look go with it.
    pub fn forget(&mut self, window: WindowId) {
        self.entries.remove(&window);
        self.focused.remove(&window);
    }

    /// `look` shows `window` focused: a transition that flies the window parked draws it so from its
    /// first frame.
    pub fn keep_focused(&mut self, window: WindowId, look: T) { self.focused.insert(window, look); }

    pub fn focused(&self, window: WindowId) -> Option<&T> { self.focused.get(&window) }

    /// Every focused look held.
    pub fn focused_looks(&self) -> impl Iterator<Item = (&WindowId, &T)> { self.focused.iter() }

    /// Drops `window`'s focused look if it is still what `still` says: a newer capture showed
    /// different content, which the look would fly over.
    pub fn drop_focused_if(&mut self, window: WindowId, still: impl FnOnce(&T) -> bool) -> bool {
        let stale = self.focused.get(&window).is_some_and(still);
        if stale {
            self.focused.remove(&window);
        }
        stale
    }

    /// Puts `with` in place of `window`'s focused look, if it is still what `still` says.
    pub fn replace_focused_if(&mut self, window: WindowId, still: impl FnOnce(&T) -> bool, with: T) -> bool {
        match self.focused.get_mut(&window) {
            Some(held) if still(held) => {
                *held = with;
                true
            }
            _ => false,
        }
    }

    /// Every cached entry.
    pub fn iter(&self) -> impl Iterator<Item = (&WindowId, &T)> { self.entries.iter() }
}

/// The picture each client was last delivered of each window, with the tag it came under: what a
/// client names when it says a picture it was delivered shows the window focused.
pub struct Delivered<T> {
    last: HashMap<(Client, WindowId), (Tag, T)>,
}

impl<T> Default for Delivered<T> {
    fn default() -> Self { Self { last: HashMap::new() } }
}

impl<T> Delivered<T> {
    pub fn record(&mut self, tag: Tag, window: WindowId, picture: T) {
        self.last.insert((tag.client, window), (tag, picture));
    }

    /// What `tag`'s client was last delivered of `window`, if it came under `tag`.
    pub fn under(&self, tag: Tag, window: WindowId) -> Option<&T> {
        self.last
            .get(&(tag.client, window))
            .filter(|(held, _)| *held == tag)
            .map(|(_, picture)| picture)
    }

    pub fn forget(&mut self, window: WindowId) { self.last.retain(|(_, held), _| *held != window); }

    /// Every client is done with what it was delivered: what a region holds of its composite goes.
    pub fn clear(&mut self) { self.last.clear(); }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn size(w: f64, h: f64) -> Coverage {
        Coverage {
            covered: (w, h),
            window: (w, h),
        }
    }

    /// A resize means the old picture no longer fits the window, blur or not.
    #[test]
    fn a_resized_window_takes_the_new_picture_even_without_its_blur() {
        assert!(!keeps_blur(
            true,
            false,
            size(859.0, 1081.0),
            size(1720.0, 1081.0)
        ));
        assert!(keeps_blur(true, false, size(859.0, 1081.0), size(859.0, 1081.0)));
    }

    /// A newer picture WITH its blur always replaces an older one, so an on-screen window refreshes.
    #[test]
    fn a_newer_blurred_picture_replaces_an_older_one() {
        assert!(!keeps_blur(true, true, size(859.0, 1081.0), size(859.0, 1081.0)));
        assert!(!keeps_blur(
            false,
            false,
            size(859.0, 1081.0),
            size(859.0, 1081.0)
        ));
    }

    fn wid(idx: u32) -> WindowId {
        WindowId {
            pid: 1,
            idx: NonZeroU32::new(idx).unwrap(),
        }
    }

    fn coverage(covered: (f64, f64), window: (f64, f64)) -> Coverage { Coverage { covered, window } }

    fn cache() -> PictureCache<Coverage> { PictureCache::new() }

    #[test]
    fn cache_applies_the_no_downgrade_rule() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.insert(wid(1), coverage((40.0, 1081.0), (859.0, 1081.0)));
        assert_eq!(cache.get(wid(1)).unwrap().covered.0, 859.0);
        assert!(cache.usable(wid(1)).is_some());
    }

    #[test]
    fn cache_holds_a_sliver_but_reports_it_as_not_worth_drawing() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((40.0, 1081.0), (859.0, 1081.0)));
        assert!(cache.get(wid(1)).is_some(), "held");
        assert!(cache.usable(wid(1)).is_none(), "but not drawable");
    }

    /// A copy replaces only the picture it was made of: a capture that landed while it was being
    /// made is newer, and stays.
    #[test]
    fn a_copy_replaces_only_the_picture_it_was_made_of() {
        let mut cache = cache();
        let held = coverage((859.0, 1081.0), (859.0, 1081.0));
        cache.insert(wid(1), held);
        let copy = coverage((859.0, 1081.0), (859.5, 1081.0));
        assert!(!cache.replace_if(wid(1), |now| *now != held, copy), "replaced meanwhile");
        assert_eq!(cache.get(wid(1)), Some(&held));
        assert!(cache.replace_if(wid(1), |now| *now == held, copy));
        assert_eq!(cache.get(wid(1)), Some(&copy));
        assert!(!cache.replace_if(wid(2), |_| true, copy), "nothing held");
        assert!(cache.get(wid(2)).is_none());
    }

    /// The focused look is kept until it is dropped as stale or the window goes.
    #[test]
    fn a_focused_look_is_kept_until_stale_or_forgotten() {
        let mut cache = cache();
        let look = coverage((859.0, 1081.0), (859.0, 1081.0));
        cache.keep_focused(wid(1), look);
        assert_eq!(cache.focused(wid(1)), Some(&look));
        assert!(!cache.drop_focused_if(wid(1), |held| *held != look), "not the look checked");
        assert!(cache.drop_focused_if(wid(1), |held| *held == look));
        assert!(cache.focused(wid(1)).is_none());

        cache.keep_focused(wid(2), look);
        cache.insert(wid(2), look);
        cache.forget(wid(2));
        assert!(cache.focused(wid(2)).is_none(), "gone with the window");
        assert!(cache.get(wid(2)).is_none());
    }

    #[test]
    fn a_focused_looks_copy_replaces_only_the_look_it_was_made_of() {
        let mut cache = cache();
        let look = coverage((859.0, 1081.0), (859.0, 1081.0));
        let copy = coverage((859.0, 1081.0), (859.5, 1081.0));
        cache.keep_focused(wid(1), look);
        assert!(!cache.replace_focused_if(wid(1), |held| *held != look, copy));
        assert!(cache.replace_focused_if(wid(1), |held| *held == look, copy));
        assert_eq!(cache.focused(wid(1)), Some(&copy));
        assert!(!cache.replace_focused_if(wid(2), |_| true, copy), "no look");
    }

    fn tag(client: u32, generation: u64) -> Tag {
        Tag {
            client: Client(client),
            generation,
        }
    }

    /// A client names a delivered picture by the tag it came under: only the window's last delivery
    /// answers, and only under that exact tag, so a look named for a picture since superseded is
    /// never kept.
    #[test]
    fn a_delivered_picture_is_found_under_its_own_tag_only() {
        let mut delivered = Delivered::default();
        delivered.record(tag(1, 3), wid(1), 'a');
        assert_eq!(delivered.under(tag(1, 3), wid(1)), Some(&'a'));
        assert_eq!(delivered.under(tag(1, 2), wid(1)), None, "an older generation");
        assert_eq!(delivered.under(tag(2, 3), wid(1)), None, "another client");
        delivered.record(tag(1, 4), wid(1), 'b');
        assert_eq!(delivered.under(tag(1, 3), wid(1)), None, "superseded");
        assert_eq!(delivered.under(tag(1, 4), wid(1)), Some(&'b'));
        delivered.record(tag(2, 4), wid(1), 'c');
        delivered.forget(wid(1));
        assert_eq!(delivered.under(tag(1, 4), wid(1)), None);
        assert_eq!(delivered.under(tag(2, 4), wid(1)), None);
    }

    #[test]
    fn forget_drops_one_window() {
        let mut cache = cache();
        cache.insert(wid(1), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.insert(wid(2), coverage((859.0, 1081.0), (859.0, 1081.0)));
        cache.forget(wid(1));
        assert!(cache.get(wid(1)).is_none());
        assert!(cache.get(wid(2)).is_some(), "only the one");
    }
}
