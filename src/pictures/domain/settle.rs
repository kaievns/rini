//! When a window has finished painting: two consecutive thumbprints agree, or one differs from the
//! rendering before the change. See "A resize flies at once, then reveals" in
//! `src/transitions/docs/coordination.md`.

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

/// Whether a capture of a window that has to repaint counts as its settled rendering.
pub fn repaint_settled(prev: Option<&[u8]>, print: &[u8], pre_resize: Option<&[u8]>) -> bool {
    prev.is_some_and(|previous| renderings_match(previous, print))
        || pre_resize.is_some_and(|before| !renderings_match(before, print))
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
    fn repaint_settled_on_a_match_or_a_repaint() {
        let p = vec![10u8; 64];
        let q = vec![200u8; 64];
        assert!(!repaint_settled(None, &p, None), "nothing to compare against");
        assert!(repaint_settled(Some(&p), &p, None), "two consecutive match");
        assert!(
            repaint_settled(None, &p, Some(&q)),
            "differs from the pre-resize picture"
        );
        assert!(!repaint_settled(None, &p, Some(&p)), "still the old rendering");
        assert!(
            repaint_settled(Some(&q), &p, Some(&q)),
            "a repaint settles even after a change"
        );
    }
}
