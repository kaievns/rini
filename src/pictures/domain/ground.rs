//! A window's ground: the colour its content sits on, which a crop-drawn tile is filled with where
//! its picture ends rather than stretching the picture. Read off a picture's body samples, taken by
//! `crate::pictures::platform::measure`; choosing it is pure.

/// A colour with its alpha, each channel in `0.0..=1.0`, unpremultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ground {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// Bits a channel is counted at: neighbours of one background, as a gradient or a compression
/// leaves them, count as one colour, and text and its antialiasing do not join it.
const COUNTED_BITS: u32 = 4;

/// The most common colour among `samples`, each premultiplied RGBA as a picture's body is read:
/// counted at `COUNTED_BITS` a channel, then the mean of the samples counted with it, so it is the
/// background's own colour rather than its bucket's. `None` with no samples.
pub fn ground(samples: &[[u8; 4]]) -> Option<Ground> {
    let shift = 8 - COUNTED_BITS;
    let mut counts: std::collections::BTreeMap<[u8; 4], (u32, [u32; 4])> = Default::default();
    for sample in samples {
        let (count, sums) = counts.entry(sample.map(|channel| channel >> shift)).or_default();
        *count += 1;
        for (sum, channel) in sums.iter_mut().zip(sample) {
            *sum += u32::from(*channel);
        }
    }
    let (count, sums) = counts.into_values().max_by_key(|(count, _)| *count)?;
    let [r, g, b, a] = sums.map(|sum| sum as f64 / count as f64 / 255.0);
    let unpremultiplied = |channel: f64| if a > 0.0 { (channel / a).min(1.0) } else { 0.0 };
    Some(Ground {
        r: unpremultiplied(r),
        g: unpremultiplied(g),
        b: unpremultiplied(b),
        a,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(got: Ground, want: Ground) -> bool {
        [
            (got.r, want.r),
            (got.g, want.g),
            (got.b, want.b),
            (got.a, want.a),
        ]
        .iter()
        .all(|(got, want)| (got - want).abs() < 1.0 / 255.0)
    }

    /// A terminal: a dark background, a little noisy, under lines of light text and a few coloured
    /// glyphs, is grounded on the background, not on a mix of everything.
    #[test]
    fn a_terminal_is_grounded_on_its_background() {
        let noisy = |i: u8| [35 + i % 2 * 2, 35 + i % 2 * 2, 43 + i % 2 * 2, 255];
        let mut samples: Vec<[u8; 4]> = (0..1300).map(|i| noisy((i % 2) as u8)).collect();
        samples.extend([[220, 220, 220, 255]; 250]);
        samples.extend([[80, 200, 120, 255]; 50]);
        let got = ground(&samples).expect("a ground");
        let want = Ground {
            r: 36.0 / 255.0,
            g: 36.0 / 255.0,
            b: 44.0 / 255.0,
            a: 1.0,
        };
        assert!(close(got, want), "{got:?}");
    }

    /// A translucent window's ground keeps its alpha and is unpremultiplied: Ghostty at 0.95 over a
    /// background of 0.1, 0.1, 0.12, read premultiplied as it is drawn.
    #[test]
    fn a_translucent_window_keeps_its_alpha() {
        let a = 0.95;
        let premultiplied = |c: f64| (c * a * 255.0).round() as u8;
        let background = [
            premultiplied(0.1),
            premultiplied(0.1),
            premultiplied(0.12),
            242,
        ];
        let mut samples = vec![background; 1500];
        samples.extend(vec![[200, 200, 200, 242]; 100]);
        let got = ground(&samples).expect("a ground");
        let want = Ground {
            r: 0.1,
            g: 0.1,
            b: 0.12,
            a: 242.0 / 255.0,
        };
        assert!(close(got, want), "{got:?}");
        let clear = ground(&[[0, 0, 0, 0]; 4]).expect("a ground");
        assert_eq!(
            clear,
            Ground { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
            "clear stays clear"
        );
    }

    #[test]
    fn no_samples_is_no_ground() {
        assert_eq!(ground(&[]), None);
    }
}
