# `pictures` — pictures of windows, the desktop and the bar

What a picture is worth and when one may be taken. A flight draws a picture of every window it moves,
the desktop behind them and the bar above; the switcher draws a picture of every window it offers.
This feature decides whether a picture covers its window, whether it fits the frame it is drawn
into, what the cache keeps, and which capture work a running flight permits. The plan it is part of,
and the steps still to come, are in `docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **Coverage and fit** | `domain/coverage.rs` — `Coverage`, whether a capture is usable, whether it fits a frame, whether a grow outgrows it, whether a warm must recapture, whether a desktop picture spans its display, when a picture is stale |
| **The cache** | `domain/cache.rs` — `PictureCache` and its rules: never downgrade a usable picture, keep a blurred one over a grey one of the same size, never cache a stand-in, carry the hairline over |
| **The blur** | `domain/translucency.rs` — putting a window's blur back into its picture, merging its own capture with a composite of what is below it |
| **Settled** | `domain/settle.rs` — when a chase's capture is the window's settled rendering, by thumbprint |
| **The stand-in** | `domain/stand_in.rs` — which window with no picture flies as the dark stand-in |
| **Desktop and bar levels** | `domain/backdrop.rs` — which window-server levels are the desktop and which the bar |
| **The port** | `domain/target.rs` — `PictureTarget`, a window to picture and the size it should be |
| **The schedule** | `domain/schedule.rs` — which capture work each phase of a flight permits, the parking of warms, and the capture budgets and cadences |

The stretch tolerance a fit is judged by is `rini_geometry::stretch`, shared with `animation`'s
"is this a resize" so the two agree.

## The shape that matters

**Usable is not the same as fitting.** A capture is usable when it covers the window it was taken
from; it fits when it can be drawn into a frame without visible stretch. A cached picture of a window
that has since been resized is usable and does not fit, and both checks exist for that case. See "A
capture can be usable and still be the wrong shape" in
[`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md).

**The cache never downgrades.** A clipped capture never replaces a usable one, and a capture of a
window on its own never replaces one of the same size that carries its blur.

## Reading order

`domain/coverage.rs` → `domain/cache.rs` → `domain/schedule.rs`.

## Detail

- [`src/animation/docs/capture-overlay-research.md`](../../animation/docs/capture-overlay-research.md)
  — every capture route measured: cost, what each returns, and why the desktop and the bar are
  pictured on their own
- [`src/animation/docs/animation-smoothness.md`](../../animation/docs/animation-smoothness.md) — the
  capture work a flight does, and when

## Known debt

The captures themselves, the picture type and the ScreenCaptureKit queue are still in
`animation::platform` (`window_snapshot.rs`, `snapshot_service.rs`, `backdrop.rs`,
`edge_dressing.rs`), and the switcher and the bar import their picture helpers from there.
