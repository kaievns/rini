# `animation` — how things move on screen

Every animated movement is played on one Core Animation overlay: window pictures composited in one
opaque window, the real windows placed once behind it. This feature is how things move there: rigid
pieces and how a pass merges into them, the curves, the depth bands, the accents riding them, the
score all of it is written in, the look that composes and bends it, and the renderer that plays it. When each step happens is `transitions`' (`src/transitions/docs/README.md`),
and the pictures it draws are `pictures`' (`src/pictures/docs/README.md`).

## What it owns

| | |
|---|---|
| `domain/motion/plan.rs` | `FlightPlan`, rigid groups, `reflow_plan` and `surface_plan`, `merge_plans` for a pass arriving mid-flight, a frame in its container and back (`group_relative`, `overlay_of`, and `installed` for a fresh plan), the containers an edge bounce or a nudge carries, and an entrance's shape |
| `domain/motion/surface.rs` | How a strip movement carries a surface: what the viewport at an offset shows (`viewport`), `surface_travel`, `pan_travel`, and what a window on the surface and a tile have to say (`SurfaceGeometry`, `TileGeometry`) |
| `domain/motion/glide.rs` | `Leg`: a container's movement over time; `Leg::toward`, the spring a merge bends it on, from where the score has its movement (`Score::along`) and as fast as its leg is going |
| `domain/motion/easing.rs` | `MOTION_CURVE`, and the ease-in-out a stepping move's strip crosses on |
| `domain/motion/score.rs` | `Score`: a transition's movement as tracks on containers and tiles, which container each tile rides and how deep each is drawn, evaluating to what Core Animation draws; the fold of its `Edit`s (a place, a track added or cut, a ride, a drop, a bank), the one way in; each track's `Form`, the class that plays it; `ending_at`, a track's end moved; `presented`, where every container is; `plays`, whether a channel plays; `MediaClock`, the media time an `Instant` is at, from one reading of both |
| `domain/motion/bounce.rs` | `Bounce`: the edge bounce's shape, a push arriving where one is drawn (`from_drawn`), and its track |
| `domain/motion/nudge.rs` | A move's nudge: the step's share and stretch, its keyframes and track, and the curve a stepping flight flies on |
| `domain/motion/accents.rs` | `Accent`, what a press asks to ride its movement (a push, a step), and the tracks each plays on its carriers: `push_tracks`, each container's bounce from where its own is drawn, never held off; `steps`, whether a step is taken, only while none plays; `step_tracks`, the step on every carrier |
| `domain/motion/holes.rs` | Where the overlay is holed over a window that ends on screen, so the window keeps painting |
| `domain/motion/z_group.rs` | `stack`, `container_z`: the three z-bands, and the application set that comes forward |
| `domain/motion/tile.rs` | How a tile fits its picture to its frame: stretch or crop, the crop pieces, the stand-in's mode; when it snaps onto its window's real frame (`snap_to`), and the snap as the score's edits (`snap`) |
| `domain/motion/hairline.rs` | The hairline every tile wears: the ring's width and white by focus, the outline's rect and stroke, the corner radius |
| `domain/choreography.rs` | The looks: `Look`, every look by name (`ALL`), its `Choreography` for a flight (the plan, frame zero posed, the motion composed, a press before it folded and one after it bent) and its staging of a press on the strip (`stage`, over a `Cue`, a `Strip` and its `PairStart`s, giving a `Staging`); what a look composes from (`Stage`), bends with (`Pass`) and gives back (`Retarget`); and the tests every look passes |
| `domain/choreography/slide.rs` | `Slide`, the default look: frame zero posed, the motion composed on one curve with the accents that waited, a press before the motion folded and one after it bent from the score at its legs' speed, everything on whole points; a move whose column fills the viewport held and stepped, any other crossing |
| `platform/overlay.rs` | `TileOverlay`, the window the renderer draws on: it poses frame zero and plays a score's edits, every animation installed from a track, and commits nothing itself; its `Tree` (the `CALayer` tree and the replica it reads only to install) hangs off any root layer, so it is tested without a window; `server_id()` is its window's number, which a handover check pictures alone; `window_origin` is where the window over a display starts on screen, which that picture is read from, and `same_layout` whether a display lays the window out again; at the lift it lets go of the tiles and of the desktop behind them (`release`), which the pictures service still holds for the next flight's dressing |
| `platform/renderer.rs` | `Renderer`, the main thread's actor: it owns the overlay, makes it on the first flight it dresses or poses, and plays the flight engine's `Batch`es of `Render` commands, each one of the overlay's calls; the batches queued when it wakes are one drain (`in_order`), played in one `CATransaction` with the window's alpha and background set around it and answered once (`run`), not drawn only when it dressed or posed a flight (`needs_overlay`) and there is no overlay (`answer`); `media_clock`, Core Animation's media clock read against `Instant`, which the engine begins every track on |
| `event.rs` | What the renderer answers each drain with: `Committed`, with the overlay's window number once the window server has numbered it, whether the drain put the overlay up or down, and when its oldest batch was sent, when the renderer began playing it and when it was flushed; or `Unavailable`, a flight dressed or posed with no overlay to draw it on |

Its interface and `domain/` speak rini-geometry's spaces (`crates/rini-geometry/docs/README.md`):
a layout pass's requests in `Screen`, tiles, container positions, travels and accents in `Overlay`,
a strip movement's windows and viewport offsets in `Surface`, a frame in its container in `Group`,
and a tile's own size, crop pieces and hairline in `Tile`. CG stays in `platform/`: in its objc2
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
holes need and alpha 1 for a drain that shows it, which puts frame zero up holed and moving.

**`tokio::time` panics here.** There is no tokio reactor; see
[`crates/rini-runloop/docs/run-loop-executor.md`](../../../crates/rini-runloop/docs/run-loop-executor.md).

## Reading order

`domain/motion/plan.rs` → `domain/motion/glide.rs` → `domain/motion/score.rs` →
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
