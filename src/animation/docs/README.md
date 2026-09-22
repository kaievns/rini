# `animation` — movement on screen

One engine. Every animated movement is a flight of a Core Animation overlay: window
bitmaps composited in one opaque window, the real windows placed once behind it. Layout
passes, strip pans, workspace switches, resizes, entrances and the edge bounce are all
flights of the same thing.

## What it owns

| | |
|---|---|
| `domain/motion/plan.rs` | `FlightPlan`, rigid groups, and `merge_plans` for a pass arriving mid-flight |
| `domain/motion/travel.rs`, `surface.rs` | How far each tile goes, and what a pinned one does |
| `domain/motion/easing.rs` | `MOTION_CURVE`, the one curve |
| `domain/motion/z_group.rs` | `tile_depth`, `container_z`: the z-bands |
| `domain/motion/strip_stack.rs` | The stacked-workspace geometry a switch moves through |
| `domain/motion/fit.rs` | Whether a captured picture still fits the tile it is for |
| `domain/timing.rs`, `flight.rs`, `admission.rs` | When work happens, what a flight is and when it may capture, and how a mid-flight request is admitted |
| `domain/pass.rs` | Sorting a layout pass into moves, unmoved windows and warm targets |
| `platform/engine.rs` | `FlightEngine`: the actor. The overlay, the snapshot cache, the timer |
| `platform/overlay.rs` | `TileOverlay`: the `CALayer` tree and the one `CATransaction` per flight |
| `platform/window_snapshot.rs`, `snapshot_service.rs` | Capturing windows, through SkyLight and ScreenCaptureKit |
| `platform/backdrop.rs`, `edge_dressing.rs` | The desktop behind the tiles, and the dressing on them |

## The shape that matters

**Containers carry rigid pieces.** One `CALayer` container per rigid group; a
container's `position` is the only animated translation its members get. Per-tile
animations were tried and reverted: each tile bent from its own presented position on
its own clock, and a pass merging mid-flight sent tiles of one strip off on different
legs, which the user saw as a teleport.

**One `CATransaction`, one timebase, one curve.** Model layers jump to their
destinations; the animations carry the presentation.

**Every commit is flushed at once.** `commit_now`, because the overlay shares the main
thread with the reactor and an explicit commit nested in the run loop's implicit
transaction is only sent after the reactor's synchronous AX calls. Sent late, a flight
skipped its first frames.

**`tokio::time` panics here.** There is no tokio reactor; see
[`crates/rini-runloop/docs/run-loop-executor.md`](../../../crates/rini-runloop/docs/run-loop-executor.md).

## Reading order

`domain/motion/plan.rs` → `domain/pass.rs` → `platform/overlay.rs` →
`platform/engine.rs` last, which is the actor and the largest file.

## Detail

- [`animation-smoothness.md`](animation-smoothness.md) — the engine as it is, what
  each earlier model cost, and what is still open
- [`capture-overlay-research.md`](capture-overlay-research.md) — the capture
  measurements everything above rests on
