# `animation` — how things move on screen

Every animated movement is played on one Core Animation overlay: window pictures composited in one
opaque window, the real windows placed once behind it. This feature is how things move there: rigid
pieces and how a pass merges into them, the curves, the depth bands, the accents riding them, and the
renderer that plays all of it. When each step happens is `transitions`' (`src/transitions/docs/README.md`),
and the pictures it draws are `pictures`' (`src/pictures/docs/README.md`).

## What it owns

| | |
|---|---|
| `domain/motion/plan.rs` | `FlightPlan`, rigid groups, `reflow_plan` and `surface_plan`, `merge_plans` for a pass arriving mid-flight, the containers an edge bounce or a nudge carries, and an entrance's shape |
| `domain/motion/surface.rs` | How a strip movement carries a surface: `surface_travel`, `pan_travel`, and what a window on the surface and a tile have to say (`SurfaceGeometry`, `TileGeometry`) |
| `domain/motion/glide.rs` | `Leg`: a container's movement over time; `Leg::toward`, the spring a merge bends it on, from where the score draws it and as fast as its leg is going |
| `domain/motion/easing.rs` | `MOTION_CURVE`, and the ease-in-out a stepping move's strip crosses on |
| `domain/motion/score.rs` | `Score`: a transition's movement as tracks on containers and tiles, evaluating to what Core Animation draws; each track's `Form`, the class that plays it; `ending_at`, a track's end moved; `presented`, where every container is |
| `domain/motion/bounce.rs` | `Bounce`: the edge bounce's shape, a push arriving where one is drawn (`from_drawn`), and its track |
| `domain/motion/nudge.rs` | A move's nudge: the step's share and stretch, its keyframes and track, and the curve a stepping flight flies on |
| `domain/motion/accents.rs` | Which accent may start: one nudge at a time, a nudge waiting for its flight to move, and the bounce never held off; `push_tracks`, each container's bounce from where its own is drawn |
| `domain/motion/holes.rs` | Where the overlay is holed over a window that ends on screen, so the window keeps painting |
| `domain/motion/z_group.rs` | `stack`, `container_z`: the three z-bands, and the application set that comes forward |
| `domain/motion/tile.rs` | How a tile fits its picture to its frame: stretch or crop, the crop pieces, the stand-in's mode; and the snap onto its window's real frame |
| `domain/motion/hairline.rs` | The hairline every tile wears: the ring's width and white by focus, the outline's rect and stroke, the corner radius |
| `platform/overlay.rs` | `TileOverlay`, the renderer: the `CALayer` tree, the one `CATransaction` per flight, every animation installed from a track, and the replica of what it installed that every read of where things are goes to |

## The shape that matters

**Containers carry rigid pieces.** One `CALayer` container per rigid group; a
container's `position` is the only animated translation its members get. Per-tile
animations were tried and reverted: each tile bent from its own presented position on
its own clock, and a pass merging mid-flight sent tiles of one strip off on different
legs, which the user saw as a teleport.

**One `CATransaction`, one timebase, one curve per flight.** Model layers jump to their
destinations; the animations carry the presentation.

**Every commit is flushed at once.** `commit_now`, because the overlay shares the main
thread with the reactor and an explicit commit nested in the run loop's implicit
transaction is only sent after the reactor's synchronous AX calls. Sent late, a flight
skipped its first frames.

**`tokio::time` panics here.** There is no tokio reactor; see
[`crates/rini-runloop/docs/run-loop-executor.md`](../../../crates/rini-runloop/docs/run-loop-executor.md).

## Reading order

`domain/motion/plan.rs` → `domain/motion/glide.rs` → `domain/motion/z_group.rs` →
`platform/overlay.rs`. The actor that drives the overlay is `src/transitions/platform/engine.rs`.

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

## Known debt

**The overlay composes its own tracks.** It installs every animation from a track and reads where
things are from its replica, but it still makes each flight's tracks itself, from the plan and the
legs it keeps, rather than playing a score composed elsewhere. Step 5 of
`docs/pictures-motion-transitions.md` has a look compose the score and the overlay play it.
