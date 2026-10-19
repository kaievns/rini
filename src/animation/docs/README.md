# `animation` — how things move on screen

Every animated movement is played on one Core Animation overlay: window pictures composited in one
opaque window, the real windows placed once behind it. This feature is how things move there: rigid
pieces and how a pass merges into them, the curves, the depth bands, the accents riding them, the
score all of it is written in, the look that composes and bends it, and the renderer that plays it. When each step happens is `transitions`' (`src/transitions/docs/README.md`),
and the pictures it draws are `pictures`' (`src/pictures/docs/README.md`).

## What it owns

| | |
|---|---|
| `domain/motion.rs` | The root of `motion`: how a transition moves, as pure functions over rects, its modules below |
| `domain/motion/plan.rs` | `FlightPlan` and where it lands each window (`FlightPlan::destination`), rigid groups and the windows on paths of their own (`OwnPath`), `reflow_plan` over a pass's `Resolved` windows and `surface_plan`, each strip drawn beside the one a movement moves in a group of its own (`RigidGroup::beside`), a frame in its container and back (`group_relative`, `overlay_of`, and `installed` for a fresh plan), the containers an edge bounce or a nudge carries, and an entrance's shape |
| `domain/motion/plan/merge.rs` | `merge_plans`, a pass arriving mid-flight folded into the plan: a pan carrying every group, a strip beside only up and down, the members' votes for where their containers go, one a pan names of another strip than its container's voting too, the largest cluster keeping each and the rest reparented into none a later vote moves, a newcomer riding beside the windows it sits beside, never in another strip's container, and loose and floating tiles retargeted |
| `domain/motion/surface.rs` | How a strip movement carries a surface: what the viewport at an offset shows (`viewport`), `surface_travel`, `pan_travel`, and what a window on the surface has to say (`SurfaceGeometry`), a window of a strip beside travelling with the view up and down only |
| `domain/motion/glide.rs` | `Leg`: a container's movement over time; `Leg::toward`, the spring a merge bends it on, from where the score has its movement (`Score::along`) and as fast as its leg is going |
| `domain/motion/easing.rs` | `MOTION_CURVE`, and the ease-in-out a stepping move's strip crosses on; a curve cut where its progress reaches a share, its speed unbroken through the cut (`CubicBezier::split`) |
| `domain/motion/score.rs` | `Score`: a transition's movement as tracks on the strip's zoom (`Zoom`, a scale and a shift), containers, tiles, lifts and the overview's tint (`Node::Tint`), which container each tile rides and how deep each is drawn, evaluating to what Core Animation draws; the fold of its `Edit`s (a place, a track added or cut, a ride, a drop, a bank), the one way in; each track's `Form`, the class that plays it |
| `domain/motion/score.rs`, continued | `ending_at`, a track's end moved; `presented`, where every container is; `plays`, whether a channel plays; `rate`, how fast a node's movement is drawn going; `MediaClock`, the media time an `Instant` is at, from one reading of both |
| `domain/motion/bounce.rs` | `Bounce`: the edge bounce's shape, a push arriving where one is drawn (`from_drawn`), and its track |
| `domain/motion/nudge.rs` | A move's nudge: the step's share and stretch, its keyframes and track, and the curve a stepping flight flies on |
| `domain/motion/accents.rs` | `Accent`, what a press asks to ride its movement (a push, a step, a zoom look's arc and the pair it swaps, `Swap`), and the tracks the push and the step play on their carriers: `push_tracks`, each container's bounce from where its own is drawn, never held off; `steps`, whether a step is taken, only while none plays; `step_tracks`, the step on every carrier |
| `domain/motion/holes.rs` | Where the overlay is holed over a window that ends on screen, and over the band it wears at rest, so both keep painting |
| `domain/motion/z_group.rs` | `stack`, `container_z`: the three z-bands, and the application set that comes forward |
| `domain/motion/tile.rs` | `OverlayTile`: one window's tile on the overlay, generic over the picture it is drawn with (the overlay draws a captured `Picture`); how a tile fits its picture to its frame: stretched only where that is exact, a picture fitting both ends and the stand-in, cropped otherwise (`content_mode`); the crop's pieces, the ground where its picture ends and the edges they stand still against (`crop`, `Anchors`) |
| `domain/motion/tile.rs`, continued | a tile's leg, one movement or, growing on one axis as it shrinks on the other, two steps turning at a corner (`two_steps`, `leg`, `corner_key`); when it snaps onto its window's real frame (`snap_to`), and the snap as the score's edits (`snap`) |
| `domain/choreography.rs` | The looks: `Look`, every look by name (`ALL`), its `Choreography` for a flight (the plan, frame zero posed, the motion composed, a press before it folded and one after it bent) and its staging of a press on the strip (`stage`, over a `Cue`, a `Strip` and its `PairStart`s, giving a `Staging`); what a look composes from (`Stage`), bends with (`Pass`) and gives back (`Retarget`); and the tests every look passes |
| `domain/choreography.rs`, continued | how far apart a look draws the strips above and below the one a flight moves, if it draws them (`Look::stack_gap`) |
| `domain/choreography.rs`, continued | a bend's structure, every look's alike (`Merged::bend`): containers opened, members riding a new one, tiles joining, every node placed where it ends, what the flight stops drawing dropped, each edit folded into a copy of the score playing (`Making`); the look gives each node's movement in it (`Bending`) |
| `domain/choreography/slide.rs` | `Slide`, the default look: frame zero posed, the motion composed on one curve with the accents that waited, a press before the motion folded and one after it bent from the score at its legs' speed (`Bent`), everything on whole points; a move whose column fills the viewport held and stepped, any other crossing |
| `domain/choreography/zoom.rs` | `ZoomArc`, the zoom look: a press along the strip as one arc, the strip zoomed out and back on a sin² bell (`arc_scale`) as deep as the pair needs (`zoom_depth`), the travel every container carries eased to show the pair at the middle (`pair_view`, `camera_share`, `camera_progress`), the swap on `ZOOM_CURVE` on top, the moved window lifted (`lift`); a pan folded into the arc or a spring at the speed the score draws; a push bounced zoomed |
| `domain/choreography/zoom.rs`, continued | a switch as one arc as deep as the two strips it travels between need (`switch_arc`), and a pan up or down the stack folded into an arc as one along the strip is (`folds`) |
| `domain/choreography/zoom.rs`, continued | the strips above and below drawn beside the strip, `STACK_GAP` beyond a usable height apart, zoomed with it and left still by the arc |
| `domain/choreography/overview.rs` | How the overview moves, no look itself: each row a strip container and its stacks another, under the strip's zoom (`held`), and the screen drawn the same way at rest (`at_rest`); every node from where it is drawn to where a scene has it (`toward`), on Slide's curve at a press's pace or a spring; a push bounced (`pushes`); bands as wide on screen as at rest (`screen_wide`) |
| `domain/choreography/overview.rs`, continued | every thumb lifted while held, the selected one furthest (`THUMB_LIFT`, `SELECTED_LIFT`), and the desktop darkened (`Places::tint`); none of either at rest; the selected thumb's ring the card's (`accented`) |
| `domain/choreography/linear.rs` | `Linear`, a look only the tests build and `ALL` holds under them: every node straight to where its pass sends it at an even speed, and bent through `Merged::bend` from the speed it is moving at, that speed changing evenly, so the contract and the overlay's playback tests run over motion neither Slide's nor Zoom's |
| `platform/overlay.rs` | `TileOverlay`, the window the renderer draws on: it poses frame zero and plays a score's edits, every animation installed from a track, and commits nothing itself; each tile wears its picture and `decorations::platform::layers`, which it fills a resize for from the tile's track (`Resize`); its `Tree` (the `CALayer` tree, the desktop behind it included, and the replica it reads only to install) hangs off any root layer, so it is tested without a window |
| `platform/overlay/outline.rs` | The overview's outline of a workspace with no windows: its tiling area ringed in the focus ring's band, active while its row is selected, hung in its row's container so it moves and zooms with it |
| `platform/overlay/tint.rs` | `Tint`, the overview's darkening of the desktop: `chrome`'s card `BACKDROP` over the whole overlay, between the desktop and every container, its opacity the score's `Node::Tint` share of `BACKDROP_ALPHA` (`opacity`); none once the overlay is let go |
| `platform/overlay/lift.rs` | `Lift`, a tile's lift as the overlay draws it: a caster beside the tile's own carrying only `LIFT_SHADOW`, cast under the zoom the overlay names (`recast`, `TileOverlay::cast_lifts`), its opacity the score's `Node::Lift` share (`opacity`), masked `LIFT_REACH` out, grown with the blur (`reach`); made the first time a tile is lifted and gone with the tile or its lift |
| `platform/overlay.rs`, continued | the tree's strip layer, which every container but the floating one and its twin hangs under, the strips beside the strip's included, plays the strip's zoom on its `transform` (`zoom_transform`) |
| `platform/overlay.rs`, continued | `server_id()` is its window's number, which a handover check pictures alone; `window_origin` is where the window over a display starts on screen, which that picture is read from, and `same_layout` whether a display lays the window out again; at the lift it lets go of the tiles and of the desktop behind them (`release`), which the pictures service still holds for the next flight's dressing |
| `platform/renderer.rs` | `Renderer`, the main thread's actor: it owns the overlay, makes it on the first flight it dresses or poses, and plays the flight engine's `Batch`es of `Render` commands, each one of the overlay's calls |
| `platform/renderer.rs`, continued | the batches queued when it wakes are one drain (`in_order`), played in one `CATransaction` with the window's alpha and background set around it and answered once (`run`), not drawn only when it dressed or posed a flight (`needs_overlay`) and there is no overlay (`answer`) |
| `platform/renderer.rs`, continued | `media_clock`, Core Animation's media clock read against `Instant`, which the engine begins every track on |
| `event.rs` | What the renderer answers each drain with: `Committed`, with the overlay's window number once the window server has numbered it, what the drain `Played`: which way it turned the overlay (`OverlayTurn`, `Shown` or `Lifted`, or neither), whether it placed what is drawn at rest (`RestPlacement`) and what it spent on crop-drawn tiles (`Crops`: the tiles it drew on the crop grid and their ground reads) |
| `event.rs`, continued | and when its oldest batch was sent, when the renderer began playing it and when it was flushed; or `Unavailable`, a flight dressed or posed with no overlay to draw it on |

Its interface and `domain/` speak rini-geometry's spaces (`crates/rini-geometry/docs/README.md`):
a layout pass's requests in `Screen`, tiles, container positions, travels and accents in `Overlay`,
a strip movement's windows and viewport offsets in `Surface`, a frame in its container in `Group`,
and a tile's own size and crop pieces in `Tile`. CG stays in `platform/`: in its objc2
calls, its calls into `displays`, and its private helpers and structs.

## The shape that matters

**Containers carry rigid pieces.** One `CALayer` container per rigid group; a
container's `position` is the only animated translation its members get. Per-tile
animations were tried and reverted: each tile bent from its own presented position on
its own clock, and a pass merging mid-flight sent tiles of one strip off on different
legs, which the user saw as a teleport.

**One timebase, one curve per flight.** Its motion is installed in the one transaction of the drain
that carries it (below). Model layers jump to their destinations; the animations carry the
presentation.

**One transaction a drain, flushed.** The renderer plays every batch queued when it wakes together,
one drain, in one `CATransaction` and flushes it, so its answer means the render server has it: an explicit commit
nested in the run loop's implicit transaction is otherwise sent only as that pass of the main
thread's loop ends, after whatever else runs in it. Sent late, a flight skipped its first frames.
The window's alpha and background are the window's, not the layers', so they are set around the
transaction: alpha 0 before a drain that lifts the overlay, and after the flush the background its
holes need and alpha 1 for a drain that shows it, which puts frame zero up holed and moving. What
draws the focus ring and the border at rest on the flight's display draws nothing on the main thread
from the flight's begin, which the reactor tells it of, until the renderer places it; after a
show's alpha 1 the renderer covers it (`Handover`, which the decorations' rest windows own). The
coordinator gives `PlaceRest` once a flight end, at the first tick from `FRAME_INTERVAL` before it
that reads the real windows landed, or with the lift when none did, and never with nothing drawn
at rest, and the renderer plays it
before the transaction of the drain carrying it: every band on the display put where its window
is, still covered (the place and the lift each pass on the latest press the engine had taken as it
gave them, so a press heard after them waits for its own flight), so a lift's drain only tells the rest windows what came since and brings the
overlay down (`steps`, pinned by
`a_drain_carrying_place_rest_places_what_rests_before_its_commands` and
`a_drain_s_steps_follow_its_turn_and_its_placement`). Between the place
and the hide, one tick when the windows landed in time, a band shows only through its hole ("Holes"
in [`animation-smoothness.md`](animation-smoothness.md)).

**`tokio::time` panics here.** There is no tokio reactor; see
[`crates/rini-runloop/docs/run-loop-executor.md`](../../../crates/rini-runloop/docs/run-loop-executor.md).

## Adding a look

A look touches these files, and nothing in `transitions`, which stages every flight and pass on the
display the overlay covers (`viewport`), unless it adds an `Accent`, which the tests there match on
by kind. Nor the reactor, unless it needs a press no `Cue` names yet: a `Cue` is a move or a focus
press along the strip (`EventResponse::moved`, `stepped` and `pushed` in
`src/workspaces/engine.rs`) or a switch through the stack (`Cue::Switch`), and a bounce carries the
press that ran into it: at a side of the strip the move or focus press, at an end of the stack a
switch.
A look that gives a `stack_gap` has the reactor draw the strips above and below beside every pan and
bounce it sends, and stack a switch's strips the same gap apart, every one but the destination's
named a strip beside; the strips it draws beside a pan's or a bounce's strip or beyond a switch's
ends wear no ring and no focus (`Worn::beside`), and the strips a switch travels between wear what
they wear under slide. The flight engine drops a pan's or a bounce's from a flight whose look gives
none, and keeps a switch's as strips of the surface. The flight's clock ends with the score the
motion starts with (`Event::Fit`), so a look shorter than its press lifts as it lands; a merge still
restarts it at the plain length (`specs/animation.md`, "The zoom look", not yet met):

1. `domain/choreography/<name>.rs`: a `Choreography` and a `stage`. Its `retarget` calls
   `Merged::new` and `Merged::bend` with a `Bending` of its own, and adds only motion; `linear.rs`
   is the smallest one. A rigid member's tile has no track of its own: `Merged::bend` moves one to
   another container by riding and placing it alone, and the bend contract holds every look to it.
2. `domain/choreography.rs`: a `Look` variant, a slot in `ALL`, and an arm in `choreography` and
   in `stage`. `ALL` puts it under every contract test there and the overlay's playback tests in
   `platform/overlay.rs`.
3. This README: a row for its file.
4. Its name: a word in `kebab_scalar!(Look …)` in `src/app/config/kdl/value.rs`, which
   `animations › look` decodes and `config get` prints. Every flight the reactor sends takes the
   look the config names (`specs/animation.md`, "Looks").
5. `rini.default.kdl`'s comment over `animations › look`, `docs/config.md`'s row for it, and
   "Looks" in `specs/animation.md`, each of which names every look, and the look's requirements in
   a section of their own there.

A zoom of the strip is a look like any other (`domain/choreography/zoom.rs`): its tracks on
`Node::Strip` hold a `Zoom` about the display's centre with no camera, and the travel a camera would
carry rides each container's own track, so a zoom track lerps exactly between its samples
(`Zoom::about`), and `Merged::bend` bends that travel with the rest of every container's movement.
The speed contract reads a window's speed on the strip and the zoom's apart, a strip the score holds
no zoom for at rest: drawn, the two turning opposite ways cancel its allowance though neither jumps.
Across a pass bringing an arc, Zoom's alone, the edge of a window whose container the pass carries
on a spring over the pass's stretched length is also allowed the change that spring makes over its
first frame, let go where the edge is drawn at the speed it is drawn moving, and only the way that
spring makes it ("Mid-flight passes" in `animation-smoothness.md`).

## Reading order

`domain/motion/plan.rs` → `domain/motion/plan/merge.rs` → `domain/motion/glide.rs` → `domain/motion/score.rs` →
`domain/motion/accents.rs` → `domain/motion/z_group.rs` → `domain/choreography.rs` →
`domain/choreography/slide.rs` → `platform/overlay.rs` → `platform/renderer.rs`. The flight's
score is kept by `src/transitions/domain/flight_score.rs`, and the actor that hands the renderer its
batches is `src/transitions/platform/engine.rs`.

## Detail

- [`animation-smoothness.md`](animation-smoothness.md) — how a flight moves: containers, merges,
  curves, depth bands, the bounce and the nudge, resizes and the score, and what each earlier model
  cost
- [`capture-overlay-research.md`](capture-overlay-research.md) — the overlay measured: why it is
  one opaque window, its level and coverage, its shadows, a focus change sequenced behind it and
  where a moving one spends its time, and the handover
- What the pictures it draws cost and return is
  [`src/pictures/docs/capture-research.md`](../../pictures/docs/capture-research.md); when a flight
  does what is [`src/transitions/docs/coordination.md`](../../transitions/docs/coordination.md)
