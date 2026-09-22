# Animation smoothness: findings and plan

An audit of both animation engines, written 2026-08-27. Numbers quoted from code
comments were measured when those fixes landed; anything marked *estimated* has
not been measured yet. Companion to `capture-overlay-research.md`, which holds
the capture measurements this builds on.

## One engine

Every animated movement runs through the overlay engine
(`src/animation/platform/`: `engine.rs` + `overlay.rs`, geometry in `src/animation/domain/motion/`): window
bitmaps composited in one opaque overlay window, the real windows placed once
behind it (see "The apply point"). Layout passes, strip pans, workspace
switches, resizes, entrances and the edge bounce are all flights of it.
`AnimationManager` (`src/app/reactor/animation.rs`) is the layout side: it
gathers a `PassWindow` per window from the stores, sorts the pass with
`crate::animation::domain::pass::plan` (moves, unmoved windows the overlay must still
draw, warm targets), decides whether the overlay flies (`config.settings.animate`,
not low power, not a drag, something visibly travels) and places the real windows
directly when it does not.

The per-frame Accessibility engine that preceded it (`AXPosition`/`AXSize`
writes into every owning app on every tick, `animation_fps`, the
`BeginWindowAnimation`/`AnimationFrames`/`EndWindowAnimation` app requests,
the top-clamped vertical slide) was removed once resizes rode the overlay:
its cross-app skew (100-150px between neighbouring columns mid-scroll) was
the mechanism's, not a bug in it, and the overlay never had it. The
"Endgame" this document set out is done; what follows describes the one
engine that is left.

## The overlay engine: containers carry the rigid pieces

Every overlay flight is a `FlightPlan` (`crate::animation::domain::motion::plan`): a
set of rigid groups (`RigidGroup`, keyed `GroupKey::Rigid`), a loose set (resizes and entrances), and the
floating windows. Each group is one `CALayer` container under the overlay's
root (`TileOverlay::install` in `overlay.rs`). A container's `position` is the
only animated translation its members get; the tiles inside sit at
group-relative frames and never move on their own. Resizes and entrances
are loose tiles under `StripLoose`; floating windows sit in the `Floating`
container. `fly` hands the whole plan to the render server in ONE
`CATransaction`: one position animation per moving container, one resize
per loose tile, one timebase, one curve. Model layers jump to their
destinations; the animations carry the presentation and are removed on
completion. The tick loop paces only the mid-flight orchestration (frame
placement at the apply point, the one destination recapture, teardown);
nothing is drawn on ticks. Every overlay commit is flushed to the render
server at once (`commit_now`): the overlay shares the main thread with the
reactor, and an explicit commit nested in the run loop's implicit
transaction is only sent when that iteration ends, after the reactor's
synchronous AX and window-server calls. Sent late, a flight skipped its
first frames. Every leg's animations carry an explicit `beginTime`
(`Timing`), so the render server's clock and the actor's agree; "flight
landed" logs how late the last frame was presented (`late_ms`).

The model has been round the loop twice. The original manual tick loop
(60Hz `RepeatingTimer` posting into the actor queue) was not vsync-aligned
and dropped drawn frames. A dedicated canvas layer then carried switches
and pans as one animated property, rigid by construction. The canvas was
dissolved into per-tile animations committed in one transaction: that
bought one machinery for every movement, and continuity across a chained
switch and slide, because each tile bent from its own presented position.
It cost rigidity on every merge. Each tile was retargeted from its own
presented position on its own fresh clock, and a pass merging mid-flight
(a pan 56ms after an open, a resize during a pan) sent tiles of one strip
off on slightly different legs. The user saw the strip teleport. Containers
keep the continuity (a container is retargeted from its presented position
the same way) and restore rigidity structurally: members of a group cannot
drift because only the group moves.

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
  alone, the tile flew in from, or slid into, the bottom-right corner. The
  resolved `(start, end)` pairs become a `ReflowPlan` (`reflow_plan`): tiles
  moving by the same vector within 2pt (`GROUP_TOLERANCE`) share a group,
  a resize is loose, a floating window is floating, and the still windows
  are `groups[0]`. The park rule above decides which group a displaced
  window rides, so it slides with its neighbour by construction. A pass is
  flown when something drawable moves or a flight is running
  (`worth_flying`).
- **Strip movements** (`Event::AnimateSurface` — workspace switches and strip
  pans; the wire event the reactor builds from the stacked-workspace
  geometry in `animation/domain/motion/strip_stack.rs`, carrying `SurfaceWindow`s) start `Immediate`: they arrive once
  per keystroke and latency is the enemy. `surface_plan` puts every window
  on the surface in ONE group travelling by the viewport's travel
  (`surface_travel`, `pan_travel`): one container, one position
  animation. Pinned (floating) windows stand in the floating container with
  `from == to`; a switch moves the floating container itself by the same
  travel (`floating_travel`). Visual destinations are deliberately distinct
  from `final_frames`: a leaving window animates off-screen while its real
  frame goes to a park.

Mid-flight passes go through `merge_plans` (pure, tested), which retargets
containers, not tiles. A rigid member of the incoming pass votes for its
group's new position (`p = to - rel`); the largest cluster keeps the
container, which bends from its presented position to the new destination
under the same animation key (`GROUP_ANIMATION_KEY`, `"rini.group.move"`). Members voting
elsewhere are reparented at the frame they are drawn at, into a group whose
remaining travel matches theirs, or into a new one. A member the pass sends
off the viewport does not vote while its container moves (`rides_out`): it
has no visible destination, and on a switch the layout pass 16ms later
parked the departing row's windows, each of which opened a sideways group
and swept to the park's edge while the row travelled vertically (the
zig-zag; log 1:07:36, "member reparented" to (1727,1065) and (-1719,1076)).
A member of a still container has no motion to ride and leaves on its own
vector. A rigid member the pass now resizes goes loose (`StripLoose`) at
its presented frame. A newcomer
joins a group with a matching remaining travel or opens one; joins are
resolved after the votes, because the votes change the remaining travel. A
pan adds its travel to every group and every loose strip tile and changes
no membership; members the pan does not compose (no usable picture) ride
their group all the same. A pass confirming the flight's destinations is an
empty `PlanDelta`: nothing is touched, and rapid presses neither restart
nor extend the flight. Any real change restarts the orchestration clock so
the frame placement and teardown cover the newest legs. Reserved entrances
still waiting for a picture take the pass's slot (`retarget_entrances`).
The overlay applies the delta in one transaction (`retarget`): reads of
every container's presented position first, then reparents, container
animations, joins, loose retargets. Without this a pan merging 56ms after
an open (log 3:27:20, 22 tiles scrolled 574pt) left the newcomer at its
pre-pan slot: a tear between the active window and its neighbours, then a
pop at lift. Cross-container z is a tie rule, not a guarantee: strip
windows do not overlap at rest, so overlap between two moving groups is
transient, and the group holding focus is drawn first.

Depth is banded by z-group (`tile_depth` in `animation/domain/motion/z_group.rs`) at the
container level (`band_plan`, `rebank`). The floating container sits at
`container_z`: zero with a floating focus, one `GROUP_STRIDE` behind with a
strip focus (or no focus the flight draws); strip containers the other way
round, a quarter step apart in `Banding.group_order`. Each tile sits at its
within-band depth inside its container (`Banding.within`; a companion a
quarter step in front of its window, a shadow half a step behind its
picture). `container_z - within` is `-tile_depth`, so the drawn order is
what `restack` computes for `tile.depth` and what the reactor's regroup
(`regroup_tiled`, "Real order" in `capture-overlay-research.md`) puts on the
real screen at lift. Core Animation sorts `zPosition` among siblings only,
so a floating tile can never land between two strip tiles whatever its own
depth: that is the structural fix for the 50/50 interleave (the 1.1 case in
`.kiro/specs/exit-entrance-animation-regressions`). Within a band the
window server's order is kept; a window the server did not report goes to
the back of its own band. An entrance carries `server_order: Some(0)`
because a window is raised on open. A pass that changes any final frame
after the apply point re-sends the frames (`mark_stale_on_untiled_change`).
Parked windows have no tile, so a tile-only check missed their moves and
left them at the old park.

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
has one slot per flight, at 0.5 (`REFRESH_DESTINATION_AT`), and recaptures
only on a focus change, only its two ends (`refresh_targets`, against the
previous flight's `last_focus`): the window being switched into and the one
being left, so both land in their focus rendering. Focus changes a
window's rendering without changing its size (measured on a 1pt window
border: 65 of 255 focused against 42 unfocused), so `Event::RefreshFocus`
also recaptures a window whenever focus moves to or from it, whatever the
size test says about its cached picture. A flight that moves
focus nowhere recaptures nothing. It used to recapture the two frontmost
tiles by depth on every flight; a translucent window's two captures differ
by the wallpaper behind it, so that cut the two front Ghostty tiles at 0.55
on every strip pan (log 2026-09-16 2:05). It asks ONE capture
route, the ScreenCaptureKit service `warm_windows` fills the cache from
(`refresh_requests`), and the swap requires the landing picture to come by
the cached picture's route (`same_source`, from `SnapshotSource`). Racing
the service against a framed SkyLight capture swapped every refresh target
two or three times per flight (log 22:34:04: swaps at 0.539, 0.549, 0.561
in one pan): the two routes render a translucent window differently, so a
route change alone failed the thumbprint match, and the next flight's cache
held the other route's picture, repeating forever. The check blocks the
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
does not. After the lift the owed captures wait for a quiet period
(`SETTLE_BEFORE_CAPTURES`, 400ms; `Event::Quiet`, `after_flight_captures`).
They used to run the instant the overlay lifted and took 600-800ms (8-15
window captures and the desktop render), so a press inside that window flew
the next flight against a busy compositor: frame-difference profiles of a
screen recording showed 50-130ms freezes followed by catch-up jumps. A flight
beginning inside the quiet period cancels the timer; the work carries over to
its own lift. The bar is recaptured `BAR_REFRESH_DELAY` (250ms) after a
flight, never at the start of one: a bar composite measures 31ms median,
and the delay lets the compositor drop the hidden overlay from the
framebuffer and folds a burst of switches into one capture, at the end.

**Real windows land before lift.** Frames go out on-screen destinations
first, parks last (`frame_send_order`): a park write nobody sees no longer
delays an arriving window. On 54 switches the leaving window was still on
screen at lift, 1000-3446pt from its park. The handover metric
(`handover_report`) excludes off-screen intents. It reports the count over
2pt, the total measured, and the worst visible error. A park clamped by
macOS (y=1116 on a 1117pt display, clamped to 1051) had reported 65pt on 180
flights and masked every smaller error. `finish()` logs "overlay lifted", so
the placing-to-lift gap can be read from the log. The lift itself waits for
the render server (`lift_now`, `TileOverlay::settled`): the clock has
to run out AND every container and picture has to be presented at its model
position within half a point, or `LIFT_GRACE` (350ms) past the clock. The
render server runs a frame or so behind the actor's clock, and lifting on
the clock alone showed the real windows one frame ahead of their tiles: a
small jerk at the end of every flight. Strip movements send their frames at
frame zero (`APPLY_FRAMES_AT_PAN` 0.0): 24 of 162 flights had lifted with
every window 1700-2600pt from its tile because 17 writes across Electron apps
took longer than half a flight. A park-to-park write is not sent at all
(`is_park_to_park`, reactor `apply_overlay_frames`): 12 of a pan's 20 frames
moved windows from one park to another, nothing visible changed, and each
made its app repaint under the flight. A park is a sliver still touching the
display; a frame wholly off it is not. A switch leaves its departing row a
display height below, which macOS clamps to a 41pt band along the bottom
edge, and the next pass moves that band to the corner. Treating the row as a
park skipped that write: the band stayed on screen and the windows drifted
into the active workspace on the next reconciliation. The current frame is
the window server's, not the model's (`frame_write_needed`, pinned by
`park_write_is_judged_from_the_real_frame_not_the_model`): a write the app
dropped leaves the model saying "parked" while the window sits on screen,
and every later pass would have skipped the repair (a full Ghostty window
sat on the right half of the display under the strip). Tiles sit on whole points (`whole` in
the overlay): layout offsets are fractional (4592.67) and the window server
places windows on whole points, so a tile drawn at the fraction was resampled
and popped by the fraction at the lift.

Mechanics worth remembering:

- **The curve.** One cubic Bezier, `MOTION_CURVE` `(0.16, 1, 0.3, 1)`, an
  exponential ease-out. The actor's clock evaluates it by
  solving the Bezier for time (`CubicBezier::ease`, Newton then bisection)
  and Core Animation gets the same four control points (`motion_timing`),
  so the apply point and the drawn motion agree; pinned by
  `the_clock_and_the_render_server_run_one_curve`. Progress at quarter,
  half and 70% of the duration: 0.87, 0.97, 0.995. Ease-out cubic
  (`1 - (1-t)^3`, the thirds Bezier `(1/3, 1, 2/3, 1)`, for which x(t) is
  the identity) ran before it and read as sluggish at 350ms: 0.58, 0.875,
  0.973 at the same marks, so the whole second half of every flight was a
  crawl through the last 12.5% of the distance. The duration was right; the
  tail was the problem. Pinned by `the_motion_is_nearly_home_by_half_time`.
- `CAAnimation` treats a zero duration as "use the default 0.25s", so zero
  durations bypass the animation and draw the final frame directly.
- `NSValue::valueWithPoint` (the from/to carrier) requires the
  `NSGeometry` + `objc2-core-foundation` features of `objc2-foundation`.

Found on the first live run of the per-tile strip path: **floating tiles
were drawn behind the desktop backdrop.** The back group sits one
`GROUP_STRIDE` deep (about 1<<20) and the backdrop sat at -10000, so the
floating Settings window's tile was in every composition and visible in
none. `BACKDROP_Z` is derived from `z_group::MAX_TILE_DEPTH`, pinned by
`every_possible_tile_draws_between_the_backdrop_and_the_bar`; the containers
sit between it and the bar. During strip pans a floating window deliberately
stands still (`pinned`); during switches it rides its workspace row in the
floating container.

Still open: `take_strip_movement` only exists to decide strip-vs-per-window
routing, and both routes land in the same machinery. It also feeds the
switch's claim on the destination's scroll offset, which needs care. The
window-voting classifier (`strip_pan_delta`) is gone: it only ran when the
space had no strip, which is when there is nothing to pan.

**Edge bounce.** A command that pushes past an end of the strip (focus
left/right at the first/last column) or of the workspace stack (next/prev at
the bottom/top with `prevent_wrapping`, or nothing further to skip to) used
to stop dead, which read as a dropped keypress. The layout reports it as
`EventResponse::edge_hit` (`move_focus_internal`'s fallback for left/right
only; `handle_virtual_workspace_command` for up/down), distinct from
`boundary_hit`, which is the gesture's threshold crossing. The reactor
(`start_edge_bounce`) sends `Event::Bounce` with the active workspace's surface
and `edge_bounce_overshoot` (`src/app/reactor/animation.rs`): `EDGE_BOUNCE_OVERSHOOT` (36pt) the way the
content would have gone, so focus right pulls the strip left and the next
workspace pulls the row up. The actor (`start_bounce`) composes a flight with
no travel when none is running (every tile at rest, `final_frames` the layout
the windows already sit at) and adds the bounce to whatever is running
otherwise, extending the clock to cover the return (`clock_for_bounce`). The
overlay's `bounce` is one additive `CAKeyframeAnimation` per container
(`0, overshoot, 0` at `0, BOUNCE_TURN, 1`; ease-out then ease-in-out) under
its own key, so an in-flight movement is neither replaced nor disturbed and
the model positions stay put; `settled` reads false until it is home, which
holds the lift. The floating container rides only a vertical bounce
(`bounce_carries`), the rule a pan (pinned) and a switch (carried) already
follow. Real windows never move. The shape is `bounce_displacement`, pinned
by `a_bounce_goes_out_once_and_comes_home`.

## Resizes through the overlay

A resize rides the per-window overlay path, ported
from the parked `resize-rounds-1-2` branch onto the per-tile Core Animation
machinery. The tile travels between its two rects like any other tile; what
changes is how the picture maps onto it (`content_mode` in
`crate::animation::domain::motion::tile`):

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
  seam a few points mid-flight inside the window's own content. A loose
  tile's retarget continues from the PRESENTED position and size, so rapid
  preset cycling bends the resize instead of snapping it.
- **The band is dynamic:** `min(40pt, 45% of the frame's short side)`, so it
  degrades continuously into a plain reveal as a frame approaches zero — no
  seam pops mid-flight, no band ever wider than its frame.
- **A window that opens travels from its spawn frame.** macOS shows a new
  window at its own frame about 40ms before rini's first pass (AX
  `WindowCreated` latency plus the coalesce window); that time is accepted.
  `start` reads that frame from the window server and takes ONE framed
  capture there (`capture_via_framed_with_dressing`, 16-24ms), at most
  `MAX_SYNC_ENTRANCE_CAPTURES` (4) per pass. The tile is a loose
  `Member::Entrance` from the spawn frame to the slot, stretched
  (`placeholder_mode`) until the reveal chase lands the slot-size picture,
  which `claim` takes at frame zero (`Claimed::Refreshed`) or the
  `Swap("reveal")` cut takes before 0.6. No hold: the newcomer's slot alone
  goes out at frame zero (`frame_zero_work`) so the chase has something to
  capture, and again with every frame at the apply point, one redundant AX
  write per open. A frame off this display is not a spawn: a parked window
  with a cold cache took a 1s off-screen capture and flew in from the park,
  so it takes the reservation instead. A window already at its slot with no
  picture is captured as a still tile and not chased. Growing from zero
  width (`entrance_from`; a centred zero-size zoom was tried first and read
  as the window inflating, which nothing else on the strip does) was the
  previous entrance and read as a pop then a
  vanish; it survives only in the reservation fallback below.
- **The reservation fallback.** With no server frame, a zero frame, an
  unusable capture or no budget left (`entrance_plan`, reason logged as
  "entrance reserved"), `entrance_reservation` registers a `PendingEntrance`
  AND puts the window in `awaiting`, next to any grow. The flight applies
  EVERY real frame at frame zero, the newcomer's slot included (the overlay
  already covers them), so the chase captures the window at slot size.
  Holding the slot back until the picture landed was tried: the chase then
  captured the window at its SPAWN size, `claim` took it, and drawn top-left
  in the slot it left the growth as backdrop, a hole in the strip (recording
  of 2026-09-15 3:28:10). `chase_reveal_pictures` follows the entering
  window like a grow: the queued SkyLight capture measured 170-300ms under
  load, most of a flight; the chase's framed capture takes 16-24ms once the
  app has painted. `claim` requires the fit; a smaller picture is refused
  and the hold goes on. `claim_reveal` adds the zero-width tile to the
  frame-zero composition (`install`). The flight starts when the last
  awaited picture lands or `reveal_hold_limit` passes, so every tile flies
  in one `fly` transaction. A picture landing after lift-off is admitted
  (`admit_entrance`, `add_tile` under `StripLoose`) with
  `late_join_duration`: what is left of the flight. A picture that misses
  the flight altogether shows when the overlay lifts. A coalescing merge
  that moves a window whose frame was already applied re-requests the
  merged frames (`reapply_set`); without that the window sat where the
  first pass put it until the strip scrolled.
- **A closed window disappears.** Its survivors' pass is an ordinary
  reflow: one group sliding by the closed width. Nothing is drawn for the
  window itself. A shrinking ghost was tried (a resize to zero width from
  the cached picture, `PendingExit`, `AnimateExit`) and removed: a ghost is
  a per-tile discontinuity in a flight that is otherwise rigid, and a
  picture of a window that no longer exists. Under load it ran 25ms ahead
  of the survivors' clock; on the window-server close path it composed
  `exits=0` and left a gap (log 2026-09-16 2:05). What stays is
  `ForgetWindow`, sent once per close on either path: the AX
  `WindowDestroyed` while the window still exists, or the window-server
  disappearance through `EventOutcome::forgotten_windows` ahead of the
  layout events. The gap between the window vanishing and the survivors
  moving is AX latency plus the coalesce window, and is accepted.
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
  (`chase_settled`): it matches the previous print (`renderings_match` on
  32x32 RGBA thumbprints, a sample differing when any channel moves by more
  than 8, up to 3% of samples allowed so cursors and clocks do not stall it), or it differs from
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
  mid-flight after all, it is hard-cut onto the tile before 0.6 progress
  (`Swap("reveal")`) and `set_tile_picture` re-installs the resize leg on
  the SAME `Timing` (begin on the media clock plus duration, kept per tile
  in `resize_leg`): the frame keeps its curve and only the pixels cut.
  Re-keying from the presented frame over the remaining duration restarted
  the ease-out and the tile lurched at every swap. Later than 0.6 the
  picture is cached for the next flight.
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
  and the next `install` wears it. Rebuilding placed the pieces at the
  destination rectangle while the picture was still halfway.
- **The apply point.** Layout passes place the real windows at 0.75
  (`APPLY_FRAMES_AT`), or at 0.5 (`APPLY_FRAMES_AT_RESIZE`) when any tile
  resizes: the real resize behind the overlay costs three synchronous AX round
  trips per window and needs more runway to land before the overlay lifts.
  Strip movements place at frame zero (`APPLY_FRAMES_AT_PAN`; the
  measurements are under "The apply point"). Their frames are pure moves,
  but a switch sends about 17 of them, serialized per app actor and sharing
  it with window rediscovery. At 0.75 the gap from "placing real windows" to
  lift measured 90ms median, p90 156, and 0.75 of a 300ms switch leaves 75ms;
  0.5 was tried next and still lost 24 of 162 flights.
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
the picture cache's own staleness model. The check believes a ring when the
straight runs' mean alpha is at least 0.5 (`MIN_MEAN_ALPHA`): a parked
window reads 0, the most translucent real edge measured (a see-through
terminal) 244 of 255, and a border tool's overlay window fails it without a
special case. Corner boxes are legitimately transparent outside the arc, so
only the four runs are judged. The ring is 1pt (`RING_PT`), two device
pixels at 2x. The harvest runs on a plain thread after `collect`, never
inside a ScreenCaptureKit completion: on modern macOS
`CGWindowListCreateImage` is proxied through the same capture machinery, so
a call from the delivery queue deadlocks its own reply until a ~20s
timeout and every capture in the process serialises behind it (measured as
half-minute window switches). The focused ring lands with the
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
- Nothing rini manages can be a companion (`managed_server_ids`: the pass's
  windows plus every window the cache holds or owes a picture to), and a
  parked window never traces or is traced (`companion_of`). Every parked
  window shares the park's frame, so a window arriving from the park matched
  another parked window as its "border" and flew in wearing that window's
  picture: two Chrome windows on different workspaces swapped pictures on
  every switch between them. The geometric test alone did not catch it
  because macOS clamps the park to 41pt visible, one past `is_off_screen`'s
  40. An arriving row's window therefore gets no companion, which is what
  the real screen does, since the border tool only catches up after the
  window lands.
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

**Companion geometry.** A border window is recognised by geometry alone:
same centre within `COMPANION_CENTER_SLACK` (4pt), and at most
`COMPANION_EXPANSION` (8pt) larger per axis. JankyBorders draws its stroke
on a sibling window about twice the stroke width larger than the traced
one, plus rounding, so 8pt covers any plausible stroke without reaching the
next column. The companion tests use this machine's bordersrc geometry
(width 1.5, square: a 865x1087 border at 1,29 around an 859x1081 window at
4,32).

## Snapshot staleness

Staleness is accepted by construction ("a slightly stale moving image is not
perceptible", `capture-overlay-research.md`) and the worst case — the
destination's focus appearance — is patched mid-flight
(`refresh_destination_among`, at 0.5, the two ends of a focus change). What that
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

1. **`animate_layout` is the real strategy point.** The per-window half is
   now `crate::animation::domain::pass::plan`, a pure step over `PassWindow`s producing
   a `PassPlan` (moves, unmoved windows, warm targets), tested on its own. The
   frame-writing half is `app::reactor::present::Present`: two borrows — the window
   store and the transaction table — instead of `&mut Reactor`, which said nothing
   about what a layout pass touches. `Present` owns the rule that one application's
   batch shares ONE transaction id, because the app applies the frames together and a
   per-window id would have each report matched against a different write. That rule
   was inline and untested; it has five tests now.

   What is left in `animate_layout` is the flight decision (skip reasons, pan
   detection) and dispatch. It still takes `&mut Reactor`, and legitimately: it reads
   the config, the layout engine and the drag state, and sends to the animation actor.
   Those are the reactor's, not a narrower capability's.
2. **Reactor-side strip builders share boilerplate.** `start_strip_switch`,
   `start_strip_pan`, `start_edge_bounce` and `warm_all_workspaces` each
   repeat the screen-lookup / gaps / `calculate_layout_for_workspace` loop,
   and `warm_all_workspaces` recomputes every workspace's layout immediately
   after `start_strip_switch` computed the same layouts.

## Endgame (done)

The overlay learned resizes (anchored, cropped rather than scaled, the real
resize applied once behind the overlay), then switches, pans, entrances and
closes, and the AX engine's last job was the `overlay_animations = false`
fallback. That switch is gone: `animate` is on/off, on means the overlay,
off means placement. The AX resize trial ("Detour" in earlier revisions)
ended in the overlay's favour.

## Order of attack

1. ~~CA-driven canvas animation~~ — subsumed by 3 and 4.
2. ~~Steady ticker + matching curve for the AX engine~~ — done, then the
   engine itself was removed (see "One engine").
3. ~~CA-driven per-window overlay path~~ — done, see the overlay section.
4. ~~Dissolve the canvas into per-tile groups~~ — done, then reversed: the
   per-tile groups teleported on every merge, and containers carry the
   rigid pieces again (see the overlay section). The group event is
   `AnimateSurface`, the geometry module `strip_stack`.
5. Pan routing collapse (`take_strip_movement`, routing in `animate_layout`),
   once the strip visuals are validated; it also feeds the switch's
   scroll-offset claim and needs care. `strip_pan_delta` is already gone.
6. ~~Shared `motion` module + wire or delete `animation_easing`~~ — the AX
   engine and `animation_easing` are gone; `MOTION_CURVE` is the one curve.
7. Staleness: change-driven warming or a stream pool; measure the mid-flight
   refresh cap first since it is nearly free.
8. `animate_layout` decomposition: the pass analysis is out (`pass::plan`); the flight decision and dispatch remain, next time selection logic changes anyway.
9. ~~The resize question again~~ — resizes ride the overlay; AX removed.
