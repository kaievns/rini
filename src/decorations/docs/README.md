# `decorations` — what a window wears

macOS composites a hairline around every window and casts its shadow, and no capture carries either.
The overlay draws both over every tile, so a flying window wears what it wears on the desktop. This
feature is their shape, their paint by focus and how deep each sits against the picture. It names
no other feature: `animation` draws its decorations on each tile, `transitions` clips a baked
picture to its corner radius, and `pictures` draws the stand-in's corners at its fallback radius.

## What it owns

| | |
|---|---|
| `domain/shape.rs` | The corner radius a tile is drawn with (`radius`: its window's, measured, else `CORNER_RADIUS`, clamped to half the shorter side); the hairline's outline rect (`outline`); the shadow's silhouette (`silhouette`), the mask it is cut to (`shadow_mask_frame`, `SHADOW_REACH`) and the mask's hole (`shadow_mask`), floored so two of them interpolate |
| `domain/style.rs` | The hairline's paint: the white line's width (`LIGHT_PT`), the outline's stroke and black (`OUTLINE_PT`, `OUTLINE_ALPHA`); the shadow's `ShadowStyle`; and `Style::of`, the white line's alpha and the shadow for a window focused or not |
| `domain/stack.rs` | How deep each decoration sits against its picture: the shadow caster behind it (`SHADOW_Z`), the hairline in front of it (`HAIRLINE_Z`) |

Its `domain/` speaks rini-geometry's `Tile` space (`crates/rini-geometry/docs/README.md`): every rect
is in a tile's own points, origin at its top-left, the shadow mask's whole rect included, which starts
one `SHADOW_REACH` out. The layer that draws a rect converts it into its own points.

## Where it is drawn and measured

- **Drawn:** the overlay's tiles (`src/animation/platform/overlay.rs`), as "The hairline" in
  `src/animation/docs/animation-smoothness.md` describes.
- **The hairline, measured:** "The hairline is composited outside every capture" in
  `src/pictures/docs/capture-research.md`: the white line's width and alpha by focus, and the black
  outline just outside it.
- **The shadow, measured:** "Shadows are never in the surface" and "A layer shadow covers the whole
  layer" in `src/animation/docs/capture-overlay-research.md`: the two styles' fit, why the caster is
  masked, and why the mask reaches 70pt.

## Reading order

`domain/shape.rs` → `domain/style.rs` → `domain/stack.rs`.
