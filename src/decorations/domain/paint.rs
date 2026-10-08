//! The paint a band rini draws is filled with, as the config writes it: a colour, or niri's gradient,
//! and how Core Animation is given each gradient: its axis over the band (`Gradient::axis`) and the
//! colours it is drawn through (`Gradient::stops`). See "Gradients" in
//! `src/decorations/docs/README.md`.

use rini_geometry::space::{Contents, Point, TileSize};
use serde::Serialize;

/// An sRGB colour with its alpha, each channel in `0.0..=1.0`, unpremultiplied, as niri and the
/// parser it shares with niri hold one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Colour {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Colour {
    /// `0xRRGGBB`, opaque.
    pub const fn rgb(rgb: u32) -> Colour {
        Colour { r: channel(rgb, 16), g: channel(rgb, 8), b: channel(rgb, 0), a: 1.0 }
    }
}

const fn channel(rgb: u32, shift: u32) -> f32 { ((rgb >> shift) & 0xff) as f32 / 255.0 }

/// What a band is filled with: a colour, or a gradient. A gradient written beside a colour is drawn
/// instead of it, and a colour written later replaces the gradient, so a band has one paint, as niri
/// selects it (`niri:src/layout/focus_ring.rs:73-98`, `niri:niri-config/src/macros.rs:31-57`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum Paint {
    Solid(Colour),
    Gradient(Gradient),
}

/// niri's linear gradient over the band's own outer rect: `from` to `to` along `angle`, in degrees
/// as CSS has them, 0 pointing up and 90 right, mixed in `space`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Gradient {
    pub from: Colour,
    pub to: Colour,
    pub angle: f64,
    pub space: ColourSpace,
}

/// The space niri mixes a gradient's two colours in (`niri:src/render_helpers/shaders/border.frag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize)]
pub enum ColourSpace {
    #[default]
    Srgb,
    SrgbLinear,
    Oklab,
    Oklch(Hue),
}

/// Which way round the hue circle an `oklch` gradient goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize)]
pub enum Hue {
    #[default]
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

/// Where a gradient's axis starts and ends over its layer, in the layer's unit space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub start: Point<Contents>,
    pub end: Point<Contents>,
}

/// One colour Core Animation draws a gradient through, at `location` along its axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub location: f64,
    pub colour: Colour,
}

/// How finely a gradient is sampled to place its stops: about a device pixel apart along a band
/// around a window filling the largest display at 2x.
pub const SAMPLES: usize = 4096;

/// How far, in levels of 255, what Core Animation draws between two stops may stray from niri's
/// colour at any sample, whichever way it interpolates.
pub const TOLERANCE: f64 = 0.5;

impl Gradient {
    /// The axis over a layer of `size` that draws niri's gradient: the line through the centre at
    /// `angle`, long enough that the corners lie on the lines across its ends, each line across it
    /// at right angles in points. Core Animation draws those lines at right angles in unit space, so
    /// the axis is stretched by the size to make them right angles in points. A layer of no size
    /// draws top to bottom.
    pub fn axis(&self, size: TileSize) -> Axis {
        let (w, h) = (size.width, size.height);
        let radians = (self.angle - 90.0).to_radians();
        let length = (w * radians.cos()).abs() + (h * radians.sin()).abs();
        let (vx, vy) = (radians.cos() * length, radians.sin() * length);
        let stretched = (vx * w).powi(2) + (vy * h).powi(2);
        if w <= 0.0 || h <= 0.0 || stretched <= 0.0 {
            return Axis { start: Point::new(0.5, 0.0), end: Point::new(0.5, 1.0) };
        }
        let k = (vx * vx + vy * vy) / stretched;
        let start = Point::new((w - vx) / 2.0 / w, (h - vy) / 2.0 / h);
        Axis { start, end: Point::new(start.x + k * vx * w, start.y + k * vy * h) }
    }

    /// Whether its axis moves with its layer's size: at any angle but a right angle the corners it
    /// reaches depend on the band's shape.
    pub fn follows_size(&self) -> bool { self.angle.rem_euclid(90.0) != 0.0 }

    /// niri's colour `t` of the way along, unpremultiplied.
    pub fn colour_at(&self, t: f64) -> Colour { unpremultiplied(self.premultiplied_at(t)) }

    /// niri's `color_mix`: the two colours mixed premultiplied in `space`, sRGB's curve taken as
    /// niri's power of 2.2, and a colour mixed out of sRGB clamped into it.
    fn premultiplied_at(&self, t: f64) -> [f64; 4] {
        let (from, to) = (rgba(self.from), rgba(self.to));
        if self.space == ColourSpace::Srgb {
            return lerp(premultiply(from), premultiply(to), t);
        }
        let linear = |c: [f64; 4]| [decode(c[0]), decode(c[1]), decode(c[2]), c[3]];
        let (from, to) = (linear(from), linear(to));
        let mixed = match self.space {
            ColourSpace::Srgb | ColourSpace::SrgbLinear => mix_premultiplied(from, to, t, 3),
            ColourSpace::Oklab => {
                let lab = mix_premultiplied(with_alpha(oklab(from), from[3]), with_alpha(oklab(to), to[3]), t, 3);
                with_alpha(linear_of(lab), lab[3])
            }
            ColourSpace::Oklch(hue) => {
                let (a, b) = (lch(oklab(from)), lch(oklab(to)));
                let mut mixed = mix_premultiplied(with_alpha(a, from[3]), with_alpha(b, to[3]), t, 2);
                mixed[2] = hue_between(a[2], b[2], t, hue);
                with_alpha(linear_of(lab_of(mixed)), mixed[3])
            }
        };
        let channel = |c: f64| encode(c.clamp(0.0, 1.0));
        premultiply([channel(mixed[0]), channel(mixed[1]), channel(mixed[2]), mixed[3]])
    }

    /// The colours Core Animation draws the gradient through, its two ends and as many between as
    /// keep what it draws within `TOLERANCE` of niri's colour at every one of `SAMPLES` steps,
    /// whether it interpolates between them in sRGB's values or in linear light.
    pub fn stops(&self) -> Vec<Stop> {
        let exact: Vec<[f64; 4]> = (0..=SAMPLES).map(|i| self.premultiplied_at(i as f64 / SAMPLES as f64)).collect();
        let holds = |a: usize, b: usize| (a + 1..b).all(|i| within(&exact, a, b, i));
        let mut ends = vec![0];
        let mut a = 0;
        while a < SAMPLES {
            let (mut good, mut step) = (a + 1, 1);
            while good + step <= SAMPLES && holds(a, good + step) {
                good += step;
                step *= 2;
            }
            let mut bad = (good + step).min(SAMPLES + 1);
            while bad - good > 1 {
                let mid = (good + bad) / 2;
                if holds(a, mid) {
                    good = mid;
                } else {
                    bad = mid;
                }
            }
            ends.push(good);
            a = good;
        }
        ends.into_iter()
            .map(|i| Stop { location: i as f64 / SAMPLES as f64, colour: unpremultiplied(exact[i]) })
            .collect()
    }
}

/// Whether sample `i`, between the stops at samples `a` and `b`, is drawn within `TOLERANCE` either
/// way Core Animation could interpolate.
fn within(exact: &[[f64; 4]], a: usize, b: usize, i: usize) -> bool {
    let u = (i - a) as f64 / (b - a) as f64;
    [Interpolation::Srgb, Interpolation::Linear].into_iter().all(|how| {
        let drawn = how.between(exact[a], exact[b], u);
        (0..4).all(|k| (drawn[k] - exact[i][k]).abs() * 255.0 <= TOLERANCE)
    })
}

/// How a renderer may mix two stops: unpremultiplied, each channel and the alpha alone, in sRGB's
/// values or in linear light, or, in tests, at a power between.
#[derive(Clone, Copy, Debug)]
enum Interpolation {
    Srgb,
    Linear,
    #[cfg(test)]
    Power(f64),
}

impl Interpolation {
    /// `u` of the way from `a` to `b`, both premultiplied, premultiplied.
    fn between(self, a: [f64; 4], b: [f64; 4], u: f64) -> [f64; 4] {
        let (a, b) = (unpremultiply(a), unpremultiply(b));
        let power = match self {
            Interpolation::Srgb => 1.0,
            Interpolation::Linear => SRGB_POWER,
            #[cfg(test)]
            Interpolation::Power(gamma) => SRGB_POWER / gamma,
        };
        let mix = |x: f64, y: f64| (x.powf(power) + (y.powf(power) - x.powf(power)) * u).max(0.0).powf(1.0 / power);
        premultiply([mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]), a[3] + (b[3] - a[3]) * u])
    }
}

/// niri's stand-in for sRGB's curve.
const SRGB_POWER: f64 = 2.2;

fn decode(c: f64) -> f64 { c.max(0.0).powf(SRGB_POWER) }

fn encode(c: f64) -> f64 { c.max(0.0).powf(1.0 / SRGB_POWER) }

fn rgba(c: Colour) -> [f64; 4] { [c.r.into(), c.g.into(), c.b.into(), c.a.into()] }

fn with_alpha(c: [f64; 3], a: f64) -> [f64; 4] { [c[0], c[1], c[2], a] }

fn premultiply(c: [f64; 4]) -> [f64; 4] { [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]] }

fn unpremultiply(c: [f64; 4]) -> [f64; 4] {
    if c[3] <= 0.0 {
        return [0.0; 4];
    }
    [c[0] / c[3], c[1] / c[3], c[2] / c[3], c[3]]
}

fn unpremultiplied(c: [f64; 4]) -> Colour {
    let c = unpremultiply(c);
    let f = |x: f64| x.clamp(0.0, 1.0) as f32;
    Colour { r: f(c[0]), g: f(c[1]), b: f(c[2]), a: f(c[3]) }
}

fn lerp(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] { std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t) }

/// niri's `premul_mix_unpremul`: the first `channels` multiplied by the alpha, mixed, and divided
/// by the mixed alpha again.
fn mix_premultiplied(a: [f64; 4], b: [f64; 4], t: f64, channels: usize) -> [f64; 4] {
    let scale = |mut c: [f64; 4]| {
        let alpha = c[3];
        c.iter_mut().take(channels).for_each(|x| *x *= alpha);
        c
    };
    let mut mixed = lerp(scale(a), scale(b), t);
    let alpha = mixed[3];
    if alpha != 0.0 {
        mixed.iter_mut().take(channels).for_each(|x| *x /= alpha);
    }
    mixed
}

fn times(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

const TO_LMS: [[f64; 3]; 3] = [
    [0.4122214708, 0.5363325363, 0.0514459929],
    [0.2119034982, 0.6806995451, 0.1073969566],
    [0.0883024619, 0.2817188376, 0.6299787005],
];
const LMS_TO_LAB: [[f64; 3]; 3] = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];
const LAB_TO_LMS: [[f64; 3]; 3] = [
    [1.0, 0.3963377774, 0.2158037573],
    [1.0, -0.1055613458, -0.0638541728],
    [1.0, -0.0894841775, -1.2914855480],
];
const FROM_LMS: [[f64; 3]; 3] = [
    [4.0767416621, -3.3077115913, 0.2309699292],
    [-1.2684380046, 2.6097574011, -0.3413193965],
    [-0.0041960863, -0.7034186147, 1.7076147010],
];

/// Linear sRGB to Oklab, with the matrices niri's shader has.
fn oklab(c: [f64; 4]) -> [f64; 3] { times(LMS_TO_LAB, times(TO_LMS, [c[0], c[1], c[2]]).map(f64::cbrt)) }

fn linear_of(lab: [f64; 4]) -> [f64; 3] { times(FROM_LMS, times(LAB_TO_LMS, [lab[0], lab[1], lab[2]]).map(|x| x * x * x)) }

/// Oklab as lightness, chroma and a hue in `(0, 360]`, as niri's `lab_to_lch` has it.
fn lch(lab: [f64; 3]) -> [f64; 3] {
    let hue = lab[2].atan2(lab[1]).to_degrees();
    [lab[0], lab[1].hypot(lab[2]), if hue <= 0.0 { hue + 360.0 } else { hue }]
}

fn lab_of(lch: [f64; 4]) -> [f64; 4] {
    let hue = lch[2].to_radians();
    [lch[0], lch[1] * hue.cos().clamp(-1.0, 1.0), lch[1] * hue.sin().clamp(-1.0, 1.0), lch[3]]
}

/// The hue `t` of the way from `a` to `b` the way `hue` goes round, as niri's shader takes it.
fn hue_between(a: f64, b: f64, t: f64, hue: Hue) -> f64 {
    let (low, high) = (a.min(b), a.max(b));
    let (direct, wrapped) = (high - low, 360.0 - high + low);
    let round = if a == low { (a - wrapped * t).rem_euclid(360.0) } else { (a + wrapped * t).rem_euclid(360.0) };
    let straight = if a == low { a + direct * t } else { a - direct * t };
    let wraps = match hue {
        Hue::Shorter => direct > wrapped,
        Hue::Longer => direct <= wrapped,
        Hue::Increasing => a > b,
        Hue::Decreasing => a <= b,
    };
    if wraps { round } else { straight }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Colour = Colour::rgb(0xff0000);
    const GREEN: Colour = Colour::rgb(0x00ff00);
    const BLUE: Colour = Colour::rgb(0x0000ff);
    const BLACK: Colour = Colour::rgb(0x000000);
    const WHITE: Colour = Colour::rgb(0xffffff);

    fn gradient(from: Colour, to: Colour, space: ColourSpace) -> Gradient { Gradient { from, to, angle: 180.0, space } }

    #[test]
    fn a_hex_colour_is_its_three_channels_opaque() {
        assert_eq!(Colour::rgb(0xff8000), Colour { r: 1.0, g: 128.0 / 255.0, b: 0.0, a: 1.0 });
    }

    /// niri's `t` at a point of a rect of `size`: its distance along the angle from the line
    /// through the corner the gradient starts at, over the gradient's length.
    fn niris(angle: f64, size: TileSize, x: f64, y: f64) -> f64 {
        let radians = (angle - 90.0).to_radians();
        let (dx, dy) = (radians.cos(), radians.sin());
        let length = (size.width * dx).abs() + (size.height * dy).abs();
        ((x - size.width / 2.0) * dx + (y - size.height / 2.0) * dy) / length + 0.5
    }

    /// Core Animation's `t` at the same point: its projection onto the axis in unit space.
    fn core_animations(axis: Axis, size: TileSize, x: f64, y: f64) -> f64 {
        let (ux, uy) = (x / size.width, y / size.height);
        let (ax, ay) = (axis.end.x - axis.start.x, axis.end.y - axis.start.y);
        ((ux - axis.start.x) * ax + (uy - axis.start.y) * ay) / (ax * ax + ay * ay)
    }

    #[test]
    fn an_angle_maps_to_start_and_end_points_as_niri() {
        let wide = TileSize::new(200.0, 100.0);
        let down = gradient(RED, BLUE, ColourSpace::Srgb);
        let near = |axis: Axis, start: (f64, f64), end: (f64, f64)| {
            [axis.start.x - start.0, axis.start.y - start.1, axis.end.x - end.0, axis.end.y - end.1].iter().all(|d| d.abs() < 1e-12)
        };
        assert!(near(down.axis(wide), (0.5, 0.0), (0.5, 1.0)), "180, niri's default, is top to bottom");
        assert!(near(Gradient { angle: 90.0, ..down }.axis(wide), (0.0, 0.5), (1.0, 0.5)), "90 is left to right");
        for angle in [0.0, 30.0, 45.0, 90.0, 135.0, 180.0, 200.0, 270.0, 315.0, 359.0, -45.0] {
            for size in [wide, TileSize::new(100.0, 300.0), TileSize::new(1148.0, 1079.0)] {
                let axis = Gradient { angle, ..down }.axis(size);
                for (x, y) in [(0.0, 0.0), (size.width, 0.0), (0.0, size.height), (size.width, size.height), (13.0, 71.0)] {
                    let (ours, theirs) = (core_animations(axis, size, x, y), niris(angle, size, x, y));
                    assert!((ours - theirs).abs() < 1e-9, "{angle} at {x},{y} over {size:?}: {ours} for niri's {theirs}");
                }
            }
        }
    }

    #[test]
    fn an_axis_at_a_right_angle_holds_through_a_resize() {
        for angle in [0.0, 90.0, 180.0, 270.0, -90.0, 45.0, 200.0] {
            let at = |w, h| Gradient { angle, ..gradient(RED, BLUE, ColourSpace::Srgb) }.axis(TileSize::new(w, h));
            let (a, b) = (at(200.0, 100.0), at(100.0, 300.0));
            let holds = [a.start.x - b.start.x, a.start.y - b.start.y, a.end.x - b.end.x, a.end.y - b.end.y].iter().all(|d| d.abs() < 1e-12);
            assert_eq!(holds, !Gradient { angle, ..gradient(RED, BLUE, ColourSpace::Srgb) }.follows_size(), "{angle}");
        }
    }

    #[test]
    fn a_band_of_no_size_draws_top_to_bottom() {
        let axis = Gradient { angle: 45.0, ..gradient(RED, BLUE, ColourSpace::Srgb) }.axis(TileSize::new(0.0, 0.0));
        assert_eq!(axis, Axis { start: Point::new(0.5, 0.0), end: Point::new(0.5, 1.0) });
    }

    #[test]
    fn each_space_mixes_as_niris_shader() {
        let level = |c: f32| (c * 255.0).round();
        let mid = |space| gradient(BLACK, WHITE, space).colour_at(0.5);
        assert_eq!(level(mid(ColourSpace::Srgb).r), 128.0);
        assert_eq!(level(mid(ColourSpace::SrgbLinear).r), 186.0, "half in linear light, at niri's power of 2.2");
        assert_eq!(level(mid(ColourSpace::Oklab).g), level(mid(ColourSpace::Oklch(Hue::Shorter)).g), "a grey has no hue to go round");
        let fade = gradient(RED, Colour { a: 0.0, ..BLUE }, ColourSpace::Srgb).colour_at(0.5);
        assert_eq!((fade.r, fade.b, fade.a), (1.0, 0.0, 0.5), "mixed premultiplied, a colour fading out keeps its own");
        for space in [ColourSpace::Srgb, ColourSpace::SrgbLinear, ColourSpace::Oklab, ColourSpace::Oklch(Hue::Longer)] {
            let ends = gradient(RED, BLUE, space);
            for (t, end) in [(0.0, RED), (1.0, BLUE)] {
                let c = ends.colour_at(t);
                assert!([c.r - end.r, c.g - end.g, c.b - end.b].iter().all(|d| d.abs() < 1e-3), "{space:?} at {t}: {c:?}");
            }
        }
    }

    #[test]
    fn stops_in_oklch_longer_hue_go_the_long_way() {
        let hue = |c: Colour| lch(oklab([decode(c.r.into()), decode(c.g.into()), decode(c.b.into()), 1.0]))[2];
        let (red, blue) = (hue(RED), hue(BLUE));
        assert!(blue - red > 180.0, "red at {red}, blue at {blue}: the short way round passes 0");
        assert!((hue_between(red, blue, 0.5, Hue::Shorter) - ((red + blue) / 2.0 + 180.0).rem_euclid(360.0)).abs() < 1e-9);
        assert!((hue_between(red, blue, 0.5, Hue::Longer) - (red + blue) / 2.0).abs() < 1e-9);
        let at = |hue| gradient(RED, BLUE, ColourSpace::Oklch(hue)).colour_at(0.5);
        assert!(at(Hue::Longer).g > 0.5 && at(Hue::Shorter).g < 0.1, "the long way from red to blue passes green, the short way magenta");
        assert_eq!(at(Hue::Increasing), at(Hue::Longer), "red's hue is below blue's");
        assert_eq!(at(Hue::Decreasing), at(Hue::Shorter));
        let stops = |hue| gradient(RED, BLUE, ColourSpace::Oklch(hue)).stops().len();
        assert!(stops(Hue::Longer) > stops(Hue::Shorter), "the long way needs more stops");
    }

    /// Hard cases for each space: black to white for the curves, primaries and complements for the
    /// hue paths, and colours fading out for the alpha.
    fn hard_cases() -> Vec<Gradient> {
        let pairs = [
            (BLACK, WHITE),
            (RED, BLUE),
            (GREEN, Colour::rgb(0xff00ff)),
            (RED, Colour::rgb(0xffff00)),
            (RED, Colour { a: 0.0, ..BLUE }),
            (Colour::rgb(0x7fc8ff), Colour { a: 0.3, ..Colour::rgb(0x505050) }),
        ];
        let spaces = [
            ColourSpace::Srgb,
            ColourSpace::SrgbLinear,
            ColourSpace::Oklab,
            ColourSpace::Oklch(Hue::Shorter),
            ColourSpace::Oklch(Hue::Longer),
            ColourSpace::Oklch(Hue::Increasing),
            ColourSpace::Oklch(Hue::Decreasing),
        ];
        spaces.iter().flat_map(|space| pairs.map(|(from, to)| gradient(from, to, *space))).collect()
    }

    /// The most what a renderer draws between `stops`, interpolating `how`, strays from niri's
    /// colour at any sample, in levels.
    fn worst(gradient: &Gradient, stops: &[Stop], how: Interpolation) -> f64 {
        let at = |i: usize| gradient.premultiplied_at(i as f64 / SAMPLES as f64);
        let index = |stop: &Stop| (stop.location * SAMPLES as f64).round() as usize;
        let mut worst: f64 = 0.0;
        for pair in stops.windows(2) {
            let (a, b) = (index(&pair[0]), index(&pair[1]));
            for i in a..=b {
                let drawn = how.between(at(a), at(b), (i - a) as f64 / (b - a) as f64);
                worst = (0..4).map(|k| (drawn[k] - at(i)[k]).abs() * 255.0).fold(worst, f64::max);
            }
        }
        worst
    }

    /// Core Animation mixes stops in a space of its own: offscreen it drew black to white at 147 in
    /// the middle, a power of 1.8, where sRGB's values give 128 and linear light 186 ("Gradients" in
    /// `src/decorations/docs/README.md`). Stops held within half a level in sRGB and in linear light
    /// hold within one at that power.
    #[test]
    fn the_stop_count_keeps_each_space_within_one_level() {
        let mut most = 0;
        for gradient in hard_cases() {
            let stops = gradient.stops();
            assert_eq!((stops[0].location, stops[stops.len() - 1].location), (0.0, 1.0));
            for how in [Interpolation::Srgb, Interpolation::Linear, Interpolation::Power(1.8)] {
                let worst = worst(&gradient, &stops, how);
                assert!(worst < 1.0, "{:?} {:?} to {:?}, {how:?}: {worst:.2} levels", gradient.space, gradient.from, gradient.to);
            }
            most = most.max(stops.len());
        }
        assert!(most <= 128, "{most} stops at most");
    }

    /// The spec's `srgb_needs_no_extra_stops` assumed Core Animation interpolates in sRGB's values;
    /// it does not, so an sRGB gradient takes stops as every other does.
    #[test]
    fn two_stops_alone_miss_by_more_than_a_level() {
        for gradient in [gradient(RED, BLUE, ColourSpace::Srgb), gradient(BLACK, WHITE, ColourSpace::SrgbLinear)] {
            let ends = [Stop { location: 0.0, colour: gradient.from }, Stop { location: 1.0, colour: gradient.to }];
            let off = [Interpolation::Srgb, Interpolation::Linear].map(|how| worst(&gradient, &ends, how));
            assert!(off.iter().any(|levels| *levels > 10.0), "{:?}: {off:?}", gradient.space);
        }
    }

    /// Except the longer way round in `oklch`, which goes the whole circle, as niri's does.
    #[test]
    fn a_gradient_of_one_colour_is_its_two_ends() {
        for space in [ColourSpace::Srgb, ColourSpace::SrgbLinear, ColourSpace::Oklab, ColourSpace::Oklch(Hue::Shorter)] {
            assert_eq!(gradient(RED, RED, space).stops().len(), 2, "{space:?}");
        }
        assert!(gradient(RED, RED, ColourSpace::Oklch(Hue::Longer)).colour_at(0.5).g > 0.5);
    }
}
