# `pictures` — pictures of windows, the desktop and the bar

Every capture rini draws. A flight draws a picture of every window it moves, the desktop behind them
and, under `bar { off; }`, another app's bar above; the switcher draws a picture of every window it
offers; the bar grounds itself on a picture of the wallpaper. This feature takes those pictures,
decides what each is worth, keeps them, and hands them to layers. The plan it is part of is in
`docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **Coverage and fit** | `domain/coverage.rs` — `Coverage`: whether a capture is usable, fits a frame, must be warmed again, spans its display, or is stale |
| **The cache's rules** | `domain/cache.rs` — `PictureCache`: never downgrade a usable picture, keep a blurred one over a grey one of the same size, put a copy back only over the picture it was made of |
| **Regions** | `domain/region.rs` — where a window lies in a batched composite: its pixel rect, whether the composite holds it and has the shape asked for, its unit rect, and a crop piece's rect |
| **What is known of each window** | `domain/knowledge.rs` — per window server id: translucent, with a material, or seen with none only while inactive; the window last reported focused; which SkyLight call each window goes in; the measurements that learn them, over plain bytes |
| **Routes** | `domain/route.rs` — `Route`, which way a picture was taken (SkyLight alone, SkyLight baked, ScreenCaptureKit, the stand-in), and which of them the overlay clips to the tile's corner |
| **Prints** | `domain/print.rs` — whether two pictures of one window show the same thing: band prints for a focus change, bodies for content |
| **The ground** | `domain/ground.rs` — `Ground`, the colour a window's content sits on, with its alpha: the commonest body sample, which a crop-drawn tile fills where its picture ends |
| **The blur** | `domain/translucency.rs` — a window's blur put back into its picture, merging its own capture with a composite of what is below it |
| **The handover check** | `domain/handover.rs` — `HandoverCheck`, a lift to measure, and `compare`: how far the screen after it is from the overlay before it, inside, either side of the outline and over a band outside it |
| **Settled** | `domain/settle.rs` — when a repainting window's capture is its settled rendering, by thumbprint |
| **The stand-in** | `domain/stand_in.rs` — which window with no picture flies as the dark stand-in |
| **Desktop and bar levels** | `domain/backdrop.rs` — which window-server levels are the desktop and which the bar |
| **The port** | `domain/target.rs` — `PictureTarget`, a window to picture and the size it should be |
| **The schedule** | `domain/schedule.rs` — `Schedule`: asks, watches, refreshes, hints, holds, display changes and handover checks in, the next call out; the warm filter, the capture budgets and cadences |
| **The watches** | `domain/watch.rs` — windows on screen captured through SkyLight: frame zero and the final asks, the shared types, the focus hint, the band prints each client was delivered; `Watches` hands each watch to its kind and holds the windows behind the strip back while a landing or change capture is due |
| **A window landing** | `domain/watch/landing.rs` — the `Landing` watch: each window captured once where it ends, delivered at once or once settled, lost at the deadline; one cut by the display's edge settled through ScreenCaptureKit (`Outcome::elsewhere`, `Watches::captured_elsewhere`) |
| **A change of focus** | `domain/watch/change.rs` — the `Change` watch: both ends of a focus change captured until each band print turns, each with its own baseline, tries and `EndState`; a target cut by the display's edge owed one ScreenCaptureKit capture (`Outcome::clipped`) |
| **The windows behind the strip** | `domain/watch/live.rs` — the `Live` watch: refreshed at its pace once the watch it waits for is done, and once at `initial` regardless |
| **Refreshes** | `domain/refresh.rs` — windows pictured afresh at rest once their face has turned: which are due a look, each held until its face settles, and which are owed ScreenCaptureKit instead |
| **The service** | `platform/service.rs` — the `pictures` thread, which runs the schedule through its `Capturer` and holds the cache, each window's focused face and knowledge and the overlay display's pictures; `Pictures`, the handle every client holds |
| **What it tells clients** | `event.rs` — a picture the cache took, the pictures delivered under a tag, a landing or a turn that will not come, and a handover check's overlay picture and measure |
| **A picture** | `platform/picture.rs` — `Picture`: its `Pixels` (bitmap, surface or region), `Coverage`, `Route` and `Ground`, read once on first ask; the one-pixel stand-in; what the cache keeps, never a stand-in; a region kept as a copy of its own |
| **What the thread reads and captures** | `platform/capturer.rs` — `Capturer`, the port the service is generic over, for every read and capture its thread makes; `MacCapturer`, the calls, through the `WindowServer` port from `window_server::live()`; under test `FakeCapturer`, which answers as told and records each call |
| **SkyLight** | `platform/skylight.rs` — every SkyLight and `CGWindowList` capture but the bar's own (`src/bar/platform/menu_extras.rs`): a composite of a window list, the batched capture of windows on screen, a framed capture with its blur, and a handover check's overlay and screen |
| **Measurement** | `platform/measure.rs` — a picture read small in its own format (`sample_format`): band print, body samples, ground, thumbprint (`picture_thumbprint`, `surface_thumbprint`), first-picture translucency and material; and a picture drawn whole for a handover check |
| **ScreenCaptureKit** | `platform/screencapturekit.rs` — one listing of many windows, the capture of one listed window, and the desktop render, each reporting back through a callback |
| **The desktop and the bar, listed** | `platform/desktop.rs` — which windows a desktop or bar picture is composited from, and the rect each set covers |
| **IOSurfaces** | `platform/surface.rs` — `OwnSurface`, a surface written only by the `fill` of the `new_surface` that makes it, so `read_locked` reads it safely; any other surface read through the unsafe `read_unwritten`, a capture's only inside its delivery completion |
| **Bitmaps** | `platform/bitmap.rs` — a fresh context, RGBA or in a format given, an image's copy of its own, and an image's thumbprint |
| **Layers** | `platform/layer.rs` — a picture or a region handed to a layer (`contents_of`, `contentsRect`), and a layer filled with a picture's ground in the space it was read in (`ground_colour`) |

The stretch tolerance a fit is judged by is `rini_geometry::stretch`, shared with `animation`'s "is
this a resize" so the two agree.

Its interface and `domain/` speak rini-geometry's spaces (`crates/rini-geometry/docs/README.md`):
frames, display bounds, sizes, coverage, watches and deliveries in `Screen`, a region's rect and an
image's size in `Pixel`, a layer's `contentsRect` in `Contents`. CG stays in `platform/`.

## The service

One thread, `pictures`, makes every capture but two, one SkyLight call at a time: background
captures, what a transition asks for or watches, every refresh, and a measured lift's two pictures.
The two not yet its are a transition's desktop and bar before its display's first render lands, and
the flight engine's spawn capture of a new window (both Known debt). It is started once, in
`src/main.rs`, with a sink per client; the flight engine, the reactor and the switcher each hold a
`Pictures` handle.

| A client | Through the handle |
|---|---|
| asks | `ask` windows anywhere at a priority, `ask_on_screen` windows on screen under a tag, `refresh` windows whose face has just turned, wherever they are, `forget` a window that is gone, `set_display` for the display the overlay flies on |
| watches | `watch` windows landing, changing or live, `retarget` a watch, `stop` it; `hint` that a window was focused |
| keeps | `keep_focused` the picture delivered under a tag as the window's focused face |
| measures | `check_handover` a lift: the overlay alone as the next SkyLight call, `OverlayTaken` told, the screen `AFTER_LIFT` later, and `Handover`, each window compared on a thread of the check's own |
| holds | `hold` at a transition's begin, before anything is composed, `Settling` at its lift, `Released` at its quiet; the overview `Released` as its zoom out ends, while it is held, and `hold` again as it zooms back in |
| reads | `usable`, `usable_many`, `focused`, `knowledge`, `windows`, `stand_in`, `dressing`, from any thread under one lock |
| hands over | `insert`, for the flight engine's one capture of its own, of a window that has just opened |

**Every call the thread makes goes through its `Capturer`,** so its rounds run in a test against
`FakeCapturer`: a refresh stopped by a press asked mid-round
(`a_refresh_makes_no_call_once_a_transition_is_asked_and_a_watch_round_makes_every_one`), what it
leaves unseen (`a_window_whose_call_a_press_stopped_is_not_seen`), how a set is split into calls
(`overlapping_windows_go_in_calls_of_their_own_and_every_set_in_its_own`), a misshapen call
(`a_misshapen_batch_leaves_the_desktop_windows_to_be_listed_again`) and what a round reads once
(`what_a_round_knows_is_read_once_and_only_once_a_window_is_wanted`). The handle's own `dressing`
composites, on the caller's thread, list windows through the `WindowServer` port the caller hands
it, the flight engine's own.

**Messages are applied in order, reads copy out under a lock.** Asks, refreshes, watches, hints,
holds and forgets are applied on the thread, every one queued before the next call, so the newest
wins; the switcher and the engine read what is held at that instant without a round trip.

**A client draws only a picture the cache took.** Every capture goes into the cache first, and every
sink hears of each one it took (`event.rs`), so a flight and the switcher never draw what the next
flight will not. A refresh's captures are judged against the cache (`takes`) while it polls, and
only the one each window settles on is put. A delivery under a tag carries only pictures the cache
took, each with the server frame it was taken at, and when it was sent, so a client can tell the
capture from its own wait for it; a refused capture is neither delivered nor news. Every sink hears
a delivery, and a client takes its own by the tag's client.

**No capture call is made on ScreenCaptureKit's queues.** They only copy pixels out and post them
back: `CGWindowListCreateImage` is proxied through the same queue and deadlocks, and
`SLSCaptureWindowsContentsToRectWithOptions` reaches the same proxy
(`docs/data/2026-10-08-skylight-lock/callgraph.txt`). A handover check's screen is one SkyLight
capture of every window on a whole display, made on the thread while a transition settles; its two
display-sized pictures are compared on a thread of their own, `handover`, which posts the result
back like a completion, since a press asks its frame zero of the service's thread.

**Window captures are taken at 2x on any display;** ScreenCaptureKit costs per call, not per pixel,
and a 1x picture flies soft on Retina. The desktop and the bar are taken at their display's scale.

**The desktop and the bar belong to their display.** The service keeps one display's pictures, and
a read names its display, so a transition is never dressed with the display left behind; which
display that is, only the schedule records. Until that display's first render lands, a transition
composites its desktop on the caller's thread, or it would draw black. The render starts only at the
quiet after a flight, so a burst of transitions on a newly entered display composites the desktop on
every one, and an empty or wrong-sized render is asked again at the next quiet. The bar is
composited once per display, kept, and read for presence on every transition, so a bar that has
gone is not drawn.

**SkyLight reads the framebuffer,** so a bar read while the overlay goes up has the overlay in it.
The handle counts every `hold(Transition)` as it is asked, and the thread drops a bar picture that a
transition began during or before.

## The schedule

`domain/schedule.rs` is pure: what goes in carries `now`, and nothing reads a clock. `next(now)` says
what to call next: picture the overlay or the screen for a handover check, list windows, capture one
listed window, render the desktop, picture the bar, wait until a time, or nothing. Each rule in full,
and why: "The schedule" in [`capture-research.md`](capture-research.md), under the same id.

| | Rule |
|---|---|
| S1 | While a transition holds the service, only the ScreenCaptureKit captures it waits for start, and, with the handover measured, the screen a check takes as it settles |
| S2 | A handover check's overlay picture is the next call; its screen follows once, `AFTER_LIFT` later, and a transition beginning drops the check |
| S3 | What waited starts at the release, most urgent first: repaint, focus, background, warm |
| S4 | A background or warm ask, and a refresh's next look, wait `SETTLE_BEFORE_STARTING` first |
| S5 | A refresh is looked at only while nothing holds the service, every window due in one round of as few SkyLight calls as kinds and overlaps allow; a window not wholly on the display is owed ScreenCaptureKit |
| S6 | A hold cancels every refresh, and the release asks again for each window not yet pictured `REPAINT_SETTLED` after its face turned |
| S7 | A refresh polls every `CHANGE_EVERY`, `CHANGE_TRIES` times at most, puts nothing while it polls, and puts each window once, with the capture its face settled on |
| S8 | A capture listed and unable to start through a hold, or asked again at another size, is listed again at the release |
| S9 | A warm is dropped where the picture held fits and is fresh (`wants_warm`) |
| S10 | One listing serves every window that may start, with at most `MAX_CONCURRENT` captures out |
| S11 | A window owed a capture is not asked twice, and a forgotten one is captured no further |
| S12 | A display change renders the desktop for the new display and refuses the old one's render |
| S13 | Window captures carry on through a display change |
| S14 | The desktop is rendered again at a release when missing or stale |
| S15 | The bar is pictured `BAR_REFRESH_DELAY` after the overlay lifts or the display changes, never while it is up |

The costs are measured in [`capture-research.md`](capture-research.md): "Capture cost", and the
listing against a kept filter in "What a capture costs in a press".

## The watches

`domain/watch.rs` is the SkyLight half of the schedule a transition drives: windows on screen,
captured at the frames the window server has them at (`SLSGetWindowBounds`: a parked window's
written frame is there within 0.1ms of the write returning; an app still activating applies a write
late) and delivered under the
asker's `Tag`, a client and its generation. Each kind of watch is a file under `domain/watch/`, its
tests beside it. None of it waits for a release; a refresh at rest is cancelled by one and asked
again at the release (`domain/refresh.rs`). Each rule in full, and why: "The watches" in
[`capture-research.md`](capture-research.md), under the same id.

| | Rule |
|---|---|
| W1 | SkyLight work goes before any ScreenCaptureKit call: frame zero, then the strip's captures in one round, then the windows behind the strip, then a refresh once released |
| W2 | Frame zero takes the windows within 2pt of where their tiles start and wholly on the display, strip and background apart, in one delivery that always comes |
| W3 | No call holds two overlapping windows, or windows with different `behind` |
| W4 | `Landing` captures a window once it is within `slack` of its end and wholly on the display, and reports it `Lost` at `until` |
| W5 | A landing whose capture the cache refuses is `Lost` at once |
| W6 | `Settle::Repainted` captures until two in a row match and delivers that one, or reports `Lost` out of tries |
| W7 | `Change` captures both ends of a focus change where each ends, from the focus report, until each one's band print turns, the window losing focus listed first |
| W8 | A `Settle::Repainted` window cut by the display's edge is settled through ScreenCaptureKit instead (`Outcome::elsewhere`) |
| W9 | A focus report is remembered for a second, so a `Change` watch created after it starts armed |
| W10 | A `Change` target cut by the display's edge takes one ScreenCaptureKit capture; a window losing focus cut so takes none |
| W11 | `Live` refreshes the windows behind the strip only once its `after` watch's target is done, and once at `initial` regardless |
| W12 | Deliveries carry only band prints that are news to the client; frame zero starts them afresh |
| W13 | A retarget keeps what still holds for each end that keeps its size, and watches the rest from now |

The constants are measured in `src/animation/docs/capture-overlay-research.md` ("Where a moving
focus change spends its time", "The handover, measured", "Sequencing a focus change behind the
overlay") and in [`capture-research.md`](capture-research.md) ("What a capture costs in a press",
"SkyLight in parallel", "What the prototype's journal showed of a focus change").

**Focused faces.** A client says which delivered picture shows a window focused (`keep_focused`),
and `focused(window)` returns it, so a parked focus target flies its focused face from its first
frame. A newer capture whose body differs drops it, as "A focused face, kept and dropped" in
[`capture-research.md`](capture-research.md) gives.

## The shape that matters

**Usable is not the same as fitting.** A capture is usable when it covers its window; it fits when
it can be drawn into a frame without visible stretch. A cached picture of a resized window is
usable and does not fit ("A capture can be usable and still be the wrong shape" in
[`capture-research.md`](capture-research.md)).

**The cache never downgrades.** A clipped capture never replaces a usable one, and a capture of a
window alone never replaces one of the same size carrying its blur. A stand-in is never cached, or
the switcher and the next flight would take it for a picture.

**A picture's ground is read only when a crop-drawn tile asks for it, and in the picture's own
format.**
`Picture::ground` reads it off the body samples the first time, on the asking thread, the overlay's
main thread, and every copy shares that read; a stand-in's is its own colour, and an unreadable
picture takes the stand-in's. Each crop-drawn tile asks, since each new picture brings a ground not
yet read: a resize's, a navigation's flying a stale picture that does not fit, at the show and at
each swap, and a two-step leg's repaint as its grid is cut onto it at the corner (`cut_onto` in
`src/animation/platform/overlay.rs`, `a_corner_cut_returns_how_long_its_repaints_ground_took_to_read`);
the drain counts the read in its `crops` (`a_tree_counts_the_ground_a_corner_cut_reads`). The format rules and their cost: "A
picture's ground, read in its own format" in [`capture-research.md`](capture-research.md).

**Two routes, chosen by visibility.** SkyLight is fresh and cheap but captures only what is on
screen; ScreenCaptureKit captures a window anywhere but costs about 40ms, so it fills the cache in
the background and is never waited on. A refresh takes whichever its window's place allows.

**One SkyLight call, two ways of making it.** `SLSHWCaptureWindowList` returns one composite of the
windows it lists, the first in front, covering what each has on its display, served one call at a
time (27ms for one, 122ms for four at once). It is made on rini's read connection, where it costs
what it does on the main one and waits on no commit of rini's own windows
(`src/windows/docs/window-server-reads.md`). A batch of windows on screen, none overlapping, is
captured in up to two calls (`capture_on_screen`):

| call | lists | takes | why |
|---|---|---|---|
| alpha | the windows alone | translucent windows, unknown ones, and opaque ones with no material | the window keeps its own alpha, so the overlay draws what is really behind it |
| baked | the opaque windows, then `behind`, then the display's desktop windows | opaque windows with a material, and an opaque window of unknown material after its alpha capture | a material carries the blur of what is composited after it: 0.5 levels from the screen, against 24.9 for the window alone |

A window is measured from its first alpha crop and, if opaque, its baked one; a material found is
kept until the window goes, and none found while it is inactive is no answer. A misshapen composite
yields no picture. The thresholds and the windows they were set against: "A window's translucency
and material, learned" in [`capture-research.md`](capture-research.md).

**One place turns a picture into layer contents.** The flight overlay, the switcher's cards and the
bar all go through `platform/layer.rs`: a bitmap and a surface the one way objc2 accepts, a region as
its whole composite with `contentsRect` on its part. A region's rect counts down from the top, as
`contentsRect` does in a flipped tree, which every client draws under; in a tree that is not
flipped it counts up from the bottom (`a_layer_shows_a_region_as_its_crop`).

**A region is drawn in a transition and kept as a copy.** A crop handed to Core Animation costs the
main thread 2-5ms of decode per quarter window, and a copy of each 14ms per flight ("Where a moving
focus change spends its time" in `src/animation/docs/capture-overlay-research.md`), so a flight draws
regions. A region keeps its whole composite alive, so at a release every region in the cache is
replaced by a copy made on the service's thread, only where the cache still holds the picture it
was made of; what each client was delivered, which it names only while its flight is up
(`keep_focused`), is dropped rather than copied.

**A window is pictured once, under the id it flies by.** Every flight and the switcher draw a managed
window by its own id. Why: "The pictures held" in `specs/animation.md`.

## Reading order

`domain/coverage.rs` → `domain/cache.rs` → `domain/route.rs` → `platform/picture.rs` →
`domain/region.rs` → `domain/knowledge.rs` → `domain/schedule.rs` → `domain/watch.rs` →
`domain/refresh.rs` → `platform/service.rs` → `platform/capturer.rs` → `platform/skylight.rs` →
`platform/measure.rs` → `platform/screencapturekit.rs`.

## Detail

- [`capture-research.md`](capture-research.md) — every capture route measured: cost, what each
  returns, and why the desktop and the bar are pictured on their own; the hairline no capture
  carries; how stale a picture may be; each rule of the schedule and the watches in full
- [`src/transitions/docs/coordination.md`](../../transitions/docs/coordination.md) — the capture
  work a flight does, and when

## Known debt

**A face turned off the display is a capture of the window alone.** A window not wholly on the
overlay's display at the look, clipped, parked or on another display, is refreshed through
ScreenCaptureKit, and the cache keeps its blurred picture over that grey one of the same size: a
window with a material keeps its old face, in the switcher and the next flight, until a flight next
pictures it on screen. The overlay's display is the only one whose frame and desktop windows the
service holds, so a window wholly on another display is refreshed this way too. The window losing
focus that ends a flight clipped or parked is one.

**A refresh blurs only the desktop.** Its baked call lists the window, then the display's desktop
windows, with nothing in `behind`, since the service knows nothing of what lies below a window at
rest: a floating window below a window with a material is missing from
that picture's blur until a flight next pictures it on screen.

**A transition on a display before its first render lands composites on the `transitions` thread.**
`dressing` takes the desktop, and the first time the bar, synchronously while the service holds
none of that display. The coordinator waits for
frame zero's window pictures, not for a desktop: that would be one more ask at the begin.

**The flight engine takes one capture itself.** A window that has just opened is pictured at its
spawn frame by one framed capture (`capture_via_framed`, 16-24ms, at most
`MAX_SYNC_ENTRANCE_CAPTURES` a pass) on the `transitions` thread as the engine composes, handed over
through `insert` ("A window that opens" in `src/transitions/docs/coordination.md`). It is the one
picture a transition neither asks for nor watches, and the engine waits for it.

**The bar still captures on its own.** Its menu extras and its ground are pictured on the bar's own
thread (`src/bar/platform/menu_extras.rs`), with its own pacing and its own pause for flights.
