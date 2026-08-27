# Animation smoothness: findings and plan

An audit of both animation engines, written 2026-08-27. Numbers quoted from code
comments were measured when those fixes landed; anything marked *estimated* has
not been measured yet. Companion to `capture-overlay-research.md`, which holds
the capture measurements this builds on.

## The two engines

Rini animates through two mechanisms, selected per layout pass in
`AnimationManager::animate_layout` (`src/actor/reactor/animation.rs`):

1. **The AX engine** (`src/actor/reactor/animation.rs` + `src/actor/app.rs`).
   Per-frame `AXPosition`/`AXSize` writes into each owning application, ticked
   by a CFRunLoopTimer at `animation_fps` (default 100). Handles everything the
   overlay cannot: real resizes, and all animation when `overlay_animations`
   is off.
2. **The overlay engine** (`src/actor/workspace_animation.rs` +
   `src/ui/workspace_overlay.rs`). Window bitmaps composited in one opaque
   overlay window; the real windows are placed once at 75% progress, hidden
   behind it. Ticked by a CFRunLoopTimer at a fixed 60fps. Handles pure
   translations: workspace switches (canvas), strip pans (canvas), and
   per-window slides.

## The AX engine is at its physical ceiling

Every frame write is a synchronous Mach round trip into a process that answers
at its own speed, and AX has no atomic set-frame call. The engine already
carries every fix that mechanism admits:

- One message per app per tick (`Request::AnimationFrames`), not per window.
  Cut inter-window drift from 155pt to the residual cross-app skew.
- `pending_frames` coalescing in the app actor: a slow app drops frames
  instead of queueing them.
- The 1-vs-3 write distinction in `flush_frames`: a resize costs
  `set_size; set_position; set_size`, a pure move costs one `set_position`.
- `BeginWindowAnimation` seeds `last_animation_frame` so the first frame of a
  slide does not take the 3x path ("jumps the first one or two times").
- Wall-clock frame indexing, so late ticks skip instead of stretching.
- A refcounted `AXEnhancedUserInterface` lease held across the animation.

What remains is either consolidation or marginal:

- **The 100fps default is wasted.** No app accepts AX writes at 100Hz; the
  coalescing collapses most ticks anyway, but each one still costs a channel
  send and an app-thread wakeup. ~60 (or the display refresh rate, which
  `display_link.rs` can query) loses nothing visible. *Estimated.*
- ~~The ticker shares the reactor's executor.~~ Fixed: `AnimationManager::run`
  now runs on its own `animation` thread with its own run loop, so an arrange
  pass cannot delay a tick and the wall-clock skip has nothing to skip.
- ~~The curve disagrees with the overlay.~~ Fixed: the engine's `ease` now
  delegates to the overlay's `ease_out_cubic`, so a resize (AX) next to a pan
  (overlay) from one keystroke follows one curve, and the sluggish
  ease-in-out start is gone.
- **Cross-app skew is unfixable here.** The real fix is to stop using AX for
  animation entirely — see "Endgame" below.

## The overlay engine: canvas movements are Core Animation-driven

Canvas movements (workspace switches and strip pans) hand their interpolation
to an explicit `CABasicAnimation` on the canvas layer
(`WorkspaceOverlay::animate_canvas_offset`). The render server runs it
vsync-locked at the display's native refresh, immune to main-thread stalls.

The manual tick loop it replaced — a `RepeatingTimer` at `FRAME_INTERVAL` =
16.667ms posting `Event::Tick` into the actor's own queue, with `step_canvas`
setting the layer position — had three weaknesses:

1. **Not vsync-aligned.** A free-running 60Hz timer beats against the
   display's refresh, so frames land just before or just after a vsync
   boundary and the animation judders even when no tick is late.
2. **Capped at 60fps.** On a ProMotion display that is half the refresh rate.
   (The 60fps figure came from a 120fps experiment on the old per-frame AX
   engine, which could not keep up; the canvas writes one property per frame.)
3. **Main-thread coupled.** Ticks share the actor queue with
   `SnapshotsReady` processing and warm filtering, and share the main thread
   with everything else that must run there. Dropped drawn frames were the
   observed difference between smooth and "instant cut".

How the CA handoff works:

- The curve is preserved exactly. `ease_out_cubic` (`1 - (1-t)^3`) is
  precisely the cubic Bezier timing function with control points
  `(1/3, 1)` and `(2/3, 1)`: with x-control-points at 1/3 and 2/3 the
  Bezier's x(t) collapses to t (the Bernstein terms sum to
  `t[(1-t) + t]^2 = t`), and y(t) with both y-controls at 1 expands to
  `1 - (1-t)^3`. So `CAMediaTimingFunction(controlPoints: 1/3, 1, 2/3, 1)`
  is not an approximation. Pinned by
  `the_core_animation_curve_is_exactly_ease_out_cubic` in
  `workspace_overlay.rs`.
- Pinned (floating) tiles get their own position animations counter-moving
  them, committed in the same transaction so they run in lockstep with the
  canvas. `addAnimation` copies, so one instance serves picture and shadow.
- The model layer is set to its final position up front; the explicit
  animation carries the presentation from start to finish and is removed on
  completion, revealing the model value — no snap-back, no delegate needed.
  The flip side: an instant `set_canvas_offset` must cancel in-flight
  animations first, across every pooled tile layer, because a chained
  movement replaces the `pinned` list before the cancel runs.
- The mid-flight hooks (`APPLY_FRAMES_AT` = 0.75, destination recapture at
  0.0 and 0.5, finish at 1.0) do not need per-frame ticking. The tick loop
  survives as orchestration only; its rate no longer affects what is drawn,
  and `RunningCanvas.frames` now counts ticks, not drawn frames.
- Chaining a switch onto one in flight reads the presentation layer
  (`current_canvas_offset`) for the current offset instead of re-deriving it
  from the model clock, falling back to model arithmetic before the first
  presentation frame exists.
- `CAAnimation` treats a zero duration as "use the default 0.25s", so zero
  durations bypass the animation and set the offset directly.
- `NSValue::valueWithPoint` (the from/to carrier) is gated behind the
  `NSGeometry` + `objc2-core-foundation` features of `objc2-foundation`.

The per-window path (`Event::Animate` — window open/close, column reorder,
join/unjoin, and any layout change whose windows move by different vectors) is
CA-driven the same way, per tile:

- `start_moving` (after the 25ms coalesce window) hands every tile to Core
  Animation in ONE transaction (`animate_tiles`): one commit, one timebase,
  one curve, so tiles start on the same beat and cannot tear against each
  other. Model layers jump to their destinations; the animations carry the
  presentation. The tick loop paces only the mid-flight work.
- Mid-flight passes are classified per tile (`merge_action`, tested):
  **redundant** (same destination within `same_as` tolerance — touched not at
  all, which is what keeps rapid presses from restarting or extending the
  flight), **retargeted** (the tile bends from its presentation position to
  the new destination over a fresh duration, canvas-chaining style), or
  **joined** (installed and animated from its own start, full duration). Any
  real change restarts the orchestration clock so the frame placement and the
  teardown cover the newest flights.
- `set_tiles` is pre-flight only: it places tiles at their START, which would
  end an in-flight tile's animation on the wrong frame. Mid-flight changes go
  through `retarget_tile`/`add_tile`.

With that, nothing in the overlay is hand-drawn — canvas and tiles are both
render-server-driven — and the open question is whether the canvas path is
still needed at all: per-tile animations in one transaction share clock and
curve, so a rigid group slide should hold together without the canvas's
single-layer guarantee. The deciding test is chained retargeting under rapid
presses (15 tiles staying coherent through repeated replacement). If it
holds, the canvas AND the entire pan classifier (`strip_pan_delta`,
`take_strip_movement`) collapse into the per-tile path; if it shows seams,
the canvas stays.

## Snapshot staleness

Staleness is accepted by construction ("a slightly stale moving image is not
perceptible", `capture-overlay-research.md`) and the worst case — the
destination's focus appearance — is patched mid-flight (`refresh_destination`,
capped at `MAX_DESTINATION_CAPTURES` = 1). What that does not cover: content
that changed while parked. Terminal output, chat, anything live — warmed only
at animation end, focus change, and layout passes, so a window that repainted
itself while hidden is stale until the next switch touches it. Capturing at
switch time instead is ruled out by measurement: 4 windows cost 94.5ms against
a 180ms budget.

Options, in order of expected value:

1. **Low-rate `SCStream` for likely targets.**
   `SCContentFilter(desktopIndependentWindow:)` works for off-screen windows,
   and a persistent stream (unlike one-shot `SCScreenshotManager` calls)
   delivers frames only when the content actually changes. A small pool
   covering adjacent workspaces keeps the cache continuously fresh with zero
   work at switch time. Wants a budget (frontmost window of workspace n±1),
   not every window. *Per-stream overhead unmeasured.*
2. **Change-signal-driven warming.** Use signals already flowing — AX title
   changes, the CGS window events `window_notify` subscribes to — as "picture
   is dirty" triggers. Misses silent repaints; catches most others without
   polling.
3. **Aged-cache sweeps.** Timestamp `WindowSnapshot`, re-warm the oldest
   pictures of nearby workspaces on a slow idle timer. Breaks the "nothing
   polls" principle deliberately.
4. **Widen the mid-flight refresh.** `MAX_DESTINATION_CAPTURES = 1` dates
   from when the recapture ran on the main thread and cost a frame. It now
   runs on a dedicated thread and through the async SCK path, so refreshing
   the top 2-3 destination tiles is probably nearly free. *Worth measuring.*

## Structural findings

The engines' mechanisms are genuinely different (per-frame IPC writes vs one
layer transform), so a trait over the engines themselves would be forced. The
duplication that hurts is elsewhere:

1. **Three copies of the motion math.** Wall-clock progress with clamp and
   zero-duration guard: `ActiveAnimation::frame_for_now`,
   `RunningCanvas::progress`, `RunningAnimation::progress`. Rect
   interpolation twice: `get_frame`/`blend` (AX) vs `lerp_rect` (overlay).
   Easing twice, with *different curves*: circular ease-in-out (AX,
   hardcoded) vs ease-out cubic (overlay). A resize (AX path) next to a pan
   (overlay path) from the same keystroke follows two different curves.
   Extract a `motion` module: one progress clock, one lerp, one easing table.
2. **`config.settings.animation_easing` is dead.** Plumbed through protocol,
   CLI (`set-animation-easing`), and the config actor — and never read by
   either engine. Wire it into the shared easing table or delete it.
3. **`animate_layout` is the real strategy point, and it is a god function.**
   It computes eligibility, builds *both* engines' inputs (the AX `Animation`
   is fully constructed and then discarded on the overlay path), decides skip
   conditions, detects pans, and dispatches — as a static method reaching
   into `&mut Reactor`. The decomposition: a pure pass-analysis step producing
   a `LayoutMotion` value (moved/unmoved windows, warm targets, pan delta,
   all-translations flag, skip reasons) — independently testable — then
   engine selection, then dispatch through a narrow `present(motion)`
   boundary. That is the strategy seam: "how a settled layout is presented,"
   not "how frames are produced."
4. **app.rs mechanics.** The `set_size; set_position; set_size` triple
   appears four times (`SetWindowFrame`, `SetBatchWindowFrame`,
   `EndWindowAnimation`, `flush_frames`) — one helper. `AnimationFrame`
   (singular) is subsumed by `AnimationFrames`. The `BeginWindowAnimation`
   handler carries two overlapping copies of the same explanatory comment.
5. **Reactor-side canvas builders share boilerplate.** `start_canvas_switch`,
   `start_canvas_pan`, and `warm_all_workspaces` each repeat the
   screen-lookup / gaps / `calculate_layout_for_workspace` loop — and
   `warm_all_workspaces` recomputes every workspace's layout immediately
   after `start_canvas_switch` computed the same layouts.

## Endgame

If the overlay learns resizes — anchor the bitmap top-left in the tile
(`contentsGravity`), animate the tile frame so the picture is cropped or
revealed rather than scaled, apply the real resize once behind the overlay,
recapture at the end — the AX engine's remaining jobs shrink to the
`overlay_animations = false` fallback and the vertical slide in
`workspace_switch_layout`, whose own comments describe it as fundamentally
compromised by the AX top-edge clamp ("instaswap from the top"). At that
point the trajectory is: promote the overlay to the only engine, keep instant
placement as the no-animation path, delete the per-frame AX machinery.

## Detour: resizes stay on AX while it gets a fair trial

The first pass at overlay resizes (a `contentsCenter` nine-part draw, then a
2x2 `contentsRect` crop grid, window entrances, focused shadows and borders,
park-entry fixes, capture-size fixes) accumulated visual bugs faster than it
fixed them, and is parked in `git stash` ("overlay resize round 1+2"). The
trial: give the AX engine — real windows resizing live, real borders,
shadows and blur, apps re-rendering mid-flight — a steady ticker and the
right curve (the two fixes above), and judge whether it is good enough for
resizes. The overlay keeps switches, pans and slides either way. Next after
the trial: per-tile CA animations for the overlay's per-window path, then
the resize question again with whichever engine earned it.

## Order of attack

1. ~~CA-driven canvas animation~~ — done, see the overlay section above.
2. ~~Steady ticker + matching curve for the AX engine~~ — done, see the
   detour note.
3. ~~CA-driven per-window overlay path~~ — done, see the overlay section.
4. Canvas replacement trial: if per-tile chaining stays coherent under rapid
   presses, collapse the canvas path and the pan classifier into per-tile.
5. Shared `motion` module + wire or delete `animation_easing` (small; the
   curves already agree, the definitions should live in one place).
5. Staleness: change-driven warming or a stream pool; measure the mid-flight
   refresh cap first since it is nearly free.
6. `animate_layout` decomposition, next time selection logic changes anyway.
7. The resize question again — un-stash the overlay resize work or keep AX,
   whichever the trial earns.
