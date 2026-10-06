# `decorations` — what a window wears

macOS composites a hairline around every window and casts its shadow, and no capture carries either.
The overlay draws both over every tile, so a flying window wears what it wears on the desktop. This
feature is their shape, their paint by focus, how deep each sits against the picture, and the
layers each tile wears them on. It names no other feature, and `animation`, which draws its
decorations on each tile, is the one feature that names it.

## What it owns

| | |
|---|---|
| `domain/shape.rs` | The corner radius a tile is drawn with (`radius`: its window's, measured, else `CORNER_RADIUS`, clamped to half the shorter side); the hairline's outline rect (`outline`); the shadow's silhouette (`silhouette`), the mask it is cut to (`shadow_mask_frame`, `SHADOW_REACH`) and the mask's hole (`shadow_mask`), each at the corner it is given and floored so two of them interpolate |
| `domain/style.rs` | The hairline's paint: the white line's width (`LIGHT_PT`), the outline's stroke and black (`OUTLINE_PT`, `OUTLINE_ALPHA`); the shadow's `ShadowStyle`; and `Style::of`, the white line's alpha and the shadow for a window focused or not |
| `domain/stack.rs` | How deep each decoration sits against its picture: the shadow caster behind it (`SHADOW_Z`), the hairline in front of it (`HAIRLINE_Z`) |
| `platform/layers.rs` | `DecorationLayers`, the Core Animation layers one tile wears: the caster with its even-odd mask (`shadow`, `shadow_mask`), and the hairline holding the outline (`hairline`, `outline`). It makes them (`new`), names the two that ride the tile's frame (`frame_layers`), stacks them against the picture (`stack`), shapes them for a size at one corner (`corner_at`, `place`), paints them by focus (`show`), hides them for a companion (`set_hidden`), and installs and takes away what a resize animates on them, each under a key of its own (`install_resize`, `remove_resize`); the silhouette's and the mask's paths at a size and corner (`silhouette_path`, `shadow_mask_path`); `add_under` and `remove_under`, an animation under a key and its removal, which the overlay uses for every key of its own |
| `platform/layers.rs`: `Resize` | A tile's resize as Core Animation plays it: an animation of a key path through each value a closure maps from the sizes the tile passes through (`point`, `rect`, `number`, `path`), and whether a value changes on the way (`changes`). The overlay fills it from the tile's track, so `decorations` never names the score. A platform trait, not a port: it speaks Core Animation |

Its `domain/` speaks rini-geometry's `Tile` space (`crates/rini-geometry/docs/README.md`): every rect
is in a tile's own points, origin at its top-left, the shadow mask's whole rect included, which starts
one `SHADOW_REACH` out. The layer that draws a rect converts it into its own points.

## Where it is drawn and measured

- **Drawn:** on the overlay's tiles (`src/animation/platform/overlay.rs`), as "The hairline and the
  shadow, as drawn" below describes.
- **The hairline, measured:** "The hairline is composited outside every capture" in
  `src/pictures/docs/capture-research.md`: the white line's width and alpha by focus, and the black
  outline just outside it.
- **The shadow, measured:** "Shadows are never in the surface" and "A layer shadow covers the whole
  layer" in `src/animation/docs/capture-overlay-research.md`: the two styles' fit, why the caster is
  masked, and why the mask reaches 70pt.
- **The corner, measured:** "The corner, measured" below: the window's own curve against the one
  rini draws, and why every corner is clamped.

## The hairline and the shadow, as drawn

**The hairline.** No capture carries the hairline macOS composites around a window ("The hairline
is composited outside every capture" in `src/pictures/docs/capture-research.md`), so the overlay
draws it over every tile as a constant. The white line is the `hairline` layer, a `CALayer` with a
1pt border (`LIGHT_PT`) of white at 0.20 on the focused window and 0.18 on the rest (`Style::of`),
the fit measured there in dark mode; light mode is not measured. The outline is a `CAShapeLayer`
stroking 0.5pt (`OUTLINE_PT`) of black at 0.9 along a rounded rect a quarter point outside the
window (`shape::outline`), so the stroke covers the half point just past its edge; a border layer
at a half-point origin landed one pixel too far out ("The handover, measured" in
`src/animation/docs/capture-overlay-research.md`). Both round to the window's radius as `pictures`
measured it (`OverlayTile::radius`, which the flight engine fills from `Pictures::knowledge`), else
10pt, never past half the shorter side (`DecorationLayers::corner_at`, over `shape::radius`), the
corner the overlay cuts a clipped picture at too. The hairline is a layer beside the
picture and the caster, an eighth step in front of the picture and under a companion's quarter
step (`HAIRLINE_Z`), with the outline as its child: inside the picture, a corner clip would cut the
outline away (`a_corner_clip_leaves_the_hairline_whole` in the overlay). It rides every move with
the picture; a resize carries its bounds on the tile's own track and animates its corner and the
outline's path between the two sizes (`a_resize_carries_the_hairline_on_the_tiles_timing`); a snap
moves it with the tile. Both are laid after the tile's frame, on every placement
(`DecorationLayers::place`), or a pooled layer's autoresizing carries the old size. Companions wear
none.
`TileOverlay::set_tile_focused` switches one tile's white line and shadow depth in a hard cut, as
that window's turned picture comes in mid-flight; the prototype set the white line only at
install. When each tile turns is the coordinator's (`Command::Focus`).

**The shadow.** A caster behind the picture (`shadow`, `SHADOW_Z`) holds no contents, only the
shadow of the window's silhouette (`shape::silhouette`) in the style focus picks (`Style::of`). An
even-odd `CAShapeLayer` masks it (`shadow_mask`, `shape::shadow_mask`) to what lies outside the
window, out to `SHADOW_REACH`. The silhouette and the mask's hole round at the hairline's corner
(`a_resize_carries_one_corner_on_every_piece` in the overlay). `DecorationLayers::place` keeps the
size and corner it last shaped them for and shapes them again when either differs, so a pooled tile
dressed for a window of another radius casts that window's corner at the same size
(`a_pooled_tile_dressed_for_a_window_of_another_radius_rebuilds_its_shadow_at_the_same_size`). Why the shadow is drawn at all, why on a masked caster rather than
the picture, and how far the mask reaches: "Shadows are never in the surface" and "A layer shadow
covers the whole layer" in `src/animation/docs/capture-overlay-research.md`.

## The corner, measured

Every piece a tile wears rounds to one corner, `DecorationLayers::corner_at` over `shape::radius`:
the hairline, the outline, the shadow's silhouette, the hole its mask cuts and a clipped picture.
Each is drawn separately, so any two that differ show as a step at the corner. A resize carries the
one corner on every piece at each of its keys, the picture's under a key of the overlay's own.

**The window's corner is continuous, and rini draws a circular one.** Measured offline 2026-10-06
on macOS 26.7.1: the four corners of `screencapture -o` captures of 5 windows, each fitted to Core
Animation's own render of a layer at radius 6-40pt in 0.25pt steps, on each curve. The error is in
alpha, 0 to 1, over the corner, at the radius the window server reports:

| Window | Radius | Continuous, mean / max | Circular, mean / max |
|---|---|---|---|
| Finder | 26pt | 0.0033 / 0.357 | 0.0079 / 0.906 |
| Chrome | 20pt | 0.0034 / 0.294 | 0.0079 / 0.804 |
| Obsidian | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Zoom | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Ghostty | 16pt | 0.0389-0.0502 / 0.275 | 0.0434-0.0547 / 0.690 |

Both curves fit best at the reported radius plus 0.25pt. Ghostty fits both loosely; why is not
known. Every piece rini draws is circular: Core Animation's default for a layer's `cornerRadius`,
and the only curve `CGPath::with_rounded_rect` draws. So the pieces match each other exactly and
miss the window's corner by under one device pixel: the worst pixel is 0.57-0.91 off in alpha, the
mean under 0.01 on all but Ghostty. A continuous corner would need a path builder of rini's own for the outline, the silhouette
and the mask's hole, which have no continuous rounded rect in Core Graphics; it is not built.

**A continuous corner runs 1.38-1.42 × r along each edge**, against r for a circular one, in Core
Animation's render at 2x: 16.5pt at 12pt, 22pt at 16pt, 28pt at 20pt, 37pt at 26pt.

**Core Animation does not clamp a radius past half a side.** A 20x100 layer at radius 16 renders
unlike the same layer at radius 10: up to 255 levels apart in alpha, on either curve, as are a
12pt-wide layer at 26 and a 0.5pt-wide one at 16 (254). So `shape::radius` clamps every corner to
half the tile's shorter side before any piece takes it, and a resize carries the picture's clamped
corner on its track rather than leaving the clip to Core Animation.

Data in `docs/data/2026-10-06-radii/`: `p8.swift` and `p8.txt`, the fit, whose `win_` names are
window ids (56 Ghostty, 2412 Finder, 97 Chrome, 60 Obsidian, 3851 Zoom); `reach.swift` and
`reach.txt`; `clamp.swift` and `clamp.txt`. The captures are not kept.

## Reading order

`domain/shape.rs` → `domain/style.rs` → `domain/stack.rs` → `platform/layers.rs`.
