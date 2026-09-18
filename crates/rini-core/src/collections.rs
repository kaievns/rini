use std::borrow::Borrow;
pub use std::collections::{BTreeMap, BTreeSet, hash_map};

// We don't need or want the random state of the default std collections.
// We also don't need cryptographic hashing, and these are faster.
pub use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::ids::{WindowId, pid_t};

pub trait BTreeExt {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self;
}

// There's not currently a stable way to remove only a range, so we have
// to do this split/extend dance. Is it faster than scanning through all
// the keys? Who knows!

impl BTreeExt for BTreeSet<WindowId> {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self {
        let mut split = self.split_off(&PidRange(pid));
        self.extend(split.split_off(&PidRange(pid + 1)));
        split
    }
}

impl<V> BTreeExt for BTreeMap<WindowId, V> {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self {
        let mut split = self.split_off(&PidRange(pid));
        self.extend(split.split_off(&PidRange(pid + 1)));
        split
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
#[repr(transparent)]
struct PidRange(pid_t);

// Technically this violates the Borrow requirements by having Ord/Eq
// behave differently than the original type, but we are working around
// API limitations and it should not matter for a reasonable implementation
// of `split_off`.
impl Borrow<PidRange> for WindowId {
    fn borrow(&self) -> &PidRange {
        // Safety: PidRange is repr(transparent).
        unsafe { &*std::ptr::addr_of!(self.pid).cast() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Vec<WindowId> {
        vec![
            WindowId::new(1, 1),
            WindowId::new(2, 1),
            WindowId::new(2, 2),
            WindowId::new(2, u32::MAX),
            WindowId::new(3, 1),
        ]
    }

    #[test]
    fn remove_all_for_pid_takes_exactly_that_pid_from_a_set() {
        let mut set: BTreeSet<WindowId> = ids().into_iter().collect();
        let removed = set.remove_all_for_pid(2);
        assert_eq!(removed.len(), 3);
        assert!(removed.iter().all(|w| w.pid == 2));
        assert_eq!(set.len(), 2);
        assert!(set.iter().all(|w| w.pid != 2));
    }

    #[test]
    fn remove_all_for_pid_takes_exactly_that_pid_from_a_map() {
        let mut map: BTreeMap<WindowId, u8> = ids().into_iter().map(|w| (w, 0)).collect();
        let removed = map.remove_all_for_pid(2);
        assert_eq!(removed.len(), 3);
        assert_eq!(map.len(), 2);
        assert!(map.remove_all_for_pid(9).is_empty());
        assert_eq!(map.len(), 2);
    }
}
