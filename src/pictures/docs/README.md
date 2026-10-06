# `pictures` — pictures of windows, the desktop and the bar

Every capture rini draws. A flight draws a picture of every window it moves, the desktop behind them
and, under `bar { off; }`, another app's bar above; the switcher draws a picture of every window it
offers; the bar grounds itself on a picture of the wallpaper. This feature takes those pictures,
decides what each is worth, keeps them, and hands them to layers. The plan it is part of is in
`docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **Coverage and fit** | `domain/coverage.rs` — `Coverage`, whether a capture is usable, whether it fits a frame, whether a grow outgrows it, whether a warm must recapture, whether a desktop picture spans its display, when a picture is stale |
| **The cache's rules** | `domain/cache.rs` — `PictureCache`: never downgrade a usable picture, keep a blurred one over a grey one of the same size, put a copy back only over the picture it was made of |
| **Regions** | `domain/region.rs` — where a window lies in a batched composite: its pixel rect, whether the composite holds it, whether the composite is the shape of what it was asked for, its unit rect, and a crop piece's rect within a region |
| **What is known of each window** | `domain/knowledge.rs` — per window server id, whether it is translucent, whether it has a material, its corner radius; which SkyLight call each window goes in; the measurements that learn them, over plain bytes |
| **Prints** | `domain/print.rs` — whether two pictures of one window show the same thing: their band prints, where a focus change shows, and their bodies, where content does |
| **The blur** | `domain/translucency.rs` — putting a window's blur back into its picture, merging its own capture with a composite of what is below it |
| **The handover check** | `domain/handover.rs` — `HandoverCheck`, a lift to measure, and `compare`: how far the screen after it is from the overlay before it, inside each window and either side of its outline, sampled as the research measured it |
| **Settled** | `domain/settle.rs` — when a capture of a window that has to repaint is its settled rendering, by thumbprint |
| **The stand-in** | `domain/stand_in.rs` — which window with no picture flies as the dark stand-in |
| **Desktop and bar levels** | `domain/backdrop.rs` — which window-server levels are the desktop and which the bar |
| **The port** | `domain/target.rs` — `PictureTarget`, a window to picture and the size it should be |
| **Identity** | `domain/identity.rs` — the id a window is pictured under, its own or a synthetic one, which windows rini does not manage a warm pictures, and which of their pictures go |
| **The schedule** | `domain/schedule.rs` — `Schedule`: the asks, watches, refreshes, hints, holds, display changes and handover checks in, the next call out; the warm filter, and the capture budgets and cadences |
| **The watches** | `domain/watch.rs` — windows on screen captured through SkyLight: the frame-zero and final asks, the `Landing`, `Change` and `Live` watches, the focus and placement hints, and the band prints each client was last delivered, which decide what is news |
| **Refreshes** | `domain/refresh.rs` — windows pictured afresh at rest once their look has turned: which are due a look, each one's captures held until its look settles and the one it settled on, and which are owed ScreenCaptureKit's capture instead |
| **The service** | `platform/service.rs` — the `pictures` thread that runs the schedule and makes the service's own SkyLight calls, the cache, each window's focused look and knowledge, the pictures of the display the overlay flies on, and `Pictures`, the handle every client holds |
| **What it tells clients** | `event.rs` — a picture the cache took, the pictures a client asked for or watched under its tag, a landing whose picture will not come or a turn of the window losing focus that will not be seen, and a handover check's overlay picture taken and its measure |
| **A picture** | `platform/picture.rs` — `Picture`: its `Pixels` (a bitmap, a surface, or a region of a batched composite), its `Coverage` and its `Route`; the stand-in, one pixel; what the cache keeps of a picture and that it never keeps a stand-in; a region kept as a copy of its own |
| **SkyLight** | `platform/skylight.rs` — every SkyLight and `CGWindowList` capture but the bar's own (its menu extras and ground, `src/bar/platform/menu_extras.rs`): one composite of a list of windows (the desktop backdrop, another app's bar), the batched capture of the windows on screen as regions of the calls it makes, the framed capture of one window with, wholly on a display, its blur, and a handover check's overlay alone and whole screen |
| **Measurement** | `platform/measure.rs` — a picture read small: its band print and its body samples, what a window's first picture says of its translucency and corner, and whether its baked picture shows a material; and a picture drawn whole for a handover check |
| **ScreenCaptureKit** | `platform/screencapturekit.rs` — one listing of many windows, the capture of one listed window, and the desktop render; each reports back through a callback |
| **The desktop and the bar, listed** | `platform/desktop.rs` — which windows a desktop or bar picture is composited from, and the rect each set covers |
| **Bitmaps** | `platform/bitmap.rs` — a fresh RGBA context, an image's copy of its own, and an image's thumbprint |
| **Layers** | `platform/layer.rs` — handing a picture to a layer, and a region through `contentsRect` |

The stretch tolerance a fit is judged by is `rini_geometry::stretch`, shared with `animation`'s "is
this a resize" so the two agree.

Its interface and `domain/` speak rini-geometry's spaces (`crates/rini-geometry/docs/README.md`):
frames, display bounds, a target's size, coverage, watches and deliveries in `Screen`, a region's rect
and an image's size in `Pixel`, and what a layer's `contentsRect` is given in `Contents`. CG stays in `platform/`: in its objc2 and SkyLight calls, its reads from `windows`, and
its private helpers and structs.

## The service

One thread, `pictures`, makes every background capture of a window, the desktop and the bar, every
capture of windows on screen a transition asks for or watches, every refresh, one SkyLight call at a
time, and a measured lift's two pictures. Two kinds of capture are not its yet: a transition's desktop
and bar, composited on the caller's thread until its display's first render lands, and the flight
engine's spawn capture of a window that has just opened, taken on the `transitions` thread as it
composes (both Known debt). It is started once, in `src/main.rs`, with a sink per client; the flight
engine, the reactor and the switcher each hold a `Pictures` handle.

| A client | Through the handle |
|---|---|
| asks | `ask` windows anywhere at a priority, `ask_on_screen` windows on screen under a tag, `refresh` windows whose look has just turned, wherever they are, `forget` a window that is gone, `set_display` for the display the overlay flies on, `warm_visible` every window on it rini does not manage |
| watches | `watch` windows landing, changing or live, `retarget` a watch, `stop` it; `hint` that a window was focused or placed |
| keeps | `keep_focused` the picture delivered under a tag as the window's focused look |
| measures | `check_handover` a lift: the overlay alone as the next SkyLight call, `OverlayTaken` told, the screen `AFTER_LIFT` later, and `Handover`, each window compared on a thread of the check's own |
| holds | `hold` at a transition's begin, before anything is composed, `Settling` at its lift, `Released` at its quiet |
| reads | `usable`, `usable_many`, `focused`, `knowledge`, `windows`, `stand_in`, `dressing`, from any thread under one lock |
| hands over | `insert`, for the flight engine's one capture of its own, of a window that has just opened |

Asks, refreshes, watches, hints, holds and forgets are messages, applied in order on the thread, every
one already queued before the next call is made, so the newest wins. Reads are not: they lock the shared state and
copy out, so the switcher draws without a round trip and the engine composes frame zero with what is
held at that instant. Every capture a watch, an ask or ScreenCaptureKit brings back goes into the cache
first, and every sink hears of each one the cache took (`event.rs`); a refresh's captures are only
judged against the cache (`takes`) while it polls, and only the one each window settles on is put and
told. So a client draws only a picture the cache took, so a flight and the
switcher never draw what the next flight will not. The watches are told which pictures the cache took
before they decide anything: a delivery under a tag carries only those, each with the server frame it
was taken at, and says when the service sent it, so a client can tell the capture from its own wait
for it; a capture the cache refused is neither delivered nor news. Every sink hears a
delivery, and a client takes its own by the tag's client.

ScreenCaptureKit is asked from the thread and completes on its own queues, which only copy the pixels
out and post them back. No capture call is made there: `CGWindowListCreateImage` is proxied through the
same delivery queue and deadlocks. A handover check's screen is the one `CGWindowListCreateImage` of a
whole display, made on the thread while a transition settles: it goes through that queue too. Its two
pictures, a display's worth each, are drawn and compared on a thread of their own, `handover`, which
posts the result back like a completion: a press asks its frame zero of the service's thread.

Window captures are taken at 2x on any display: ScreenCaptureKit costs per call, not per pixel, and a
picture taken at 1x flies soft on a Retina display. What is read off a display's framebuffer, the
desktop and the bar, is taken at that display's own scale.

**The desktop and the bar belong to their display.** The service keeps one display's pictures, and a
read names the display it is for, so a transition can never be dressed with the display left behind;
which display that is, only the schedule records. A transition is dressed with a desktop composited
on the caller's thread until the first render of its display lands, which is what rini always did:
without one it draws black. That render starts only once the service is released, at the quiet after
a flight, so a burst of transitions on a newly entered display composites the desktop on every one,
and a render that comes back empty or the wrong size is asked again at the next quiet. The bar is
composited once per display and kept, and read for presence on every transition, so a bar that has
gone is not drawn.

**SkyLight reads the framebuffer.** A bar read while the overlay goes up has the overlay in it. The
handle counts every `hold(Transition)` as it is asked, ahead of the message, and the thread drops a bar
picture that a transition began during or before.

## The schedule

`domain/schedule.rs` is pure: what goes in carries `now`, and nothing reads a clock. `next(now)` says
what to call next: picture the overlay or the screen for a handover check, list windows, capture one
listed window, render the desktop, picture the bar, wait until a time, or nothing.

| Rule | Why |
|---|---|
| While a transition holds the service, only a focus capture starts, of a focus target clipped by the display's edge, and, with the handover measured, the screen a check takes while the transition settles | ScreenCaptureKit work queued at a focus change slows every step of it: with four captures out the frame-zero capture took 92-97ms against 26-33; what the transition waits for is the exception, and the check is opt-in and taken once the overlay is down |
| A handover check's overlay picture is the next call, ahead of every other; the screen comes `AFTER_LIFT` (45ms) after it, once, and never when the overlay's picture was not taken; a transition beginning drops the check and the overlay's picture it holds, and neither picture is taken while a transition is asked and not yet applied | the lift's hide waits for the overlay's picture; the research took the screen 45ms after its hide; a screen taken once the next overlay is up would measure that overlay, and a press asks its frame zero of this thread, so it waits at most for the one call under way as it lands |
| What waited starts at the release, most urgent first: focus, background, warm | a capture of a focus end is never queued behind warms |
| A background or warm ask settles `SETTLE_BEFORE_STARTING` before it is listed, and a refresh, with every refresh already owed, before its next look | the reactor sends a pass's warm just after the request for its flight, and the engine a click's refresh just before the request for the flight the click starts, which the engine holds for as it takes it: the settle usually covers the gap; a warm it does not cover is listed again at the release (below), and a refresh it does not cover is cancelled by the hold (below) |
| A refresh is looked at only while nothing holds the service, after the transition's SkyLight work and before any ScreenCaptureKit call is issued, every window due in one round: one call for windows that share a call kind and do not overlap, two when one needs the alpha call and another the baked one, and overlapping windows apart, each group its own calls, as for any set. Each is taken wholly on the display wherever the window server has it, with only the desktop composited behind it; a window the window server does not have wholly on the display is owed one ScreenCaptureKit capture at background priority | Kai, 2026-10-05: "why are you capturing each old/new window independently? seems like a waste, could capture both at the same time as a group call, no?"; both ends of a focus change are asked together, so they are due together, and one window a round doubled a click's rounds; SkyLight returns a material window's blur and ScreenCaptureKit's capture of the window alone does not, and the cache keeps a blurred picture over one without at the same size, so through ScreenCaptureKit a window with a material keeps its old look (Known debt); SkyLight captures only what is on a display |
| A hold cancels every refresh, looked at or not, and one asked while held as it arrives; the release asks again for each from `SETTLE_BEFORE_STARTING` on, judged from captures of its own with fresh tries, unless the cache holds a picture of it taken `REPAINT_SETTLED` (70ms) after its look turned. Before each SkyLight call of a refresh round the thread checks for a transition asked and not yet applied, and makes no call once one is; a window it did not call spends no try and is owed nowhere else. A watch's round is never stopped short | Kai, 2026-10-05: "cancel the previous polling and let the last capture finish?"; calls are served one at a time, the handle counts a transition before it sends the hold, and the thread applies every command queued before its next call, so only the call already out is ahead of a press's frame zero, the baked call after a first sight's alpha one included; a capture from before the flight is stale once the flight has moved or resized the window; the app repaints 68ms after make-key at p90, so a picture taken sooner, frame zero's at the press among them, can predate the turn; what a watch captures, the transition is waiting on |
| A refresh captures its windows every `CHANGE_EVERY`, `CHANGE_TRIES` times at most each, and puts none of it to the cache while it polls. A window's look has settled at a capture whose band print matches the capture before it and that was taken `REPAINT_SETTLED` after its look turned; it drops out of the rounds after, and the refresh ends when every window has. Each window is put once, with the capture it settled on, or its latest once its tries run out, and so told once; a capture that missed a window wholly on the display spends a try, and a window no try captured is owed ScreenCaptureKit's capture; a capture the cache would refuse ends the window's refresh with nothing put | Kai, 2026-10-05: "are you pushing new images on every scan or only once when they change?" and "stop polling when both re-rendered": every capture was put and told to every sink, a message each to the flight engine and the switcher, and a refresh ended only at a capture differing from its first, so one whose first look already showed the turn spent all six tries, 12 calls and 12 pushes a click (inferred from the rule, not counted); the window server changes the front process 1.1-1.6ms after the make-key call, and the app shows its new look 47ms after it at p50, 68ms at p90 for Chrome and 54ms for Ghostty (idle; "Where a moving focus change spends its time" in `src/animation/docs/capture-overlay-research.md`); the focus report rini hears comes from that change (inferred, not timed), so a capture at the report shows the look before, and two captures alike from 70ms on have seen the repaint: rounds at about 52, 67 and 82ms, three for both ends, and a repaint after 67ms costs one round more; the picture held from before can differ in its band for other reasons, a title changed or what was composited behind it, so the turn is judged from the refresh's own captures; a focus change shows in the title band, as it does for `Change`; a call that comes back empty, as one does once the desktop's windows have changed, succeeds once they are listed again; the cache refuses every later capture of the window at that size too |
| A capture listed and unable to start through a hold is listed again at the release, as is one asked again at another size | the buffer a listing prepares is the size it found the window at, and the transition may resize it |
| A warm is dropped where the picture held fits and is fresh (`wants_warm`) | re-capturing every animated window per keystroke is churn |
| One listing serves every window that may start; at most `MAX_CONCURRENT` captures are out | a fresh listing costs about 83ms against 26ms for the capture; ScreenCaptureKit serialises past four |
| A window owed a capture is not asked twice; a forgotten one is captured no further | a burst of events cannot queue one window twice, and a closed window's capture is not kept |
| A display change re-renders the desktop for the new display, after the render of the old one is refused | the render it asked for used to be thrown away when it landed |
| Window captures carry on through a display change | only the desktop belongs to a display |
| The desktop is rendered again at a release when missing or stale | asked during a transition it would land mid-flight |
| The bar is pictured `BAR_REFRESH_DELAY` after the overlay lifts or the display changes, never while it is up | a bar composite costs 31ms; a burst of switches pays it once, after the compositor has dropped the overlay |

The costs are measured in [`capture-research.md`](capture-research.md): "Capture cost", and the
listing against a kept filter in "What a capture costs in a press".

The flight engine's spawn capture, one framed capture of a window that has just opened, is not
scheduled here: it takes it on the `transitions` thread as it composes and hands the picture over
through the handle. Every other picture a transition draws it asks for or watches.

## The watches

`domain/watch.rs` is the SkyLight half of the schedule a transition drives: windows on screen, captured
at the frames the window server has them at (`SLSGetWindowBounds`: a parked window's written frame is
there within 0.1ms of the write returning, an app still activating applies a write late) and delivered
under the asker's `Tag`, a client and its generation. None of it waits for a release: it is what a
transition waits on. The schedule's other SkyLight work, a refresh at rest, is cancelled by one
and asked again at its release (`domain/refresh.rs`, "The schedule").

| Rule | Why |
|---|---|
| SkyLight work goes before any ScreenCaptureKit call is issued: frame zero first, one ask at a time, then every strip capture due in one round, then the windows behind the strip, and a refresh last, once released | a SkyLight call behind four ScreenCaptureKit captures took 204ms against 27; a focus capture is never behind the background |
| Frame zero takes the windows within 2pt of where their tiles start and wholly on the display, the strip in one set and the background in another, and delivers everything in one delivery, even an empty one | the overlay waits for it, so it must always come; in one composite with the strip, a translucent window would bake what is behind it |
| No two overlapping windows are in one call, and windows with different `behind` are never in one call | the crop of the window behind would show the one in front; a material blurs what is composited after it |
| `Landing` reads each window's frame every 1ms, captures it once it is within `slack` (20pt) of its end and wholly on the display, and reports it `Lost` at `until`; `Placed` reads at once | the server rounds each window on its own and an app may settle a point or two off; a window that never lands must not hold anything up |
| A landing whose capture the cache refuses is `Lost` at once | the cache refuses a capture without the blur for a picture with it at the same size, and so every later capture there; what it holds is the picture to fly |
| `Settle::Repainted` keeps capturing every 8ms, 125 times at most once there, until `repaint_settled`, and delivers that capture whatever its band print; out of tries it is `Lost` | a grow or an entrance is not painted at its new size yet; the flight holds for it, so the picture must come or the hold must hear it will not |
| `Change` starts at the window's focus report, captures it where it ends every 15ms, six times at most, and stops at the first band print that differs from what the client was delivered of it as the watch started; with none then, from what it was delivered by the first capture, and with none at all the first capture is delivered as landed and the change judged from it; it spends no try before the window is there, and is done at a capture the cache refuses; its second end, the window focus leaves, is captured in the same rounds and set, listed first, and done the same way from its own baseline with its own tries; the watch is done when both ends are; an end not there yet is looked for alone every 1ms between the rounds, the other still looked at every 15ms, however long a poll's look takes, and once there it joins the next round; the window focus leaves done with no change, out of tries, refused or clipped, is reported `Lost`, and again under the new tag when a retarget carries it on | a focus change shows in the title band; macOS turns the window focus leaves first, and watched alone the target's turn came before it (Kai, 2026-10-05); one round for both costs one call where their kinds and frames allow; a landing looked at first in the same round delivers the focused look of a window focused before it landed, and judged from the latest delivery the change was never seen; with nothing to compare, a first capture cannot be called a change; the cache refuses every later capture of the window at that size too, and the windows behind the strip wait on the watch; a target not there yet when the old window was captured waited the whole 15ms with it, up to 14ms more for its turn and so for the handover; the target's turn waits for the old window's, so the flight must hear when that will not come, and it drops a report under the generation before a merge |
| A focus report is remembered for a second, and a `Change` watch created after it starts armed | the report came before the watch in 136 of 225 flights of the prototype, and 135 of those never captured the focus |
| A `Change` target whose frame is where it ends but cut by the display's edge takes one ScreenCaptureKit capture, at focus priority, through the hold, delivered as its focus change; one already out is followed by its own. A window losing focus cut so is done with no capture | SkyLight captures only what is on a display, and `specs/animation.md` has the window gaining focus recaptured even clipped, the one ScreenCaptureKit capture a flight lets through (`docs/pictures-motion-transitions.md`) |
| `Live` refreshes every 16ms at most, never while a landing is pending or a change watch's target capture is due within 8ms, and only once its `after` watch's target is done, but once at `initial` regardless; every capture the cache took is delivered, whatever its band print | refreshing ahead of the focus capture held it up 31ms; the window losing focus can go on alone for its six tries, 75-90ms, when its band never changes, and one never where it ends is polled for every 1ms to the flight's end, and the background waited for it (inferred from the rule, not measured); content behind the strip changes below the band, which a band print would not see |
| Landing, change and final deliveries carry only band prints that are news to the client; frame zero starts the client's prints afresh, and frame zero asked again under the same tag adds to them | a picture the tile already shows is not swapped again; a window flying the cache's picture this time gets its landing whatever it looked like last time; a transition asks frame zero again for what a pass merged before its show brought on screen |
| A retarget keeps a window landed where it still ends, the picture a repainting window that has not landed settles against (none if it had none), a focus report already seen, and each end of a change watch's baseline, tries and finish while that end keeps its size, and watches the rest from now; a window watched to land somewhere new starts its print afresh, and one that had landed settles there against the picture held now | a merged press moves some ends and not others; until a window lands the cache may hold the watch's own captures, not the picture from before the change, while one that landed is held as it landed, which is what the server's resize shows before the app repaints; a band print is drawn from the window's whole width, so one taken at another size never matches, and the cache refuses a capture only at the size of what it holds; a window new to the watch is watched too, which the prototype missed; what the client was delivered for the end before, it no longer draws |

The constants are measured in `src/animation/docs/capture-overlay-research.md`: "Where a moving focus
change spends its time" (the server's frame, the focused look), "The handover, measured" (the 20pt
landing, the final capture) and "Sequencing a focus change behind the overlay" (queued ScreenCaptureKit
work slowing frame zero); and in [`capture-research.md`](capture-research.md): "What a capture costs
in a press" (frame zero's cost, the kept filter), "SkyLight in parallel" (204ms behind four
ScreenCaptureKit captures against 27 alone) and "What the prototype's journal showed of a focus
change" (the focus report before the watch, the refresh that held up the focus capture).

**Focused looks.** A client says which picture it was delivered shows a window focused
(`keep_focused(tag, window)`): the focus target's once its change was delivered, and the frame-zero
picture of the window losing focus when it was the key window as the press went out. `focused(window)` returns it, so a parked focus target flies its
focused look from its first frame. After a release, never during a hold, a newer capture of the window
whose body differs from it (a 40x40 grid over 0.04-0.96 across and 0.08-0.96 down, more than three
levels apart on average) drops it, and forgetting the window drops it with everything else known of
it.

## The shape that matters

**Usable is not the same as fitting.** A capture is usable when it covers the window it was taken
from; it fits when it can be drawn into a frame without visible stretch. A cached picture of a window
that has since been resized is usable and does not fit, and both checks exist for that case. See "A
capture can be usable and still be the wrong shape" in [`capture-research.md`](capture-research.md).

**The cache never downgrades.** A clipped capture never replaces a usable one, and a capture of a
window on its own never replaces one of the same size that carries its blur. A stand-in is never
cached: cached, the switcher would read it as a picture and the next flight would take it for one.

**Two routes, chosen by visibility.** SkyLight is fresh and cheap but captures only what is on screen;
ScreenCaptureKit captures a window anywhere but costs about 40ms, so it fills the cache in the
background and is never waited on. A refresh takes whichever its window's place allows.

**One SkyLight call, two ways of making it.** `SLSHWCaptureWindowList` with several ids returns one
composite of them, the first listed in front, covering what each has on its display, and calls are
served one at a time (27ms for one, 122ms for four at once). A batch of windows on screen, none
overlapping another, is captured in up to two calls (`capture_on_screen`):

| call | lists | takes | why |
|---|---|---|---|
| alpha | the windows alone | translucent windows, unknown ones, and opaque ones with no material | the window keeps its own alpha, so the overlay draws what is really behind it |
| baked | the opaque windows, then `behind`, then the display's desktop windows | opaque windows with a material, and an opaque window of unknown material after its alpha capture | a material carries the blur of what is composited after it: 0.5 levels from the screen, against 24.9 for the window alone |

A window seen for the first time is measured from its alpha crop: translucent when more than a fifth
of a 40x40 interior grid of a 48x48 draw is below alpha 250; its corner radius from where each row of
its 48pt top-left corner turns opaque (`r = x + y + sqrt(2xy)` at pixel centres, the median of at
least four rows, 4-40pt). An opaque one is baked too, and has a material when the two crops drawn
64x64 differ by more than two levels away from their border. A baked picture of a window with a
material carries its blur (`carries_blur`), so the cache keeps it over a grey capture of the window
alone. Each composite is checked against the shape of what it listed, the desktop windows included
for the baked one, and a misshapen one yields no picture: a frame read is wrong, or a listed window
has gone. The measurements are in "SLSHWCaptureWindowList captures only the visible portion" and
"Translucency is three things" in [`capture-research.md`](capture-research.md).

**One place turns a picture into layer contents.** The flight overlay, the switcher's cards and the
bar all go through `platform/layer.rs`, so a bitmap and a surface are handed over the one way objc2
accepts, and a region as its whole composite with `contentsRect` on its part. A region's rect counts
down from the composite's top, which is what `contentsRect` does in a flipped layer tree, and every
client draws under a flipped `NSView`; in a tree that is not flipped it counts up from the bottom. A
test renders both (`a_layer_shows_a_region_as_its_crop`).

**A region is drawn in a transition and kept as a copy.** A picture cut from a batched composite stays a region of it while
a transition draws it: a crop handed to Core Animation costs the main thread 2-5ms of decode per
quarter window, and a copy of each 14ms per flight ("Where a moving focus change spends its time" in
`src/animation/docs/capture-overlay-research.md`). A region keeps its whole composite alive, so at a
release every region in the cache is replaced by a copy of its own, made on the service's thread, and
only where the cache still holds the picture it was made of; what each client was delivered, which it
names only while its flight is up (`keep_focused`), is dropped there and then rather than copied.

**A window is pictured once, under the id it flies by.** A window rini manages is pictured under its
own id, and the debug slide flies it by that id; one it does not, such as a border tracing a tile,
under a synthetic id, pid 0 (`synthetic_window_id`), and the slide flies it by that (`picture_id`).
A warm, at each change of display and on the debug command, pictures only windows rini does not
manage, told by what the cache holds or owes under a real id, and only those showing at least
`WARM_MIN_SHOWN` (100pt) of themselves on the display each way: a window parked at the corner keeps
its full size and shows a sliver, which the window server counts as on screen. At boot the cache may
hold nothing of a managed window yet, so the warm, or a debug slide, can picture it under a
synthetic id too; at each release the service lets go of every synthetic picture a managed window
also has under its own id, and of every one whose window the window server no longer has, asked by
id wherever the window is (`unwanted`). Why: "The pictures held" in `specs/animation.md`.

## Reading order

`domain/coverage.rs` → `domain/cache.rs` → `platform/picture.rs` → `domain/region.rs` →
`domain/knowledge.rs` → `domain/schedule.rs` → `domain/watch.rs` → `domain/refresh.rs` →
`platform/service.rs` → `platform/skylight.rs` → `platform/measure.rs` →
`platform/screencapturekit.rs`.

## Detail

- [`capture-research.md`](capture-research.md) — every capture route measured: cost, what each
  returns, and why the desktop and the bar are pictured on their own; the hairline no capture
  carries; how stale a picture may be
- [`src/transitions/docs/coordination.md`](../../transitions/docs/coordination.md) — the capture
  work a flight does, and when

## Known debt

**Nothing hints a window placed.** `Hint::Placed` reads a window's frame at once rather than at the
next poll; nothing sends it yet, so a landing is seen at the next 1ms poll.

**A look turned off the display is a capture of the window alone.** A window the window server does
not have wholly on the overlay's display at the look, clipped, parked or on another display, is
refreshed through ScreenCaptureKit, and the cache keeps a picture carrying its blur over a capture of
the same size without it: a window with a material keeps its old look, in the switcher and the next
flight, until a flight next pictures it on screen. The overlay's display is the only one whose frame
and desktop windows the service holds, so a window wholly on another display is refreshed this way
too. The window losing focus that ends a flight clipped or parked is one.

**A refresh blurs only the desktop.** Its baked call lists the window and then the display's desktop
windows, with nothing in `behind`: the service knows nothing of what lies below a window at rest, so a
floating window below a window with a material is missing from that picture's blur until a flight next
pictures it on screen.

**The cache is a registry too.** A border companion may never be a window rini manages, and the engine
learns which those are from `Pictures::windows`, the windows with a picture held or owed. The managed
set belongs to the reactor's world; it comes from there once the coordinator composes flights.

**A transition on a display before its first render lands composites on the `transitions` thread.**
`dressing` takes the desktop, and the first time the bar, synchronously while the service holds none
of that display. The coordinator waits for frame zero's window pictures, not for a desktop: that would be one
more ask at the begin.

**The flight engine takes one capture itself.** A window that has just opened is pictured at its spawn
frame by one framed capture (`capture_via_framed`, 16-24ms, at most `MAX_SYNC_ENTRANCE_CAPTURES` a
pass), taken on the `transitions` thread as the engine composes and handed over through `insert` ("A
window that opens" in `src/transitions/docs/coordination.md`). It is the one picture of a window a
transition draws that it neither asks for nor watches, and the engine waits for it.

**The bar still captures on its own.** Its menu extras and its ground are pictured on the bar's own
thread (`src/bar/platform/menu_extras.rs`), with its own pacing and its own pause for flights.

