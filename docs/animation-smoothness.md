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
   overlay cannot, which since "Resizes through the overlay" landed means: all
   animation when `overlay_animations` is off.
2. **The overlay engine** (`src/actor/workspace_animation.rs` +
   `src/ui/workspace_overlay.rs`). Window bitmaps composited in one opaque
   overlay window; the real windows are placed once mid-flight, hidden
   behind it (see "The apply point"). Ticked by a CFRunLoopTimer at a fixed
   60fps. Handles pure translations: workspace switches (strip), strip pans
   (strip), and per-window slides.

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
(frame placement at the apply point, the one destination recapture,
teardown); nothing is drawn on ticks.

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
  A pass where nothing drawable moves, drains no exit, and joins no flight
  is not flown (`worth_flying`): its windows are placed directly, no overlay.
  A tile starts from the window server's frame when that differs from the
  request (`resolve_start`). A window displaced to a park, or returning from
  one, travels by its neighbour's vector: `neighbour_travel` reads the
  `to - from` of the nearest strip window (by centre x) that is on screen at
  both ends and moves. Leaving, the tile ends at `start + travel`
  (`resolve_end`); returning, it starts at `to - travel`. `final_frames`
  keeps the real park either way. The display edge on the park's side, in
  the tile's own row (`entry_frame`), is only the fallback when no neighbour
  moves. Aimed at the edge, the displaced tile covered a different distance
  from its neighbour under one duration and curve, so it ran at its own speed
  and overlapped it (seen 2026-09-15). A park is judged from BOTH the
  requested frame and the server's: apps clamp a park past the 40pt
  `is_off_screen` threshold (Kiro shows 41pt, Finder 52pt, log 20:35:56),
  and the server may already report the slot. Judged from the server's frame
  alone, the tile flew in from, or slid into, the bottom-right corner. A
  later pass merging into the flight applies the same rule to the tiles and
  entrances it moves by frame alone (`PassTravel`): a strip pass lends its
  pan, a layout pass the vector of its composed tiles.
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
frame placement and teardown cover the newest flights. A pass also moves
what it did not compose: reserved entrances take its frame for their window
(`retarget_entrances`), a flight tile it names by frame but not by tile is
retargeted to that frame (`retargets_from_frames`; a park frame goes through
`PassTravel`, see "Layout changes"), and a strip movement
carries every exit ghost by its travel (`shift_ghosts`, the pan delta from
`AnimateStrip`'s offsets), so the flight ends where the strip ends. Without
that, a pan merging 56ms after an open (log 3:27:20, 22 tiles scrolled
574pt) left the newcomer and the closed window's ghost at their pre-pan
slots: a tear between the active window and its neighbours, then a pop at
lift. `set_tiles` is
pre-flight only: it places tiles at their START, which would end an in-flight
tile's animation on the wrong frame; mid-flight changes go through
`retarget_tile`/`add_tile`. Depth is banded by z-group (`tile_depth` in
`model/z_group.rs`): the strip is one z-order group, so with the focus on a
strip window (or on nothing the flight draws) every floating tile is behind
every strip tile, and with a floating focus the reverse; within a band the
window server's order is kept, and a window the server did not report goes
to the back of its own band. After every merge `restack` rebands the whole
flight from the flight's latest focus, whichever pass composed each tile:
computing depth per pass left a floating tile between two strip tiles when
a later pass named a different focus (the 1.1 interleave in
`.kiro/specs/exit-entrance-animation-regressions`). The real windows are
put in the same order by the reactor's regroup (`strip_regroup`, applied to
the on-screen windows with the raise a focus move issues and again once the
layout pass has been applied; "Real order" in
`capture-overlay-research.md`), so the order the overlay draws is the order
the screen has at lift. Entrance and exit ghosts carry `server_order: Some(0)`, the
front of their own band: an entrance because a window is raised on open, an
exit because the window is gone from the server and has no order to follow.
A pass that
changes any final frame after the apply point re-sends the frames
(`mark_stale_on_untiled_change`). Parked windows have no tile, so a
tile-only check missed their moves and left them at the old park.

A picture landing mid-flight goes to the cache first. It reaches a moving
tile only if the tile is waiting for it (`should_swap_mid_flight`): an
entrance's first picture (claimed or admitted), a grow's settled reveal, or
the destination refresh. Reveal and refresh cut only before 0.6 progress;
later landings are cached for the next flight. Background captures never
reach a tile. They landed mid-flight in 310 of 321 flights (median 4 per
flight), and every cut read as a flicker or a change of transparency:
SkyLight and ScreenCaptureKit render a translucent window differently. On a
resizing tile the cut also re-keyed the resize from the presented state,
which staggered that tile against its neighbours. The destination refresh
runs once per flight at 0.5 (`REFRESH_DESTINATION_AT`), two windows at most
(`MAX_DESTINATION_CAPTURES`): the window being switched into and the one
being left, so both land in their focus rendering. It asks ONE capture
route, the ScreenCaptureKit service `warm_windows` fills the cache from
(`refresh_requests`), and the swap requires the landing picture to come by
the cached picture's route (`same_source`, from `SnapshotSource`). Racing
the service against a framed SkyLight capture swapped every refresh target
two or three times per flight (log 22:34:04: swaps at 0.539, 0.549, 0.561
in one pan): the two routes render a translucent window differently, so a
route change alone failed the thumbprint match, and the next flight's cache
held the other route's picture, repeating forever. The gate is on the
refresh only; a chase's framed reveal is the truth for a grow whatever the
cache holds. Every cut logs "picture swapped mid-flight" with its reason;
that line is the acceptance counter, at most one `reason=refresh` per window
per flight.

**Capture work in flight.** Between frame zero and lift the window server
serves only the chases and the one refresh (`capture_work_allowed`).
Everything else waits for `finish()`. The reactor's `warm_all_workspaces`
(about 15 captures per switch, queued 0.3ms after the start) goes to
`deferred_warm`; the desktop render goes to `deferred_desktop`; hairline
harvests for landed pictures go to the finish harvest set, once per window
per flight. Under the old load the refresh took 203ms median (p90 441,
n=535) against 16-24ms at idle, and 298 of 535 landed inside the flight.
271 of 321 desktop renders landed mid-flight. `warm_all_workspaces` stays in
the reactor's switch handler: the actor knows the flight phase, the reactor
does not.

**Real windows land before lift.** Frames go out on-screen destinations
first, parks last (`frame_send_order`): a park write nobody sees no longer
delays an arriving window. On 54 switches the leaving window was still on
screen at lift, 1000-3446pt from its park. The handover metric
(`handover_report`) excludes off-screen intents. It reports the count over
2pt, the total measured, and the worst visible error. A park clamped by
macOS (y=1116 on a 1117pt display, clamped to 1051) had reported 65pt on 180
flights and masked every smaller error. `finish()` logs "overlay lifted", so
the placing-to-lift gap can be read from the log.

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

Found on the first live run of the dissolved strip path: **floating tiles
were drawn behind the desktop backdrop.** The z-grouping puts the back group
nearly two `GROUP_STRIDE`s deep (about 1<<20), tiles draw at `zPosition =
-depth`, and the backdrop sat at -10000 — so the floating Settings window's
tile was in every composition and visible in none, and never appeared to
ride workspace switches (it rode, behind the wallpaper). `BACKDROP_Z` is now
derived from `z_group::MAX_TILE_DEPTH` instead of guessed, pinned by
`every_possible_tile_draws_between_the_backdrop_and_the_bar`. During strip
pans a floating window deliberately stands still (`pinned`); during switches
it rides its workspace row.

Still open from the canvas dissolution: the reactor-side pan classifier
(`strip_pan_delta`, `take_strip_movement`) only exists to decide
strip-vs-per-window routing, and both routes now land in the same machinery.
Once the strip visuals are validated by eye — rapid chained switches and
pans are the test — the classifier can likely collapse too, though
`take_strip_movement` also feeds the switch's claim on the destination's
scroll offset, which needs care.

## Resizes through the overlay

A resize rides the per-window overlay path instead of the AX engine, ported
from the parked `resize-rounds-1-2` branch onto the per-tile Core Animation
machinery. The tile travels between its two rects like any other tile; what
changes is how the picture maps onto it (`content_mode` in
`workspace_overlay.rs`):

- **A movement with a matching picture stretches.** Picture and frame are the
  same shape, so `kCAGravityResize` is exact. Strip movements always stretch,
  since their tiles never change size mid-flight.
- **Everything else crops, anchored the way each axis really resizes.** A 2x2
  grid of sublayers (`crop_pieces`), every piece mapped 1:1 via
  `contentsRect`. Horizontally, content anchors LEFT and the right band —
  carrying the window's right border and corners — rides the moving right
  edge. Vertically, the TITLE BAR band stays pinned at the top and content
  anchors to the BOTTOM (a terminal's prompt rides the bottom edge), so the
  seam sits just below the title bar; cutting at the bottom instead read as
  the window sliding into a slot. Content never stretches — the moving edges
  swallow or reveal it — and all four rounded corners plus the harvested
  hairline ride through intact. A first cut used `contentsCenter` (nine-part
  stretching); it kept the corners but read as the window stretching, which
  it is. Wrong-shaped cached pictures use the same mapping instead of being
  dropped — rapid preset cycling used to drop the resized window's tile
  because its picture lagged one press behind.
- **The animation is Core Animation end to end.** Piece frames and
  contentsRects are linear functions of the tile frame while the band is
  constant (any side ≥ 89pt), so interpolating between the two endpoint grids
  IS the per-frame crop layout; one transaction installs frame, contentsRect,
  shadow-path, ring-mask and hairline-band animations on the shared curve.
  Below 89pt the band's `min` curve is approximated linearly, drifting the
  seam a few points mid-flight inside the window's own content. Retargets
  continue from the PRESENTED position and size, so rapid preset cycling
  bends the resize instead of snapping it.
- **The band is dynamic:** `min(40pt, 45% of the frame's short side)`, so it
  degrades continuously into a plain reveal as a frame approaches zero — no
  seam pops mid-flight, no band ever wider than its frame.
- **New windows resize in.** A window with no cached picture at all is almost
  always one that just opened; a capture takes ~50-90ms, so there is nothing
  to draw at frame zero. Its tile grows from zero WIDTH at its own left edge,
  full height (`entrance_from`) — a resize from nothing to its final width,
  matching how every other column movement reads. Centred zoom was tried and
  rejected: nothing else on the strip inflates.
- **Entrances are holds.** `entrance_reservation` registers a
  `PendingEntrance` AND puts the window in `awaiting`, next to any grow. The
  flight applies EVERY real frame at frame zero, the newcomer's slot
  included (the overlay already covers them), so the chase captures the
  window at slot size. Holding the slot back until the picture landed was
  tried: the chase then captured the window at its SPAWN size, `claim` took
  it, and drawn top-left in the slot it left the growth as backdrop, a hole
  in the strip (recording of 2026-09-15 3:28:10). `chase_reveal_pictures`
  follows the entering window like a grow: the queued SkyLight capture
  measured 170-300ms under load, most of a flight; the chase's framed
  capture takes 16-24ms once the app has painted. It waits for the real
  frame to fit the slot, then settles on two matching prints. `claim`
  requires the fit for an entrance and a grow alike; a smaller picture is
  refused and the hold goes on. `claim_reveal` adds the zero-width tile to
  the frame-zero composition. The flight
  starts when the last awaited picture lands or `reveal_hold_limit` passes,
  so every tile, entrance included, flies in one `animate_tiles` transaction
  on one duration, and the hairline rides `animate_tile_resize` from the
  zero-width dressing layout. A picture landing after lift-off is admitted
  (`admit_entrance`) with `late_join_duration`: what is left of the flight,
  so it lands with its neighbours and never outlives the overlay. A picture
  that misses the flight altogether shows when the overlay lifts. Before the
  hold, the entrance joined mid-flight with the whole duration and was
  re-keyed to its final size in the same turn: the tile was cut at partial
  width and the real window popped to full. A coalescing merge that moves a
  window whose frame was already applied re-requests the merged frames
  (`reapply_set`); without that the window sat where the first pass put it,
  parked or behind a neighbour, until the strip scrolled.
- **Closed windows resize out.** The reverse of an entrance: the tile shrinks
  to zero width at its own left edge (`exit_to`, the exact mirror of
  `entrance_from`). The real window is gone from the window server before
  rini hears about it, so the exit draws the cached snapshot — the reactor
  sends `AnimateExit` with the last known frame BEFORE dropping the window's
  state, then `ForgetWindow`, in that order: `PendingExit` clones the picture
  before the cache drops it. An exit is never a flight of its own. It waits
  one `COALESCE_WINDOW` for the layout pass that reflows the survivors;
  `start` and `start_strip` drain it into that pass as a ghost tile (from the
  closed frame, the window's own group, front of its band via
  `server_order: Some(0)`), so ghost and survivors go through one
  `set_tiles`/`animate_tiles`. Unclaimed after the window, it is dropped: a
  floating close and a close with no survivors show no overlay. Flown alone,
  a floating close covered the desktop with one ghost and no strip, and a
  strip close under load ran the ghost 25ms+ ahead of the survivors' clock.
  The exit carries no final frame (there is no window left to place) and
  leaves `apply_at` alone. Unmanaged and minimized windows are filtered in
  the reactor; parked and scrolled-off ones by `worth_animating`. A closed
  window with no usable cached picture just disappears, which is the old
  behaviour. The gap between the window vanishing and the ghost appearing is
  AX latency plus the coalesce window, and is accepted: `WindowServerDestroyed`
  does not fire for ordinary closes (3.5M-line log: 384 AX `WindowDestroyed`,
  0 window-server promotions for tracked windows).
- **A grow holds, then reveals.** Every fill for the not-yet-rendered region
  of a grow was tried and rejected by eye: `contentsRect` past the picture's
  edge extends its outermost pixels (a hole to the backdrop on a translucent
  window), and stretching the lead reads as stretching, because it is. So the
  truthful pixels are made to exist first. A pass whose destination outgrows
  its picture (`outgrows`) applies the real frames IMMEDIATELY: the overlay
  is already covering the windows, so the app rerenders at its new size
  behind a still frame. A chase thread polls the real frame every 8ms
  (`REVEAL_CHASE_INTERVAL`, 125 attempts, about a second in all), a cheap
  window-server read; SkyLight-capture polling measured 170-300ms per attempt
  under load and lost the race. Once the size is there it takes ONE framed
  capture per attempt (`capture_via_framed_with_dressing`): the window plus
  one ring, hairline included, the picture cropped back out of the same
  pixels. The frame resizes instantly while the app's pixels lag behind, and
  a capture taken between the two is a half-painted surface: delivering one
  flew the whole reveal with garbage. So a capture counts only when settled
  (`chase_settled`): it matches the previous print (`renderings_match`, 3%
  sample tolerance so cursors and clocks do not stall it), or it differs from
  the picture cached before the resize, which means the app has repainted.
  The second test saves one poll interval on most grows; the first is all an
  entrance has. When it lands (`claim_reveal`), the tile's grid re-maps to it
  and the flight begins: the moving edge reveals genuine final-size content,
  1:1. A shrink crops the picture it has and flies immediately. The hold is
  capped at 300ms (`HOLD_CAP`; `reveal_hold_limit` is 40%-of-flight with a
  300ms floor, then the cap, so the cap wins for every duration). The old
  chase held 235ms median (min 173, max 299; 25ms poll, two framed captures,
  then a separate harvest), and 22 of about 45 holds timed out at 300ms
  anyway. A 150ms cap was tried for a blink-length freeze and raised back
  to 300ms with the fit requirement above: an entrance chase now waits for
  the real frame to reach slot size before it captures, so it needs the
  runway a grow's does. *How often 300ms is enough is not measured yet.*
  An app that misses the cap flies with the old picture STRETCHED over the
  tile (`placeholder_mode`, `Stretch`): every mode fills the whole frame. A
  top-left crop that stopped the grid at the picture's edge and showed the
  backdrop in the growth was tried and read as a hole (3:28:10), the same
  hole recorded above for `contentsRect` past the edge. If the reveal lands
  mid-flight after all,
  it is hard-cut onto the tile before 0.6 progress (`Swap("reveal")`) and
  `set_tile_picture` re-keys the grid from the PRESENTED state over the
  remaining duration; later than that it is cached for the next flight.
- **A fresh picture or hairline swaps in place.** Rebuilding the dressing on a
  mid-flight recapture snapped the border to its final layout while the tile
  was still travelling; a matching harvest now swaps pixels into the existing
  layers and rides their animations. The match is on harvested pieces only,
  not on the tile's size: a run with no extent (an entrance dressed at zero
  width) still gets a zero-size layer, so a tile wears the same 8 pieces at
  every size. A harvest can still come back short: a corner the alpha check
  rejects on a partly covered window, or a window caught mid-resize. A
  mismatched set is never rebuilt while a resize animates
  (`dressing_rebuild_allowed`, `Tile.resize_until`): the worn ring stays on
  the resize timeline, the new dressing is already on the cached snapshot,
  and the next `set_tiles` wears it. Rebuilding placed the pieces at the
  destination rectangle while the picture was still halfway.
- **The apply point.** Layout passes place the real windows at 0.75
  (`APPLY_FRAMES_AT`), or at 0.5 (`APPLY_FRAMES_AT_RESIZE`) when any tile
  resizes: the real resize behind the overlay costs three synchronous AX round
  trips per window and needs more runway to land before the overlay lifts.
  Strip movements place at 0.5 too (`APPLY_FRAMES_AT_STRIP`). Their frames
  are pure moves, but a switch sends about 17 of them, serialized per app
  actor and sharing it with window rediscovery. At 0.75 the gap from
  "placing real windows" to lift measured 90ms median, p90 156, and 0.75 of
  a 300ms switch leaves 75ms.
- A pass containing a resize never becomes a strip pan, even when the strip
  offset moved: the strip surface draws final sizes, which would snap the
  resize. The strip movement is still consumed so the offset bookkeeping
  stays current.
- The caster's ring mask is created once per tile and reshaped in place; a
  resize animates its path and the shadow's silhouette between the two
  endpoint shapes, which interpolate because both are built by the same
  constructors.

## Window borders during animations

The long-standing "windows go flat and flicker" complaint took three attempts
because the border's identity was misdiagnosed twice:

1. A config-driven drawn border (bronze, mirroring the bordersrc) mismatched
   reality in the other direction — a redrawn border flickers against the
   real one at the handover just as visibly as no border did.
2. Companion tiles for JankyBorders' border windows — correct mechanism,
   wrong target: the `borders` process turned out not to be running at all
   (`bordersrc` exists, nothing draws it). Live enumeration showed zero
   border windows.
3. **The border that actually flickers is macOS's own window outline**: a
   1pt hairline the window server composites over every window's outermost
   point, present at rest on every window, absent on every tile mid-flight,
   because it is drawn outside the app's surface exactly like the shadow.

A fourth attempt — drawing it as a measured constant (1pt white stroke,
0.25 alpha focused / 0.16 unfocused) — was pixel-exact on opaque windows
and wrong by 2x on translucent ones: lossless captures read the focused
hairline at rgb 80 on an opaque editor but rgb 44-46 on a see-through
terminal, because the composite depends on the window's own edge pixels
and translucency. No drawn constant can match every window.

So the real composited pixels are harvested instead (`edge_dressing`):
`CGWindowListCreateImage` is the one capture API that composites framing
into its output, and a rect-bounded call returns exactly the asked rect
with the framing composited, at 16-24ms. The framing is two lines — the
light hairline on the window's outermost point and a near-black outline
just outside the bounds, which is what separates border from shadow — so
the harvested band straddles the boundary, one point in and one out. Four
straight runs plus four corner boxes clipped to the outline's arc, ~200KB
against the 28MB framed capture they are cropped from, cached on the
snapshot and worn by the tile as sublayers. It only renders windows
actually composited, so a parked window harvests transparent pixels and is
rejected by an alpha check, keeping the ring from when it was last seen:
the picture cache's own staleness model. The focused ring lands with the
post-flight harvest of the animated set (`finish`), not mid-flight: the
destination refresh no longer harvests, since it no longer takes the framed
route (see "Mid-flight passes").

The companion-tile machinery from attempt 2 stays (`companion_of` /
`companion_tiles`): it is the right answer for anyone whose border tool IS
running, carrying real border windows as tiles:

- Detection is geometric and tool-agnostic: an unmanaged window concentric
  with a managed one (centers within 4pt) and the same size or up to 8pt
  larger is that window's border. Candidates exclude every window in the
  pass, so stacked twins cannot match each other; one border window traces
  one window (`claimed`).
- The companion rides at its real relative offset from the window's tile,
  drawn a quarter depth-step in front of it (under the next tile forward,
  clear of the half-step shadow casters), with no shadow of its own.
- Captured and cached like any window, keyed by synthetic ids; no picture
  yet means skipped this flight and warmed for the next. Companions join the
  post-flight warm set because borders recolor with focus.
- During strip movements the border rides only where the window genuinely
  is: an arriving row's window sits parked with its real border parked too,
  so no companion matches — which is what the real screen does, since the
  border tool only catches up after the window lands.
- The mid-flight focus recapture rode the framed route (16-24ms measured,
  against 170-300ms for SkyLight under animation load) until racing it
  against the ScreenCaptureKit route was found to swap the tile 2-3 times
  per flight; it now takes the service route only, and the swap requires
  the same route as the cached picture (see "Mid-flight passes"). The swap
  itself is a hard cut ON PURPOSE: a ~120ms crossfade veil was tried and rejected,
  because stacking two copies of a translucent window pulses its net opacity
  mid-fade — there is no constant-alpha crossfade with layers. Gratuitous
  cuts are avoided upstream instead: a swap whose picture renders the same
  as the one on screen (thumbprint comparison) is skipped entirely.
- Companions are excluded from the mid-flight destination recapture, which
  exists for the window the eye is on. They wear no harvested hairline
  either: a border window's ring is transparent almost everywhere, so the
  harvest's alpha check rejects it without a special case.

No configuration in either mechanism: the outline is the platform's, and the
companions reproduce whatever a border tool draws, or nothing.

## Snapshot staleness

Staleness is accepted by construction ("a slightly stale moving image is not
perceptible", `capture-overlay-research.md`) and the worst case — the
destination's focus appearance — is patched mid-flight
(`refresh_destination_among`, once per flight at 0.5, two windows). What that
does not cover: content that changed while parked. Terminal output, chat,
anything live — warmed only at animation end, focus change, and layout
passes, so a window that repainted itself while hidden is stale until the
next switch touches it. Capturing at switch time instead is ruled out by
measurement: 4 windows cost 94.5ms against a 180ms budget. Warming DURING the
flight is ruled out too: see "Capture work in flight" above.

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
3. **Aged-cache sweeps.** `WindowSnapshot` is timestamped (`taken`) and
   `warm_windows` re-warms anything older than `SNAPSHOT_STALE_AFTER` at
   animation end. A sweep of nearby workspaces on a slow idle timer is not
   done; it would break the "nothing polls" principle deliberately.
4. **Widen the mid-flight refresh.** Ruled out by measurement. The refresh
   runs on its own thread, but the window server does not: with warms, the
   desktop render and harvests queued behind it, the framed refresh took
   203ms median against 16-24ms at idle (see "Capture work in flight").
   Two windows at 0.5 is the cap; more capture work in flight, not less, is
   what slowed it.

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
