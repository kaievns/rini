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
behind it (see "Real windows land before lift"). Layout passes, strip pans, moves along
the strip, workspace switches, resizes, entrances and the edge bounce are all
flights of it. When each step of one happens is the coordinator's ("The lifecycle").
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

## The lifecycle

The engine holds the coordinator (`Coordinator` in `src/transitions/domain/lifecycle.rs`) and decides
nothing about when: it stages each request, merges each pass and does what the coordinator says. A
request that finds no transition running opens one from what it alone says, before anything is
composed (`Event::Begin`): the coordinator holds the pictures service and asks, through SkyLight and
under the transition's generation, for the strip windows wholly on screen where they start and the
floating windows seen behind them. Only then is the pass composed from the cache, the overlay
installed hidden, and the coordinator told what was composed (`Event::Merge`). The overlay goes up
once those pictures arrive, each put on its tile, or 80ms pass (`FRAME_ZERO_PATIENCE`); the real frames
are written then and never before, for every kind; a 1pt hole is cut over every window that ends wholly
on screen, so the window server counts it visible and its app keeps painting; and the end state is
watched. A layout pass then waits for the reactor's follow-up passes (25ms) and for the windows it grows
or opens to repaint (up to `HOLD_CAP`); a strip movement moves at once.

Frame zero's pictures that come after the overlay went up are swapped onto their tiles where they are
(`Why::FrameZero`), and frame zero is never installed again, which under a flight that moves would jump
the strip back. They were taken at the press and come before any picture of the end, so each replaces
only the cache's. Dropped, they would be lost for the flight: the service has started the client's
prints from them, so a landing that looks the same is not delivered.

The end state is four watches, kept and retargeted from one generation to the next (`EndWatches` in the
engine): every end window, captured as soon as it is where it ends (`Settle::AsSoonAsThere`, watched for
150ms); a window a layout pass grew or opened, or one it flies as a stand-in, captured once it has
repainted there (`Settle::Repainted`), while a stand-in on a strip movement, which never holds, is an
end window like any other; the focus target, from its focus report until its look changes (`Change`);
and the floating windows behind the strip, kept fresh once that change is in (`Live`). One more capture
of every end window is asked 110ms before the clock ends and never in its first 40ms (`AskFinal`). A
press merged in flight starts the next generation and retargets every watch and the holes. A delivered
picture is swapped onto its tile only if it was taken for the end the tile has now, under a generation
no older than the one since which the tile has ended there (`end_since`), and its window was within
20pt of that end in x, y, width and height when it was taken (`END_SLACK`); the tile then snaps onto
the window's real frame when that is 0.25-20pt off, once per window per generation, once the motion has
started. A picture the service decided under the generation before is still drawn when the merge kept
its window's end: the service runs on its own thread, so a delivery is often on its way while a pass is
composed, and it is never delivered again, since a retarget keeps a window landed where it still ends
and a change already seen. A window the merge sends somewhere new starts its print afresh in the
service, so its landing there is delivered whatever it looks like.

The lift waits for the clock, the tiles presented where they end and the real windows where they were
written, or for `LIFT_GRACE` past the clock ("Real windows land before lift"). What follows it is
"Capture work in flight".

## Layout changes and strip movements

The two entry points feed the same `begin_group`:

- **Layout changes** (`Event::Animate` — window open/close, column reorder,
  join/unjoin, anything whose windows move by different vectors) start
  `Coalesced`: the overlay shows at frame zero and the movement begins once
  the reactor's layout passes settle (25ms, `COALESCE_WINDOW`).
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
presses neither restart nor extend the flight. Any real change restarts the coordinator's clock
(`Merge { moved }`), so the frame placement and teardown cover the newest legs. A pass arriving before
the plan plays, behind the frame-zero wait or the coalesce window, is folded into frame zero instead:
the plan is rebuilt from the merged tiles and installed again (`recompose`). Behind the frame-zero
wait, frame zero is asked again for every window the pass brought wholly on screen where its tile
starts that no ask expected there, under the same generation, and the overlay waits for that ask too,
never past the 80ms the begin set. The request alone is what the first ask knew: a window rejoining
the pass, or one a layout pass starts from the window server's frame rather than the request's, would
otherwise fly the cache's picture to its landing. Reserved entrances still waiting for a picture take
the pass's slot (`retarget_entrances`).

A tile the pass retargets keeps the picture it has and how focus is drawn on it: a pass is composed
from the cache, and copied over it erased frame zero's picture, a picture the flight took or a parked
target's focused look, before the overlay was up as much as after. A pass moving focus on redraws which
tiles are drawn focused, the window focused as the flight began still among them until focus hands
over, and a new target parked off the display flies its focused look, as the first pass's does.

A pass merged once the overlay is up writes every final frame it changed, a parked window's included.
Parked windows have no tile, so a tile-only check missed their moves and left them at the old park.
Before the overlay is up nothing is written: the show writes the merged frames.

A picture reaches a tile only as a delivery under the flight's tag: frame zero's, which the tiles take
before the overlay goes up or the coordinator swaps in where they are when they come after it, and the
watches' and the final capture's, which the coordinator swaps in ("The lifecycle"). Every other capture, a ScreenCaptureKit warm or background picture, goes to the
cache only, for the next flight. Background captures landed mid-flight in 310 of 321 flights (median 4
per flight), and every cut of one read as a flicker or a change of transparency. On a resizing tile the
cut also re-keyed the resize from the presented state, which staggered that tile against its
neighbours. Every cut logs "picture swapped mid-flight".

**Focus changing during a flight.** Focus changes a window's rendering without changing its size
(measured on a 1pt window border: 65 of 255 focused against 42 unfocused). The reactor passes every
focus report the window server sends, rini's own raise's included, to the pictures service as a hint,
and every request names the window the layout had focused as the event asking for it began
(`focused_before`): the one focus leaves. A click, a cmd-` or an app activation is reported before the
pass it causes, so the window last reported focused is the target by then; taken as the one focus
leaves, it had focus move nowhere, and nothing watched the target's look. Frame zero pictures that
window focused, and its tile is drawn focused; the focus target's tile is drawn unfocused, unless
it flies its focused look (below). The target's `Change` watch, armed by its report (one remembered for
a second arms a watch started after it), captures it where it ends every 15ms, six times at most, until
its band print differs from what the engine was delivered of it as the watch started, frame zero's for
a window on screen at the press. It is judged from that, not from the latest delivery: the landing
watch is looked at first in a round, and a window focused before it landed delivered its focused look
as its landing, which the change watch then never saw change. That picture is swapped in, kept as its
focused look, and focus hands over (`HandOverFocus`): the target's ring and shadow are drawn focused
and the window left's not (`TileOverlay::set_tile_focused`). The window left is pictured again by the
final capture if it ends wholly on screen and its look has changed by then; one ending clipped by the
display's edge keeps its frame-zero picture to the lift, only its ring and shadow turning, and its look
turns at rest: SkyLight returns only its visible part, and the one ScreenCaptureKit capture a flight
lets through is the target's ("Decisions taken while Kai was away" in
`docs/pictures-motion-transitions.md`). The target is pictured again by the final capture too, if its
change watch ran out first, and that hands focus over. If nothing has handed it over when the final capture is asked, focus
hands over then, while the flight still moves: the window is reported focused, and a look that never
showed a change (one already focused when the watch started, or an app whose title band looks the
same either way) would otherwise turn at the lift. A target a layout pass opened, grew or flies as a
stand-in hands over with its repainted picture; a stand-in on a strip movement is pictured as it lands
and hands over on its focus change or the final capture, as any target does. A
target clipped by the display edge where it ends takes one ScreenCaptureKit capture through the hold, the
only one a transition lets through: SkyLight returns only its visible sliver ("A clipped destination
needs ScreenCaptureKit, not SkyLight" in `src/pictures/docs/capture-research.md`). A flight that moves
focus nowhere watches no change.

The recapture this replaced cut both ends of a focus change mid-flight, at 0.25 and 0.55 of the
flight, through ScreenCaptureKit and a framed capture. Asked at 0.53 through the service, its pictures
landed at 0.82 to 1.16 of the flight (debug log 2026-09-28, two flights, 154ms and 222ms after the
ask), and with four ScreenCaptureKit captures out the frame-zero capture took 92-97ms against 26-33.
The watches capture through SkyLight, one call at a time, as the windows land.

**A parked focus target flies its focused look.** A focus target that starts off the display, where
frame zero cannot picture it, flies the last picture the pictures service kept of it focused
(`Pictures::focused`), drawn focused, when that picture fits where its tile starts; the service drops a
look once a newer capture's body differs. So does a target a merged pass moves focus to. Its landing
does not replace the look: it is often taken
before the window repaints focused (the prototype's medians, landed at 76.9ms and the focused picture
at 104.9ms after the press, "Where a moving focus change spends its time" in
`src/animation/docs/capture-overlay-research.md`), and cut in it would turn the window unfocused and
back. The coordinator holds the latest landing back (`StageSummary::focused_look`) and swaps it in
only if focus hands over at the final capture's ask with nothing newer; a focus change or the final
capture replaces the look as it hands over. Kai, 2026-10-03: "fcache doesn't bother me … windows
arrive in focus every single time".

The engine's thread stalled mid-flight, so a press landed after the strip had stopped. Reading
every window's frame in one query when a surface flight starts and at handover took the stalls over
30ms from 14 (909ms in all, longest 178ms) to 9 (424ms, longest 99ms), on the burst traced
2026-09-29 for "One flight however many presses" in `src/animation/docs/animation-smoothness.md`.
Most of what is left is the two full window lists and the desktop and bar captures a new flight
takes.

## Capture work in flight

From the begin to the quiet after the lift the
pictures service holds background capture off (`crate::pictures::domain::schedule`).
The coordinator holds it at the begin, before anything is composed (`Hold::Transition`), lets the
bar be pictured again at the lift (`Hold::Settling`), and releases it at
the quiet (`Hold::Released`), or at once for a transition it abandons. While it holds, only the
transition's SkyLight work runs, the frame-zero and final asks and the watches, and one
ScreenCaptureKit capture of a clipped focus end. A pass's ask for the windows it has no picture of
is a warm, for the next flight. The
rest waits and starts at the release, most urgent first. The reactor's
`warm_all_workspaces` (about 15 captures per switch, queued 0.3ms after the
start) waits; so does the desktop render. A warm
also settles `SETTLE_BEFORE_LISTING` (50ms) before it is listed: the reactor
sends a pass's warm straight to the service just after the request for its
flight, so it usually arrives before the hold the engine sends as it takes that
request, and one listed before a later hold is listed again at the release.
Under the old load a focus recapture through the
service took 203ms median (p90 441, n=535) against 16-24ms at idle, and 298 of 535 landed inside the flight.
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
(`SETTLE_BEFORE_CAPTURES`, 400ms; the coordinator's `Event::Quiet`), which warms every window the
flight drew, across its merged passes, at the frame it last ended at.
They used to run the instant the overlay lifted and took 600-800ms (8-15
window captures and the desktop render), so a press inside that window flew
the next flight against a busy compositor: frame-difference profiles of a
screen recording showed 50-130ms freezes followed by catch-up jumps. A flight
beginning inside the quiet period holds the service
again; the work carries over to its own quiet. The same quiet period is what
`set_on_flight` reports as settled (`ReportUp`, `ReportSettled`): `true` when a transition
begins, `false` at its quiet, so a burst of flights is one `true` and
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
flights and masked every smaller error. The lift logs "overlay lifted", so
the placing-to-lift gap can be read from the log. The lift itself waits for
the render server and the real windows (the coordinator's tick, `TileOverlay::settled`,
`handover_report`): the clock has
to run out AND every container and picture has to be presented at its model
position within half a point AND every window has to be within 2pt of where it was written, or
`LIFT_GRACE` (350ms) past the clock. The
render server runs a frame or so behind the coordinator's clock, and lifting on
the clock alone showed the real windows one frame ahead of their tiles: a
small jerk at the end of every flight. Every kind writes its frames as the overlay goes up. Written
later in the flight they missed the lift: layout passes wrote at 0.75 of it (0.5 when a tile
resized, the real resize costing three synchronous round trips per window), and the gap from
"placing real windows" to lift measured 90ms median, p90 156, where 0.75 of a 300ms switch leaves 75ms;
at 0.5, 24 of 162 strip movements lifted with every window 1700-2600pt from its tile, because 17 writes
across Electron apps, serialized per app actor, took longer than half a flight. A park-to-park write is not sent at all
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
notice — 2% of a 1720pt viewport; 72 is 4%, still far short of a column. The actor (`start_bounce`) composes a transition of
the bounce kind when none is running: every tile at rest, nothing written and nothing watched, the
bounce played as the motion starts, once frame zero is up. On a flight that moves it adds the bounce at
once, and on one that does not move yet as it starts; either way the coordinator's clock runs on to cover
the return (`Event::Extend`). Every push that reaches a moving flight shows: the
actor keeps the bounce playing and when it began (`BounceRecord`), and a push while it plays starts
the next from where that one is drawn (pinned by
`every_push_at_an_end_starts_a_bounce_from_where_the_last_is_drawn`), the clock extended to the new
one's return. The record goes at the lift and whenever frame zero is installed again, which removes
the animations it describes, so the next push starts from rest
(`a_push_after_the_bounce_is_gone_starts_from_rest`); kept, it would push from where nothing is drawn
and the surface would jump. How the bounce is drawn, and how a push re-kicks it, is "Edge bounce" in
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
nothing of it lands on screen to be watched. A
move that does not step keeps both columns crossing.

The step starts only on a flight that moves: one still collecting passes
recomposes its containers, which drops any additive animation, so a move
arriving then waits and steps when the flight starts moving (`nudge_admission`,
`NudgeAdmission::Wait`, `starting_nudge`); it used to be dropped, and a
full-width move merged into a collecting layout pass showed nothing. The engine
stretches a flight that steps to `NUDGE_STRETCH` (1.3x) the plain duration,
when the step starts, so a flight that does not step keeps its length; the
reactor asked for the stretch with every nudge it sent, stepped or not. The
clock and lift wait for the step to come home (`Event::Extend`, `settled`).

**Pictures.** The column a full-width move passes is parked at both ends, so a
cold cache had no picture of it, and the strip surface drew nothing for a
window with no picture: a hole in the side the step opens. The surface path now
takes the stand-in the per-window path does (`picture_or_stand_in` in
`src/pictures/domain/stand_in.rs`, shared by both): a known window parked off
this display with no picture flies as the dark `--n3` tile, uncached. A window
on this display with no picture is still placed and not drawn. A strip
movement starts at once and never holds, so the real picture of a stand-in
whose window lands on this display, the column a move at the edge brings in
from its park, comes from the landing watch once the window is where it ends, and is
swapped onto the same tile mid-flight. A held column is drawn going beneath the
moved window while its real window parks, so nothing of it lands. It used to fly
dark for the whole flight: nothing asked for its picture. One parked at both
ends has nothing on screen to capture and keeps the stand-in to lift.

Real windows go to the layout's frames as the overlay goes up, as for any
transition, and only the ones whose frames changed; the
step and the held column are drawn only.

How the strip steps and on which curve, what the side the step opens shows, and how a burst merges
is "The move flight" in `src/animation/docs/animation-smoothness.md`.

## A window that opens

**A window that opens travels from its spawn frame.** macOS shows a new
window at its own frame about 40ms before rini's first pass (AX
`WindowCreated` latency plus the coalesce window); that time is accepted.
`start` reads that frame from the window server and takes ONE framed
capture there (`capture_via_framed`, 16-24ms), at most
`MAX_SYNC_ENTRANCE_CAPTURES` (4) per pass. The tile is a loose
`Member::Entrance` from the spawn frame to the slot, stretched
(`stand_in_mode`) until its picture at the slot is swapped in: the window is
watched until it has repainted where it ends (`StageSummary::repainting`,
`Settle::Repainted`) and never held for, and its slot is written with every
other frame as the overlay goes up. A frame off this display is not a spawn: a parked window
with a cold cache took a 1s off-screen capture and flew in from the park.
It used to take the reservation instead, which drew nothing in its slot
until a picture landed — a hole in the strip after every restart. It now
flies with a stand-in (`stands_in`, `picture::stand_in_picture`): a
dark `--n3` tile, drawn nine-slice from a 42px image so its corners keep the
window's radius, moving exactly as its picture would. The stand-in claims
to cover nothing, so the flight holds for it like a grow, and its picture once it
has repainted where it ends is swapped onto the same tile. It is never cached. Strip movements draw
the same stand-in (`picture_or_stand_in`; see "The move flight, staged"), without
the hold, since they start `Immediate`: the picture of one landing on this
display comes from the landing watch all the same. A window already at its slot with no
picture is captured as a still tile. Growing from zero
width (`entrance_from`; a centred zero-size zoom was tried first and read
as the window inflating, which nothing else on the strip does) was the
previous entrance and read as a pop then a
vanish; it survives only in the reservation fallback below.

**The reservation fallback.** With no server frame, a zero frame, an
unusable capture or no budget left (`entrance_plan`, reason logged as
"entrance reserved"), the window becomes a `PendingEntrance`, held for next
to any grow, which the coordinator sees as a tile from zero width to its slot.
Every real frame is written as the overlay goes up, the newcomer's slot
included, so the watch captures the window at slot size. Holding the slot back
until the picture landed was tried: the capture then caught the window at its
SPAWN size and, drawn top-left in the slot, left the growth as backdrop, a
hole in the strip (recording of 2026-09-15 3:28:10). Its first picture once it
has repainted there, taken within 20pt of the slot, admits it
(`RunningAnimation::admit`): before the motion starts the zero-width tile
joins the frame-zero composition (`install`), and the flight starts when the
last awaited picture lands or `HOLD_CAP` passes, so every tile flies in one
`fly` transaction; after, it joins the flight in `GroupKey::Loose`
(`add_tile`) for what is left of the clock. A picture that misses the flight
altogether shows when the overlay lifts. SkyLight is quick only with nothing
in front of it: 27ms for one call alone, 170-300ms under load and 204ms behind
four ScreenCaptureKit captures ("SkyLight in parallel" in
`src/pictures/docs/capture-research.md`), which is why nothing else captures
while a transition holds the service.

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
its picture (`outgrows`) is held for: its frames are written as the overlay
goes up, so the app rerenders at its new size behind a still frame, and the
pictures service watches it (`Settle::Repainted`). It reads the window's
server frame every 1ms until it is within 20pt of where it ends, a cheap
window-server read, then captures it every 8ms, 125 times at most, about a
second in all. The frame resizes instantly
while the app's pixels lag behind, and
a capture taken between the two is a half-painted surface: delivering one
flew the whole reveal with garbage. So a capture counts only when settled
(`repaint_settled`): it matches the previous print (`renderings_match` on
32x32 RGBA thumbprints, a sample differing when any channel moves by more
than 8, up to 3% of samples allowed so cursors and clocks do not stall it), or it differs from
the picture cached before the resize, which means the app has repainted.
The second test saves one poll interval on most grows; the first is all an
entrance has. When it is delivered, the tile takes it, frame zero is installed
again with it, and the flight begins: the moving edge reveals genuine final-size content,
1:1. A shrink crops the picture it has and flies immediately. The hold is
300ms (`HOLD_CAP`) whatever the flight's duration. A share of the flight
with a 300ms floor was computed and then capped at 300ms, so it was the cap
for every duration, and it went. The old
chase held 235ms median (min 173, max 299; 25ms poll, two framed captures,
then a separate harvest), and 22 of about 45 holds timed out at 300ms
anyway. A 150ms cap was tried for a blink-length freeze and raised back
to 300ms with the fit requirement above: an entrance's watch waits for
the real frame to reach slot size before it captures, so it needs the
runway a grow's does. *How often 300ms is enough is not measured yet.*
An app that misses the cap flies with the old picture STRETCHED over the
tile (`stand_in_mode`, `Stretch`): every mode fills the whole frame. A
top-left crop that stopped the grid at the picture's edge and showed the
backdrop in the growth was tried and read as a hole (3:28:10), the same
hole recorded above for `contentsRect` past the edge. If the repainted
picture lands once the motion has started after all, it is hard-cut onto the tile and
`set_tile_picture` re-installs the resize leg on
the SAME `Timing` (begin on the media clock plus duration, kept per tile
in `leg`, which a plain move keeps too, so a crop-drawn tile on a move
re-keys the same way): the frame keeps its curve and only the pixels cut.
Re-keying from the presented frame over the remaining duration restarted
the ease-out and the tile lurched at every swap.

## Border companions

Companion tiles for a border tool's windows were the second attempt at the border that flickers at
the handover ("The hairline" in `src/pictures/docs/capture-research.md`). They stay
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
- A swap is a hard cut ON PURPOSE: a ~120ms crossfade veil was tried and
  rejected, because stacking two copies of a translucent window pulses its net
  opacity mid-fade — there is no constant-alpha crossfade with layers.
  Gratuitous cuts are avoided upstream instead: a watch delivers only a picture
  whose band print is news.
- Companions are never asked for or watched: frame zero, the watches and the
  final capture picture the windows they trace. They wear no hairline either:
  the window a companion traces wears its own.

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
