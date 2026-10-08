//! What a burst of the window server's move and resize notifications comes to.

use rustc_hash::FxHashSet as HashSet;

/// The distinct windows in `ids`, each where it last appears: the order to read their frames in, so
/// that no frame read earlier is published after a newer one.
pub fn latest(ids: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::default();
    let mut windows: Vec<u32> = ids.iter().rev().copied().filter(|id| seen.insert(*id)).collect();
    windows.reverse();
    windows
}

#[cfg(test)]
mod tests {
    use super::latest;

    #[test]
    fn a_burst_reads_each_window_once_at_its_last_place() {
        assert_eq!(latest(&[7, 9, 7, 4, 9]), vec![7, 4, 9]);
        assert_eq!(latest(&[]), Vec::<u32>::new());
    }
}
