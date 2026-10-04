# `pictures` — pictures of windows, the desktop and the bar

Every capture rini draws. A flight draws a picture of every window it moves, the desktop behind them
and, under `bar { off; }`, another app's bar above; the switcher draws a picture of every window it
offers; the bar grounds itself on a picture of the wallpaper. This feature takes those pictures,
decides what each is worth, keeps them, and hands them to layers. The plan it is part of, and the
steps still to come, are in `docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **Coverage and fit** | `domain/coverage.rs` — `Coverage`, whether a capture is usable, whether it fits a frame, whether a grow outgrows it, whether a warm must recapture, whether a desktop picture spans its display, when a picture is stale |
| **The cache's rules** | `domain/cache.rs` — `PictureCache`: never downgrade a usable picture, keep a blurred one over a grey one of the same size, carry the hairline over, put a copy back only over the picture it was made of |
| **Regions** | `domain/region.rs` — where a window lies in a batched composite: its pixel rect, whether the composite holds it, whether the composite is the shape of what it was asked for, its unit rect, and a crop piece's rect within a region |
| **What is known of each window** | `domain/knowledge.rs` — per window server id, whether it is translucent, whether it has a material, its corner radius; which SkyLight call each window goes in; the measurements that learn them, over plain bytes |
| **Prints** | `domain/print.rs` — whether two pictures of one window show the same thing: their band prints, where a focus change shows, and their bodies, where content does |
| **The blur** | `domain/translucency.rs` — putting a window's blur back into its picture, merging its own capture with a composite of what is below it |
| **Settled** | `domain/settle.rs` — when a chase's capture is the window's settled rendering, by thumbprint |
| **The stand-in** | `domain/stand_in.rs` — which window with no picture flies as the dark stand-in |
| **Desktop and bar levels** | `domain/backdrop.rs` — which window-server levels are the desktop and which the bar |
| **The port** | `domain/target.rs` — `PictureTarget`, a window to picture and the size it should be, and the id a window rini does not manage is pictured under |
| **The schedule** | `domain/schedule.rs` — `Schedule`: the asks, watches, hints, holds and display changes in, the next call out; the warm filter, and the capture budgets and cadences |
| **The watches** | `domain/watch.rs` — windows on screen captured through SkyLight: the frame-zero and final asks, the `Landing`, `Change` and `Live` watches, the focus and placement hints, and the band prints each client was last delivered, which decide what is news |
| **The service** | `platform/service.rs` — the `pictures` thread that runs the schedule and makes every SkyLight call, the cache, each window's focused look and knowledge, the pictures of the display the overlay flies on, and `Pictures`, the handle every client holds |
| **What it tells clients** | `event.rs` — a picture the cache took, a covering capture it refused for the blurred one it holds, the pictures a client asked for or watched under its tag, and a window that never landed |
| **A picture** | `platform/picture.rs` — `Picture`: its `Pixels` (a bitmap, a surface, or a region of a batched composite), its `Coverage` and its `Route`; the stand-in and its nine-slice image; what the cache keeps of a picture and that it never keeps a stand-in; a region kept as a copy of its own |
| **SkyLight** | `platform/skylight.rs` — every SkyLight and `CGWindowList` capture but the bar's own (its menu extras and ground, `src/bar/platform/menu_extras.rs`): one composite of a list of windows (the desktop backdrop, another app's bar), the batched capture of the windows on screen as regions of the calls it makes, and the framed capture of one window with its hairline and, wholly on a display, its blur |
| **Measurement** | `platform/measure.rs` — a picture read small: its band print and its body samples, what a window's first picture says of its translucency and corner, and whether its baked picture shows a material |
| **ScreenCaptureKit** | `platform/screencapturekit.rs` — one listing of many windows, the capture of one listed window, and the desktop render; each reports back through a callback |
| **The desktop and the bar, listed** | `platform/desktop.rs` — which windows a desktop or bar picture is composited from, and the rect each set covers |
| **The hairline** | `platform/edge_dressing.rs` — the window's hairline, cut out of a framed capture, and where on a tile it is worn |
| **Bitmaps** | `platform/bitmap.rs` — a fresh RGBA context, an image's copy of its own, and an image's thumbprint |
| **Layers** | `platform/layer.rs` — handing a picture to a layer, a region through `contentsRect`, and the stand-in's nine-slice region |

The stretch tolerance a fit is judged by is `rini_geometry::stretch`, shared with `animation`'s "is
this a resize" so the two agree.

## The service

One thread, `pictures`, makes every background capture of a window, the desktop and the bar, and
every SkyLight capture of windows on screen, one call at a time. It is started once, in `src/main.rs`,
with a sink per client; the flight engine, the reactor and the switcher each hold a `Pictures` handle.

| A client | Through the handle |
|---|---|
| asks | `ask` windows anywhere at a priority, `ask_on_screen` windows on screen under a tag, `forget` a window that is gone, `set_display` for the display the overlay flies on, `warm_visible` every window on it |
| watches | `watch` windows landing, changing or live, `retarget` a watch, `stop` it; `hint` that a window was focused or placed |
| keeps | `keep_focused` the picture delivered under a tag as the window's focused look |
| holds | `hold` at a transition's compose, `Settling` at its lift, `Released` at its quiet |
| reads | `usable`, `usable_many`, `get`, `focused`, `knowledge`, `windows`, `wants_warm`, `stand_in`, `dressing`, from any thread under one lock |
| hands over | `insert` and `set_dressing`, for what the flight engine's own captures take in flight |

Asks, watches, hints, holds and forgets are messages, applied in order on the thread, every one already
queued before the next call is made, so the newest wins. Reads are not: they lock the shared state and
copy out, so the switcher draws without a round trip and the engine composes frame zero with what is
held at that instant. Every capture that comes back goes into the cache first, and every sink hears of
each one (`event.rs`): a client draws only a picture the cache took, so a flight and the switcher never
draw what the next flight will not. The watches are told which pictures the cache took before they
decide anything: a delivery under a tag carries only those, each with the server frame it was taken
at, and a capture the cache refused is neither delivered nor news. Every sink hears a delivery, and a
client takes its own by the tag's client.

ScreenCaptureKit is asked from the thread and completes on its own queues, which only copy the pixels
out and post them back. No capture call is made there: `CGWindowListCreateImage` is proxied through the
same delivery queue and deadlocks.

Window captures are taken at 2x on any display: ScreenCaptureKit costs per call, not per pixel, and a
picture taken at 1x flies soft on a Retina display. What is read off a display's framebuffer, the
desktop and the bar, is taken at that display's own scale.

**The desktop and the bar belong to their display.** The service keeps one display's pictures, and a
read names the display it is for, so a transition can never be dressed with the display left behind;
which display that is, only the schedule records. A transition is dressed with a desktop composited on
the caller's thread until the first render of its display lands, which is what rini always did:
without one it draws black, or the other display's wallpaper at its own size. That render starts only
once the service is released, at the quiet after a flight, so a burst of transitions on a newly
entered display composites the desktop on every one, and a render that comes back empty or the wrong
size is asked again at the next quiet. The bar is composited once per display and kept, and read for
presence on every transition, so a bar that has gone is not drawn.

**SkyLight reads the framebuffer.** A bar read while the overlay goes up has the overlay in it. The
handle counts every `hold(Transition)` as it is asked, ahead of the message, and the thread drops a bar
picture that a transition began during or before.

## The schedule

`domain/schedule.rs` is pure: what goes in carries `now`, and nothing reads a clock. `next(now)` says
what to call next: list windows, capture one listed window, render the desktop, picture the bar, wait
until a time, or nothing.

| Rule | Why |
|---|---|
| While a transition holds the service, only frame-zero and focus captures start | ScreenCaptureKit work queued at a focus change slows every step of it; what the transition waits for is the exception |
| What waited starts at the release, most urgent first: frame zero, focus, background, warm | a capture of a focus end is never queued behind warms |
| A background or warm ask settles `SETTLE_BEFORE_LISTING` before it is listed | a pass's warm is sent just after the flight it starts, and usually reaches the service before that flight's hold |
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

The flight engine's own captures in flight, the reveal chase, the focus refresh's framed captures, the
spawn capture and the hairline harvest, are not scheduled here. It takes them on its own threads and
hands the pictures over through the handle. The focus refresh asks for a clipped end at focus priority.

## The watches

`domain/watch.rs` is the SkyLight half of the schedule: windows on screen, captured at the frames the
window server has them at (`SLSGetWindowBounds`, which has a written frame within 0.1ms of the write
returning) and delivered under the asker's `Tag`, a client and its generation. None of it waits for a
release: it is what a transition waits on.

| Rule | Why |
|---|---|
| SkyLight work goes before any ScreenCaptureKit call is issued: frame zero first, one ask at a time, then every strip capture due in one round, then the windows behind the strip | a SkyLight call behind four ScreenCaptureKit captures took 204ms against 27; a focus capture is never behind the background |
| Frame zero takes the windows within 2pt of where their tiles start and wholly on the display, the strip in one set and the background in another, and delivers everything in one delivery, even an empty one | the overlay waits for it, so it must always come; in one composite with the strip, a translucent window would bake what is behind it |
| No two overlapping windows are in one call, and windows with different `behind` are never in one call | the crop of the window behind would show the one in front; a material blurs what is composited after it |
| `Landing` reads each window's frame every 1ms, captures it once it is within `slack` (20pt) of its end and wholly on the display, and reports it `Lost` at `until`; `Placed` reads at once | the server rounds each window on its own and an app may settle a point or two off; a window that never lands must not hold anything up |
| A landing whose capture the cache refuses is `Lost` at once | the cache refuses a capture without the blur for a picture with it at the same size, and so every later capture there; what it holds is the picture to fly |
| `Settle::Repainted` keeps capturing every 8ms, 125 times at most once there, until `chase_settled`, and delivers that capture whatever its band print; out of tries it is `Lost` | a grow or an entrance is not painted at its new size yet; the flight holds for it, so the picture must come or the hold must hear it will not |
| `Change` starts at the window's focus report, captures it where it ends every 15ms, six times at most, and stops at the first band print that differs from the last picture delivered of it; with none delivered, the first capture is delivered as landed and the change judged from it; it spends no try before the window is there, and one on a capture the cache refuses | a focus change shows in the title band; with nothing to compare, a first capture cannot be called a change |
| A focus report is remembered for a second, and a `Change` watch created after it starts armed | the report came before the watch in 136 of 225 flights of the prototype, and 135 of those never captured the focus (D1) |
| A `Change` window whose frame is where it ends but cut by the display's edge takes one ScreenCaptureKit capture, at focus priority, through the hold, delivered as its focus change; one already out is followed by its own | SkyLight captures only what is on a display, and `specs/animation.md` has both ends of a focus change recaptured even clipped (`docs/pictures-motion-transitions.md`) |
| `Live` refreshes every 16ms at most, never while a landing is pending or a change capture is due within 8ms, and only once its `after` watch is done, but once at `initial` regardless; every capture is delivered | refreshing ahead of the focus capture held it up 31ms; content behind the strip changes below the band, which a band print would not see |
| Landing, change and final deliveries carry only band prints that are news to the client; frame zero starts the client's prints afresh | a picture the tile already shows is not swapped again; a window flying the cache's picture this time gets its landing whatever it looked like last time |
| A retarget keeps a window landed where it still ends, the picture a repainting window settles against, and a focus already seen, and watches the rest from now | a merged press moves some ends and not others; by then the cache holds the watch's own captures, not the picture from before the change; a window new to the watch is watched too, which the prototype missed |

The constants are measured in `src/animation/docs/capture-overlay-research.md`: "Where a moving focus
change spends its time" (the server's frame, the focused look), "The handover, measured" (the 20pt
landing, the final capture) and "Sequencing a focus change behind the overlay" (SkyLight behind
ScreenCaptureKit); and in [`capture-research.md`](capture-research.md), "What a capture costs in a
press" (frame zero's cost, the kept filter).

**Focused looks.** A client says which picture it was delivered shows a window focused
(`keep_focused(tag, window)`): the focus target's once its change was delivered, and the frame-zero
picture of the window losing focus. `focused(window)` returns it, so a parked focus target flies its
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
background and is never waited on.

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
only where the cache still holds the picture it was made of.

## Reading order

`domain/coverage.rs` → `domain/cache.rs` → `platform/picture.rs` → `domain/region.rs` →
`domain/knowledge.rs` → `domain/schedule.rs` → `domain/watch.rs` → `platform/service.rs` →
`platform/skylight.rs` → `platform/measure.rs` → `platform/screencapturekit.rs`.

## Detail

- [`capture-research.md`](capture-research.md) — every capture route measured: cost, what each
  returns, and why the desktop and the bar are pictured on their own; the hairline harvest; how
  stale a picture may be
- [`src/transitions/docs/coordination.md`](../../transitions/docs/coordination.md) — the capture
  work a flight does, and when

## Known debt

**The flight engine still takes its in-flight pictures itself.** The reveal chase, the focus refresh,
the spawn capture and the hairline harvest run on the engine's own threads or on the main thread, and
hand their pictures over through the handle. Step 4 of the plan replaces them with the end-state
captures the coordinator asks the service for.

**Nothing asks for windows on screen or watches them yet.** The frame-zero and final asks, the
watches, the hints and the focused looks are exercised by their tests only, until the coordinator is
wired to them (`docs/pictures-motion-transitions.md`, step 4). Until then the engine ignores the
deliveries and the lost windows every sink hears.

**ScreenCaptureKit's frame-zero and focus asks still start through a hold.** The engine asks for the
pictures a flight has none of at frame zero, and the reactor for both ends of a focus change, through
ScreenCaptureKit. Once the coordinator asks frame zero of SkyLight and the reactor's focus change
becomes a `Focused` hint, the clipped focus end is the only ScreenCaptureKit capture a hold lets
through.

**The cache is a registry too.** A border companion may never be a window rini manages, and the engine
learns which those are from `Pictures::windows`, the windows with a picture held or owed. The managed
set belongs to the reactor's world; it comes from there once the coordinator composes flights.

**A transition on a display before its first render lands composites on the main thread.** `dressing`
takes the desktop, and the first time the bar, synchronously while the service holds none of that
display. Step 4's coordinator waits for frame-zero pictures instead.

**The hairline harvest lives here until it is deleted.** `platform/edge_dressing.rs`, the ring the
framed capture takes for it, and the dressing a `Picture` carries go when step 4 replaces the harvest
with a constant ring the renderer draws; `boundary_layout`, `RING_PT` and the corner radius move to
`animation` then. They are here rather than in `animation` because the framed capture harvests the
ring and `Picture` carries it, and either would have had `pictures` name `animation`.

**The bar still captures on its own.** Its menu extras and its ground are pictured on the bar's own
thread (`src/bar/platform/menu_extras.rs`), with its own pacing and its own pause for flights.

