//! Whether two pictures of one window show the same thing: their band prints, where a focus change
//! shows, and their bodies, where content does. The pixels are sampled by
//! `crate::pictures::platform::measure`; comparing them is pure.

/// The band a focus change shows in: the window's top, in points, wherever its title bar is drawn.
pub const BAND_PT: f64 = 40.0;
/// The size the band is drawn down to, in pixels, for its print.
pub const BAND_PRINT: (usize, usize) = (96, 8);

/// The band print of the picture last delivered for a window, against a new one: a delivery that
/// carries an unchanged print is no news. With none delivered, every print is new.
pub fn print_changed(last: Option<&[u8]>, print: &[u8]) -> bool {
    last != Some(print)
}

/// The grid a picture's body is sampled on, per side.
pub const BODY_GRID: usize = 40;
/// Where the body lies in the picture, in unit space across and down: the edges, and the top band
/// where focus shows, are left out.
pub const BODY_ACROSS: (f64, f64) = (0.04, 0.96);
pub const BODY_DOWN: (f64, f64) = (0.08, 0.96);
/// Mean RGB difference above which two bodies show different content.
const BODIES_DIFFER_ABOVE: f64 = 3.0;

/// The unit-space points a body is sampled at, row by row: the centre of each cell of the grid.
pub fn body_points() -> impl Iterator<Item = (f64, f64)> {
    let at =
        |(from, to): (f64, f64), i: usize| from + (to - from) * (i as f64 + 0.5) / BODY_GRID as f64;
    (0..BODY_GRID).flat_map(move |row| {
        (0..BODY_GRID).map(move |col| (at(BODY_ACROSS, col), at(BODY_DOWN, row)))
    })
}

/// Whether two pictures' bodies, sampled at [`body_points`] as RGBA, show different content in
/// their colour. Samples that do not pair up are a picture that could not be read, which counts
/// as different.
pub fn bodies_differ(a: &[[u8; 4]], b: &[[u8; 4]]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return true;
    }
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(a, b)| a[..3].iter().zip(b).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / 3.0)
        .sum();
    sum / a.len() as f64 > BODIES_DIFFER_ABOVE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_print_is_news_when_it_differs_from_the_last_one_delivered_or_none_was() {
        assert!(print_changed(None, &[1, 2, 3]));
        assert!(print_changed(Some(&[1, 2, 4]), &[1, 2, 3]));
        assert!(!print_changed(Some(&[1, 2, 3]), &[1, 2, 3]));
    }

    /// The grid covers the body and nothing of the band above it.
    #[test]
    fn the_body_is_sampled_on_a_grid_below_the_band() {
        let points: Vec<(f64, f64)> = body_points().collect();
        assert_eq!(points.len(), BODY_GRID * BODY_GRID);
        assert!(
            points
                .iter()
                .all(|(u, v)| (0.04..=0.96).contains(u) && (0.08..=0.96).contains(v))
        );
        assert!(points[0].1 < points[BODY_GRID].1, "row by row, top first");
        assert!(points[0].0 < points[1].0, "left to right");
    }

    fn flat(level: u8) -> Vec<[u8; 4]> {
        vec![[level, level, level, 255]; BODY_GRID * BODY_GRID]
    }

    /// More than three levels on average is different content; a cursor or a clock is not.
    #[test]
    fn bodies_differ_by_more_than_three_levels_on_average() {
        assert!(!bodies_differ(&flat(100), &flat(100)));
        assert!(!bodies_differ(&flat(100), &flat(103)), "three levels");
        assert!(bodies_differ(&flat(100), &flat(104)), "four");
        let mut cursor = flat(100);
        for sample in cursor.iter_mut().take(10) {
            *sample = [255, 255, 255, 255];
        }
        assert!(!bodies_differ(&flat(100), &cursor), "ten samples of 1600");
        let mut scrolled = flat(100);
        for sample in scrolled.iter_mut().take(400) {
            *sample = [20, 20, 20, 255];
        }
        assert!(bodies_differ(&flat(100), &scrolled), "a quarter of it");
    }

    #[test]
    fn a_body_that_could_not_be_read_differs() {
        assert!(bodies_differ(&flat(100), &[]));
        assert!(bodies_differ(&[], &[]));
        assert!(bodies_differ(&flat(100), &flat(100)[..10]));
    }
}
