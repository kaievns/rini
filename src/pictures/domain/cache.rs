//! The picture cache and its rules: what an incoming capture replaces, and what survives of the one it
//! replaces. Generic over what it holds, so the rules are tested on plain sizes.

use std::collections::HashMap;

use rini_core::ids::WindowId;

use crate::pictures::domain::coverage::{Coverage, should_replace};

/// Anything the cache can hold and judge, so the replacement policy can be tested on plain sizes.
pub trait HasCoverage {
    fn coverage(&self) -> Coverage;
}

/// State a cache payload keeps alive across captures, independently of the pixel replacement rule.
/// The dressing comes from the screen composite, not the capture buffer, so it replaces on its own.
pub trait CarriesOver: Sized {
    /// Called on an incoming payload that is about to replace `previous`.
    fn inherit(&mut self, _previous: &Self) {}
    /// Called on the kept payload when `refused` lost to the replacement rule.
    fn absorb(&mut self, _refused: Self) {}
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

/// Which dressing the cache holds after a capture lands: a fresh harvest wins, otherwise the
/// window keeps what it last wore, whether or not the capture's pixels were accepted.
pub fn dressing_after_insert<D>(worn: Option<D>, harvested: Option<D>) -> Option<D> {
    harvested.or(worn)
}

impl HasCoverage for Coverage {
    fn coverage(&self) -> Coverage { *self }
}

impl CarriesOver for Coverage {}

/// Pictures held per window, so a switch can composite without capturing anything synchronously.
/// Keyed by [`WindowId`] because window server ids are recycled when a window closes and reopens.
pub struct PictureCache<T> {
    entries: HashMap<WindowId, T>,
}

impl<T: HasCoverage> Default for PictureCache<T> {
    fn default() -> Self { Self { entries: HashMap::new() } }
}

impl<T: HasCoverage + CarriesOver> PictureCache<T> {
    pub fn new() -> Self { Self::default() }

    /// Stores a picture, subject to [`should_replace`] for its pixels, with [`CarriesOver`]
    /// deciding what survives of the rest either way. True when its pixels are now the ones held.
    pub fn insert(&mut self, window: WindowId, mut picture: T) -> bool {
        if !picture.cacheable() {
            return false;
        }
        if let Some(existing) = self.entries.get_mut(&window) {
            if !should_replace(Some(existing.coverage()), picture.coverage())
                || existing.keeps_over(&picture)
            {
                existing.absorb(picture);
                return false;
            }
            picture.inherit(existing);
        }
        self.entries.insert(window, picture);
        true
    }

    pub fn get(&self, window: WindowId) -> Option<&T> { self.entries.get(&window) }

    pub fn get_mut(&mut self, window: WindowId) -> Option<&mut T> { self.entries.get_mut(&window) }

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

    pub fn forget(&mut self, window: WindowId) { self.entries.remove(&window); }

    /// Every cached entry.
    pub fn iter(&self) -> impl Iterator<Item = (&WindowId, &T)> { self.entries.iter() }
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

    #[test]
    fn a_fresh_harvest_replaces_what_the_window_wore() {
        assert_eq!(dressing_after_insert(Some(1), Some(2)), Some(2));
    }

    #[test]
    fn a_failed_harvest_keeps_the_last_known_dressing() {
        assert_eq!(dressing_after_insert(Some(1), None), Some(1));
        assert_eq!(dressing_after_insert::<u32>(None, None), None);
    }

    fn wid(idx: u32) -> WindowId {
        WindowId {
            pid: 1,
            idx: NonZeroU32::new(idx).unwrap(),
        }
    }

    fn coverage(covered: (f64, f64), window: (f64, f64)) -> Coverage { Coverage { covered, window } }

    fn cache() -> PictureCache<Coverage> { PictureCache::new() }

    /// Stands in for a picture and its dressing without needing a bitmap.
    #[derive(Clone, Copy)]
    struct Dressed {
        coverage: Coverage,
        dressing: Option<u32>,
    }

    impl HasCoverage for Dressed {
        fn coverage(&self) -> Coverage { self.coverage }
    }

    impl CarriesOver for Dressed {
        fn inherit(&mut self, previous: &Self) {
            self.dressing = dressing_after_insert(previous.dressing, self.dressing);
        }

        fn absorb(&mut self, refused: Self) {
            self.dressing = dressing_after_insert(self.dressing, refused.dressing);
        }
    }

    #[test]
    fn a_refused_capture_still_delivers_its_dressing() {
        let mut cache: PictureCache<Dressed> = PictureCache::new();
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        let sliver = coverage((40.0, 1081.0), (859.0, 1081.0));
        cache.insert(
            wid(1),
            Dressed {
                coverage: good,
                dressing: Some(1),
            },
        );
        cache.insert(
            wid(1),
            Dressed {
                coverage: sliver,
                dressing: Some(2),
            },
        );
        let held = cache.get(wid(1)).unwrap();
        assert_eq!(held.coverage.covered.0, 859.0, "the pixels were refused");
        assert_eq!(held.dressing, Some(2), "but the fresher ring was kept");
    }

    #[test]
    fn an_accepted_capture_without_a_harvest_inherits_the_worn_dressing() {
        let mut cache: PictureCache<Dressed> = PictureCache::new();
        let good = coverage((859.0, 1081.0), (859.0, 1081.0));
        cache.insert(
            wid(1),
            Dressed {
                coverage: good,
                dressing: Some(1),
            },
        );
        cache.insert(wid(1), Dressed { coverage: good, dressing: None });
        assert_eq!(cache.get(wid(1)).unwrap().dressing, Some(1));
    }

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
