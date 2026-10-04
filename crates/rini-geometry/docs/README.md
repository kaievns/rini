# `rini-geometry` — rect arithmetic and coordinate spaces

Extension traits over `CGRect`, `CGPoint` and `CGSize`, the handful of predicates
every layout decision needs, and the coordinate spaces (`space`) a feature's
interface speaks. Pure, and the only crate that tests without linking AppKit:
`cargo test -p rini-geometry` builds plain Rust.

## What it owns

`CGRectExt` (`intersection`, `union`, `area`, `contains`, `contains_rect`), `Round`
(`rounded`), `SameAs` and `IsWithin` for float comparison, on CG values and typed
ones alike, `centered_in`, `is_off_screen`, `park_entry_frame`, and
`to_overlay_space` and `from_overlay_space`, a display rect in the overlay's own
coordinates and back. `stretch`: the stretch tolerance (`MIN_STRETCH`,
`MAX_STRETCH`), `within_stretch` and `is_a_resize`. `space`: the units, the typed
`Rect`, `Point`, `Size` and `Vector` over them with one alias per unit and kind a
feature uses, `PointSpace` and `Respace`, and `FromCg`/`ToCg`.

## The spaces

A typed rect, point, size or displacement names its space, so one in the wrong
space fails to compile. The types are euclid's (0.22, `default-features = false`).
Only this crate names euclid: the features use its aliases, so swapping the
library touches one crate.

| Unit | Points of |
|---|---|
| `Screen` | The window server's global space: points, origin at the main display's top-left, y down |
| `Overlay` | The overlay's root layer: `Screen` less the covered display's origin. The bar's band is included; the window clips it |
| `Surface` | A plane a viewport travels across: `Overlay` is `Surface` less the viewport |
| `Group` | A container layer's own points: `Overlay` less the container's position |
| `Tile` | A tile layer's own points, origin at its top-left |
| `Pixel` | An image's pixels, origin top-left, y down |
| `Contents` | A layer's contents in unit space, y down under rini's flipped views |

The first five are `PointSpace`s. They are apart by a translation, so a size or a
displacement means the same in each, and `Respace` carries one across; it is the
only `cast_unit` in the tree. A rect reaches `Pixel` or `Contents` only where a
function divides by what carries the scale. The scale itself stays `f64`.

**CG is the boundary, not the arithmetic.** In a feature on the spaces, a CG value
becomes typed (`FromCg`) on the line where it enters an interface or `domain/`, and
CG again (`ToCg`) on the line where it reaches objc2, in `platform/`. `From` cannot
be written: euclid's types and CG's are both foreign. `TYPED` in
`tests/architecture.rs` lists the features on the spaces, and
`a_typed_feature_names_no_cg_geometry_outside_platform` holds them to it. Every
other feature speaks CG and converts at its calls into them.

Where each conversion between spaces lives:

| Conversion | Where |
|---|---|
| CG into a space and back | `space::FromCg`, `space::ToCg` |
| A size or a displacement between point spaces | `space::Respace` |
| `Screen` to `Pixel`: a frame in a composite, at its scale | `pictures::domain::region::pixel_rect` |
| `Pixel` to `Contents`: a pixel rect over its image's size | `pictures::domain::region::unit_rect` |
| `Contents` within `Contents`: a piece of a region | `pictures::domain::region::within` |
| `Pixel` to `Contents`: the stand-in's middle over its image's width | `pictures::platform::layer::stretch_region` |

## euclid against the CG operations

euclid's inherent methods win over same-named trait methods, so a call that
compiled against `CGRectExt` or `Round` can silently start calling euclid's. An
expression becomes a euclid method only where the result is the same; everything
else keeps its own formula. `euclid_agrees_with_the_cg_ops_on_what_the_features_ask`
and `euclid_differs_from_the_cg_ops_at_the_edges` in `space.rs` pin the table.

| Op | rini-geometry / objc2 | euclid | Rule |
|---|---|---|---|
| `round` | `f64::round` on both edges: halves away from zero | `(x + 0.5).floor()`: −860.5 gives −860, not −861 | Does not compile on `f64` (below). rini's is `rounded()` |
| `lerp` | `a + (b − a)t` | `(1 − t)a + tb`: (864, −861, 0.3) gives 346.49999999999994, not 346.5 | Never call euclid's `lerp`; interpolation keeps its own formula |
| `intersection(..).area() > 0` | Clamps to an empty rect | `None` when empty; when `Some`, the same `max − min` | `a.intersection(&b).is_some()`, and `filter_map` where the rect is kept. Not `intersects`, which is true for a zero-width rect inside another |
| `union` | Spans an empty rect too | Skips an empty one | Union only non-empty rects. A union of FFI bounds is taken in CG and converted |
| `contains(point)` | Inclusive at the max edge | Exclusive | Not used on typed values |
| `contains_rect` | By value, empty is not special | By reference, empty is always contained | Not used on typed values |
| `mid` → `center` | `o + s * 0.5` | `o + s / 2` | The same |
| `translate`, `p − q`, `p + v`, `area` | Hand-rolled | The same operations | The same; they replace the hand-rolled forms |
| `is_empty`, `min`, `max` | objc2's | The same operations | The same |

**euclid's float module is off.** With `default-features = false` neither `std` nor
`libm` is on, and nothing the features ask needs either: intersection, union,
center, translate, `Translation2D`, `lerp` and `is_empty` all build without them.
Without the float module, euclid's `round` on an `f64` rect, point or size does
not compile (E0599). That turns its halves-up rounding into a compile error rather
than a frame a point off at a negative half (`rounded_takes_halves_away_from_zero`).
The switch is euclid's own feature, so `num-traits/std` from another crate does not
turn it on.

## The shape that matters

**`is_off_screen` has a threshold, and it is measured.** 40pt of overlap. Applications
clamp a park past it — Kiro reports 41pt, Finder 52pt — which is why a park is judged
from both the requested frame and the server's. See "Parking" in
[`src/layout/docs/strip.md`](../../../src/layout/docs/strip.md).

**Float comparison is explicit.** `SameAs` and `IsWithin` exist so no layout test
compares `f64` with `==` and no production code rounds by accident.

**One stretch tolerance.** Half a percent either way. A picture fits a frame, and a size change
is not a resize, by the same test (`within_stretch`), so `pictures` and `animation` cannot
disagree about which moves need a fresh picture.
