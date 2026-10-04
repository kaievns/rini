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
| **The schedule** | `domain/schedule.rs` — `Schedule`: the asks, holds and display changes in, the next call out; the warm filter, and the capture budgets and cadences |
| **The service** | `platform/service.rs` — the `pictures` thread that runs the schedule, the cache and the pictures of the display the overlay flies on, and `Pictures`, the handle every client holds |
| **What it tells clients** | `event.rs` — a picture the cache took, or a covering capture it refused for the blurred one it holds |
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

One thread, `pictures`, makes every background capture of a window, the desktop and the bar. It is
started once, in `src/main.rs`, with a sink per client; the flight engine, the reactor and the switcher
each hold a `Pictures` handle.

| A client | Through the handle |
|---|---|
| asks | `ask` windows at a priority, `forget` a window that is gone, `set_display` for the display the overlay flies on, `warm_visible` every window on it |
| holds | `hold` at a transition's compose, `Settling` at its lift, `Released` at its quiet |
| reads | `usable`, `usable_many`, `get`, `windows`, `wants_warm`, `stand_in`, `dressing`, from any thread under one lock |
| hands over | `insert` and `set_dressing`, for what the flight engine's own captures take in flight |

Asks, holds and forgets are messages, applied in order on the thread. Reads are not: they lock the
shared state and copy out, so the switcher draws without a round trip and the engine composes frame
zero with what is held at that instant. Every capture that comes back goes into the cache first, and
every sink hears of each one (`event.rs`): a client draws only a picture the cache took, so a flight and
the switcher never draw what the next flight will not.

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

The costs are measured in `src/animation/docs/capture-overlay-research.md`: "Capture cost", and the
listing against a kept filter in "Sequencing a focus change behind the overlay".

The flight engine's own captures in flight, the reveal chase, the focus refresh's framed captures, the
spawn capture and the hairline harvest, are not scheduled here. It takes them on its own threads and
hands the pictures over through the handle. The focus refresh asks for a clipped end at focus priority.

## The shape that matters

**Usable is not the same as fitting.** A capture is usable when it covers the window it was taken
from; it fits when it can be drawn into a frame without visible stretch. A cached picture of a window
that has since been resized is usable and does not fit, and both checks exist for that case. See "A
capture can be usable and still be the wrong shape" in
[`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md).

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
"Translucency is three things" in `src/animation/docs/capture-overlay-research.md`.

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

`domain/coverage.rs` → `domain/cache.rs` → `platform/picture.rs` → `domain/schedule.rs` →
`platform/service.rs` → `platform/screencapturekit.rs` → `platform/skylight.rs`.

## Detail

- [`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md)
  — every capture route measured: cost, what each returns, and why the desktop and the bar are
  pictured on their own
- [`src/animation/docs/animation-smoothness.md`](../../animation/docs/animation-smoothness.md) — the
  capture work a flight does, and when

## Known debt

**The flight engine still takes its in-flight pictures itself.** The reveal chase, the focus refresh,
the spawn capture and the hairline harvest run on the engine's own threads or on the main thread, and
hand their pictures over through the handle. Step 4 of the plan replaces them with the end-state
captures the coordinator asks the service for.

**The cache is a registry too.** A border companion may never be a window rini manages, and the engine
learns which those are from `Pictures::windows`, the windows with a picture held or owed. The managed
set belongs to the reactor's world; it comes from there once the coordinator composes flights.

**The first transition on a display composites on the main thread.** `dressing` takes the desktop and
the bar synchronously before the service holds any of that display. Step 4's coordinator waits for
frame-zero pictures instead.

**The hairline harvest lives here until it is deleted.** `platform/edge_dressing.rs`, the ring the
framed capture takes for it, and the dressing a `Picture` carries go when step 4 replaces the harvest
with a constant ring the renderer draws; `boundary_layout`, `RING_PT` and the corner radius move to
`animation` then. They are here rather than in `animation` because the framed capture harvests the
ring and `Picture` carries it, and either would have had `pictures` name `animation`.

**The bar still captures on its own.** Its menu extras and its ground are pictured on the bar's own
thread (`src/bar/platform/menu_extras.rs`), with its own pacing and its own pause for flights.

**The capture findings are still in animation's research doc.** `src/animation/docs/capture-overlay-research.md`
holds both what captures cost and return and how the overlay draws; its capture sections move here when
step 3 splits the animation docs by owner.
