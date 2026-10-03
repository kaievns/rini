# `pictures` — pictures of windows, the desktop and the bar

Every capture rini draws. A flight draws a picture of every window it moves, the desktop behind them
and, under `bar { off; }`, another app's bar above; the switcher draws a picture of every window it
offers; the bar grounds itself on a picture of the wallpaper. This feature takes those pictures,
decides what each is worth, and hands them to layers. The plan it is part of, and the steps still to
come, are in `docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **Coverage and fit** | `domain/coverage.rs` — `Coverage`, whether a capture is usable, whether it fits a frame, whether a grow outgrows it, whether a warm must recapture, whether a desktop picture spans its display, when a picture is stale |
| **The cache's rules** | `domain/cache.rs` — `PictureCache`: never downgrade a usable picture, keep a blurred one over a grey one of the same size, carry the hairline over |
| **The blur** | `domain/translucency.rs` — putting a window's blur back into its picture, merging its own capture with a composite of what is below it |
| **Settled** | `domain/settle.rs` — when a chase's capture is the window's settled rendering, by thumbprint |
| **The stand-in** | `domain/stand_in.rs` — which window with no picture flies as the dark stand-in |
| **Desktop and bar levels** | `domain/backdrop.rs` — which window-server levels are the desktop and which the bar |
| **The port** | `domain/target.rs` — `PictureTarget`, a window to picture and the size it should be |
| **The schedule** | `domain/schedule.rs` — which capture work each phase of a flight permits, the parking of warms, and the capture budgets and cadences |
| **A picture** | `platform/picture.rs` — `Picture`: its `Pixels` (a bitmap or a surface), its `Coverage` and its `Route`; the stand-in and its nine-slice image; what the cache keeps of a picture and that it never keeps a stand-in |
| **SkyLight** | `platform/skylight.rs` — every SkyLight and `CGWindowList` capture: one composite of a list of windows (the desktop backdrop, another app's bar), and the framed capture of one window with its hairline and, wholly on a display, its blur |
| **ScreenCaptureKit** | `platform/screencapturekit.rs` — `ScreenCaptureKitQueue`: window captures at any visibility and the desktop render, in the background, at most four at once |
| **The desktop and the bar, listed** | `platform/desktop.rs` — which windows a desktop or bar picture is composited from |
| **The hairline** | `platform/edge_dressing.rs` — the window's hairline, cut out of a framed capture, and where on a tile it is worn |
| **Bitmaps** | `platform/bitmap.rs` — a fresh RGBA context, and an image's thumbprint |
| **Layers** | `platform/layer.rs` — handing a picture to a layer, and the stand-in's nine-slice region |

The stretch tolerance a fit is judged by is `rini_geometry::stretch`, shared with `animation`'s "is
this a resize" so the two agree.

## The shape that matters

**Usable is not the same as fitting.** A capture is usable when it covers the window it was taken
from; it fits when it can be drawn into a frame without visible stretch. A cached picture of a window
that has since been resized is usable and does not fit, and both checks exist for that case. See "A
capture can be usable and still be the wrong shape" in
[`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md).

**The cache never downgrades.** A clipped capture never replaces a usable one, and a capture of a
window on its own never replaces one of the same size that carries its blur. A stand-in is never
cached: cached, it would be lent to the switcher as a picture and taken for one by the next flight.

**Two routes, chosen by visibility.** SkyLight is fresh and cheap but captures only what is on screen;
ScreenCaptureKit captures a window anywhere but costs about 40ms, so it fills the cache in the
background and is never waited on. No capture call is made inside a ScreenCaptureKit completion:
`CGWindowListCreateImage` is proxied through the same delivery queue and deadlocks.

**One place turns a picture into layer contents.** The flight overlay, the switcher's cards and the
bar all go through `platform/layer.rs`, so a bitmap and a surface are handed over the one way objc2
accepts.

## Reading order

`domain/coverage.rs` → `domain/cache.rs` → `platform/picture.rs` → `platform/skylight.rs` →
`platform/screencapturekit.rs` → `domain/schedule.rs`.

## Detail

- [`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md)
  — every capture route measured: cost, what each returns, and why the desktop and the bar are
  pictured on their own
- [`src/animation/docs/animation-smoothness.md`](../../animation/docs/animation-smoothness.md) — the
  capture work a flight does, and when

## Known debt

**The flight engine still holds the instances.** `src/animation/platform/engine.rs` owns the
`PictureCache` and the `ScreenCaptureKitQueue`, decides when to warm, chase, refresh and harvest, and
lends the switcher its pictures through a callback. The pictures service and its thread are step 2 of
the plan.

**The hairline harvest lives here until it is deleted.** `platform/edge_dressing.rs`, the ring the
framed capture takes for it, and the dressing a `Picture` carries go when step 4 replaces the harvest
with a constant ring the renderer draws; `boundary_layout`, `RING_PT` and the corner radius move to
`animation` then. They are here rather than in `animation` because the framed capture harvests the
ring and `Picture` carries it, and either would have had `pictures` name `animation`.

**The bar still captures on its own.** Its menu extras and its ground are pictured on the bar's own
thread (`src/bar/platform/menu_extras.rs`), with its own pacing and its own pause for flights.
