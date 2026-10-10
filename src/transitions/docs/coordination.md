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
renderer `src/animation/platform/renderer.rs` and the overlay it draws on,
`src/animation/platform/overlay.rs`; the motion in `src/animation/domain/motion/`): window
bitmaps composited in one opaque overlay window, the real windows placed once
behind it (see "Real windows land before lift"). Layout passes, strip pans, moves along
the strip, workspace switches, resizes, entrances and the edge bounce are all
flights of it. When each step of one happens is the coordinator's ("The lifecycle"); how the engine
and the renderer talk is "Threads".

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

## Threads

The engine runs on a thread of its own, `transitions`, at the quality of service Apple gives an app's
main thread (`Executor::run_interactive`), and the renderer on the main thread, which keeps what
draws, the renderer, the switcher and the bar, and seven actors not yet moved off it (the
`src/main.rs` row of "What is still not where the map says" in `docs/architecture.md`). The engine
is made on its thread from `Parts`, since its timers run on the loop that makes them, and holds
everything that decides: the coordinator with its journal, staging and merges, the flight's score,
the pictures delivered, the frame clock, the quiet and the held lift's patience, what waits for the
screen, and the window-server reads a compose and a landing make, the spawn capture of a window
that has just opened and the dressing's composite among them. Nothing on it touches Core Animation
or AppKit (`transitions_never_touches_core_animation_or_app_kit` in `tests/architecture.rs`).
Neither thread ever waits on the other: both channels are unbounded, and each only sends.

The engine and the renderer are two actors, and talk only by message. The engine hands the renderer
what each event draws as one numbered batch of render commands (`Draw`); an event that draws nothing
sends none, so a tick is no transaction. The renderer (`Renderer::run`) wakes on a batch, takes
every batch queued behind it, plays them in order as one drain in one `CATransaction`, flushed, and
answers once, with the highest number (`Event::Rendered`): a busy main thread costs one late commit,
never a backlog of them. The answer is `Committed`, with the overlay's window number once the window
server has numbered it, whether it showed, lifted or placed, what it spent on crop-drawn tiles, and the drain's times, which the journal marks (`drawn` in `journal.md`)
before what the commit releases goes out, or `Unavailable` when the drain dressed or posed a flight
and there is no overlay to draw it on. Nothing in that drain will ever be drawn, so what it covers
releases what waited for it all the same: a lift that went out before the answer still writes its
frames, pictures the bar and arms its quiet. A flight still running is abandoned, what it owes
dropped, since its `Abandon` writes the final frames at once: nothing waits for a show sent since,
which draws nothing either.

What must not happen before the overlay's change is on screen waits for the batch that carries it
(`src/transitions/domain/drawn.rs`): the real frames for the `Show`'s, since a window written before
the overlay covering it is up jumps to its end while its tile still starts; the bar's picture
(`Settle`) and the quiet for the `Lift`'s, since the bar is read off the screen 250ms after
`Settling` (`BAR_REFRESH_DELAY`) and must not find the overlay there. Everything else goes out at
once. A new flight's hold drops the last lift's `Settle` and quiet: the transition hold leaves no
bar due (`Schedule::hold`), and a quiet is ignored once its generation is gone. A flight is settled
only once its score has ended and every batch is committed, since until then its tiles may not be
drawn where they end. `protocol` in `src/transitions/platform/engine.rs` pins each wait against the
renderer's answers given by hand.

Every instant the engine reads is the clock it is handed (`Parts`), and every track begins on the
media clock the renderer read on the main thread (`MediaClock`: `Instant` and `CACurrentMediaTime`
read together four times, the tightest read kept and anchored halfway (`MediaClock::tightest`), so a
thread preempted inside one read skews only that one, within 0.1ms of each other by
`the_media_clock_reads_what_core_animation_does`; both
count mach absolute time, so the two are inferred, not measured, never to drift apart): a command's
at the instant of the drive that gave it, a merge's, a push's and an entrance's at the instant of the
event. The engine never calls QuartzCore, and a batch played after it was sent plays from where the
score already is. A batch waits for the main thread's loop to reach the renderer, behind whatever
else it runs first, so a stalled main skips the start of a motion by the length of the stall, and
the flight still ends on time, which keeps the lift and the handover on schedule. A one-frame lead
on every begin was rejected: it costs every press 8-16ms.

The frame clock is the engine's own `RepeatingTimer`, at `FRAME_INTERVAL` from a transition's begin
to its lift, on the `transitions` loop. A tick paces decisions only: frame zero's patience, the
coalesce, the hold, the final ask, the rest windows' place, and the `settled` and `landed` checks
with the overdue lift (`LIFT_GRACE`). Core Animation draws every frame on the render server from the tracks' begin times,
so aligning ticks to the display buys no frame; the 60Hz timer that "dropped drawn frames" ("The
overlay engine" in `src/animation/docs/animation-smoothness.md`) drew on its ticks, which nothing
does now. `CADisplayLink` is rejected: it is made from main-thread objects, and scheduled on main a
stalled main would stop the clock and with it the two deadlines that exist to bound a stall; whether
it fires on another thread's loop is not verified. `CVDisplayLink` is rejected too: deprecated in
macOS 15, and its callback thread is one more hop.

*Not measured on the live build.* Moving the engine off main takes the compose, the engine's wait
for a delivery and the reactor's request queue off the main thread, and adds a hop and one commit to
the show: on an idle main the hop is expected well under 1ms, and press to overlay up no worse than
the build before it (50.5 / 84.5 / 208ms at p50 / p90 / p99 over 16 presses, its overlay counted to
the show given). The check is `journal.py summary` ("Reading it" in `journal.md`): press to overlay
up with the hop to the renderer and the show's commit, the drains' wait for the renderer at most 2ms
at p99 and the frame-zero drain played to its flush in at most 0.5ms, and `journal.py probe`'s
overlay seen up after the show against 12ms. A wait over 2ms points first at the bar, which redraws
on a focus change on main in the same milliseconds as the show's batch: deferring that redraw while
a flight is up, as the bar already pauses its menu extras, is the fix to try.

## The lifecycle

The engine holds the coordinator (`Coordinator` in `src/transitions/domain/lifecycle.rs`) with its
journal (`Journalled` in `src/transitions/domain/journal.rs`; `journal.md`) and decides nothing about
when: it stages each request, merges each pass and does what the coordinator says. A
request that finds no transition running opens one from what it alone says, before anything is
composed (`Event::Begin`): the coordinator holds the pictures service and asks, through SkyLight and
under the transition's generation, for the strip windows wholly on screen where they start and the
floating windows seen behind them. Only then is the pass composed from the cache, the overlay
installed hidden, and the coordinator told what was composed (`Event::Merge`). The overlay goes up
once those pictures arrive, each put on its tile, or 80ms pass (`FRAME_ZERO_PATIENCE`); the real frames
are written then and never before, for every kind; a 1pt hole is cut over every window that ends wholly
on screen, so the window server counts it visible and its app keeps painting, and another over the
border and focus ring rini draws around it at rest when it wears either where it ends; and the end
state is watched. A layout pass then waits for the reactor's follow-up passes (25ms); one that grows a
window, or draws one with no picture of its own, a reserved entrance or a stand-in, waits instead for
that window to repaint (up to `HOLD_CAP` from the show or from the latest pass merged before the
motion holding for one, `BURST_HOLD_CAP` in all, and no longer for one whose picture the pictures service
reports will not come, `Lost`, or one ending where no watch can see it repaint, part off the display
or sent there by a merged pass); a window it only shrinks is never held for itself and moves when
the flight does, keeping its picture from the press to the lift when frame zero took one ("A grow
waits for its repaint"); a strip movement moves at once. A pass that grows a window once the motion
runs, judged against the picture that window's tile carries, is held until that window repaints
("Mid-flight passes").

The coordinator hears every tile and frame in the window server's points (`Screen`), and says its
holes and snaps in the overlay's (`Overlay`), converted over `StageSummary.display`: the bounds of the
display the overlay covers as of the latest stage, menu bar band included. The engine passes them
through.

Frame zero's pictures that come after the overlay went up are swapped onto their tiles where they are
(`Why::FrameZero`), and frame zero is never installed again, which under a flight that moves would jump
the strip back. They were taken at the press, so each replaces only the cache's: one coming after a
landing swapped onto its tile is not swapped, since it would put the press back over that landing, a
grow's repaint the motion starts with included, while one coming after a kept shrink's landing, which
was never swapped on, still is. The window losing focus keeps its own as its focused look either way,
pictured while it was focused (`a_late_frame_zero_picture_after_a_landing_is_kept_as_the_focused_look_alone`).
Dropped, they would be lost for the flight: the service has started the client's prints from them, so
a landing that looks the same is not delivered.

The end state is four watches, kept and retargeted from one generation to the next (`EndWatches` in the
engine): every end window, captured as soon as it is where it ends (`Settle::AsSoonAsThere`, watched for
150ms); a window a layout pass resized or opened, or one it flies as a stand-in, captured once it has
repainted there (`Settle::Repainted`), while a stand-in on a strip movement, which never holds, is an
end window like any other; the focus target, from its focus report until its look changes (`Change`);
and the floating windows behind the strip, kept fresh once that change is in (`Live`). One more capture
of every end window is asked 110ms before the clock ends and never in its first 40ms (`AskFinal`), but
of a window resized or opened only once it has been pictured repainted where it ends: before that, a
capture shows it half painted, and the lift shows it as it is (`Flight::final_windows`). A
press merged in flight starts the next generation and retargets every watch and the holes. A delivered
picture is swapped onto its tile only if it was taken for the end the tile has now, under a generation
no older than the one since which the tile has ended there (`end_since`), and its window was within
20pt of that end in x, y, width and height when it was taken (`END_SLACK`); the tile then snaps onto
the window's real frame when that is 0.25-20pt off, once per window per generation, once the motion has
started. A picture the service decided under the generation before is still drawn when the merge kept
its window's end: the service runs on its own thread, so a delivery is often on its way while a pass is
composed, and it is never delivered again, since a retarget keeps a window landed where it still ends
and a change already seen. A window the merge sends somewhere new starts its print afresh in the
service, so its landing there is delivered whatever it looks like. A tile takes a delivered
picture as it comes, or the coordinator holds it to swap in later: the target's change waiting for
the window losing focus to turn, or its landing held back over its focused look. The engine keeps a
delivery only while the coordinator holds it (`Coordinator::holds`), so never one a tile turned down,
one that comes after the lift or one under a flight before: no tile takes those later, and the
service's cache already holds them.

The lift waits for the clock, the tiles presented where they end and the real windows where they were
written, or for `LIFT_GRACE` past the clock ("Real windows land before lift"). What follows it is
"Capture work in flight".

The rest windows that draw the focus ring and the border are placed ahead of it (`PlaceRest`),
once a flight end: at the first tick from `FRAME_INTERVAL` before the clock runs out that reads the
real windows landed, so their work is a drain of its own a tick before the lift's, or with the lift
when no tick saw them land sooner. The engine reads where the windows are from that tick on
(`Coordinator::wants_landing`), one read more a flight than the lift alone took. A merge that moves
the clock or writes frames places them again before its lift; one that writes nothing keeps the
place. The lift's drain then only tells the rest windows what came since and hides the overlay
("One transaction a drain, flushed" in `src/animation/docs/README.md`), and since the lift waits for
every batch to be committed, it waits for the place's too. The engine gives the place and the lift
the latest press it had taken (`through`), so the rest windows tell a flight begun after the lift
was given, which must wait for its own, from a press merged into the one lifting. With nothing drawn at rest, as the
reactor says with each `SetDisplay` and each flight carries (`StageSummary::rests`), no place is
given and the landing is read only once the clock is done, as before the place existed.

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
  one, goes along its own place on the strip: each request carries the
  window's strip frame before and after the pass (`strip_before`,
  `strip_after`, given by `pass::place_on_strip` from the two layouts
  `Reactor::take_strip_layout` returns, rounded as `to` is), and the tile
  keeps the row and size it is seen at, its edge facing the display where the
  strip frame's is. Leaving, the tile ends there (`resolve_end`); returning,
  it starts there; parked at both ends, it goes from one strip frame to the
  other when neither shows on the display, and is not drawn otherwise. So it
  moves with its column by construction, and a resize the pass gives it
  happens off screen: the stack a maximize leaves is evened out over its
  column as it parks, and drawn along its literal strip frames it would climb
  and grow on its way out. `final_frames` keeps the real park
  either way. Only a start with no strip frame (the first pass a workspace
  gets, a window new to the strip) borrows a neighbour's vector:
  `neighbour_travel` reads the `to - from` of the nearest strip window (by
  centre x) that is on screen at both ends and moves without resizing. A
  window being maximized or folded moves its corner AND resizes; lending
  that, it sent its siblings and the next column up off the screen, or in
  diagonally, at the restore back down from above (Kai, 2026-10-09). The
  display edge on the park's side, in the tile's own row
  (`park_entry_frame`), is the fallback when nothing beside it moves, and
  replaces any start or end of a parked window that would show on the
  display: the layout parks a column off the screen less the Dock, the
  overlay draws on the whole display, so with a side Dock a parked column's
  strip frame can show where the Dock is. Aimed at the edge, the displaced
  tile covered a different distance from its neighbour under one duration and
  curve, so it ran at its own speed and overlapped it (seen 2026-09-15). A
  park is judged from BOTH the requested frame and the server's: apps clamp
  a park past the 40pt `is_off_screen` threshold (Kiro shows 41pt, Finder
  52pt, log 20:35:56), and the server may already report the slot. Judged from the server's frame
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
  "The move flight, staged". A plain pan also starts from where it stands
  any window on screen at the press more than 2pt from where the travel draws
  it (`start_where_standing`): the window a left-edge drag resized already
  stands where the release's scroll puts it, and drawn back by the scroll it
  jumped out by the drag's travel and slid back. A window at another height
  at the press is parked, since a scroll never moves a window up or down:
  macOS leaves up to 52pt of some parked windows on screen, more than
  `is_off_screen` allows, and those flew in diagonally from the corner. A
  parked window starts where the strip had it at the last pass
  (`Reactor::take_strip_layout`), which is where the scroll carries it from
  unless the pass also changed its place on the strip: the columns after a
  left-edge shrink keep their place on screen, and drawn back by the scroll
  the one parked beyond the right edge came the shrink onto the screen and
  slid off. Pinned (floating) windows stand in the floating
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
taken at the top of every pass, with the strip's layout (`take_strip_layout`),
before anything can return early: a pass that placed nothing used to leave
the movement unconsumed for whichever pass came next. A strip movement places
only the pass's windows whose frames changed, as the per-window path does; a
move that scrolled nothing re-placed every window on the workspace at frame
zero. Still open: both routes land in the same machinery, and
`take_strip_movement` also feeds the switch's claim on the destination's
origin and layout, which needs care. The window-voting classifier
(`strip_pan_delta`) is gone: it only ran when the space had no strip, which is
when there is nothing to pan.

A pass containing a resize never becomes a strip pan or a move, even when
the strip moved: the strip surface draws final sizes, which would snap the
resize. The strip movement, the strip's layout and the move are still taken,
so the bookkeeping stays current and the move reaches no later pass.

## Mid-flight passes

A pass arriving while a flight runs merges into it: `merge_plans` retargets the flight's containers,
and how its pieces vote and move is "Mid-flight passes" in
`src/animation/docs/animation-smoothness.md`. What the flight does about the merge, and about a
picture landing while it runs, is here.

A pass confirming the flight's destinations is an empty `PlanDelta`: nothing is touched, and rapid
presses neither restart nor extend the flight. Any real change restarts the coordinator's clock
(`Merge { moved }`) at the pass's own length or what is left of the flight's score, whichever is
longer (`FlightScore::clock`), so the teardown covers everything playing, a bounce outlasting the pass
included; a merge that moves nothing runs the clock on only for what it started that outlasts the
clock, a step (`FlightScore::outlasting`, `Event::Extend`). Extended for every such merge, a pass
after the clock ran out moved its end up to that pass, and the lift's 350ms grace with it: while a
window had not landed, each pass repeating the destinations held the overlay up 350ms more. A pass
arriving before the plan plays, behind the frame-zero wait or the coalesce window, is folded into frame zero instead, and frame zero posed again (`RunningAnimation::take_pass`). It is
folded by the flight's look (`FlightScore::fold`; Slide's is `merge_plans`, as a pass in flight is),
with every container where frame zero installs it and taken to be where the last pass sends it: the
reactor builds a pass from the layout where the press before left it, so its frames are places on the
strip there. A burst of presses then composes as one movement from frame zero, which the look is
told shows the latest press (its `Cue`, plain or a move). Rebuilt
from the merged tiles, as it was, each window rode its own path from where frame zero drew it to where
the last press sends it. On two steps along the strip, the column the second brought started under
the one the first brought, and the column the second left behind was outrun and covered by its
neighbour; on two switches the rows did the same, and the floating windows left their container for
paths of their own (pinned by
`a_second_pan_before_the_flight_moves_keeps_the_strip_one_piece` and
`a_second_switch_before_the_flight_moves_carries_the_floating_window_with_the_surface`). Behind the frame-zero
wait, frame zero is asked again for every window the pass brought wholly on screen where its tile
starts that no ask expected there, under the same generation, and the overlay waits for that ask too,
never past the 80ms the begin set. The request alone is what the first ask knew: a window rejoining
the pass, or one a layout pass starts from the window server's frame rather than the request's, would
otherwise fly the cache's picture to its landing. Reserved entrances still waiting for a picture take
the pass's slot (`retarget_entrances`).

**A pass growing a window once the flight moves is held.** Kai's rapid Alt-R presses of 2026-10-10
(`~/Library/Logs/rini/transitions.jsonl`, g144-157, one column cycled through its widths) mostly
reached a flight already moving: each merged 3-15ms after its press with no hold, and the window it
grew flew its picture from before with its ground in the growth until its repaint was cut in,
31-287ms after the press. Only the presses that began a flight (g148, g149, g153) and one merged
before its flight moved (g150) waited for theirs. So once the plan plays, the engine holds a pass
that grows a window (`Waits::grows`), and any pass while one is held, unmerged
(`RunningAnimation::held`), and tells the coordinator each grow's end and the frames the pass writes
(`Event::HoldMerge`). A merged pass's grows are judged against the flight it reaches, before the
motion as once it moves, as the pass is composed: the one sort of a pass's window
(`EndPicture::of_request`, a strip movement's `EndPicture::of_strip`) takes the tile it lands on
(`Landing`, `FlightEngine::landing`) and judges it there (`EndPicture::against_tile`): a window it resizes,
or one a held pass grows, that it ends wider or taller than the picture its running tile carries
(`stretch::grows` from `coverage.covered`) is a grow whatever its own sizes. A press shrinking a
window from a grow whose repaint has not come (572 on the tile, held toward 1148, the next press 1148
to 860) is held so, where its own sizes would have merged every held pass at once and flown 572 into
860, and before the motion it keeps the hold, which `Flight::holds_for` would otherwise have let go.
A move of a window no held pass grows stays a navigation, however small its cached picture, and a
strip pan of one a held pass grows past that picture is held so too
(`a_strip_pan_of_a_window_a_held_pass_grows_waits_for_its_repaint`). The coordinator writes those
frames at once, so the app repaints, watches each
grow ending wholly on the display to repaint there, the watches retargeted under the same
generation (`HeldMerge`), and holds (`Phase::MergeHeld`): the flight plays on, its tiles going on to
the ends they had and resting there, with no last capture, no place, no lift and no landing read. A
repaint delivered there is held for the merge (`Coordinator::holds`). Once each grow has repainted,
is given up on (`Lost`) or `HOLD_CAP` has passed since the latest pass arrived, the coordinator says
`Command::Retarget`: the engine merges every pass it held, oldest first, from where each tile is
drawn then (`FlightEngine::merge_now`, the path any other pass takes at once), and tells it
`Event::Merge` for each, each starting the next generation. Each held pass was written as it was
held, so an earlier one's merge writes nothing, which would send a window back where that pass ended
it, and keeps the frames written (`Flight::stage`'s `final_frames`) for the latest one's to be
compared with: that one writes only what no held pass did, a pass growing nothing that merges them
at once. The latest one's merge takes the held
repaint onto the retargeted tile as a picture watched there since the hold (`watched_since`), so the
moving edge reveals content; an earlier one's tile ends elsewhere, so `HeldMerge` counts the passes
held and lasts until the latest has merged (`latest_held`).
A pass arriving while one is held replaces what the hold waits for, its own grows from its own
arrival, a window an earlier one grew watched where the latest sends it, but never past
`BURST_HOLD_CAP` (600ms) from the first one's arrival, so presses closer together than the repaint
cannot keep the tiles resting at the ends they had for as long as they come (Kai's g144-157 spanned
3.2s; `a_burst_of_held_merges_merges_at_most_its_cap_after_the_first`); one growing nothing by that
measure merges at once with every pass held. Missing the cap, the merge flies the picture the tile
has with the ground, and the repaint, still watched, is cut in when it lands. The
journal marks each pass held (`hold_merge`) and each merge it led to (`retarget`, `waited_ms` since
the latest pass held for a grow, which a pass growing nothing that merges them at once reports too). *How long a held press waits on screen is not measured yet.*

A tile the pass retargets keeps the picture it has and how focus is drawn on it: a pass is composed
from the cache, and copied over it erased frame zero's picture, a picture the flight took or a parked
target's focused look, before the overlay was up as much as after. A pass moving focus on redraws which
tiles are drawn focused, the window focus is drawn leaving kept as its own turn left it: focused until
its changed picture turns it, unfocused after. A rapid switch back to it before the window it went to
has turned finds it drawn unfocused, so focus moves to it as to any other window: watched, and drawn
focused only with its focused picture. A switch on again before then does not turn it a second time.
One starting off the display flies its focused look instead, drawn focused again, so a switch on turns
it unfocused again as focus leaves it.
A target's changed picture held for the old window is dropped when focus moves on, and one that no
longer fits its tile, a press having resized it, hands nothing over: its hairline turns with the next
picture of it that fits, or as the final capture is asked. A new target parked off the display flies
its focused look, as the first pass's does.

A pass merged once the overlay is up writes every final frame it changed, a parked window's included.
Parked windows have no tile, so a tile-only check missed their moves and left them at the old park.
Before the overlay is up nothing is written: the show writes the merged frames.

A picture reaches a tile only as a delivery under the flight's tag: frame zero's, which the tiles
take before the overlay goes up or the coordinator swaps in where they are when they come after it,
and the watches' and the final capture's, which the coordinator swaps in ("The lifecycle"). Every
other capture, a ScreenCaptureKit warm or background picture, goes to the cache only, for the next
flight. Background captures landed mid-flight in 310 of 321 flights (median 4 per flight), and every
cut of one read as a flicker or a change of transparency. On a resizing tile the cut also re-keyed
the resize from the presented state, which staggered that tile against its neighbours. Every cut
logs "picture swapped mid-flight". A cut is hard on purpose: a ~120ms crossfade veil was tried and
rejected, because stacking two copies of a translucent window pulses its net opacity mid-fade, and
there is no constant-alpha crossfade with layers. Gratuitous cuts are avoided upstream instead: a
watch delivers only a picture whose band print is news.

**Focus changing during a flight.** Focus changes a window's rendering without changing its size
(measured on a 1pt window border: 65 of 255 focused against 42 unfocused). The reactor passes every
focus report the window server sends, rini's own raise's included, to the pictures service as a hint,
and every request names the window the layout had focused as the event asking for it began
(`focused_before`): the one focus leaves. A click, a cmd-` or an app activation is reported before the
pass it causes, so the window last reported focused is the target by then; taken as the one focus
leaves, it had focus move nowhere, and nothing watched the target's look. Frame zero pictures that
window focused, and its tile is drawn focused; the focus target's tile is drawn unfocused, unless
it flies its focused look (below). Frame zero's picture of it is kept as its focused look only if it
was the key window as the request went out (the request's `key`, from `MainWindowTracker::key_window`;
`frame_zero_look`): a click's report has made its target key before the request it causes, and another
application's panel taking the keys, Spotlight's or Raycast's, leaves the window drawn unfocused while
the layout still has it focused. Kept, either picture flew the window in as its focused look looking
unfocused. Its tile is still drawn focused until its turn ("Known debt" in `src/transitions/docs/README.md`).

The target's `Change` watch, armed by its report (one remembered for a second arms a watch started
after it), captures it where it ends every 15ms, six times at most, until its band print differs from
what the engine was delivered of it as the watch started, frame zero's for a window on screen at the
press. It is judged from that, not from the latest delivery: the landing watch is looked at first in a
round, and a window focused before it landed delivered its focused look as its landing, which the
change watch then never saw change. The window losing focus is the watch's second end
(`EndWatch::losing_focus`) when it ends wholly on screen and no pass resized or opened it. Either end
resized or opened is watched only to repaint: a `Change` capture at its new size would show it half
painted, and a band print taken at another width never matches, so the first capture there would
count as its turn. Its macOS-drawn look turns with its repainted picture, or with the lift when none
settles or when the pass only shrank it and frame zero pictured it, its tile keeping that picture.
The
window losing focus, when watched, is captured in
the same rounds, in the same call as the target where their kinds and frames allow, and done at its
own change from its own frame-zero picture, out of its own tries, or at a refusal; the watch is done
when both are. An end not there yet is looked for alone every 1ms between the rounds, so the target
landing after the old window was captured is seen as soon as it is there. The windows behind the
strip wait only for the target's end. Kai,
2026-10-05: "old window blur often animates _after_ the new window focus. i understand that blur
actually renders _before_ focus in macos itself." So the window losing focus turns first: its changed
picture is swapped in and its tile drawn unfocused (`Command::Focus`), and the target's changed
picture waits for it, up to `CHANGE_EVERY` (15ms) checked on the flight's ticks, then is swapped in
anyway; two in one round swap old, then new. The watch reports the old window `Lost` once it is done
with no change, out of tries, refused or found clipped, and the target's picture waits no longer. A
merge's retarget that carries it on reports it again under the new tag: the coordinator drops a report
under the generation before. The target's changed picture is kept as its focused look,
and its tile is drawn focused: focus hands over. Each tile's hairline and shadow turn with its own
picture (`TileOverlay::set_tile_focused`); Kai, 2026-10-05: "i'd like them to be orchestrated
independently whenever blur or focus are done switching." Its focus ring and border do not: each
tile wears them as its window does where the flight ends from frame zero (`Worn::style`), so the
coordinator decides nothing about them. Kai, 2026-10-09: "can you try to move it at the beginning of
the transition? when the overlay animation starts". A merge that changes where a tile ends wears it
again at once (`Render::Wear`): the overlay is up, and the merge's drain is the first frame of its
end. A window losing focus whose turn has not shown when the
target's does, one clipped, parked, never watched or out of tries, is drawn unfocused just before the
target is drawn focused. One ending clipped by the display's edge keeps its frame-zero picture to the
lift, only its hairline and shadow turning, and its look turns at rest, pictured again at the quiet,
unless it has a material: then it keeps its blurred picture until a flight next pictures it on screen
("Known debt" in `src/pictures/docs/README.md`). SkyLight returns only its visible part, and the one
ScreenCaptureKit capture a flight lets through is the target's ("Decisions taken while Kai was away" in
`docs/pictures-motion-transitions.md`). A capture the cache refuses ends that end of the watch,
delivering nothing: the cache refuses a picture without the blur in place of one with it at the same
size, and so every later capture there. The target is pictured again by the final capture too, if its
change watch ran out first, and that hands focus over. If nothing has handed it over when the final
capture is asked, both tiles turn then, old then new, while the flight still moves: the window is
reported focused, and a look that never showed a change (one already focused when the watch started,
or an app whose title band looks the same either way) would otherwise turn at the lift. A target a
layout pass opened, resized or flies as a stand-in hands over once it has repainted, watched with no
second end, with its repainted picture, or, only shrunk, its hairline and shadow alone; a stand-in on a strip movement is pictured as it lands and hands over on its focus change
or the final capture, as any target does. A
target clipped by the display edge where it ends takes one ScreenCaptureKit capture through the hold, the
only one a transition lets through: SkyLight returns only its visible sliver ("A clipped destination
needs ScreenCaptureKit, not SkyLight" in `src/pictures/docs/capture-research.md`). A flight that moves
focus nowhere watches no change.

**Looks turned with the key window.** Every change of the key window the reactor hears of, rini's own
raise included, reaches the engine as the two windows whose look it turned, each at the frame the
reactor has it at (`Event::LooksTurned`; the key window is `MainWindowTracker::key_window`, the window
server's report while the application in front owns it, so another application's panel taking the keys
turns the window under it too). With nothing flying the coordinator asks for both at once
(`Command::Refresh`): a warm drops a picture that fits and a focus change never changes a size, so a
window kept the look it had until it was resized, in the switcher and in the next flight. In flight it
owes them to the quiet, and any picture the flight is delivered of one after the turn, a landing, its
focus change or the final capture, pays that one off, since the cache holds it. What is left, the
window losing focus that ends clipped or parked and either end whose change no capture saw, is asked for
at the quiet after the warm; a press inside the quiet carries it to its own. The pictures service
decides how each is pictured (`Pictures::refresh`): a window the window server has wholly on the
display through SkyLight, its blur baked in, both ends in one round until each one's title band has
settled, and put once; any other through ScreenCaptureKit at background priority, whose capture the cache refuses for a
window with a material ("The schedule" and "Known debt" in `src/pictures/docs/README.md`). Neither starts through a
hold, and a refresh holds every one already being captured until its own first look: a click whose
report reaches the engine just before the request for the flight it starts would otherwise have put
captures under that flight's frame zero. A SkyLight call already out when the hold arrives cannot be
recalled; it is the most a frame zero waits behind.

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
ScreenCaptureKit capture of a clipped focus end; and, with the handover measured, the lift's check:
the overlay alone through SkyLight, then the screen through `CGWindowList`, which goes through
ScreenCaptureKit's queue, while the transition settles (`journal.md`, "The handover, measured"). A pass's ask for the windows it has no picture of
is a warm, for the next flight. The
rest waits and starts at the release, most urgent first. The reactor's
`warm_all_workspaces` (about 15 captures per switch, queued 0.3ms after the
start) waits; so does the desktop render. A warm
also settles `SETTLE_BEFORE_STARTING` (50ms) before it is listed: the reactor
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
flight drew, across its merged passes, at the frame it last ended at, and asks afresh for every window
whose look the key window turned that the flight did not picture since ("Looks turned with the key
window").
They used to run the instant the overlay lifted and took 600-800ms (8-15
window captures and the desktop render), so a press inside that window flew
the next flight against a busy compositor: frame-difference profiles of a
screen recording showed 50-130ms freezes followed by catch-up jumps. A flight
beginning inside the quiet period holds the service
again; the work carries over to its own quiet. The same quiet period is what
the engine reports as settled through `OnFlight` (`ReportUp`, `ReportSettled`): `true` when a transition
begins, `false` at its quiet, so a burst of flights is one `true` and
one `false`. rini's own bar sits above the overlay and is never pictured, so
with it on a flight looks for no bar at all. Another app's bar, under the
overlay with rini's off, is pictured by the service `BAR_REFRESH_DELAY`
(250ms) after the overlay lifts, never while it is up: a bar composite
measures 31ms median, and the delay lets the compositor drop the hidden
overlay from the framebuffer and folds a burst of switches into one capture,
at the end. A flight on a display before its first desktop render lands
composites the desktop on the `transitions` thread: the render starts only once the
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
the placing-to-lift gap can be read from the log; with the handover measured, the overlay comes down
up to 60ms after it. The lift itself waits for
the render server and the real windows (the coordinator's tick, `FlightScore::settled`,
`Drawn::caught_up`, `handover_report`): the clock has
to run out AND every track of the flight's score has to have ended (`Score::settled`, which the
overlay plays exactly) AND every batch the engine sent has to be committed, since until then its
tiles may not be drawn where they end ("Threads") AND every window has to be within 2pt of where it
was written, or `LIFT_GRACE` (350ms) past the clock. The overlay used to ask the presentation
instead, every container and picture within half a point of its model, which a 1722pt curve reaches at 87% of its length: the
tracks ending says yes no earlier, so where a track outlasts the clock the lift waits as long or longer.
*Not measured on the live build*: the journal's `lift` is the check, its `at_rest` and `landed`
saying whether the lift waited for the tracks and the windows or ran out of grace (`journal.md`). The
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
as a dropped keypress. The layout reports it as `EventResponse::edge_hit`. Four
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
once; on one that does not move yet it waits for the motion (`RunningAnimation::push`, which keeps it
with `FlightScore::wait`), the latest push in place of any before it, and plays as the motion starts. Either way the coordinator's clock runs on for
as long as the score plays (`Event::Extend`). Every push that reaches a moving flight shows: each
container's bounce is a track in the flight's score, and a push while it plays starts that
container's next from where it is drawn, the clock extended to the new ones' return. Frame zero posed
again has nothing playing, and the lift drops the score, so the next push starts every bounce from
rest; an engine's record of the bounce kept past either pushed from where nothing was drawn and the
surface jumped. Both are pinned by `a_push_starts_each_carrier_from_where_its_bounce_is_drawn`, the
wait by `an_accent_before_the_motion_waits_for_it` and, where the flight decides it,
`a_push_before_the_flight_moves_waits_for_it_and_one_after_plays_at_once`. How the bounce is drawn, and how a push
re-kicks it, is "Edge bounce" in `src/animation/docs/animation-smoothness.md`.

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
shifts by one amount. What is done with the pair is the look's: the reactor
names the press (`Cue::Move`), and `stage_move` hands the strip and the pair to
the look (`Look::stage`, `src/animation/domain/choreography.rs`) and puts what
it staged on the surface. Slide, the default look
(`src/animation/domain/choreography/slide.rs`), has W cross from its start, and
N cross from its own unless the move steps, when it is held; the three rules
below are Slide's. `surface_plan` groups by travel vector, so the strip is one
container and each crossing column has its own; the moved window holds focus, so its
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

**The column passed is held.** For a move that steps, Slide holds N:
`stage_move` gives N its old slot as its surface frame and no start of its own,
so it rides the strip's container: its tile starts where N was on screen and moves only as the strip
does, W slides over it, and it finishes beneath W, covered. Only the drawing is
held. N's real window goes to its layout frame, off screen, as for any move:
`final_frames` are unchanged, the handover compares real windows with them, and
nothing of it lands on screen to be watched. A
move that does not step keeps both columns crossing.

The step starts only on a flight that moves: one still collecting passes
poses frame zero again with each, which has nothing playing, so a move
arriving then waits (`RunningAnimation::take_pass` keeps it with `FlightScore::wait`, a later step in
place of the earlier) and steps when the flight starts moving; it used to be dropped, and a full-width move merged into a collecting layout
pass showed nothing. A step is taken only while none plays (`steps`), one per burst. The look runs
the step over its own press's plain duration stretched by `NUDGE_STRETCH` (1.3x), which the step
carries, and the flight that takes it over that or its own duration, whichever is longer, so a flight
that does not step keeps its length, and a longer press folded in after the step, a switch of two or
more rows, neither stretches it nor is stretched itself; the reactor asked for the stretch with every step it
sent, stepped or not. The clock and lift wait for the step to come home (`Event::Extend` over the
score, `FlightScore::settled`).

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
`Member::Entrance` from the spawn frame to the slot, crop-drawn with its spawn
picture and the window's ground in the growth until its picture at the slot is
swapped in: the window is
watched until it has repainted where it ends (`StageSummary::repainting`,
`Settle::Repainted`) and never held for, left out of the last capture before the lift until it has
(`Flight::final_windows`), so one not repainted by then flies its spawn picture to the lift, and its slot is written with every
other frame as the overlay goes up. A frame off this display is not a spawn: a parked window
with a cold cache took a 1s off-screen capture and flew in from the park.
It used to take the reservation instead, which drew nothing in its slot
until a picture landed — a hole in the strip after every restart. It now
flies with a stand-in (`stands_in`, `picture::stand_in_picture`): a
dark `--n3` tile, one pixel stretched over the tile and clipped by the overlay
to its window's radius, as a baked picture is, moving exactly as its picture would. The stand-in is
no picture of the window, so a layout pass holds for it as for a reserved entrance, and its picture once it
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
to any stand-in, which the coordinator sees as a tile from zero width to its slot.
Every real frame is written as the overlay goes up, the newcomer's slot
included, so the watch captures the window at slot size. Holding the slot back
until the picture landed was tried: the capture then caught the window at its
SPAWN size and, drawn top-left in the slot, left the growth as backdrop, a
hole in the strip (recording of 2026-09-15 3:28:10). Its first picture once it
has repainted there, taken within 20pt of the slot, admits it
(`RunningAnimation::admit`): before the motion starts the zero-width tile
joins frame zero, posed again with it, and the flight starts when the
last awaited picture lands, is reported lost or is sent off the display by a merged pass, or
`HOLD_CAP` passes, so every tile starts in the one
transaction the motion starts in; after, it is merged in as a pass holding it alone and joins the
flight in `GroupKey::Loose` on a track of its own for what is left of the clock
(`a_late_entrance_joins_on_a_track_of_its_own`). One admitted once the clock has run out, while the
lift waits, has nothing left to travel and is placed in its slot at once
(`a_late_entrance_with_no_flight_left_is_placed_in_its_slot`): handed to Core Animation as an
animation of no length, it grew from no width over Core Animation's default 0.25s and the lift waited
for it. A picture that misses the flight
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
pictures service's forget, sent once per window that leaves:
`handle_window_destroyed`, which the AX `WindowDestroyed`, the
window-server disappearance, a discovery's stale cleanup and an
application's quit all go through, puts it on
`EventOutcome::forgotten_windows` ahead of the layout events, and finds
nothing to forget once the window is gone; a rekey forgets the old id.
An AX `WindowDestroyed` during refresh quarantine forgets nothing: the
element can be replaced while the window lives. The gap between the
window vanishing and the survivors moving is AX latency plus the
coalesce window, and is accepted.

## A grow waits for its repaint

A resize runs a navigation's sequence (Kai, 2026-10-09; `specs/animation.md` "Resizing"): frame
zero's SkyLight pictures at the press, the overlay up with them, and the real frames written as the
overlay goes up, so the app repaints at its new size behind it. Each resized window is watched until
it has repainted where it ends (`EndPicture::of_request`, sorted into the stage's lists by `Waits`,
`StageSummary::repainting`, `Settle::Repainted`). A window the pass grows on either axis, judged on the request's own sizes
(`stretch::grows`) and a merged pass's against its tile's picture as well ("Mid-flight passes"), a fold widening a window as it shortens it included, is `EndPicture::Held`: in
`StageSummary::awaiting` as well, so the overlay stands still at frame zero, in place of the 25ms
coalesce, until its repainted picture lands, up to `HOLD_CAP` from the show. That picture goes onto
the tile before the motion starts, `FlightEngine::swap` posing frame zero again with it (`reinstall`),
so the flight starts with it on the crop grid on the leg's anchors and a grow's moving edge reveals
real content from its first frame. A pass merged before
the motion holding for a window, a re-grow of one held included, holds `HOLD_CAP` from its own
arrival instead (`hold_again`), never past `BURST_HOLD_CAP` (600ms) from the show, so a burst cannot
stall the flight: holding only to the show's cap, presses every 115ms repainting 140ms after each set
off at 300ms with 572 on the tile and 1722 its end, the 1722 repaint due at 370ms
(`a_grow_merged_before_the_motion_holds_from_its_own_arrival`). A pass merged then holding for
nothing leaves the hold as it was. A window growing on one axis as it shrinks on the other, a fold or
an unfold (859x1081 to 1148x538 on n1), has no picture covering every frame: the repainted one falls
short of the start frame on the shrinking axis, so posing it cuts that span from the press picture's
content to the window's ground while the overlay still stands, and the moving edge then swallows the
ground. Flying the press picture instead would keep the ground in the growth and, at a 0.5% stretch
tolerance, cut it to content no earlier than the lift: the cut Kai reported. Holding a fold follows
Kai's 2026-10-09 choice of a lag over a flicker; the cut it makes on a fold was not put to Kai. On the
build that flew grows at once (journal n1 in `docs/data/2026-10-09-resize`, 6 flights)
the motion started 56-73ms after the press and each grow's repaint landed 162-224ms after it, 135-178ms
after the show: held, the motion starts about 160-225ms after the press. A window the pass only
shrinks is `EndPicture::Kept`, in `StageSummary::kept` as well: never held for itself, it moves
when its flight does, once the follow-ups are in when the pass grows no window, and with the held
grows when it does. Its tile flies its picture from the press to the lift. Crop-drawn on the leg's
anchors, that picture covers every frame of a shrink, so there is no ground to cut from, and neither
its repainted landing nor the last capture is swapped onto it (`Flight::keeps`); on n1 each of 5
shrinks had its repaint cut in 171-269ms after the press, and 2 flights cut the last capture in again
at 325-350ms. Its landing still snaps the tile and hands focus over, its hairline and shadow turning;
its macOS-drawn look turns with the lift. Only a window frame zero was asked for flies a picture from
the press: one starting clipped or off the display flies the cache's, whatever its age and size, so
it keeps nothing, and its repainted landing is swapped in as it lands, as a travelling entrance's
(`a_shrink_frame_zero_never_pictured_has_its_repaint_swapped_in`). The latest pass to resize a window
decides, for the hold as for the picture, judged against the picture its tile carries: a merged pass
growing a kept window takes it out of `kept`, its landing swapped as a grow's, held for if the motion
has not started, and held itself until the window repaints if it has ("Mid-flight passes"); one
shrinking a held grow inside that picture takes it out of `Flight::holds_for`, so the motion stops
waiting for it, and puts it in `kept`, while one ending it past that picture is a grow still; one
only moving it leaves it as it was, unless a held pass grows it and it ends past that picture.

Until its repainted picture lands, a tile flies the picture it has, crop-drawn on the leg's anchors,
all of it 1:1, with the window's own ground where it does not cover the frame ("Resizes through the
overlay" in `src/animation/docs/animation-smoothness.md`): a grow only once its hold ran out, or
ending part off the display, where it is never held, its growth the ground; a shrink frame zero
pictured to the lift, its picture covering every frame. A repainted picture landing in motion is
a landing like a navigation's, hard-cut onto the crop grid as it is mid-run: its content stands
against the edges that move less, and where it does not cover the frame still to come, as a
mixed-axis grow's on its shrinking axis, the ground fills the span by the moving edge
(`a_landing_smaller_than_the_frame_to_come_is_grounded_by_the_moving_edge`). On a grow that cut, from
the ground to content, is the flicker Kai reported (the third Reported line of `specs/animation.md`
"Resizing"): the hold leaves it to a grow that misses the cap.
`TileOverlay::swap` re-installs the tile's own track from the overlay's replica, the SAME begin,
length and curve, while it plays (a plain move's track too, so a crop-drawn tile on a move re-keys the
same way): the frame keeps its curve and only the pixels cut
(`a_swap_re_keys_a_crop_grid_on_its_tiles_track`). A tile with no track of its own rides its container
and has nothing to re-key. Each swap judges the tile again against the picture landing: one fitting
both ends of the leg, or the tile's model with no track, is drawn stretched, a riding tile cut from a
stale cached picture onto a fresh one included, so it reads no ground
(`a_crop_drawn_riding_tile_given_a_fitting_picture_draws_it_stretched`); a resize's repaint fits only
its leg's end and stays cropped (`a_resizing_tile_stays_cropped_when_its_repaint_lands`). Re-keying from the presented frame over the remaining duration restarted
the ease-out and the tile lurched at every swap. The flight keeps the windows the latest pass to
resize each held for (`Flight::holds_for`), and holds only for those a watch can see
(`Flight::holdable`); a grow that missed the cap is still repainting, so it is not pictured by the
last capture until it has landed (`Flight::final_windows`), a kept shrink never is, and one not
repainted by the lift is not drawn:
the lift shows the real window.

The watch reads the window's server frame every 1ms until it is within 20pt of where it ends, a cheap
window-server read, and reports it lost if it is not there `HOLD_CAP` (300ms) after the watch asking
for it, at the show or a merge. Once
there, it captures it every 8ms, 125 times at most, about a second, until a capture settles or the
lift stops the watch: `HOLD_CAP` bounds the hold for a grow, not the captures. The frame resizes instantly while the app's pixels lag behind, and a
capture taken between the two is a half-painted surface: delivering one flew the whole reveal with
garbage. So a capture counts only when settled (`repaint_settled`): it matches the previous print
(`renderings_match` on 32x32 RGBA thumbprints, a sample differing when any channel moves by more than
8, up to 3% of samples allowed so cursors and clocks do not stall it). Differing from the picture
cached before the resize once counted too, and settled nearly every resize on its first capture:
a thumbprint samples any size down to 32x32, so a surface at a new size differs from the old one
however little of it is painted, and an unfolded Ghostty flew its old rows at the top of a blank
surface the whole flight (2026-10-09). A window repainting without pause at its new size, streaming
output, never settles: held to the cap if it grows, it flies its picture from before with the ground
until the lift. *Whether a
half-painted surface can hold still for 8ms and pass, and what the watch's captures cost the motion
they now run beside, are not measured yet.*

Every fill made of the picture's own pixels for what the app has not rendered yet was tried and
rejected by eye: `contentsRect` past the picture's edge extends its outermost pixels (a hole to the
backdrop on a translucent window), stretching the lead reads as stretching, a top-left crop showing
the backdrop in the growth read as a hole (recording of 2026-09-15 3:28:10), and the old picture
stretched over the tile read as the window squashing (Kai's report of 2026-10-09). The window's own
ground is neither. Before Kai's direction of 2026-10-09 a grow its picture could not cover, judged on
the picture's own size, held the overlay still for its repaint, up to `HOLD_CAP`: the old chase held
235ms median (min 173, max 299) with 22 of about 45 holds out at 300ms, and 4 of 21 maximize holds hit
the cap on 2026-10-09, when a capture differing from the picture before still counted as a repaint.
That direction flew every resize at once, and the cut from the ground to content on a grow was the
flicker Kai reported the same day. The hold came back for a grow judged on the window's own sizes, a
merged pass's against the picture its tile carries as well ("Mid-flight passes"), so a move flying a
cached picture smaller than its window stays a navigation, crop-drawn with its ground, unless a held
pass grows that window.
`HOLD_CAP` bounds the hold for a grow, from the show or the latest pass merged before the motion
holding for one, `BURST_HOLD_CAP` bounding those in all, a held merge's wait for its grows from its
arrival, `BURST_HOLD_CAP` from the first pass held ("Mid-flight passes"), the hold for a window with no picture of its own ("The reservation
fallback"), and how long a repaint watch waits for its window to arrive; a 150ms cap was
tried and raised back to 300ms, since a watch waits for the real frame to reach its end before it
captures. *How often 300ms is enough is not
measured yet.*

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
