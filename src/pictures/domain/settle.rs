//! When a window has finished painting: two consecutive thumbprints agree. See "A grow waits for its
//! repaint" in `src/transitions/docs/coordination.md`.

/// Share of samples that may differ and still count as the same rendering: forgives a blinking
/// cursor, not a half-painted surface.
const STABLE_MAX_DIFFERING: f64 = 0.03;
const STABLE_CHANNEL_TOLERANCE: u8 = 8;

/// Whether two consecutive thumbprints show the same rendering.
fn renderings_match(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let differing = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > STABLE_CHANNEL_TOLERANCE)
        .count();
    (differing as f64) <= (a.len() as f64) * STABLE_MAX_DIFFERING
}

/// Whether a capture of a window that has to repaint counts as its settled rendering: it matches the
/// capture before it, whatever the picture held before shows.
pub fn repaint_settled(prev: Option<&[u8]>, print: &[u8]) -> bool {
    prev.is_some_and(|previous| renderings_match(previous, print))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendering_matches_itself_through_cursor_noise_but_not_through_repaints() {
        let a = vec![100u8; 4096];
        assert!(renderings_match(&a, &a), "identical");
        let mut cursor = a.clone();
        for value in cursor.iter_mut().take(80) {
            *value = 200; // ~2% of samples
        }
        assert!(renderings_match(&a, &cursor), "cursor-sized noise still matches");
        let mut repaint = a.clone();
        for value in repaint.iter_mut().take(1024) {
            *value = 200; // a quarter of the image
        }
        assert!(!renderings_match(&a, &repaint), "a repaint does not");
    }

    #[test]
    fn mismatched_or_empty_thumbprints_never_match() {
        assert!(!renderings_match(&[1, 2, 3], &[1, 2]));
        assert!(!renderings_match(&[], &[]));
    }

    #[test]
    fn repaint_settled_only_on_two_captures_in_a_row_that_match() {
        let full = vec![10u8; 64];
        let mut half = full.clone();
        for value in half.iter_mut().skip(16) {
            *value = 0; // the old rows at the top of a blank surface
        }
        assert!(!repaint_settled(None, &half), "a first capture, however it differs from before");
        assert!(!repaint_settled(Some(&half), &full), "half-painted, then painted");
        assert!(repaint_settled(Some(&full), &full), "painted twice in a row");
        assert!(repaint_settled(Some(&half), &half), "two alike, whatever they show");
    }
}
