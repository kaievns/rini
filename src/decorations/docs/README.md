# `decorations` — what a window wears

macOS composites a hairline around every window and casts its shadow, and no capture carries either.
The overlay draws both over every tile, so a flying window wears what it wears on the desktop. rini
also draws niri's focus ring and border, which macOS has no form of: on the tiles through a flight,
and at rest in windows of its own. This feature is their shape, their paint by focus and attention,
who wears the ring, how deep each sits against the picture, the layers each tile wears them on, and
the windows that draw the bands at rest.

It reads `windows` (a place's frames, the calls a rest window takes a level and sub-level with, the
window rules' matcher, which applications ask for attention) and `displays` (`ServerRead`, what its
frames thread reads in the order asked: frames, orders with their levels, introductions with radius
and ordered-in; and `Following` to ask it). `animation` draws its decorations on each tile.
`transitions` carries what each window wears to its tile, reads which ring a tile shows until its
turn, and holes the overlay as far as its bands reach (`shape::outset`). The application decides
who wears the ring, fills the settings and window rules from the config, folds the rules for each
window, hands the layout the width each tiled window gives up to its border, and wires the rest
windows.

## What it owns

| | |
|---|---|
| `domain/shape.rs` | A band: its outer rect and corner (`band`), widths in device pixels (`device_width`, `widths`, the ring reaching in under the border), how far a window's bands reach (`outset`), and its rest window cut to its display (`rest_frame`). The corner a tile is drawn with (`radius`: reported, else `FALLBACK_RADIUS`, clamped to half the shorter side); the hairline's `outline`; continuous corners as path segments (`rounded_rect`, `CONTINUOUS_REACH`); the shadow's `silhouette`, its mask's frame and hole (`shadow_mask_frame`, `SHADOW_REACH`, `shadow_mask`) |
| `domain/shape.rs`, continued | a mask reaching any distance out (`mask_frame`, `mask`), the lift's `LIFT_REACH` |
| `domain/style.rs` | The hairline's paint (`LIGHT_PT`, `OUTLINE_PT`, `OUTLINE_ALPHA`) and the shadow's `ShadowStyle`, a lifted tile's `LIFT_SHADOW` among them; a band's width and three paints, picked by its `Ring` and `Urgency` (`Band`, `paint`); what a window wears that rini draws (`Decoration`, `corner`) and through a pass (`Worn`); `Style::of`, a window's paints by focus and attention, and `Worn::style`, a tile's; who wears the ring and who is drawn urgent (`Rings`) |
| `domain/paint.rs` | `Colour`, sRGB with its alpha; `Paint`, a colour or niri's `Gradient`: two colours along an angle, mixed in a `ColourSpace` with its `Hue` (`colour_at`); its `Axis` over a band, the points Core Animation takes (`axis`) and whether they move with the size (`follows_size`); the `Stop`s Core Animation draws it through (`stops`, `TOLERANCE`, `SAMPLES`) |
| `domain/rules.rs` | What a `window-rule` says a window wears (`DecorationRule`, a `BandRule` each for the ring and the border, `geometry-corner-radius`), matched by `windows::domain::rules::applies`; the layout's settings with every rule that applies laid over in file order (`fold`); a window's decoration either side of focus (`decoration`); the border a tiled window gives out of its slot in whole device pixels (`border_width`), and whether a rule can give another (`shape_borders`) |
| `domain/rest.rs` | The bands at rest: the reactor's `RestModel` of `RestWindow`s by `Group`; `shown`, `ordered_out`, `Parking`; each turn's fold of what was heard (`Heard`, `Batch`, `Word`, `Rest::take`, `Taken`); the flight's cover (`Cover`); each window `Tracked` from its `Introduction` (`ask`, `introduce`, `Placed`, `wants_turn`); the pool (`Rest::pool_size`, `assign`, `afresh`); what changes to draw a look (`Look`, `Layers`, `delta`); the reorders (`to_reorder`, `stack_change`); `Rest::look_of`; every call of a draw in order (`plan_draw`, `PanelOp`, `PanelState`); and off (`Rest::off`, `Rest::on`) |
| `domain/rest.rs`, continued | per window, the reactor sends the workspace it is on, its `Group`, what `Held` it, its bands' paint and how far they reach; a turn folds the window server's introductions, frames and orders and a lift, the latest word winning; the `Cover` is the place ahead of the lift under a flight begun, which lets go at its patience, or the overlay up, which only its lift takes away |
| `domain/rest.rs`, continued | a `Tracked` keeps the corner radius and level the window server reported, asked once as the model first names the window; a `Draw` is what a turn draws; off as it starts and once the reactor turns it off, it holds no window, wants no rest window and takes no frame, order, place, lift or park until a model comes |
| `domain/stack.rs` | How deep each decoration sits against its picture: the shadow caster behind it (`SHADOW_Z`), the hairline in front of it (`HAIRLINE_Z`) |
| `settings.rs` | `DecorationSettings`: `layout › focus-ring` and `layout › border`, each on or off with its `Band`, niri's by default but for the ring's width, and the window rules' `DecorationRule`s in file order; `draws_bands`, whether any window can wear a band ("Off") |
| `platform/layers.rs` | `DecorationLayers`, the Core Animation layers one tile wears: the masked caster (`shadow`, `shadow_mask`) and the hairline holding the outline and the bands (`hairline`, `outline`, `bands`). `Bands`, the ring and the border in front of it, each made only while worn; `Band`, one band's `CAGradientLayer`, laid (`place`), painted (`paint`), its colours and stops kept (`solid`, `drawn`). The tile's calls: `new`, `frame_layers`, `stack`, `corner_at`, `show`, `light`, `wear`, `install_resize`, `remove_resize`; the paths (`silhouette_path`, `shadow_mask_path`, `mask_path`); `add_under`, `remove_under`; `round_continuous` |
| `platform/layers.rs`, continued | a band is made as a decoration with it is worn, taken away as one without it is, and carried through a resize with a gradient's mask and axis; a `Band` draws a colour as its border on the continuous curve, or a gradient masked to that border's shape; `solid` and `drawn` keep the last 16 of each; the overlay keys its own animations through `add_under`; the hairline and every picture wear `round_continuous` |
| `platform/rest.rs` | The rest windows: pooled borderless, click-through, non-activating panels, each holding one `Bands` above the window it decorates (`RestBorders`); the main-thread actor that takes the model and what the frames thread read a turn at a time, replaying `rest::plan_draw`'s calls (`RestActor`), closing every rest window at `Event::Off`; and the renderer's calls at its show, its landing and each lift (`Handover`) |
| `platform/rest.rs`, continued | each rest window is ordered directly above the window it decorates; the actor asks the window server of each window the model first names through `Following`, and takes the frames thread's introductions, frames and orders; the renderer calls `Handover` as its overlay goes up, as its flight has landed and before each lift |
| `platform/layers.rs`: `Resize` | A tile's resize as Core Animation plays it: a key path animated through each value a closure maps from the sizes the tile passes through (`point`, `rect`, `number`, `path`), and whether it changes (`changes`). The overlay fills it from the tile's track, so `decorations` never names the score. A platform trait, not a port: it speaks Core Animation |

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
- **Gradients, measured:** [`gradients.md`](gradients.md): how Core Animation draws a gradient's
  axis and mixes its stops offscreen, and why each gradient is drawn through stops.
- **The corner, measured:** [`corner.md`](corner.md): where a window's radius comes from and why
  16pt where none is reported, the window's own curve against the one rini draws, and why every
  corner is clamped.
- **The rest windows, measured:** [`rest-windows.md`](rest-windows.md): each rule of "At rest" in
  full, with the probes, timings and inferences behind it.

## The hairline and the shadow, as drawn

**The hairline.** No capture carries it ("The hairline is composited outside every capture" in
`src/pictures/docs/capture-research.md`), so the overlay draws it over every tile. The white line is
the `hairline` layer, a 1pt border (`LIGHT_PT`) of white at 0.20 on the focused window and 0.18 on
the rest (`Style::of`), the fit measured there in dark mode; light mode is not measured. The outline
is a `CAShapeLayer` stroking 0.5pt (`OUTLINE_PT`) of black at 0.9 a quarter point outside the window
(`shape::outline`), covering the half point past its edge; a border layer at a half-point origin
landed one pixel too far out ("The handover, measured" in
`src/animation/docs/capture-overlay-research.md`).

Both round to the window's reported radius (`OverlayTile::radius`, read as the flight engine
composes), else 16pt, never past half the shorter side (`DecorationLayers::corner_at`), the corner a
clipped picture is cut at too. The hairline sits beside the picture, an eighth step in front of it
(`HAIRLINE_Z`), with the outline as its child, since a corner clip inside the picture would cut the
outline away (`a_corner_clip_leaves_the_hairline_whole`). It rides every move with the picture; a
resize carries its bounds on the tile's track and animates its corner and the outline's path
(`a_resize_carries_the_hairline_on_the_tiles_timing`); a snap moves it with the tile. Both are laid after the tile's frame on every
placement (`DecorationLayers::place`), or a pooled layer's autoresizing carries the old size.
`TileOverlay::set_tile_focused` switches a tile's white line and shadow depth in a hard cut as its
turned picture comes in, leaving its ring and border; when is the coordinator's (`Command::Focus`).

**The shadow.** A caster behind the picture (`shadow`, `SHADOW_Z`) holds no contents, only the
shadow of the window's silhouette (`shape::silhouette`) in the style focus picks (`Style::of`). An
even-odd `CAShapeLayer` masks it (`shadow_mask`) to what lies outside the window, out to
`SHADOW_REACH`, both rounding at the hairline's corner
(`a_resize_carries_one_corner_on_every_piece`). `DecorationLayers::place` shapes them again whenever
the size or corner differs, so a pooled tile casts its new window's corner
(`a_pooled_tile_dressed_for_a_window_of_another_radius_rebuilds_its_shadow_at_the_same_size`). Why a
shadow, why a masked caster, and how far: "Shadows are never in the surface" and "A layer shadow
covers the whole layer" in `src/animation/docs/capture-overlay-research.md`.

## The focus ring and the border, as drawn

**The ring.** niri's ring is a band outside the window, worn by each workspace's active window
(`Rings`): `urgent` on a window drawn urgent, `active` on every ring on the display holding focus,
as niri paints every workspace's active window on the focused monitor
(`niri:src/layout/monitor.rs:1095-1100`), and `inactive` elsewhere (`style::Ring`). So the workspace
a switch leaves keeps its ring active as it slides away, under zoom too. A strip the zoom look
draws beside a pan's or a push's strip, or beyond either end of a switch, wears none, its active
window included: the reactor dresses its windows `Worn::beside` (`strips_at` in
`src/app/reactor/strip_motion.rs`), no ring and no focus. On a tile it is the `ring` layer, a
`Band`, the hairline's child after the outline, in front of the picture and riding every move; a
tile carries it only while its window's decoration has one (`Bands::wear`).

**Its shape.** A colour is the layer's border, `width` wide; a gradient is the layer's gradient
masked to a border of the same shape ("Gradients"). It rounds on the continuous curve at the
window's corner plus its width (`shape::band`), so its inner edge is the window's own corner; the
corner is clamped to half the band's shorter side, and a square window keeps a square band, as
niri's `expanded_by` keeps 0 at 0. Its width is whole device pixels, never below one
(`shape::device_width`, niri's rounding), laid as the tile is dressed (`DecorationLayers::wear`). A
resize carries its bounds and corner on the tile's track; its position holds.

**A tile wears the ring of where its flight ends,** from the show to the lift (`Worn::style`),
whatever its picture shows: the window gaining focus the active one from the first frame, the window
losing it what it wears at rest. A pass merged in flight that changes where a tile ends wears it
again in the merge's drain (`TileOverlay::set_tile_worn`, `Render::Wear`), on the bands the tile was
dressed with.

**The border** is the `border` layer, a second `Band`, the hairline's last child, drawn in front of
the ring as niri draws it (`niri:src/layout/tile.rs:1279-1341`); a ring made after it is put under
it. Every window whose decoration has one wears it, `urgent` while drawn urgent, `active` while it
holds focus and `inactive` otherwise (`Style::of`); on a tile, the focus held where the flight ends
(`Worn::holds_focus`). Its inner edge is the window's corner and its outer one that corner grown by
its width, as the ring's. With both on, the ring lies outside the border: its outer rect is the
window grown by both widths, and it reaches in under the border to the window's edge
(`shape::widths`), so ring and border share no seam, the corner seam with the window is the
border's alone, and the ring shows past the border by its own width. A border colour with alpha
shows the ring through it, where niri's ring stops at the border.

**A tiled window gives the border its width out of its slot,** so the strip is laid out the same
with borders on or off (`layout::domain::tile`, "A border takes its width out of the window" in
`src/layout/docs/strip.md`). The slot gives up the width the band is drawn at, whole device pixels
of the window's display (`rules::border_width`): 1.3pt at 2x takes 1.5pt, as the band draws 1.5pt. A
floating window keeps its frame and wears the border outside it, where niri grows the tile instead.

**Urgent.** A window is drawn urgent while its application asks for attention and no window of it
holds focus (`Rings::urgent`, against the key window at rest and the focus a pass ends with on a
tile). macOS asks per application, so every window of it is urgent, where niri marks one. The
attention reader hears the ask at most a second late (`src/windows/docs/attention.md`); taking focus
clears it at once. Urgent beats active beats inactive (`Band::paint`), so the focused window always
wears `active`. A ring drawn active on a window not holding focus takes the active paint the rules
give it as if it held focus (`rules::decoration`): a rule under `is-focused` recolours it where
niri's would not. A tile keeps the urgency its pass gave it, so an ask during a flight shows at the
lift.

## Gradients

A band's paint is a colour or niri's gradient (`paint::Paint`): `active-gradient` and
`inactive-gradient` beat the colour beside them, and a colour written later clears the gradient, as
niri's `merge_color_gradient` has it. A gradient is the band's own `CAGradientLayer`, masked to a
`CALayer` border of the band's width, corner and continuous curve: the contour a colour draws, with
no path and no bitmap. A colour stays the layer's border with no mask, its `CGColor` made once
(`layers::solid`), as a gradient's stops are. A mask draws the band offscreen first, at a per-frame
cost not measured. The mask rides a resize only while it masks: a colour band animates its own
bounds and corner alone, and a mask attached mid-resize takes the resize its band plays
(`a_mask_attached_mid_resize_rides_its_bands_resize`).

**The axis.** niri lays a gradient over the band's outer rect at `angle`, the lines across at right
angles in points, as CSS does (`niri:src/render_helpers/border.rs:160-175`); Core Animation draws
them at right angles in unit space, so `Gradient::axis` stretches the axis by the size
(`a_gradient_is_drawn_at_niris_angle_in_points`). Off a right angle the axis depends on the band's
shape, so a resize carries it; a band painted mid-resize at another angle keeps the axis of the size
it was laid at until the next placement. `relative-to="workspace-view"` is refused: one gradient
across the display needs each band masked over a still gradient, riding its tile through a flight.

**The colours.** niri's shader mixes the two colours premultiplied, in `srgb`, `srgb-linear`,
`oklab` or `oklch` with its hue path (`colour_at`, after `niri:src/render_helpers/shaders/border.frag`);
rini clamps a colour mixed out of sRGB back into it. Core Animation mixes between stops in a space
of its own, so every gradient is drawn through stops (`Gradient::stops`), each segment within half a
level of niri's (`the_stop_count_keeps_each_space_within_one_level`), and the last 16 gradients'
stops are kept (`layers::drawn`). What it was measured against: [`gradients.md`](gradients.md).

## At rest

Between flights macOS draws each window, and rini draws the ring and the border in windows of its
own, one above each, holding `Bands` laid exactly as a tile's
(`a_tile_wears_its_ring_as_dressed_and_as_worn_and_a_turn_leaves_it`,
`every_band_rounds_on_the_continuous_curve_at_its_outset`). Each rule below in full, with what it
rests on, is the section of the same name in [`rest-windows.md`](rest-windows.md).

**The windows.** Borderless, non-activating, click-through `NSPanel`s on rini's main connection,
`.transient`, documented to have Mission Control hide them, each a flipped view holding the band, its frame the
band's outer rect cut to its display (`shape::rest_frame`). They are made as a model arrives, never
under a flight, and pooled per display for its fullest workspace's bands plus one spare
(`Rest::pool_size`); each shown band takes one (`rest::assign`), the one that last drew it first.
Once no window can wear a band, every rest window is closed ("Off").

**Shown by alpha, never ordered out.** A rest window is ordered in once, as it is made, and every
hide after is its alpha, about 40 times cheaper. A rest window given a window, or shown again, goes
to alpha 0, commits its new layers, and only then takes its frame, its place and alpha 1
(`rest::afresh`, in the order `rest::plan_draw` gives every call).

**What a turn draws.** Each rest window keeps the look it last drew (`rest::Look`), and a turn
changes only what differs (`rest::delta`): a move the frame, a resize, paint, corner, scale or cut
the layers, a hide or show the alpha. A turn that lays no layer opens no transaction.

**What a place costs.** A place given back the rest window that last drew it lays nothing and sets
no frame; the rest of its cost is the order.

**Ordering a rest window.** AppKit orders a rest window directly above another process's window
only once it holds that window's level and sub-level, which it takes through `SLSSetWindowLevel`
and `SLSSetWindowSubLevel` (`take_stack_place`) when they differ from the last it took
(`rest::stack_change`). It is ordered again when its window is reordered or ordered back in (808,
815), when given a window, and when focus comes to its window, once a turn (`rest::to_reorder`).
For 1-2 frames after another app raises its window, that window's shadow covers its bands.

**A click-through band is never hit.** The window under a band answers hit tests and drags, and the
band leaves its inner hairline alone.

**The band's corner.** The band's inner contour is the window's own continuous corner, so its outer
corner is `corner + width` (`shape::band`).

**Following.** A rest window takes its window's frame from the window server's
(`src/displays/docs/live-frames.md`), only while the feature is on, the latest each turn. A window
the model first names is asked of the window server whole (`ServerIntro`: frame, ordered-in, corner
radius, level, sub-level) and drawn from when that comes back (`Rest::ask`, `Rest::introduce`). From
a flight's begin to its lift nothing on its display is drawn or ordered; another display draws as
uncovered. A window the window server closes (804) shows no band from then (`rest::ordered_out`).

**Parked comes from rini's layout.** macOS clamps a parked window so a sliver shows
(`PARK_CLAMP_PX` in `crates/rini-geometry/src/lib.rs`), so the reactor sends a window as parked
when its workspace is not shown or its strip has it off the viewport. A park takes effect at the
window's next frame, a landed flight's place or a lift on its display, or 250ms after
(`rest::Parking`, `Rest::place`, `Rest::lift`), so its band stays until the overlay covers it.

**The handover.** The overlay covers its display at level 18. The reactor tells the rest windows of
each flight before its model (`Event::Flight`, `publish_rest`), and from that begin (`Rest::fly`) to
the lift a turn draws only another display's changes (`Rest::turn`); the show covers them
(`Rest::cover`). A flight not shown `FLIGHT_PATIENCE` after its begin lets go (`Rest::settle`). A
place or lift given before the press of the flight begun last leaves that flight's cover alone
(`Render::PlaceRest`, `Render::Lift`, `through`). Once the flight has landed, the renderer places
the rest windows, still covered, at its first tick that reads the real windows or with the lift
(`Rest::place`); the lift then draws only what came after (`Rest::lift`, `Rest::wants_turn`).

## Off

While no window can wear a band, the ring and the border off in `layout` and no window rule turning
either on (`DecorationSettings::draws_bands`), nothing of either runs. The reactor sends the rest
windows `Event::Off` once, after the batch that turned the last band off; the actor closes every
rest window (`Panel`'s `Drop`, so none is left at alpha 0 in the window list) and, off, takes no
frame, order, place, lift or park (`Rest::off`, `Rest::turn`, `Rest::place`, `Rest::lift`,
`Rest::settle`, `Rest::pool_size` 0).

The reactor's `SetDisplay`, sent ahead of every flight, tells the flight engine there is nothing at
rest (`rests`), so the coordinator gives no `PlaceRest` and no tick reads the windows' landing early:
no place drain runs, and no lift waits behind one. The renderer still calls `Handover` at each show
and lift: the show records which display the overlay covers, so a model turning the feature on
mid-flight waits for the lift, and the lift draws and reads nothing. The reactor tells it of no
flight (`publish_rest`), and a flight begun before the feature went off keeps its cover and its
patience.

The reactor turns `Following` off at the same batch, so the window-notify actor drops each 806, 807,
808, 815, 816 and 804 for the rest windows where it arrives (`src/displays/docs/live-frames.md`);
808 and 815 still wake the focus resolver. No ring is read (`Reactor::rings_with`), and a tile
whose decoration has neither band carries no layer for one: `DecorationLayers::wear` makes or drops
each band's layer and its mask as a rule turns it on or off. Every window ends a pass wearing no
ring and holding no focus as far as the bands go (`Worn::default`), so no merge wears a tile again.
The hairline and the shadow are drawn as ever.

A config reload that turns either band on, in `layout` or a rule, sends the next model, which turns
`Following` on, makes the pool and draws, each window asked of the window server as it comes
(`Rest::ask`).

## The corner

Every piece a tile wears rounds to one corner, `DecorationLayers::corner_at` over `shape::radius`:
the hairline, the outline, the shadow's silhouette, its mask's hole and a clipped picture, and a
resize carries that corner on each. The radius is the window server's, read for every tile in one
query as the flight composes (`windows::domain::info::reported_radius`, the largest of the four
above 0), else `FALLBACK_RADIUS`, 16pt; a rule's `geometry-corner-radius` goes ahead of it for the
ring and the border alone. Every piece rounds on the continuous curve, the paths through
`shape::rounded_rect` (`every_path_follows_core_animation_s_corner_at_every_size`), and every corner
is clamped to half the tile's shorter side, since Core Animation does not clamp it. The reads, the
fits and the data: [`corner.md`](corner.md).

## Reading order

`domain/shape.rs` → `domain/paint.rs` → `domain/style.rs` → `domain/rules.rs` → `domain/stack.rs` →
`domain/rest.rs` → `settings.rs` → `platform/layers.rs` → `platform/rest.rs`.

## Detail

- [`rest-windows.md`](rest-windows.md) — the rest windows' rules in full, with the probes and
  timings behind them, and what is not measured
- [`corner.md`](corner.md) — the window server's radii, the continuous curve and the clamp, measured
- [`gradients.md`](gradients.md) — Core Animation's axis and stop mixing, measured
