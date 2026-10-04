# Coordinating a transition: findings

When each step of a flight happens, and what was measured to decide it: how a layout pass and a
strip movement start a flight, what a pass arriving mid-flight changes, which pictures may reach a
moving tile, what capture work waits for the flight, when the real windows are written and when the
overlay lifts. The code is `src/transitions/`; the reactor routes each pass to it. How a flight moves
once it runs is `src/animation/docs/animation-smoothness.md`, and what its pictures cost and return
is `src/pictures/docs/capture-research.md`. Numbers quoted from code comments were measured when
those fixes landed; anything marked *estimated* has not been measured yet.

## One engine

Every animated movement runs through the overlay engine
(the actor `src/transitions/platform/engine.rs`, its decisions in `src/transitions/domain/`; the
renderer `src/animation/platform/overlay.rs`, the motion in `src/animation/domain/motion/`): window
bitmaps composited in one opaque overlay window, the real windows placed once
behind it (see "The apply point"). Layout passes, strip pans, moves along
the strip, workspace switches, resizes, entrances and the edge bounce are all
flights of it.
`AnimationManager` (`src/app/reactor/animation.rs`) is the layout side: it
gathers a `PassWindow` per window from the stores, sorts the pass with
`crate::transitions::domain::pass::plan` (moves, unmoved windows the overlay must still
draw, warm targets), decides whether the overlay flies (no `animations › off` in the
config, not low power, not a drag, something visibly travels) and places the real windows
directly when it does not.

The per-frame Accessibility engine that preceded it (`AXPosition`/`AXSize`
writes into every owning app on every tick, `animation_fps`, the
`BeginWindowAnimation`/`AnimationFrames`/`EndWindowAnimation` app requests,
the top-clamped vertical slide) was removed once resizes rode the overlay:
its cross-app skew (100-150px between neighbouring columns mid-scroll) was
the mechanism's, not a bug in it, and the overlay never had it.

Before that the overlay learned resizes (anchored, cropped rather than scaled, the real
resize applied once behind the overlay), then switches, pans, entrances and
closes, and the AX engine's last job was the `overlay_animations = false`
fallback. That switch is gone: animation is on or off (`animations › off`), on means
the overlay, off means placement. The AX resize trial ("Detour" in earlier revisions)
ended in the overlay's favour.

## Layout changes and strip movements

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
- **Strip movements** (`Event::AnimateSurface` — workspace switches, strip
  pans and moves along the strip; the wire event the reactor builds from the
  stacked-workspace geometry in `src/transitions/domain/strip_stack.rs`, carrying
  `SurfaceWindow`s) start `Immediate`: they arrive once per keystroke and
  latency is the enemy. `surface_plan` puts every window on the surface in
  ONE group travelling by the viewport's travel (`surface_travel`,
  `pan_travel`): one container, one position animation. The exception is a
  move's swapped columns, which start from their old slots
  (`SurfaceWindow::from`) and group by their own vectors, but for the column
  a stepping move passes, which is held at its old slot on the strip; see
  "The move flight, staged". Pinned (floating) windows stand in the floating
  container with `from == to`; a switch moves the floating container itself
  by the same travel (`floating_travel`). Visual destinations are
  deliberately distinct from `final_frames`: a leaving window animates
  off-screen while its real frame goes to a park, and a held column is drawn
  going beneath the moved window while its real window parks.

## Routing

`animate_layout` sends a pass to the strip surface when the strip
moved on screen (`take_strip_movement`) or the pass carries a move along the
strip (`EventResponse::moved`, stashed by the reactor for the passes its
command asked for and taken by the one pass that lays the moved window out,
`take_move_in`), and no window in it resizes. How far the strip moved is the
change of where its first column starts, `strip_origin`: the anchor less the
scroll offset, recorded by `calculate_layout` against the tiling area. The
scroll offset alone missed a change of anchor. Under niri navigation with
centre alignment a move clears a column's centring, so the anchor went from the
centre to the left edge with the offset unchanged: every window moved 576pt on
a 1728pt display, the pass reported no movement, and the move was drawn as the
finished layout at frame zero; pinned by
`a_move_that_drops_the_centring_starts_every_window_where_it_was`. Both are
taken at the top of every pass, before anything can return early: a pass that
placed nothing used to leave the movement unconsumed for whichever pass came
next. A strip movement places only the pass's windows whose frames changed,
as the per-window path does; a move that scrolled nothing re-placed every
window on the workspace at frame zero. Still open: both routes land in the same
machinery, and `take_strip_movement` also feeds the switch's claim on the
destination's origin, which needs care. The window-voting classifier
(`strip_pan_delta`) is gone: it only ran when the space had no strip, which is
when there is nothing to pan.

A pass containing a resize never becomes a strip pan or a move, even when
the strip moved: the strip surface draws final sizes, which would snap the
resize. The strip movement and the move are still taken, so the origin
bookkeeping stays current and the move reaches no later pass.

## Mid-flight passes

A pass arriving while a flight runs merges into it: `merge_plans` retargets the flight's containers,
and how its pieces vote and move is "Mid-flight passes" in
`src/animation/docs/animation-smoothness.md`. What the flight does about the merge, and about a
picture landing while it runs, is here.

A pass confirming the flight's destinations is an empty `PlanDelta`: nothing is touched, and rapid
presses neither restart nor extend the flight. Any real change restarts the orchestration clock so
the frame placement and teardown cover the newest legs. Reserved entrances still waiting for a
picture take the pass's slot (`retarget_entrances`).

A pass that changes any final frame after the apply point re-sends the frames
(`mark_stale_on_untiled_change`). Parked windows have no tile, so a tile-only check missed their
moves and left them at the old park.

A picture landing mid-flight goes to the cache first. It reaches a moving
tile only if the cache took it and the tile is waiting for it
(`should_swap_mid_flight`): an entrance's first picture (claimed or
admitted), a grow's settled reveal, or the focus refresh. A picture the
cache refuses never reaches a tile, so a flight never draws the grey capture
of a window whose cached picture carries its blur. A reveal cuts only before
0.6 progress (`REVEAL_APPLY_BEFORE`), a refresh before 0.9
(`REFRESH_APPLY_BEFORE`); later landings are cached for the next flight.
Background captures never reach a tile. They landed mid-flight in 310 of 321
flights (median 4 per flight), and every cut read as a flicker or a change
of transparency. On a resizing tile the cut also re-keyed the resize from
the presented state, which staggered that tile against its neighbours.

The focus refresh recaptures only on a focus change, only its two ends
(`refresh_targets`, against the previous flight's `last_focus`): the window
being switched into and the one being left, so both land in their focus
rendering. Focus changes a window's rendering without changing its size
(measured on a 1pt window border: 65 of 255 focused against 42 unfocused).
The reactor asks the pictures service for both ends, at focus priority,
when focus moves outside rini, by a click or an app activating itself
(`refresh_focus_pictures`). A move rini makes returns before it,
since the layout already holds that focus, so for those the flight's own
refresh is the only recapture. A flight that moves focus nowhere recaptures
nothing. It used to recapture the two frontmost tiles by depth on every
flight; a translucent window's two captures differ by the wallpaper behind
it, so that cut the two front Ghostty tiles at 0.55 on every strip pan (log
2026-09-16 2:05).

It takes two passes, at 0.25 and 0.55 of the flight (`REFRESH_PASSES_AT`,
`FocusRefresh`); the second is for an app that repaints its focus late. A
single pass at 0.5 through the ScreenCaptureKit service never once landed in
time: asked at 0.53, its pictures landed at 0.82 to 1.16 (debug log
2026-09-28, two flights, 154ms and 222ms after the ask), past the 0.6 cutoff
it had then. The window being left kept its focused rendering to the lift
and changed at rest, which is the flicker reported that day. A pass
captures a window wholly on a display framed, on a plain thread
(`capture_framed`), with its blur and its hairline. A window clipped by the
display edge goes to the pictures service instead, at focus priority, which
starts through the flight's hold, because a framed capture returns only the
visible sliver ("A clipped destination needs ScreenCaptureKit, not
SkyLight" in `src/pictures/docs/capture-research.md`).
A strip pan places the real windows at frame zero, so by the first pass both
ends are usually where they land. Usually: the placing is still in progress,
so a window read as on screen can be past the edge by the time it is
captured. That pass then gets nothing for it (`framed_coverage`), rather than
a full-size picture with the off-screen part transparent. One case still changes at rest: a clipped
window whose picture carries its blur, where the service's grey capture is
refused by the cache and so never cut in.

No likeness test and no same-route test guard the cut any more. A focus
change is a traffic light and a dimmed title bar, below what a 32x32
thumbprint resolves, and a cut to an identical picture changes nothing on
screen. Freshness guards it instead (`FocusRefresh::is_fresh`): a picture
swaps only if its capture began at or after the refresh asked, and after the
picture the refresh last cut in. The service skips a request for a window it
is already capturing, and a capture begun before the flight shows the old
focus. Both carry an older `taken`, which the service stamps when a capture
starts, not when it lands. The same-route test answered a race: the service
and a framed capture both answered every refresh, the two routes' pictures
failed the thumbprint match against each other, and every refresh target was
swapped two or three times per flight (log 22:34:04: swaps at 0.539, 0.549,
0.561 in one pan). Each pass now asks one route per window, so a window
takes at most one refresh cut per pass. The reveal never had either test; a
chase's framed reveal is the truth for a grow whatever the cache holds.
Every cut logs "picture swapped mid-flight" with its reason; that line is
the acceptance counter, at most two `reason=refresh` per window per flight.

The engine's thread stalled mid-flight, so a press landed after the strip had stopped. Reading
every window's frame in one query when a surface flight starts and at handover took the stalls over
30ms from 14 (909ms in all, longest 178ms) to 9 (424ms, longest 99ms), on the burst traced
2026-09-29 for "One flight however many presses" in `src/animation/docs/animation-smoothness.md`.
Most of what is left is the two full window lists and the desktop and bar captures a new flight
takes.

## Capture work in flight

From frame zero to the quiet after the lift the
pictures service holds background capture off (`crate::pictures::domain::schedule`).
The engine holds it as it composes a flight (`Hold::Transition`), lets the
bar be pictured again at the lift (`Hold::Settling`), and releases it at
`Event::Quiet` (`Hold::Released`). While it holds, only what the flight waits
for starts: its frame-zero pictures and a clipped end of the focus refresh.
The chases and the framed refresh are the engine's own and never ask. The
rest waits and starts at the release, most urgent first. The reactor's
`warm_all_workspaces` (about 15 captures per switch, queued 0.3ms after the
start) waits; so does the desktop render; hairline harvests for landed
pictures go to `finish_harvest_set`, harvested at the quiet, once per window per flight. A warm
also waits `SETTLE_BEFORE_LISTING` (50ms) before it is listed, for the hold of
the flight its pass started; a flight that composites its desktop on the main
thread holds later, and what was listed meanwhile is listed again at the
release ("The schedule" in `src/pictures/docs/README.md`). Under the old load the refresh took 203ms median (p90
441, n=535) against 16-24ms at idle, and 298 of 535 landed inside the flight.
271 of 321 desktop renders landed mid-flight. `warm_all_workspaces` stays in
the reactor's switch handler: the service knows a flight is holding it, the
reactor does not. Separately, a layout pass asks for a picture of each window the
first time it lays that window out (`pictured_windows`), because a pass warms
only the windows it moves, and after a restart nothing moves: the saved layout
already has every window in place. Measured 2026-09-29 over four restarts: no
picture landed in the first 11s, none at all in 40s with nothing pressed, and
49% of the flights in the first 30s were missing a window. Asking once per
display on its first pass was tried first and missed most of them: apps are
found one by one, and that pass knew one window of 21, so the first switch
still flew 8 of 16 windows with no picture. After the lift the owed captures wait for a quiet period
(`SETTLE_BEFORE_CAPTURES`, 400ms; `Event::Quiet`, `after_flight_captures`).
They used to run the instant the overlay lifted and took 600-800ms (8-15
window captures and the desktop render), so a press inside that window flew
the next flight against a busy compositor: frame-difference profiles of a
screen recording showed 50-130ms freezes followed by catch-up jumps. A flight
beginning inside the quiet period cancels the timer and holds the service
again; the work carries over to its own quiet. The same quiet period is what
`set_on_flight` reports as settled (`FlightReport`): `true` when an overlay
goes up, `false` at `Event::Quiet`, so a burst of flights is one `true` and
one `false`. rini's own bar sits above the overlay and is never pictured, so
with it on a flight looks for no bar at all. Another app's bar, under the
overlay with rini's off, is pictured by the service `BAR_REFRESH_DELAY`
(250ms) after the overlay lifts, never while it is up: a bar composite
measures 31ms median, and the delay lets the compositor drop the hidden
overlay from the framebuffer and folds a burst of switches into one capture,
at the end. A flight on a display before its first desktop render lands
composites the desktop on the main thread: the render starts only once the
service is released, at the quiet after a flight, so a burst of flights on a
newly entered display composites on every one, and a render that comes back
empty or the wrong size is asked again at the next quiet. The bar is
composited once per display and kept.

## Real windows land before lift

Frames go out on-screen destinations
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
sat on the right half of the display under the strip).
Tiles sit on whole points so the lift does not pop by a fraction ("Whole points" in
`src/animation/docs/animation-smoothness.md`).

## Edge bounce

A command that pushes past an end of the strip (the first/last
column) or of the workspace stack (next/prev at the bottom/top, which never wraps,
or nothing further to skip to) used to stop dead, which read
as a dropped keypress. The layout reports it as `EventResponse::edge_hit`,
distinct from `boundary_hit`, which is the gesture's threshold crossing. Four
places report it: `move_focus_internal`'s fallback and `MoveNode`'s, both
left/right only, and `handle_virtual_workspace_command` and
`move_window_to_workspace`, both up/down. Moving reports it because moving
without a cue was indistinguishable from a stuck key — the same reason
navigating does. A `MoveWindowToWorkspace` naming a workspace outright reports
nothing: not finding workspace 7 is a request that cannot be honoured, not a
push against an end, so `workspace_stack_direction` answers only for
`next`/`prev`. The reactor (`start_edge_bounce`) sends `Event::Bounce` with the
active workspace's surface and `edge_bounce_overshoot`
(`src/animation/domain/motion/plan.rs`): `EDGE_BOUNCE_OVERSHOOT` (72pt) the way the
content would have gone, so going right pulls the strip left and the next
workspace pulls the row up. It was 36pt and was reported as too small to
notice — 2% of a 1720pt viewport; 72 is 4%, still far short of a column. The actor (`start_bounce`) composes a flight with
no travel when none is running (every tile at rest, `final_frames` the layout
the windows already sit at) and adds the bounce to whatever is running
otherwise, extending the clock to cover the return (`clock_for_bounce`). Every push shows: the
actor keeps the bounce playing and when it began, and a push while it plays starts the next from
where that one is drawn (`next_bounce`, pinned by
`every_push_at_an_end_starts_a_bounce_from_where_the_last_is_drawn`), the clock extended to the new
one's return. The record goes at the lift and when frame zero is installed again, which removes the
animations it describes. How the bounce is drawn, and how a push re-kicks it, is "Edge bounce" in
`src/animation/docs/animation-smoothness.md`.

## The move flight, staged

Reported 2026-09-30 (`specs/animation.md`, "Moving a window along the strip"):
only the 50/50 pair at the start of a strip showed a move. Every move that
scrolled the strip took the pan route, which draws the FINAL layout and pans
the viewport, so the two windows had changed places at frame zero and what
played was a navigation pan. The pair at the start of the strip is the one
move that does not scroll. For a full-width column even a per-window flight
shows nothing: the camera follows the moved window, and the column it passes
is parked off screen at both ends, so `worth_animating` culls it.

**A move is a strip movement with a swap.** `MoveNode` that swaps two columns
reports `EventResponse::moved` (not a pull out of a stack, which resizes, not a
vertical move, not a move onto another display). The reactor routes it to
`start_strip_pan` with the strip movement, zero when the strip did not move on
screen, and the surface is built exactly as a pan's, every window at its NEW
frame. The moved column and the column it passed also get a start (`swap_starts` in
`src/transitions/domain/strip_move.rs`): the two fill the span they fill
now, in the order they had before. For W of width a and N of width b with gap
g, moving right, W started at N's new x and N at N's new x + a + g; moving
left, W at W's new x + b + g and N at W's new x. Every window of a stacked N
shifts by one amount. `stage_move` puts the pair on the surface: W crosses from
its start, and N crosses from its own unless the move steps, when it is held
(below). `surface_plan` groups by travel vector, so the strip is one container
and each crossing column has its own; the moved window holds focus, so its
container is drawn first (`band_plan`) and N slides beneath it.

**Which moves step.** Only a moved column that fills the viewport steps
(`fills_viewport`): its width is the tiling area's, the display's usable width
less both outer gaps, within a point, whether the full-width toggle or a width
preset made it so. The view follows such a column, so it does not move on
screen and nothing else shows the move. A half or two-thirds column is seen
moving and never steps. The rule was the moved window's own travel on screen
under a quarter of the viewport (`NUDGE_BELOW`), which stepped every narrower
move the view followed as well; reported 2026-09-30.

**Which way.** A third of the viewport (`NUDGE_SHARE`) away from where the
window went, and back (`nudge`): a move right steps the strip left and opens
the right side, where N was. It went toward where the window went, opening the
side N was heading for.

**The column passed is held.** For a move that steps, `stage_move` gives N its
old slot as its surface frame and no start of its own, so it rides the strip's
container: its tile starts where N was on screen and moves only as the strip
does, W slides over it, and it finishes beneath W, covered. Only the drawing is
held. N's real window goes to its layout frame, off screen, as for any move:
`final_frames` are unchanged, the handover compares real windows with them, and
the stand-in chase judges where the real window lands (`stand_in_chase`). A
move that does not step keeps both columns crossing.

The step starts only on a flight that moves: one still collecting passes
recomposes its containers, which drops any additive animation, so a move
arriving then waits and steps when the flight starts moving (`nudge_admission`,
`NudgeAdmission::Wait`, `starting_nudge`); it used to be dropped, and a
full-width move merged into a collecting layout pass showed nothing. The engine
stretches a flight that steps to `NUDGE_STRETCH` (1.3x) the plain duration,
when the step starts, so a flight that does not step keeps its length; the
reactor asked for the stretch with every nudge it sent, stepped or not. The
clock and lift wait for the step to come home (`clock_for_bounce`, `settled`).

**Pictures.** The column a full-width move passes is parked at both ends, so a
cold cache had no picture of it, and the strip surface drew nothing for a
window with no picture: a hole in the side the step opens. The surface path now
takes the stand-in the per-window path does (`picture_or_stand_in` in
`src/pictures/domain/stand_in.rs`, shared by both): a known window parked off
this display with no picture flies as the dark `--n3` tile, uncached. A window
on this display with no picture is still placed and not drawn. A strip
movement starts at once and never holds, so the real picture of a stand-in
whose window lands on this display, the column a move at the edge brings in
from its park, is chased once its frame is applied (`stand_in_chase`,
`chases_stand_in`), and the reveal swap puts it on the same tile mid-flight.
Where it lands is its real frame: a held column is drawn going beneath the
moved window while its real window parks, so it is not chased. It used to fly
dark for the whole flight: nothing asked for its picture. One parked at both
ends has nothing on screen to capture and keeps the stand-in to lift.

Real windows go to the layout's frames at frame zero, as for any strip
movement (`APPLY_FRAMES_AT_PAN`), and only the ones whose frames changed; the
step and the held column are drawn only.

How the strip steps and on which curve, what the side the step opens shows, and how a burst merges
is "The move flight" in `src/animation/docs/animation-smoothness.md`.

## A window that opens

**A window that opens travels from its spawn frame.** macOS shows a new
window at its own frame about 40ms before rini's first pass (AX
`WindowCreated` latency plus the coalesce window); that time is accepted.
`start` reads that frame from the window server and takes ONE framed
capture there (`capture_via_framed_with_dressing`, 16-24ms), at most
`MAX_SYNC_ENTRANCE_CAPTURES` (4) per pass. The tile is a loose
`Member::Entrance` from the spawn frame to the slot, stretched
(`stand_in_mode`) until the reveal chase lands the slot-size picture,
which `claim` takes at frame zero (`Claimed::Refreshed`) or the
`Swap("reveal")` cut takes before 0.6. No hold: the newcomer's slot alone
goes out at frame zero (`frame_zero_work`) so the chase has something to
capture, and again with every frame at the apply point, one redundant AX
write per open. A frame off this display is not a spawn: a parked window
with a cold cache took a 1s off-screen capture and flew in from the park.
It used to take the reservation instead, which drew nothing in its slot
until a picture landed — a hole in the strip after every restart. It now
flies with a stand-in (`stands_in`, `picture::stand_in_picture`): a
dark `--n3` tile, drawn nine-slice from a 42px image so its corners keep the
window's radius, moving exactly as its picture would. The stand-in claims
to cover nothing, so the flight holds for it and chases it like a grow,
`claim` swaps the real picture onto the same tile at frame zero, and the
reveal swap takes one mid-flight. It is never cached. Strip movements draw
the same stand-in (`picture_or_stand_in`; see "The move flight, staged"), without
the hold, since they start `Immediate`: the picture of one landing on this
display is chased all the same. A window already at its slot with no
picture is captured as a still tile and not chased. Growing from zero
width (`entrance_from`; a centred zero-size zoom was tried first and read
as the window inflating, which nothing else on the strip does) was the
previous entrance and read as a pop then a
vanish; it survives only in the reservation fallback below.

**The reservation fallback.** With no server frame, a zero frame, an
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
load, most of a flight, the load being ScreenCaptureKit captures it waits
behind (27ms alone, 204ms behind four; "SkyLight in parallel" in
`src/pictures/docs/capture-research.md`); the chase's framed capture takes 16-24ms once the
app has painted. `claim` requires the fit; a smaller picture is refused
and the hold goes on. `claim_reveal` adds the zero-width tile to the
frame-zero composition (`install`). The flight starts when the last
awaited picture lands or `HOLD_CAP` passes, so every tile flies
in one `fly` transaction. A picture landing after lift-off is admitted
(`admit_entrance`, `add_tile` in `GroupKey::Loose`) with
`late_join_duration`: what is left of the flight. A picture that misses
the flight altogether shows when the overlay lifts. A coalescing merge
that moves a window whose frame was already applied re-requests the
merged frames (`reapply_set`); without that the window sat where the
first pass put it until the strip scrolled.

## A closed window disappears

Its survivors' pass is an ordinary
reflow: one group sliding by the closed width. Nothing is drawn for the
window itself. A shrinking ghost was tried (a resize to zero width from
the cached picture, `PendingExit`, `AnimateExit`) and removed: a ghost is
a per-tile discontinuity in a flight that is otherwise rigid, and a
picture of a window that no longer exists. Under load it ran 25ms ahead
of the survivors' clock; on the window-server close path it composed
`exits=0` and left a gap (log 2026-09-16 2:05). What stays is the
pictures service's forget, sent once per close on either path: the AX
`WindowDestroyed` while the window still exists, or the window-server
disappearance through `EventOutcome::forgotten_windows` ahead of the
layout events. The gap between the window vanishing and the survivors
moving is AX latency plus the coalesce window, and is accepted.

## A grow holds, then reveals

Every fill for the not-yet-rendered region
of a grow was tried and rejected by eye: `contentsRect` past the picture's
edge extends its outermost pixels (a hole to the backdrop on a translucent
window), and stretching the lead reads as stretching, because it is. So the
truthful pixels are made to exist first. A pass whose destination outgrows
its picture (`outgrows`) applies the real frames IMMEDIATELY: the overlay
is already covering the windows, so the app rerenders at its new size
behind a still frame. A chase thread polls the real frame every 8ms
(`REVEAL_CHASE_INTERVAL`, 125 attempts, about a second in all), a cheap
window-server read; SkyLight-capture polling measured 170-300ms per attempt
behind queued ScreenCaptureKit captures and lost the race. Once the size is there it takes ONE framed
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
300ms (`HOLD_CAP`) whatever the flight's duration. A share of the flight
with a 300ms floor was computed and then capped at 300ms, so it was the cap
for every duration, and it went. The old
chase held 235ms median (min 173, max 299; 25ms poll, two framed captures,
then a separate harvest), and 22 of about 45 holds timed out at 300ms
anyway. A 150ms cap was tried for a blink-length freeze and raised back
to 300ms with the fit requirement above: an entrance chase now waits for
the real frame to reach slot size before it captures, so it needs the
runway a grow's does. *How often 300ms is enough is not measured yet.*
An app that misses the cap flies with the old picture STRETCHED over the
tile (`stand_in_mode`, `Stretch`): every mode fills the whole frame. A
top-left crop that stopped the grid at the picture's edge and showed the
backdrop in the growth was tried and read as a hole (3:28:10), the same
hole recorded above for `contentsRect` past the edge. If the reveal lands
mid-flight after all, it is hard-cut onto the tile before 0.6 progress
(`Swap("reveal")`) and `set_tile_picture` re-installs the resize leg on
the SAME `Timing` (begin on the media clock plus duration, kept per tile
in `leg`, which a plain move keeps too, so a crop-drawn tile on a move
re-keys the same way): the frame keeps its curve and only the pixels cut.
Re-keying from the presented frame over the remaining duration restarted
the ease-out and the tile lurched at every swap. Later than 0.6 the
picture is cached for the next flight.

## The apply point

Layout passes place the real windows at 0.75
(`APPLY_FRAMES_AT`), or at 0.5 (`APPLY_FRAMES_AT_RESIZE`) when any tile
resizes: the real resize behind the overlay costs three synchronous AX round
trips per window and needs more runway to land before the overlay lifts.
Strip movements place at frame zero (`APPLY_FRAMES_AT_PAN`; the
measurements are under "Real windows land before lift"). Their frames are pure moves,
but a switch sends about 17 of them, serialized per app actor and sharing
it with window rediscovery. At 0.75 the gap from "placing real windows" to
lift measured 90ms median, p90 156, and 0.75 of a 300ms switch leaves 75ms;
0.5 was tried next and still lost 24 of 162 flights.

## Border companions

Companion tiles for a border tool's windows were the second attempt at the border that flickers at
the handover ("The hairline harvest" in `src/pictures/docs/capture-research.md`). They stay
(`companion_of` / `companion_tiles`): they are the right answer for anyone whose border tool IS
running, carrying real border windows as tiles:

- Detection is geometric and tool-agnostic: an unmanaged window concentric
  with a managed one (centers within 4pt) and the same size or up to 8pt
  larger is that window's border. Candidates exclude every window in the
  pass, so stacked twins cannot match each other; one border window traces
  one window (`claimed`).
- The companion rides at its real relative offset from the window's tile,
  drawn a quarter depth-step in front of it (under the next tile forward,
  clear of the half-step shadow casters), with no shadow of its own. It
  records the window it traces and takes that window's band: a floating
  window's border is floating, in the floating container with it and lifted
  with it (`border_tile`, `band_plan`). A move's step carries every strip
  window's border with the strip and never a floating window's ("The move
  flight" in `src/animation/docs/animation-smoothness.md`).
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
  against 170-300ms for SkyLight behind queued ScreenCaptureKit captures; the
  animation itself does not slow it, 22.7ms median during a slide) until racing it
  against the ScreenCaptureKit route was found to swap the tile 2-3 times
  per flight; each pass now asks one route per window, framed where it is
  wholly on a display and ScreenCaptureKit where it is clipped (see
  "Mid-flight passes"). The swap itself is a hard cut ON PURPOSE: a ~120ms
  crossfade veil was tried and rejected, because stacking two copies of a
  translucent window pulses its net opacity mid-fade — there is no
  constant-alpha crossfade with layers. Gratuitous cuts are avoided upstream
  instead: freshness guards the cut (`FocusRefresh::is_fresh`), and one route
  per window per pass is at most one refresh cut per pass.
- Companions are excluded from the mid-flight destination recapture, which
  exists for the window the eye is on. They wear no harvested hairline
  either: a border window's ring is transparent almost everywhere, so the
  harvest's alpha check rejects it without a special case.

No configuration: the companions reproduce whatever a border tool draws, or nothing, and the
outline is the platform's.

**Companion geometry.** A border window is recognised by geometry alone:
same centre within `COMPANION_CENTER_SLACK` (4pt), and at most
`COMPANION_EXPANSION` (8pt) larger per axis. JankyBorders draws its stroke
on a sibling window about twice the stroke width larger than the traced
one, plus rounding, so 8pt covers any plausible stroke without reaching the
next column. The companion tests use this machine's bordersrc geometry
(width 1.5, square: a 865x1087 border at 1,29 around an 859x1081 window at
4,32).

## Structural findings

1. **`animate_layout` is the real strategy point.** The per-window half is
   now `crate::transitions::domain::pass::plan`, a pure step over `PassWindow`s producing
   a `PassPlan` (moves, unmoved windows, warm targets), tested on its own. The
   frame-writing half is `app::reactor::present::Present`: two borrows — the window
   store and the transaction table — instead of `&mut Reactor`, which said nothing
   about what a layout pass touches. `Present` owns the rule that one application's
   batch shares ONE transaction id, because the app applies the frames together and a
   per-window id would have each report matched against a different write. That rule
   was inline and untested; it has five tests now.

   What is left in `animate_layout` is the flight decision (skip reasons, pan
   detection) and dispatch. It still takes `&mut Reactor`, and legitimately: it reads
   the config, the layout engine and the drag state, and sends to the flight engine.
   Those are the reactor's, not a narrower capability's.
2. **Reactor-side strip builders share boilerplate.** `start_strip_switch`,
   `start_strip_pan`, `start_edge_bounce` and `warm_all_workspaces` each
   repeat the screen-lookup / gaps / `calculate_layout_for_workspace` loop,
   and `warm_all_workspaces` recomputes every workspace's layout immediately
   after `start_strip_switch` computed the same layouts.
