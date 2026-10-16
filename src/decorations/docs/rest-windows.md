# Rest windows, in full and measured

The rest windows' rules in full, and what each rests on: the probes, timings and inferences behind
them. `README.md` ("At rest") states each rule in a sentence or two under the same heading.

Between flights macOS draws each window, and rini draws the ring and the border around the windows
that wear them in windows of its own, one above each, holding `Bands` laid exactly as a tile's
(`a_tile_wears_its_ring_as_dressed_and_as_worn_and_a_turn_leaves_it`, `every_band_rounds_on_the_continuous_curve_at_its_outset`).

## The windows

Borderless, non-activating `NSPanel`s on rini's main connection, click-through,
clear, with no shadow, collection `[.moveToActiveSpace, .transient, .ignoresCycle,
.fullScreenNone]`: `.transient` has Mission Control, App Exposé and Show Desktop hide a rest window
before the reactor's alpha 0 reaches it, where a managed one would be laid out as a window of its
own, and `.stationary` would leave it over the thumbnails. A rest window's frame change leaving a
Core Animation transaction pending on the main connection, which a read made on it waits for, is
inferred from a repro outside rini, where a window's frame change did; rini reads windows on a
connection of its own (`src/windows/docs/window-server-reads.md`). Each holds a flipped view whose
layer holds the band at its window's frame. A rest window's frame is the band's outer rect cut to its
window's display (`shape::rest_frame`), so a ring at the display's edge draws nothing onto the next
one. Rest windows are made as a model arrives, never from a frame, a place, a lift or between a
flight's begin and its lift: a model that comes then waits for the place ahead of the lift, and a
pool the lift leaves short is made in the actor's next turn, once the overlay is down
(`Event::Lifted`). The pool holds, for each display, the most windows any one of its workspaces
has that wear a band and could show it, held by no more than a park, and one spare
(`Rest::pool_size`, from the workspace the reactor sends with each window, `rest::Group`), and each
window whose band shows takes one from it (`rest::assign`): a free one goes back to the window it
last drew, and a window none of them drew takes the one free longest, so the band of the window
focus left is kept at alpha 0 for focus coming back ("What a place costs"). A display shows one workspace at a
time, and a flight's place takes the parks on its display before it draws, so a flown switch wants
rest windows for the arriving bands alone. With the ring alone that is 2 a display however many
workspaces it has: the ring moving with focus, and a switch no flight covers, whose leaving and
arriving ring show together until the leaving park takes effect, make none. With the border on such
a switch can be short min(leaving, arriving) − 1 rest windows until each leaving park takes effect,
at its window's move or 250ms at most, so an arriving band can come that much late; on 4cddd692 the
pool held one for every window that could show a band: with the ring alone, one window-sized panel
per workspace with windows. The windows rini manages as it launches are pooled then, and a window
opened later is pooled in the actor's turn after its flight's lift. The overlay's first show cost 112ms against 14ms to order it in; what making a 1pt rest window costs is
not measured. A window that shows nothing keeps its rest window at alpha 0 for the next, and the
pool never shrinks while any window can wear a band; once none can, every rest window is closed
("Off" in `README.md`).

## Shown by alpha, never ordered out

Ordering a window in costs 13.94ms and out 13.07ms at the
median, against 0.36ms and 0.34ms for an alpha change ("Toggle alpha, do not order the window in
and out" in `src/animation/docs/capture-overlay-research.md`). So a rest window is ordered in once,
as it is made, and every hide after is its alpha. AppKit sends a frame and an alpha to the window
server at once, and the layers go at the transaction's commit, by another path. So a rest window
given a window, or shown again, goes to alpha 0 first, its new layers, if any, are committed and
flushed, and only then does it take its frame, its place above its window and alpha 1
(`rest::afresh`, in the order `rest::plan_draw` gives every call of a draw, a released rest
window's alpha 0 inside the transaction); in the other order one frame could show the old window's
band, clipped, in the new one's place. That the window server takes the two in the order rini sends
them is inferred, not measured.

## What a turn draws

Each rest window keeps the look it last drew (`rest::Look`): its layers,
laid from what its window wears and in which paints, the window's size and corner, the display's
scale and where the window sits in the rest window; its frame; and whether it shows. A turn works
out each one's look and changes only what differs (`rest::delta`): a move sets the frame alone, a
resize, a paint, a corner, a scale or a band cut at the display's edge lays the layers again, and a
hide or a show again is the alpha alone, the layers and the frame kept at alpha 0. A turn that lays
no layer opens, commits and flushes no transaction, so a reorder, a focus report for a window whose
look stays, or a frame that did not move changes nothing but the order it asks for. On 4cddd692 a
turn laid every rest window with a target, and committed and flushed, whatever came; with the ring
on, the lift's drain played to its flush in 3.98-4.45ms at the median against 1.66-1.91ms with it
off (the 2026-10-08 timing battery, 3 runs each way,
[`docs/data/2026-10-08-ring-perf/`](../../../docs/data/2026-10-08-ring-perf/)), part of which was that draw (inferred: no
sample split the lift). That AppKit leaves a layer-backed view's root layer as rini set it between
turns, its contents scale included, is inferred, not seen.

## What a place costs

A scratch build timed each call of every place drain on the main thread
(2026-10-09, the ring alone, 140 place drains each way over 7 ring-on batteries, the timing patch
and each drain's calls in [`docs/data/2026-10-09-place/`](../../../docs/data/2026-10-09-place/)).
Every place in the battery showed one rest window afresh, the ring's, on the window focus came to.
Given the rest window the ring left, as the pool gave it then, the place laid it, committed and
flushed (1.60ms p50, 2.48ms p90: the alpha 0 of the window it left, an AppKit change, opened the
transaction the layers' nested in, and the flush committed both), set its frame (0.47ms, 3.01ms)
and ordered it (0.30ms, 6.19ms), and the drain's own transaction then committed the frame and the
alpha 1 (1.59ms, 2.35ms): the place step took 3.18ms p50, 9.68ms p90. Any commit carrying a change
to a rest window cost about 1.5ms, its layers or its alpha alike. Given back the rest window that
last drew it, 105 of 140 places laid nothing and set no frame, so opened no transaction of their
own: the place step took 0.53ms p50, 6.10ms p90, the order most of it (0.36ms, 4.96ms), and the
drain's commit the same 1.55ms. The journal's place drains, applied: 4.6-6.1ms p50 and 11.3-12.5ms
p90 before, 2.3-2.8ms and 6.9-13.8ms after (3 batteries a run, two timed runs and the untimed
build after): the p90 is the places that still lay, a workspace switch's and a first pan's, and the
order's tail. The handover check, 3 runs each way, gave every window's interior, edge and band
differences the same to 0.3%.

## Ordering a rest window

Probe P1 (2026-10-06, an idle machine,
[`docs/data/2026-10-06-drags/p1-order.jsonl`](../../../docs/data/2026-10-06-drags/p1-order.jsonl))
ordered a 1pt panel against another process's window and read the window list after. AppKit's
`orderWindow:NSWindowAbove relativeTo:` held the panel directly above the other window;
`SLSOrderWindow` against it did nothing, and is no longer declared. Against a window at sub-level 1,
the panel stayed below until it took that window's level and sub-level too (the `level_copy` row:
one place below before the copy, above after). So a rest window takes its window's level and
sub-level, read on the frames thread when the window is introduced and after each reorder or unhide
(`ServerIntro`, `ServerOrder.level`), through `SLSSetWindowLevel` and `SLSSetWindowSubLevel`, which
act only on the caller's own windows (the port's `take_stack_place`), and only when they differ
from the ones it took last (`rest::stack_change`), and then AppKit orders it above. A sub-level the
window server gives no answer for is none, and the rest window keeps the sub-level it holds rather
than take 0. The sub-level is read by the Mach port rini already reads other apps' windows with,
not by `SLSGetWindowSubLevel` as P1 read it; that the two answer alike is inferred. After each of 10
raises of the other window, its 808 came 9.4-18.0ms after the raise, and the panel, ordered again
from the 808, was back on top 7.4-25.4ms after it: 8 of 10 within 16.7ms, p90 25.4ms. The call took
0.57-6.02ms, about 1.4ms typical. P1 ran against a scratch helper only. The 808 now reaches the
main thread after the frames thread has read the window's level, on the read connection, which
waits for no commit of rini's, so a raise puts the band back later than that by the time that read
takes (inferred, not measured). So for 1-2 frames after
another app raises its window, that window sits above its own bands: the window covers none of
them, which lie outside it, but its shadow does, and so does any window it rose past that overlaps
them. A rest window is ordered again when the window server reorders its window or orders it
back in (808, 815), when it is given a window, and when focus comes to its window, each once a turn
(`rest::to_reorder`), and for what came between a flight's begin and its lift, at its place or its lift.

## A click-through band is never hit

Probe P5 drew a click-through band around a window and found
`SLSFindWindowAndOwner` 2pt inside to 4pt outside its right edge returning the window with the band
directly above it, every time, and every posted drag resized it, with the band and without. Its pixel
comparison is confounded: its two phases compared two different looks of the target window, phase
one's posted presses having made the helper's panel key; its straight-edge rows show the band leaves
the window's inner hairline alone.

## The band's corner

P5's band was a `CALayer` border on the default, circular curve, whose inner edge
lies outside a continuous window corner everywhere: it left a crescent showing what is beneath, up
to 0.67 of a device pixel's coverage per pixel, 21.6 pixel-equivalents per corner at 2x for a 16pt
corner and a 4pt band (offscreen). A band on the continuous curve, its inner contour the window's own
corner, leaves at most 0.24 of a device pixel's coverage per pixel at its seam, 9.3
pixel-equivalents per corner (offscreen render, not the screen). Its inner edge is a border's outer
edge `width` in, so the outer corner is `corner + width` (`shape::band`).

## Following

A rest window takes its window's frame from the window server's frames
(`src/displays/docs/live-frames.md`), followed only while the feature is on ("Off" in `README.md`): 806 and 807 saw every frame a 1ms poll saw during Kai's
drags, n=1165, p90 0.08ms behind it, in Ghostty only. The actor takes what queued each turn, the
latest frame of each window. A window the model first names is asked of the window server as the
model arrives, covered or not, through `Following`'s queue, and the frames thread reads it whole:
its frame, whether it is ordered in, its corner radius, and its level and sub-level (`ServerIntro`).
It is drawn from when that comes back (`Rest::ask`, `Rest::introduce`), one hop on an idle thread
(inferred, not measured), and its corner is the one read then. An introduction back before the
model naming it is taken, as one under a flight is, takes each frame and order that comes after it,
so a window a flight opens is drawn where the flight's write put it. From a flight's begin to its lift
the rest windows follow no window on its display: the introductions, frames and orders are taken
as they come, so a park is taken with its window's move and a hide with its order, but nothing is
drawn or ordered until the place ahead of the lift ("The handover"). A frame or
an order of a window on another display, or a park there taking effect, draws the turn as if
nothing covered it, with everything that waited: the overlay covers only its own display. None is
hidden for a flight.
A window the window server closes (804) shows no band from its close on (`rest::ordered_out`),
before the reactor hears of it and drops the window from its model; that 804 reaches rini for every
window it asks the window server's notifications of is inferred, as JankyBorders listens to it, and
not seen.

## Parked comes from rini's layout

macOS clamps a parked Chrome or Finder window so 52pt of it
shows, past the 40pt `is_off_screen` allows (`PARK_CLAMP_PX` in `crates/rini-geometry/src/lib.rs`), so
a band read off the window's frame would be drawn around the sliver. The reactor sends a window as
parked when its workspace is not the one its display shows or its strip has it off the viewport. A
park, and a window brought back, takes effect at the window's next frame, at a landed flight's
place or a lift on its display, or 250ms after the reactor said so (`rest::Parking`, `Rest::place`,
`Rest::lift`): rini parks a window by moving it,
after the overlay is up, and a band that went at the press would leave its window bare before the
overlay covered it.

## The handover

The overlay covers its display at level 18, so a rest window under it is unseen
once it is up, as its window is. A press's own model reaches the rest windows before the overlay is
up. Drawn as it came, a full draw of every rest window sat on the main thread ahead of the show:
on 4cddd692's timing battery (2026-10-08, 3 runs each way, `B-ringoff` and `C-ringon` in
[`docs/data/2026-10-08-ring-perf/`](../../../docs/data/2026-10-08-ring-perf/)) the show's drain waited for the main
thread 4.9-18.7ms at p90 with the ring on, against 0.03-0.04ms with it off. So the reactor tells the
rest windows of each flight it hands the engine, with the display its overlay covers, before that
batch's model (`Event::Flight`, sent by `publish_rest` for what `Reactor::send_flight` noted), and
from that begin (`Rest::fly`) to the lift a turn draws and makes nothing unless a window on
another display changed: what came waits (`Rest::turn`). Until the overlay is up the rest windows
show the windows as they were before the press, which is what frame zero shows; the show covers them
(`Rest::cover`, after the overlay's alpha 1 in `Renderer::play`'s steps, with the display it is
laid out on). A flight that never shows sends no lift: one the engine declines with no display yet
or with an empty pass, or merges elsewhere. So a flight not shown `FLIGHT_PATIENCE`, 250ms, after
its begin lets go, and what waited is drawn (`Rest::settle`, on the timer `Rest::due` sets). Before
the bands, the journal's begin to show took at most 249ms over 1726 flights: a show after the
patience is rare, and the rest windows then draw once uncovered before it covers them again. An
abandoned flight sends its lift, which ends the cover as any lift does. A press as a flight lifts
can begin a flight of its own after the engine gave that lift and still be heard by the rest
windows before the renderer plays it: the begin goes to them from the reactor, the lift from the
engine through the renderer, and nothing orders the two. So `Event::Flight` carries the press it
answers, the engine gives each place and lift the latest press it had taken (`Render::PlaceRest`,
`Render::Lift`, `through`), and a place or a lift given before the press of the flight begun last
leaves what waited for that flight; the lift hands it the cover, with its patience from its begin
(`Rest::place`, `Rest::lift`). A press merged into the flight came before its lift was given, so
that lift draws it. Each turn under the cover
had been main-thread time the flight's drains waited behind: the frames of every window rini's writes moved, the 808s of a
raise, the key window's turn and each park's patience, each a full draw of every rest window with
its window-server reads and orders, where a turn now draws only what changed ("What a turn draws"). A 10s sample of rini at rest on 2026-10-08, while Kai typed and
no flight ran, held 13 of 7216 main-thread samples in `RestBorders::take`, 9 of them in
`SLSWindowQueryWindows` waiting on `SLSConnectionSynchronizeSLSCATransaction`'s lock, the read
the level and the corner radius were then made with on the main thread, and 3 in the turn's
commit. That lock was the main connection's, which those reads then shared with the rest windows'
own commits. The frames were symbolised against a rebuild of the same commit, so their names are
inferred
([`docs/data/2026-10-08-smoothness/`](../../../docs/data/2026-10-08-smoothness/), with the journal's
drains split by where in their flight they fall). Those reads are the frames thread's now, on
rini's read connection, whose lock no commit of rini's takes ("Reads go on a connection of their
own" in `src/windows/docs/README.md`); the main thread reads the window server at a place alone,
the frame of each window drawn there, through `SLSGetWindowBounds`, which waits on nothing for
another application's window (`src/windows/docs/window-server-reads.md`). Once the flight
has landed the renderer places the rest windows, still covered: the coordinator gives `PlaceRest`
at the first tick from a frame before its clock runs out that reads the real windows where they
were written, or with the lift when no tick did, and the renderer plays it before the transaction
of the drain carrying it. Off, the reactor tells the flight engine so with the display, and no
flight places anything or reads its windows a tick early ("Off" in `README.md`). The model that waited is taken, every park on that display that waited
taken, every rest window on it put at its window's frame as the window server has it, each one
whose window was reordered since the flight's begin or that focus came to ordered above its window
again, and every one drawn as it is then where its look changed (`Rest::place`). That work had sat
in the lift's drain, between the flight's end and the overlay coming down ("What a turn draws").
The lift, in the same main-thread turn as the hide, reads nothing: the place read what it draws,
and a covered turn took every frame since, so it takes what came after the place and draws only
that, usually nothing (`Rest::lift`). Between the two a band shows only through its hole, where
its tile draws the same pixels. A merge that moves the flight's clock or writes frames after the
place places again before its lift. A park the lift takes no move for, on another display, still waits for its
patience, and only the actor's own turn sets that timer, so the lift asks for one
(`Rest::wants_turn`, `Event::Lifted`), as it does when the pool is short. That no frame shows a band
out of place is inferred, not measured. The overlay is
holed over each band as over each window ("Holes" in `src/animation/docs/animation-smoothness.md`),
so the rest window keeps painting under it, inferred as needed; a rest window whose window moves
under the overlay stays where it was until the place, so the band's hole at the window's end shows
whatever is below it there, not seen on screen.

## Not measured

Not measured: what a rest window costs the window server in memory and the main thread in time, per
window and under an 808 burst as a strip raise makes one (probe P13, which needs an idle machine
and was not run); whether a full-size clear panel allocates a full-size backing, which would want
its shape cut to its band instead; whether a re-order brings a `.moveToActiveSpace` window onto the
Space shown; how a rest window moves through macOS's own Space animation, which slides a window
that is not `.stationary`; and that Mission Control, App Exposé and Show Desktop hide every rest
window, the shown ones and those at alpha 0, from the start of their animation, which `.transient`
is documented to give and was not seen on screen; and that focus moving between two windows of
different sizes, and a rest window shown again, show no frame of the old band in the new place.
