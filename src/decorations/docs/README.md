# `decorations` — what a window wears

macOS composites a hairline around every window and casts its shadow, and no capture carries either.
The overlay draws both over every tile, so a flying window wears what it wears on the desktop. rini
also draws niri's focus ring and border, which macOS has no form of: on the tiles through a flight,
and at rest in windows of its own. This feature is their shape, their paint by focus and attention,
who wears the ring, how deep each sits against the picture, the layers each tile wears them on, and
the windows that draw the bands at rest. It reads `windows` (levels, sub-levels, radii, frames,
whether a window is ordered in, which applications ask for attention) and `displays` (the window
server's frames and orders). `animation` draws its decorations on each tile, `transitions` carries
what each window wears to its tile, reads from it which ring a tile shows until its turn and holes
the overlay as far as its bands reach (`shape::outset`), and the application decides it, fills the ring's and the
border's settings and the window rules from the config, folds the rules for each window, hands the
layout the width each tiled window gives up to its border, and wires the rest windows.

## What it owns

| | |
|---|---|
| `domain/shape.rs` | A band outside the window, the ring's or the border's: its outer rect and corner (`band`), a width in device pixels (`device_width`), the width each band a decoration has is laid at, the ring reaching in under the border (`widths`), how far a window's bands reach outside it (`outset`), and the window that draws them at rest, cut to its display (`rest_frame`). The corner radius a tile is drawn with (`radius`: its window's, as the window server reports it, else `FALLBACK_RADIUS`, clamped to half the shorter side); the hairline's outline rect (`outline`); a rect with continuous corners as path segments (`rounded_rect`), each reaching `CONTINUOUS_REACH` radii along its sides; the shadow's silhouette (`silhouette`), the mask it is cut to (`shadow_mask_frame`, `SHADOW_REACH`) and the mask's hole (`shadow_mask`), each at the corner it is given |
| `domain/style.rs` | The hairline's paint: the white line's width (`LIGHT_PT`), the outline's stroke and black (`OUTLINE_PT`, `OUTLINE_ALPHA`); the shadow's `ShadowStyle`; a band's width and its three paints, and which one it is drawn in (`Band`, `paint`); what a window wears that rini draws, its ring, its border, its rule's corner, which the bands round to ahead of the window server's, and whether it is drawn urgent (`Decoration`, `corner`), and through a pass, with whether it wore and wears the ring (`Worn`); and `Style::of`, the white line's alpha, the shadow, the ring's paint for a window urgent, focused or neither, wearing the ring or not, and the border's the same way. Who wears the ring: each workspace's active window, the focused window on its own; and who is drawn urgent: every window of an application asking for attention while no window of it holds focus (`Rings`) |
| `domain/paint.rs` | `Colour`, sRGB with its alpha, as the config writes a colour; `Paint`, a band's fill, a colour or a `Gradient`, niri's: two colours along an angle, mixed in a `ColourSpace` (with its `Hue` for `oklch`) as niri's shader mixes them (`colour_at`); its `Axis` over a band of a size, the start and end points Core Animation takes (`axis`), and whether that moves with the size (`follows_size`); and the `Stop`s Core Animation draws it through (`stops`, to `TOLERANCE` at `SAMPLES` steps) |
| `domain/rules.rs` | What a `window-rule` says a window wears (`DecorationRule`, a `BandRule` for the ring and one for the border, and `geometry-corner-radius`), matched through the open-time rules' matcher (`windows::domain::rules::applies`); the layout's settings with every rule that applies laid over them in file order (`fold`); a window's decoration either side of focus, its active paint as the rules give it while it holds focus and the rest as they give it while it does not, urgent or not (`decoration`); the border a tiled window gives out of its slot, in whole device pixels of its display (`border_width`), and whether a rule can give any other than the layout's (`shape_borders`) |
| `domain/rest.rs` | The bands at rest: what the reactor sends (`RestModel`, a `RestWindow` per managed window with what `Held` it, its bands' paint and how far they reach), whether its bands show (`shown`), whether the window server has a window ordered out (`ordered_out`), when a park takes effect (`Parking`), what each turn makes of the model, the window server's frames and orders and a lift, and which windows show then (`Rest`, a `Tracked` per window: the window focus came to, the parks a lift on its display takes, the frames read again), how many rest windows the pool holds (`Rest::pool_size`), which rest window draws each window (`assign`), and which are ordered above their windows again this turn (`to_reorder`) |
| `domain/stack.rs` | How deep each decoration sits against its picture: the shadow caster behind it (`SHADOW_Z`), the hairline in front of it (`HAIRLINE_Z`) |
| `settings.rs` | `DecorationSettings`: `layout › focus-ring` and `layout › border`, each on or off and its `Band`, niri's by default but for the ring's width, and the window rules' `DecorationRule`s in file order; `draws_bands`, whether any window can wear a band |
| `platform/layers.rs` | `DecorationLayers`, the Core Animation layers one tile wears: the caster with its even-odd mask (`shadow`, `shadow_mask`), and the hairline holding the outline and the bands (`hairline`, `outline`, `bands`). `Bands`, the ring and the border in front of it, worn, laid, painted and carried through a resize together, on a tile and at rest alike; `Band`, one band's layer: a `CAGradientLayer` drawing a colour as its border on the continuous curve, or a gradient masked to a border of the same shape, laid around a window of a size at its corner (`place`), painted or hidden (`paint`), and carried through a resize, its mask and a gradient's axis with it; each gradient's stops as Core Animation takes them, the last 16 kept (`drawn`). It makes them (`new`), names the two that ride the tile's frame (`frame_layers`), stacks them against the picture (`stack`), shapes them for a size at one corner (`corner_at`, `place`), paints them by focus (`show`), draws them at a display's scale with a decoration's bands (`wear`), and installs and takes away what a resize animates on them, each under a key of its own (`install_resize`, `remove_resize`); the silhouette's and the mask's paths at a size and corner (`silhouette_path`, `shadow_mask_path`); `add_under` and `remove_under`, an animation under a key and its removal, which the overlay uses for every key of its own; `round_continuous`, a layer's corner on the window's curve, which the hairline and every picture wear |
| `platform/rest.rs` | The rest windows: borderless, click-through, non-activating panels, pooled, each holding one `Bands` and ordered directly above the window it decorates (`RestBorders`); the main-thread actor that takes the reactor's model, the window server's frames and orders, a turn at a time (`RestActor`), and what the renderer calls before each lift (`OnLift`) |
| `platform/layers.rs`: `Resize` | A tile's resize as Core Animation plays it: an animation of a key path through each value a closure maps from the sizes the tile passes through (`point`, `rect`, `number`, `path`), and whether a value changes on the way (`changes`). The overlay fills it from the tile's track, so `decorations` never names the score. A platform trait, not a port: it speaks Core Animation |

Its `domain/` speaks rini-geometry's spaces (`crates/rini-geometry/docs/README.md`): what a window
wears in the `Tile` space, every rect in a tile's own points, origin at its top-left, the shadow
mask's whole rect included, which starts one `SHADOW_REACH` out; and where a band is drawn at rest
in the `Screen` space, the window server's. The layer that draws a rect converts it into its own
points, and the rest windows convert the window server's frames at their edge.

## Where it is drawn and measured

- **Drawn:** on the overlay's tiles (`src/animation/platform/overlay.rs`), as "The hairline and the
  shadow, as drawn" below describes; the ring and the border also at rest, in rini's own windows
  ("At rest").
- **The hairline, measured:** "The hairline is composited outside every capture" in
  `src/pictures/docs/capture-research.md`: the white line's width and alpha by focus, and the black
  outline just outside it.
- **The shadow, measured:** "Shadows are never in the surface" and "A layer shadow covers the whole
  layer" in `src/animation/docs/capture-overlay-research.md`: the two styles' fit, why the caster is
  masked, and why the mask reaches 70pt.
- **Gradients, measured:** "Gradients" below: how Core Animation draws a gradient's axis and mixes
  its stops offscreen, and why each gradient is drawn through stops.
- **The corner, measured:** "The corner, measured" below: where a window's radius comes from and
  why 16pt where none is reported, the window's own curve against the one rini draws, and why every
  corner is clamped.

## The hairline and the shadow, as drawn

**The hairline.** No capture carries the hairline macOS composites around a window ("The hairline
is composited outside every capture" in `src/pictures/docs/capture-research.md`), so the overlay
draws it over every tile as a constant. The white line is the `hairline` layer, a `CALayer` with a
1pt border (`LIGHT_PT`) of white at 0.20 on the focused window and 0.18 on the rest (`Style::of`),
the fit measured there in dark mode; light mode is not measured. The outline is a `CAShapeLayer`
stroking 0.5pt (`OUTLINE_PT`) of black at 0.9 along a rounded rect a quarter point outside the
window (`shape::outline`), so the stroke covers the half point just past its edge; a border layer
at a half-point origin landed one pixel too far out ("The handover, measured" in
`src/animation/docs/capture-overlay-research.md`). Both round to the window's radius as the window
server reports it (`OverlayTile::radius`, which the flight engine reads as it composes the flight),
else 16pt, never past half the shorter side (`DecorationLayers::corner_at`, over `shape::radius`), the
corner the overlay cuts a clipped picture at too. The hairline is a layer beside the picture and the
caster, an eighth step in front of the picture (`HAIRLINE_Z`), with the outline as its child: inside
the picture, a corner clip would cut the outline away (`a_corner_clip_leaves_the_hairline_whole` in
the overlay). It rides every move with the picture; a resize carries its bounds on the tile's own
track and animates its corner and the outline's path between the two sizes
(`a_resize_carries_the_hairline_on_the_tiles_timing`); a snap moves it with the tile. Both are laid
after the tile's frame, on every placement (`DecorationLayers::place`), or a pooled layer's
autoresizing carries the old size.
`TileOverlay::set_tile_focused` switches one tile's white line, shadow depth, ring and border in a
hard cut, as that window's turned picture comes in mid-flight; the prototype set the white line only
at install. When each tile turns is the coordinator's (`Command::Focus`).

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

## The focus ring and the border, as drawn

niri's ring is a band outside the window, worn by each workspace's active window (`Rings`, in
`domain::style`): in `urgent` paint on a window drawn urgent, `active` on the window holding focus,
`inactive` on the others. On a tile it is the `ring` layer, a `Band`, the hairline's child after the
outline, so it is drawn in front of the picture, the hairline and the outline, and rides the
hairline through every move. In a colour it is the layer's border, `width` wide, and in a gradient
the layer's gradient masked to a border of the same shape ("Gradients"), round on the continuous
curve at the window's corner plus its width (`shape::band`), so its inner edge is the window's own
corner, on the curve every other piece takes; a border's inner edge is its outer one `width` in. The
corner is clamped to half the band's shorter side, and a square window keeps a square band, as
niri's `expanded_by` keeps 0 at 0. Its width is whole device pixels of the display, never below one
(`shape::device_width`, niri's rounding), laid as the tile is dressed (`DecorationLayers::wear`). A
resize carries its bounds on the tile's own track, and its corner while it changes; its position,
the window's corner less its width, holds. Its paint turns with the hairline (`Style::of`,
`TileOverlay::set_tile_focused`), at the turn `transitions::domain::lifecycle::drawn_ringed` gives
it.

niri's border is the `border` layer, a second `Band`, the hairline's last child, so it is drawn in
front of the ring, as niri draws it (`niri:src/layout/tile.rs:1279-1341`). Every window its
decoration has one for wears it, in `urgent` paint while it is drawn urgent, `active` while it holds
focus and `inactive` otherwise (`Style::of`), so it turns with the hairline as focus moves, on its
own focus alone. Its inner edge is the window's corner and its outer one that corner grown by its
width, as the ring's. With both on, the ring lies outside the border: its outer rect is the window
grown by both widths, and its layer reaches in under the border to the window's edge
(`shape::widths`), so ring and border share no seam, the corner seam with the window is the border's
alone, and the ring shows past the border by its own width. A border colour with alpha shows the
ring through it there; niri's ring stops at the border instead. A tiled window gives the border its
width out of its slot, so the band fills the window's place in the strip and the strip is laid out
the same with borders on or off (`layout::domain::tile`, "A border takes its width out of the
window" in `src/layout/docs/strip.md`). The slot gives up the width the band is drawn at, whole
device pixels of the window's display (`rules::border_width`), so a width between them, 1.3pt at 2x,
takes 1.5pt out of the slot as the band draws 1.5pt. A floating window keeps its frame and wears
the border outside it, where niri grows the tile instead.

A window is drawn urgent while its application asks for attention and no window of that application
holds focus (`Rings::urgent`, against the focus the ring is read against: the key window at rest,
the focus a pass ends with on a tile). macOS asks per application, so every window of it is urgent,
where niri marks one window. The `windows` attention reader hears the ask at most a second late
(`src/windows/docs/attention.md`); the application taking focus clears it at once. Urgent beats
active beats inactive, as niri picks the paint (`Band::paint`), and since no window of the
application holding focus is urgent, the focused window always wears `active`. A tile keeps the
urgency its pass gave it: an ask that starts or ends during a flight shows at the lift.

## Gradients

A band's paint is a colour or niri's gradient (`paint::Paint`): `active-gradient` and
`inactive-gradient` beat the colour beside them, and a colour written later clears the gradient, as
niri's `merge_color_gradient` has it. A gradient is drawn by the band's own layer, a
`CAGradientLayer`, masked to a `CALayer` border of the band's width, corner and continuous curve:
the same contour a colour draws, with no path and no bitmap. A colour stays the layer's border, with
no gradient and no mask. A mask makes Core Animation draw the band offscreen first; what that costs
per frame is not measured.

**The axis.** niri lays a gradient over the band's own outer rect, the line through its centre at
`angle` long enough that the corners lie on the lines across its ends, every line across at right
angles in points, as CSS does (`niri:src/render_helpers/border.rs:160-175`). Core Animation takes
the start and end points in the layer's unit space and draws the lines across at right angles
there: offscreen, `(0, 0)` to `(1, 1)` on a 200×100 layer drew the top-right corner 147, the
bottom-left 145 and the centre 146
([`axis.txt`](../../../docs/data/2026-10-08-gradients/axis.txt)), the corners on one line as unit
space has it. So `Gradient::axis` stretches the axis by the size to put the lines at right angles in
points (`a_gradient_is_drawn_at_niris_angle_in_points`). In a flipped tree, as every rini tree is,
unit `(0, 0)` is the top-left, offscreen. Off a right angle the axis depends on the band's shape, so
a resize carries it on the tile's track; painted mid-resize at another angle, the band keeps the
axis of the size it was laid at until the next placement. `relative-to="workspace-view"` is refused: one gradient across
the display needs each band masked over a still gradient, riding its tile through a flight.

**The colours.** niri's shader mixes the two colours premultiplied, in `srgb`, `srgb-linear`,
`oklab` or `oklch` with its hue path, taking sRGB's curve as a 2.2 power (`colour_at`, after
`niri:src/render_helpers/shaders/border.frag`). niri leaves a colour mixed out of sRGB in `oklab` to
the GPU's power of a negative; rini clamps it into sRGB, as niri does for `oklch`. Core Animation
mixes between stops in a space of its own, unpremultiplied
([`interpolation.txt`](../../../docs/data/2026-10-08-gradients/interpolation.txt), offscreen):

| black to white, offscreen | middle |
|---|---|
| Core Animation, into an sRGB context | 147 |
| Core Animation, into a linear context, as sRGB | 147 (74 linear) |
| sRGB's values mixed | 128 |
| linear light mixed (niri's `srgb-linear`) | 186 |

147 is a 1.8 power, and the destination's space does not move it. Red at alpha 1 to blue at alpha 0
drew `(146, 33, 150)` at half alpha in the middle, the colour of the opaque gradient's middle: each
channel and the alpha mixed alone. Its ends drew sRGB red as `(255, 38, 1)`, where a solid layer of
that red drew `(255, 0, 0)`, so the offscreen renderer is no measure of a gradient's colours on
screen. So every gradient is drawn through stops, `srgb`'s too: `Gradient::stops` places them over
4096 steps, about a device pixel apart on a band around a full-screen window at 2x, each segment as
long as the colour drawn along it stays within half a level of niri's both mixed in sRGB's values
and in linear light. A 1.8 power lies between the two, and holds within one level over 42 hard
cases, black to white, primaries, complements, fades, in every space and hue path
(`the_stop_count_keeps_each_space_within_one_level`); two stops alone miss by more than 10 levels.
Those cases take 3 to 63 stops, and placing them took 1.5 to 6.1ms in a release build
([`stops.csv`](../../../docs/data/2026-10-08-gradients/stops.csv)), so the last 16 gradients' stops
are kept (`layers::drawn`). How Core Animation mixes on screen, in the window's colour space, is not
measured.

## At rest

Between flights macOS draws each window, and rini draws the ring and the border around the windows
that wear them in windows of its own, one above each, holding `Bands` laid exactly as a tile's
(`a_tile_wears_its_ring_as_dressed_and_as_focus_turns`, `every_band_rounds_on_the_continuous_curve_at_its_outset`).

**The windows.** Borderless, non-activating `NSPanel`s on rini's main connection, click-through,
clear, with no shadow, collection `[.moveToActiveSpace, .transient, .ignoresCycle,
.fullScreenNone]`: `.transient` has Mission Control, App Exposé and Show Desktop hide a rest window
before the reactor's alpha 0 reaches it, where a managed one would be laid out as a window of its
own, and `.stationary` would leave it over the thumbnails. Each holds a flipped view whose layer
holds the band at its window's frame. A rest window's frame is the band's outer rect cut to its
window's display (`shape::rest_frame`), so a ring at the display's edge draws nothing onto the next
one. Rest windows are made as a model arrives, never from a frame or a lift: the pool holds one for
every window that wears a band and could show it, held by no more than a park, and one per display
at least (`Rest::pool_size`), and each window whose band shows takes one from it (`rest::assign`).
So the ring moving with focus and a workspace switch, whose leaving and arriving windows show
together for a moment, make none; the windows rini manages as it launches are pooled then, and a
window opened later is pooled as the model naming it arrives, at the press of its flight. The
overlay's first show cost 112ms against 14ms to order it in; what making a 1pt rest window costs is
not measured. A window that shows nothing keeps its rest window at alpha 0 for the next, and the
pool never shrinks.

**Shown by alpha, never ordered out.** Ordering a window in costs 13.94ms and out 13.07ms at the
median, against 0.36ms and 0.34ms for an alpha change ("Toggle alpha, do not order the window in
and out" in `src/animation/docs/capture-overlay-research.md`). So a rest window is ordered in once,
as it is made, and every hide after is its alpha.

**Ordering a rest window.** Probe P1 (2026-10-06, an idle machine,
[`docs/data/2026-10-06-drags/p1-order.jsonl`](../../../docs/data/2026-10-06-drags/p1-order.jsonl))
ordered a 1pt panel against another process's window and read the window list after. AppKit's
`orderWindow:NSWindowAbove relativeTo:` held the panel directly above the other window;
`SLSOrderWindow` against it did nothing, and is no longer declared. Against a window at sub-level 1,
the panel stayed below until it took that window's level and sub-level too (the `level_copy` row:
one place below before the copy, above after). So a rest window takes its window's level and
sub-level as `StackPlace::of` reads them, through `SLSSetWindowLevel` and `SLSSetWindowSubLevel`,
which act only on the caller's own windows (`window_server::take_stack_place`), and then AppKit
orders it above. The sub-level is read by the Mach port rini already reads other apps' windows with,
not by `SLSGetWindowSubLevel` as P1 read it; that the two answer alike is inferred. After each of 10
raises of the other window, its 808 came 9.4-18.0ms after the raise, and the panel, ordered again
from the 808, was back on top 7.4-25.4ms after it: 8 of 10 within 16.7ms, p90 25.4ms. The call took
0.57-6.02ms, about 1.4ms typical. P1 ran against a scratch helper only. So for 1-2 frames after
another app raises its window, that window sits above its own bands: the window covers none of
them, which lie outside it, but its shadow does, and so does any window it rose past that overlaps
them. A rest window is ordered again when the window server reorders its window or orders it
back in (808, 815), when it is given a window, and when focus comes to its window, each once a turn
(`rest::to_reorder`).

**A click-through band is never hit.** Probe P5 drew a click-through band around a window and found
`SLSFindWindowAndOwner` 2pt inside to 4pt outside its right edge returning the window with the band
directly above it, every time, and every posted drag resized it, with the band and without. Its pixel
comparison is confounded: its two phases compared two different looks of the target window, phase
one's posted presses having made the helper's panel key; its straight-edge rows show the band leaves
the window's inner hairline alone.

**The corner.** P5's band was a `CALayer` border on the default, circular curve, whose inner edge
lies outside a continuous window corner everywhere: it left a crescent showing what is beneath, up
to 0.67 of a device pixel's coverage per pixel, 21.6 pixel-equivalents per corner at 2x for a 16pt
corner and a 4pt band (offscreen). A band on the continuous curve, its inner contour the window's own
corner, leaves at most 0.24 of a device pixel's coverage per pixel at its seam, 9.3
pixel-equivalents per corner (offscreen render, not the screen). Its inner edge is a border's outer
edge `width` in, so the outer corner is `corner + width` (`shape::band`).

**Following.** A rest window takes its window's frame from the window server's frames
(`src/displays/docs/live-frames.md`): 806 and 807 saw every frame a 1ms poll saw during Kai's
drags, n=1165, p90 0.08ms behind it, in Ghostty only. The actor takes what queued each turn, the
latest frame of each window. Under the overlay the rest windows keep following; none is hidden for a
flight.

**Parked comes from rini's layout.** macOS clamps a parked Chrome or Finder window so 52pt of it
shows, past the 40pt `is_off_screen` allows (`PARK_CLAMP_PX` in `crates/rini-geometry/src/lib.rs`), so
a band read off the window's frame would be drawn around the sliver. The reactor sends a window as
parked when its workspace is not the one its display shows or its strip has it off the viewport. A
park, and a window brought back, takes effect at the window's next frame, at a lift on its display,
or 250ms after the reactor said so (`rest::Parking`, `Rest::lift`): rini parks a window by moving it,
after the overlay is up, and a band that went at the press would leave its window bare before the
overlay covered it.

**The handover.** At the press nothing changes at rest; the overlay covers its display at level 18,
so a rest window under it is unseen once it is up, as its window is. At the lift the renderer calls
the rest windows before it hides the overlay (`Renderer::play`'s steps), and every one on that
display is put at its window's frame as the window server has it, every park that waited taken, and
drawn as it is then, in the same main-thread turn as the hide. That no frame shows a band out of
place is inferred, not measured. The overlay is holed over each band as over each window ("Holes" in
`src/animation/docs/animation-smoothness.md`), so the rest window keeps painting under it, inferred
as needed.

Not measured: what a rest window costs the window server in memory and the main thread in time, per
window and under an 808 burst as a strip raise makes one (probe P13, which needs an idle machine
and was not run); whether a full-size clear panel allocates a full-size backing, which would want
its shape cut to its band instead; whether a re-order brings a `.moveToActiveSpace` window onto the
Space shown; how a rest window moves through macOS's own Space animation, which slides a window
that is not `.stationary`; and that Mission Control, App Exposé and Show Desktop hide every rest
window, the shown ones and those at alpha 0, from the start of their animation, which `.transient`
is documented to give and was not seen on screen.

## The corner, measured

Every piece a tile wears rounds to one corner, `DecorationLayers::corner_at` over `shape::radius`:
the hairline, the outline, the shadow's silhouette, the hole its mask cuts and a clipped picture.
Each is drawn separately, so any two that differ show as a step at the corner. A resize carries the
one corner on every piece at each of its keys, the picture's under a key of the overlay's own.

**The radius is the window server's.** A window rule's `geometry-corner-radius` goes ahead of it
for the ring and the border alone, on the tile and at rest (`Decoration::corner`,
`DecorationLayers::wear`); the hairline, the shadow and a clipped picture keep the server's, since
at rest macOS draws its own at the window's real corner. `SLSWindowIteratorGetCornerRadii` answers four corners in
points for any window a query names, on screen or not; `windows::domain::info::reported_radius`
takes the largest above 0. Read-only probes, macOS 26.7.1, 2026-10-06:

| Read | Result |
|---|---|
| 26 standard windows on screen | 16pt ×12 (Code, Slack, Ghostty, Zen, Kiro), 26pt ×9 (Finder, Messages, Outlook, Books, System Settings), 12pt ×3 (Zoom, Obsidian), 20pt ×2 (Chrome) |
| radius against size | one radius per app at every size: Ghostty 594-1148pt wide, all 16 |
| parked and other-workspace windows, off screen | answered: Ghostty 16, Zoom 1708x1065 12, Kiro CLI 16 |
| all 181 windows, every level | 0 null answers, 0 windows with unequal corners |
| 0pt answers | 89 of the level-0 windows: menu-bar strips (1728x33), popovers, AutoFill, cursor helpers, 0x0 helpers, and one off-screen "Privacy & Security" window, 723x884 |
| one query of 12 windows and the radii of each, n=300 | p50 0.049ms, p90 0.059ms, max 7.8ms (one read) |

So the flight engine reads every tile's radius in one query as it composes the flight, and an
entrance's as its first picture lands, since a window has drawn by then. A window that is not yet
pictured, or parked since before rini started, rounds right on its first flight. A 0 answer is
taken as none, not as square; every 0 seen is a window rini does not fly, though that is not
checked against the windows rini manages. Unequal corners round at the largest; none were seen,
and Core Animation takes one radius a layer. The call is resolved at run time, so a macOS without
it still launches and rounds every window at the fallback; no such macOS was tried.

**16pt where the server reports none**, `FALLBACK_RADIUS`: the commonest radius of a standard
window, 12 of 26. 10pt, which rini drew before, matches none.

**The radius measured from a picture's alpha is gone.** rini once learned a window's radius from
where each row of its first on-screen capture's top-left corner turned opaque. On `screencapture
-o` images of 5 windows that formula reads 1pt above the server every time: 17 against 16, 27
against 26, 21 against 20, and 12.8 against 12 twice. The bias is the formula's own threshold,
not the curve: both curves fit best at the server's radius plus 0.25pt (below). It saw only
windows captured on screen, learned once and was never corrected.

**The window's corner is continuous, and so is rini's.** Measured offline 2026-10-06 on macOS
26.7.1: the four corners of `screencapture -o` captures of 5 windows, each fitted to Core
Animation's own render of a layer at radius 6-40pt in 0.25pt steps, on each curve. The error is in
alpha, 0 to 1, over the corner, at the radius the window server reports:

| Window | Radius | Continuous, mean / max | Circular, mean / max |
|---|---|---|---|
| Finder | 26pt | 0.0033 / 0.357 | 0.0079 / 0.906 |
| Books (2 windows) | 26pt | 0.0031 / 0.357 | 0.0074 / 0.906 |
| Chrome | 20pt | 0.0034 / 0.294 | 0.0079 / 0.804 |
| Obsidian | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Zoom | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Ghostty | 16pt | 0.0389-0.0502 / 0.275 | 0.0434-0.0547 / 0.690 |

Both curves fit best at the reported radius plus 0.25pt. Ghostty fits both loosely; why is not
known. A circular corner misses the continuous one by at most 0.72 device pixels across the side at
26pt, 22-26pt from the corner, and 0.5 at 16pt; at the diagonal the two agree within 0.17. The
handover check keeps 16pt from each corner, so on a 26pt window it reads that gap as 0.6-1.2 levels
of `edge_in` and 0.2-0.5 of `edge_out`, and on a 16pt one as 0.07-0.08, in an offscreen model of the
check over the Books and Finder captures.

So every piece rounds on the continuous curve: the hairline and a clipped picture through the
layer's `cornerCurve`, the outline, the silhouette and the mask's hole through
`shape::rounded_rect`, which Core Graphics has no continuous form of. Its curve is UIKit's
continuous rounded rect, three cubics a corner, the one SwiftUI's continuous `RoundedRectangle`
draws. Rendered at 2x, no pixel of the silhouette or the mask's hole sits farther from Core
Animation's corner than Core Animation's own circular clip sits from Core Graphics' circular fill,
64 alpha levels (`every_path_follows_core_animation_s_corner_at_every_size`). At 8x its edge
sits within 0.25 of an 8x pixel of Core Animation's at 12-40pt, the same as the circular pair; with
the middle cubic's control at 0.18603501, as first built, it sat 2.45 off at 26pt, 0.61 device
pixels.
Where the corner's reach, 1.528665 radii (`shape::CONTINUOUS_REACH`, Core Animation's
`cornerCurveExpansionFactor`), passes half the tile's shorter side, Core Animation still draws the
corner at its full reach, the sides running back over each other, and so does
`shape::rounded_rect`: at 2x it is within the same 64 levels on tiles 1-80pt wide at 0.5-26pt
(`narrow`, and the test above). So every piece rounds on the one curve at every
size, and every path is the same 18 segments, so any two interpolate. Only a tile narrower than
80pt at 26pt, or 49pt at 16pt, passes the reach: an entrance growing from no width. There the
outline's stroke runs along the sides where they run back, flat over the corners' edge, which
dips at most 0.25pt below the side at the tile's middle: 127 pixels at 2x on a 52x400 tile at 26pt,
63 at 60pt wide, 3 at 70pt (`lid`).

**A continuous corner leaves each side 1.528665 × r from the corner**, against r for a circular
one, and reads as off the side from 1.38-1.42 × r in Core Animation's render at 2x, where a row's
alpha first drops below full: 16.5pt at 12pt, 22pt at 16pt, 28pt at 20pt, 37pt at 26pt.

**Core Animation does not clamp a radius past half a side.** A 20x100 layer at radius 16 renders
unlike the same layer at radius 10: up to 255 levels apart in alpha, on either curve, as are a
12pt-wide layer at 26 and a 0.5pt-wide one at 16 (254). So `shape::radius` clamps every corner to
half the tile's shorter side before any piece takes it, and a resize carries the picture's clamped
corner on its track rather than leaving the clip to Core Animation.

Data in `docs/data/2026-10-06-radii/`: `radii_all.m` and `radii_all.txt`, every window's radii and
the query's cost, whose `on=1` rows are the on-screen windows of the first two rows; `radii.m` and
`radii.txt`, an earlier on-screen read of 25 windows, each app at the same radius;
`radius-compare.csv`, the alpha formula against the server; `p8.swift` and `p8.txt`, the fit, whose
`win_` names are window ids (56 Ghostty, 2412 Finder, 97 Chrome, 60 Obsidian, 3851 Zoom);
`reach.swift` and `reach.txt`; `clamp.swift` and `clamp.txt`. The captures are not kept. In
`docs/data/2026-10-06-corners/`, each built with `squircle.swift`: `pathfit`, the two curves' gap
and the path against Core Animation's; `middle`, the middle cubic's control against Core
Animation's at 8x, standalone; `booksfit`, Books windows 5489 and 5299 on each curve;
`narrow`, the path where the reach passes half the side; `lid`, the outline's stroke there; `handover-model` and `sim-*.txt`, the
handover check over a capture at each curve, and `handover-offsets`, with the picture shifted.
`booksfit`'s hand-path column and the `path` rows of `sim-*.txt` drew the middle cubic at
0.18603501; the captures are gone, so they were not run again, and the `border model` rows, on Core
Animation's own curve, read the same `edge_in` and `edge_out` as the `path` rows.

## Reading order

`domain/shape.rs` → `domain/paint.rs` → `domain/style.rs` → `domain/rules.rs` → `domain/stack.rs` →
`domain/rest.rs` → `settings.rs` → `platform/layers.rs` → `platform/rest.rs`.
