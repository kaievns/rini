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
   translations: workspace switches (strip), strip pans (strip), and
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

## The overlay engine: one per-tile Core Animation machinery

Every overlay movement is a group of per-tile animations handed to the render
server in ONE `CATransaction` (`begin_group` in `workspace_animation.rs`,
`animate_tiles` in `workspace_overlay.rs`): one commit, one timebase, one
curve, so tiles start on the same beat, cannot tear against each other, and
render vsync-locked at the display's native refresh, immune to main-thread
stalls. Model layers jump to their destinations; the animations carry the
presentation and are removed on completion, revealing the model — no
snap-back, no delegate. The tick loop paces only the mid-flight orchestration
(frame placement at `APPLY_FRAMES_AT`, destination recaptures, teardown);
nothing is drawn on ticks.

This evolved in three steps, each replacing a weakness the previous one
measured. The original manual tick loop (60Hz `RepeatingTimer` posting into
the actor queue) was not vsync-aligned, capped at 60fps, and coupled to the
main thread — dropped drawn frames were the observed difference between
smooth and "instant cut". A dedicated canvas layer then carried switches and
pans as one animated property, guaranteeing group rigidity structurally.
Finally the canvas was dissolved into the per-tile machinery: animations
committed in one transaction share clock and curve, so a rigid group slide
holds together without a single-layer guarantee (pinned by
`a_strip_movement_translates_every_window_by_the_same_vector`), and one
machinery for every movement means a switch chaining onto a slide — or the
reverse — merges instead of superseding it with a snap.

The two entry points feed the same `begin_group`:

- **Layout changes** (`Event::Animate` — window open/close, column reorder,
  join/unjoin, anything whose windows move by different vectors) start
  `Coalesced`: the overlay shows at frame zero and the movement begins once
  the reactor's layout passes settle (25ms), installed by `start_moving`.
- **Strip movements** (`Event::AnimateStrip` — workspace switches and strip
  pans; the wire event the reactor builds from the stacked-workspace
  geometry in `model/strip_stack.rs`) start `Immediate`: they arrive once
  per keystroke and latency is the enemy. `strip_travel` translates every
  window on the strip surface by the viewport's travel; pinned (floating)
  windows get `from == to` and are drawn standing still. Visual destinations
  are deliberately distinct from `final_frames`: a leaving window animates
  off-screen while its real frame goes to a park.

Mid-flight passes are classified per tile (`merge_action`, tested):
**redundant** (same destination within `same_as` tolerance — touched not at
all, which is what keeps rapid presses from restarting or extending the
flight), **retargeted** (the tile bends from its presentation position to the
new destination over a fresh duration), or **joined** (installed and animated
from its own start). Any real change restarts the orchestration clock so the
frame placement and teardown cover the newest flights. `set_tiles` is
pre-flight only: it places tiles at their START, which would end an in-flight
tile's animation on the wrong frame; mid-flight changes go through
`retarget_tile`/`add_tile`.

Mechanics worth remembering:

- The curve is preserved exactly. `ease_out_cubic` (`1 - (1-t)^3`) is
  precisely the cubic Bezier timing function with control points
  `(1/3, 1)` and `(2/3, 1)`: with x-control-points at 1/3 and 2/3 the
  Bezier's x(t) collapses to t (the Bernstein terms sum to
  `t[(1-t) + t]^2 = t`), and y(t) with both y-controls at 1 expands to
  `1 - (1-t)^3`. So `CAMediaTimingFunction(controlPoints: 1/3, 1, 2/3, 1)`
  is not an approximation. Pinned by
  `the_core_animation_curve_is_exactly_ease_out_cubic`.
- `CAAnimation` treats a zero duration as "use the default 0.25s", so zero
  durations bypass the animation and draw the final frame directly.
- `NSValue::valueWithPoint` (the from/to carrier) is gated behind the
  `NSGeometry` + `objc2-core-foundation` features of `objc2-foundation`.

Still open from the canvas dissolution: the reactor-side pan classifier
(`strip_pan_delta`, `take_strip_movement`) only exists to decide
strip-vs-per-window routing, and both routes now land in the same machinery.
Once the strip visuals are validated by eye — rapid chained switches and
pans are the test — the classifier can likely collapse too, though
`take_strip_movement` also feeds the switch's claim on the destination's
scroll offset, which needs care.

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

1. **Two copies of the motion math.** Wall-clock progress with clamp and
   zero-duration guard: `ActiveAnimation::frame_for_now` and
   `RunningAnimation::progress`. Rect interpolation twice: `get_frame`/
   `blend` (AX) vs `lerp_rect` (overlay). The curves agree now (the AX
   `ease` delegates to the overlay's `ease_out_cubic`), but the definitions
   should live in one `motion` module.
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
5. **Reactor-side strip builders share boilerplate.** `start_strip_switch`,
   `start_strip_pan`, and `warm_all_workspaces` each repeat the
   screen-lookup / gaps / `calculate_layout_for_workspace` loop — and
   `warm_all_workspaces` recomputes every workspace's layout immediately
   after `start_strip_switch` computed the same layouts.

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

1. ~~CA-driven canvas animation~~ — subsumed by 3 and 4.
2. ~~Steady ticker + matching curve for the AX engine~~ — done, see the
   detour note.
3. ~~CA-driven per-window overlay path~~ — done, see the overlay section.
4. ~~Dissolve the canvas into per-tile groups~~ — done (and renamed: the
   group event is `AnimateStrip`, the geometry module `strip_stack`).
   Verdict pending eyes on rapid chained switches and pans.
5. Pan classifier collapse (`strip_pan_delta`, routing in `animate_layout`),
   once the strip visuals are validated; `take_strip_movement` also feeds
   the switch's scroll-offset claim and needs care.
6. Shared `motion` module + wire or delete `animation_easing` (small; the
   curves already agree, the definitions should live in one place).
7. Staleness: change-driven warming or a stream pool; measure the mid-flight
   refresh cap first since it is nearly free.
8. `animate_layout` decomposition, next time selection logic changes anyway.
9. The resize question again — un-stash the overlay resize work (re-keyed to
   the per-tile machinery) or keep AX, whichever the trial earns.
