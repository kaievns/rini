# Capture-based overlay animation: measured constraints

Research notes for replacing the Accessibility (AX) animation engine with a
capture-based overlay. All numbers below come from spikes in `/tmp/sls-spike`
run on 2026-08-16, against a live 1728x1117 built-in Retina display at 2x
backing scale, with 35 real app windows across 4 rini workspaces.

Status: the approach is viable. A warm bitmap cache is mandatory, not optional.

## Verdict

| Question | Answer | Evidence |
|---|---|---|
| Can rini capture a window hidden on another workspace? | Yes, full size, real content | `sck.swift`, window 45 |
| Can rini capture a window parked at a 2pt sliver? | Yes, full size, real content | `sck.swift`, window 28804 |
| Which API works? | ScreenCaptureKit only | `capdecide.swift`, `sck.swift` |
| Can rini move a foreign window via SLS? | No, and the calls report success | `tx2.swift`, `mv.swift` |
| Can rini set a foreign window's alpha? | No | `tx2.swift` |
| Can captures be taken on demand at switch time? | No | `cksweep.swift` |
| Does lower resolution reduce cost? | No | `cksweep.swift` |

## Capture cost

Concurrent capture, median of 5 runs, unbounded concurrency:

```
windows   2x ms    1x ms
1          56.5     63.2
2          71.3     73.1
4          94.5     93.2
6         119.4    123.1
8         145.7    148.0
12        204.0    199.4
16        271.6    251.7
```

The curve is linear: about 40ms fixed cost plus 14.5ms per window. Sequential
capture of all 35 windows took 1319ms. Concurrent capture of the same 35 took
540ms and did not improve past a concurrency limit of 4. ScreenCaptureKit
serialises internally.

Two consequences:

1. **1x costs the same as 2x.** Cost is per-call overhead, not pixel count.
   Always capture at full 2x. Downscaling saves nothing.
2. **On-demand capture cannot work.** Even 4 windows cost 94.5ms, which exceeds
   the whole budget for a 180ms animation. Captures must be warm before the
   switch key is pressed.

## What ScreenCaptureKit returns

`SCContentFilter(desktopIndependentWindow:)` plus `SCScreenshotManager` returns
the window's own surface, not a screen region. Verified for 6 windows in 3
visibility states:

| State | Window | Requested | Returned | Content |
|---|---|---|---|---|
| Fully visible | Ghostty 31648 | 859x1081 | 859x1081 | yes |
| Hidden workspace | Obsidian 45 | 859x1081 | 859x1081 | yes |
| Off-strip, 40pt visible | Slack 96 | 859x1081 | 859x1081 | yes |
| Off-strip, 40pt visible | Kiro 26995 | 1720x1081 | 1720x1081 | yes |
| Parked, 2pt visible | Zen 28804 | 859x1081 | 859x1081 | yes |
| Off-strip, 40pt visible | Chrome 3508 | 1720x1081 | 1720x1081 | yes |

Content was confirmed two ways. A pixel-difference metric scored 0.995 to 1.000
against a flat-buffer baseline of 0.000. The images were then inspected
directly: window 45 shows the correct Obsidian note from the hidden `comms`
workspace, and window 28804 shows the full Zen browser window.

`SCShareableContent` must be requested with `onScreenWindowsOnly: false`.
Otherwise hidden windows are not enumerated at all.

## Dead ends, with the reason each one fails

### SLSHWCaptureWindowList captures only the visible portion

This is the SkyLight SPI yabai uses. It works and it is fast, but it grabs the
framebuffer, not the window surface. A window with 40pt showing returns a
40x1081 image. A window on a hidden workspace returns 1x28.

Batched behaviour is worth recording because it surprised us. Passing 19 window
ids returns 1 composited 3456x2170 image of the display, not 19 images, in
about 21ms. That composite is correct and full fidelity for on-screen content.
It remains useful for anything that only needs what is currently displayed. It
cannot drive a slide-in, because incoming windows have no pixels yet.

Per-window calls cost about 16ms each, so batching beats them 12 to 1.

### CGWindowListCreateImage is gone

Not deprecated. Unavailable. It is a hard compile error on this SDK:
"Please use ScreenCaptureKit instead". There is no pre-macOS-14 fallback.

The system tool inherited this. `screencapture -l <windowid>` fails with "could
not create image from window" even for a fully visible window.

### Window server mutation does not work for foreign windows

Retested on 2026-08-16 with a valid control, which the first attempt lacked. The
verdict is unchanged but 2 details recorded earlier were wrong. Corrections are
at the end of this section.

**The control.** The spike creates its own `NSWindow` and applies the identical
transform to it. This separates an ownership restriction from a bad function
signature. Result: `SLSSetWindowTransform` returned 0, the readback held
`[tx 0 ty 400]`, and 6.37% of display pixels changed. The window visibly moved.
So the call is correct, and any foreign-window failure is a real restriction.

**Foreign window results.** Judged on display pixels, not on return codes:

| Attempt | Return code | Readback held | Pixels changed | Moved |
|---|---|---|---|---|
| `SLSSetWindowTransform` plain | 0 `kCGErrorSuccess` | no, reverted | 0.0003 | no |
| `...AtPlacement` placement 0 | 1000 `kCGErrorFailure` | no | 0.0001 | no |
| `...AtPlacement` placement 1 | 1000 `kCGErrorFailure` | no | 0.0003 | no |
| `...AtPlacement` placement 2 | 1000 `kCGErrorFailure` | no | 0.0004 | no |
| inside `SLSDisableUpdate` | 0 `kCGErrorSuccess` | no, reverted | 0.0003 | no |
| `SLSMoveWindow` legal 100pt | 0 `kCGErrorSuccess` | see below | 0.0004 | no |
| `SLSMoveWindow` to y = -300 | 0 `kCGErrorSuccess` | see below | 0.0003 | no |
| `SLSSetWindowAlpha` 0.3 | 0 `kCGErrorSuccess` | n/a | 0.0003 | no effect |

`SLSMoveWindow` deserves its own note, because it lies in a more convincing way
than the others. After the call, `SLSGetWindowBounds` reports the **new**
position, while `CGWindowListCopyWindowInfo` still reports the **old** one and no
pixel changes. So SLS stores the requested value against our connection and the
window server never applies it. Reading back through SLS confirms a move that
did not happen.

`SLSGetWindowTransform` does work, and returns the negated origin. For window
28809 at origin (-857, 32) it returns `[tx 857 ty -32]`. That explains the
revert: for a foreign window the transform is derived from the real frame rather
than being independent state, so the window server recomputes it and discards
what was written.

**Corrections to earlier notes in this file's history:**

1. `SLSMoveWindow` on a foreign window returns **0 `kCGErrorSuccess`**, not error
   1000. The earlier note was wrong.
2. Code 1000 is **`kCGErrorFailure`**, not `kCGErrorIllegalArgument`, which is
   1001.

**Consequence for the overlay design.** `SLSSetWindowAlpha` does not work on
foreign windows, so the original plan of parking real windows at their final
positions with opacity 0 cannot be implemented that way. Use an opaque
edge-to-edge overlay above all windows instead, which rini owns and can
therefore control. Real windows are repositioned underneath while covered, so
their tear is never visible, and the overlay is dismissed only once the writes
have landed.

Note for future spikes: SLS symbols live in the dyld shared cache. `nm` reports
them as absent, which is misleading. Link against
`/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` and they
resolve.

Never judge an SLS mutation by its return code. Three of the calls above return
`kCGErrorSuccess` and do nothing, and one of them echoes the written value back
on read. Verify with `CGWindowListCopyWindowInfo` bounds or with pixels.

## Trap that cost about 40 minutes

Every ScreenCaptureKit capture failed with error -3811, "Failed to start stream
due to audio/video capture failure". Window enumeration worked and returned 181
windows with titles, so the Screen Recording grant was clearly present.

The cause was a sleeping display. `SCShareableContent.displays` was empty, and
`SCScreenshotManager` cannot start a stream with no display behind it. The error
message names audio and video and does not mention displays at all.

Diagnosis path, for next time:

```
CGGetOnlineDisplayList  -> 1     display is present
CGGetActiveDisplayList  -> 0     display is not drawable
CGDisplayIsAsleep(1)    -> true  root cause
```

`caffeinate -u -t 3` wakes it, after which all captures succeed.

Two related warnings. `CGGetActiveDisplayList(0, nil, &count)` always yields 0,
because `maxDisplays: 0` caps the count. Pass a real array. And
`CGPreflightScreenCaptureAccess` returned true throughout, so it does not
indicate that capture will succeed.

Bundling is not a factor. A signed `.app` bundle with
`NSScreenCaptureUsageDescription` behaved identically to the bare binary.

## Consequences for the design

**Warm cache, refreshed in the background.** At switch time rini must capture
nothing. It composites bitmaps it already holds. This matches the original
instinct that only on-screen and incoming windows need fresh pixels, and the
rest can be refreshed occasionally.

**Staleness is acceptable.** The bitmap is on screen for the duration of one
animation and is replaced by the real window at the end. A slightly stale
moving image is not perceptible.

**rini needs the Screen Recording grant.** It does not have it today. This is
also why Mission Control currently renders black. `src/ui/mission_control.rs`
already contains the full ScreenCaptureKit pipeline, so that feature has never
worked on this machine, and fixing the grant may fix it for free.

**Capture in a background thread.** At 14.5ms per window, refreshing even a few
windows would blow several frames if it ran on the animation thread.

## Reproducing

Spikes live in `/tmp/sls-spike`. Build them with:

```
swiftc -O -F /System/Library/PrivateFrameworks -framework SkyLight <name>.swift -o <name>
swiftc -O -framework ScreenCaptureKit <name>.swift -o <name>
```

| Spike | Question it answers |
|---|---|
| `capbench.swift` | SkyLight capture latency and batching behaviour |
| `capshape.swift` | What a batched SkyLight capture actually contains |
| `capcontent.swift` | Blank buffer against real pixels, measured |
| `capdecide.swift` | Visible-portion limit, against rini's own window states |
| `sck.swift` | Full-size capture of hidden, parked and off-strip windows |
| `sck2.swift`, `sck3.swift`, `sck4.swift`, `dpy.swift` | The -3811 diagnosis |
| `tx.swift` | Own-window transform control, proving the call is correct |
| `tx2.swift` | Foreign-window transform and alpha, verified by pixels |
| `mv.swift` | Whether SLSMoveWindow moves a foreign window (it does not) |
| `ckthru.swift` | Concurrency ceiling |
| `cksweep.swift` | Cost against set size, 2x against 1x |

Ground truth for window states comes from `rini-cli query diagnostics`, which
reports `is_parked`, `visible_width` and `workspace_name` per window. Do not use
`CGWindowListCopyWindowInfo` with `optionOnScreenOnly` to find hidden windows.
rini parks windows as 40pt slivers, so CoreGraphics counts them as on-screen and
that route silently measures the wrong set.
