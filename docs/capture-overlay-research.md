# Capture-based overlay animation: measured constraints

Research notes for replacing the Accessibility (AX) animation engine with a
capture-based overlay. All numbers below come from spikes in `/tmp/sls-spike`
run on 2026-08-16, against a live 1728x1117 built-in Retina display at 2x
backing scale, with 35 real app windows across 4 rini workspaces.

Status: the approach is viable. The design is a hybrid: capture on-screen windows
fresh through SkyLight at switch time, and serve off-strip and hidden-workspace
windows from a ScreenCaptureKit cache refreshed in the background.

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

## What yabai actually does

Read from the source at commit depth 1 of `github.com/koekeishiya/yabai`, not
from recollection. The relevant code is `window_manager_animate_window_list_async`
in `src/window_manager.c:604-710`.

yabai animates **proxy windows it owns**, never the real windows. The sequence:

1. `SLSNewConnection` for a dedicated animation connection (`window_manager.c:607`).
2. Per window, on its own pthread so captures run in parallel
   (`window_manager.c:666`):
   - `SLSGetWindowBounds` for the start frame
   - `SLSHWCaptureWindowList(cid, &wid, 1, (1 << 11) | (1 << 8))`
     (`window_manager.c:521`)
   - `SLSNewWindowWithOpaqueShapeAndContext` to create its own window, then
     `SLSSetWindowOpacity`, `SLSSetWindowResolution(2.0)`, `SLSSetWindowAlpha`,
     `SLSSetWindowLevel`, `SLSSetWindowSubLevel`, then `SLWindowContextCreate`
     and `CGContextDrawImage` to paint the bitmap in (`window_manager.c:463-484`)
3. `pthread_join` on all capture threads (`window_manager.c:679`).
4. `scripting_addition_swap_window_proxy_in` (`window_manager.c:685`), which asks
   the injected payload to run
   `SLSTransactionOrderWindowGroup(transaction, proxy_wid, 1, wid)`
   (`src/osax/payload.m:827`). That orders the proxy above the real window.
5. `window_manager_set_window_frame` moves the real window to its final position
   **once** (`window_manager.c:694`), hidden behind the proxy.
6. `CVDisplayLinkCreateWithActiveCGDisplays` drives the tick
   (`window_manager.c:700`).
7. Each tick lerps, then applies `SLSTransactionSetWindowTransform` to each
   **proxy** and commits all of them in one `SLSTransactionCommit`
   (`window_manager.c:556-571`).
8. At t == 1, `scripting_addition_swap_window_proxy_out` orders the proxy back
   under the real window, proxies are destroyed, and the connection is released
   (`window_manager.c:578-594`).

This confirms the measurements in this file rather than contradicting them.
`SLSSetWindowTransform` appears in exactly 2 files: `src/misc/extern.h`, which is
only the declaration, and `src/osax/payload.m`, which is the scripting addition
injected into Dock.app and requires partial SIP disable. yabai's own process
never transforms a foreign window. Every transform it applies goes to a proxy it
created itself, which matches the own-window control succeeding here while every
foreign attempt failed.

### Three things rini should copy

1. **Set the real window frame once, not per frame.** rini currently writes
   `AXPosition` on every frame of every window. That is the direct cause of the
   cross-app tear, because each write is a synchronous request into a process
   that answers at its own speed. yabai writes the final frame one time.
2. **One transaction per frame for all windows.** `SLSTransactionCommit` applies
   every proxy's transform atomically, so yabai's windows cannot tear against
   each other by construction. rini's per-app batching reduced the tear but
   cannot eliminate it, because separate apps still answer separately.
3. **`CVDisplayLink` for the tick.** This is item 39 in the backlog. Note that
   `src/sys/display_link.rs` is currently dead code and its `Drop` is unsound,
   because `CVDisplayLinkStop` does not wait for an in-flight callback before the
   `Box` is freed.

### Two places rini's situation differs

1. **rini does not need the scripting addition, and must not adopt yabai's
   z-ordering approach.** yabai needs partial SIP disable purely to order each
   proxy directly above its own real window, preserving interleaving with
   unmanaged windows. rini's design only needs one opaque full-screen overlay
   above everything, and setting a level on a window rini owns needs no
   privilege. This is the reason rini can get the same visual result without
   SIP disable, and it should be treated as a constraint on the design rather
   than an accident.
2. **rini cannot use `SLSHWCaptureWindowList` the way yabai does.** yabai only
   ever animates windows that are already fully on screen, so a
   visible-portion-only capture is sufficient for it. rini's incoming windows sit
   off-strip or on a hidden workspace, where that call returns a 40x1081 or 1x28
   sliver, as measured above. rini therefore needs ScreenCaptureKit, which costs
   40ms plus 14.5ms per window against yabai's roughly 16ms per window in
   parallel. That difference is exactly why rini needs a warm cache and yabai
   does not.

## Claims tested and rejected

An external analysis proposed that the measured ScreenCaptureKit cost is an
artefact, and that `SLSHWCaptureWindowList` can capture the whole working set
fresh at animation start with no cache. Each claim was tested. The useful parts
are folded into the design section below.

**Rejected: "the cost is a per-shot `SCShareableContent` round trip."** Already
excluded. `cksweep.swift:77` enumerates once, outside every measured loop.

**Rejected: "issue captures concurrently instead of in a loop."** Already done.
`cksweep` ran them concurrently and unbounded through a `TaskGroup`.

**Rejected: "the `captureSampleBuffer` / IOSurface route reaches 20-40ms for the
batch."** Measured head to head against `captureImage`, same windows, same scale,
interleaved and repeated 5 times, stopping at the `IOSurface` without building any
image:

```
windows   captureImage   sampleBuffer   result
1              38.9ms         37.5ms    no real difference
2              49.6ms         51.6ms    no real difference
4              72.6ms         88.0ms    captureImage 21% faster
6              92.6ms        121.5ms    captureImage 31% faster
8             129.3ms        150.7ms    captureImage 17% faster
```

The sample-buffer route is slower once past 2 windows. The cost is the capture
session `SCScreenshotManager` spins up per call, not image materialisation, so no
rearrangement of the public API avoids it.

**Rejected: "pass the whole wid array and get a CFArray of CGImages back, so N
windows costs roughly the same as one."** N windows returns ONE flattened
composite, at every count tested:

```
requested 1 -> 1 image,  859x1081pt
requested 2 -> 1 image, 1720x1081pt
requested 3 -> 1 image, 1724x1081pt
requested 5 -> 1 image, 1724x1081pt
```

The time claim is true and the result claim is not. A flattened composite cannot
drive per-window animation. yabai agrees: it has exactly 1 call site,
`window_manager.c:521`, and it passes `window_count = 1`. It never batches.

**Rejected: "capture fresh at animation start, no pre-warming, no cache."** This
is the load-bearing claim and it does not survive rini's layout. The call returns
only the visible portion, and the option flags do not change that, including
yabai's exact flags:

```
case         own size       options                returned      full?
visible      859x1081pt     1<<11                  859x1081pt    YES
visible      859x1081pt     (1<<11)|(1<<8) yabai   859x1081pt    YES
hidden-ws   1147x1081pt     1<<11                  1x28pt        no
hidden-ws   1147x1081pt     (1<<11)|(1<<8) yabai   1x28pt        no
offstrip40   859x1081pt     1<<11                  2x1081pt      no
offstrip40   859x1081pt     (1<<11)|(1<<8) yabai   2x1081pt      no
```

`1<<9` is a half-resolution variant and clips identically. The limit is the API,
not the flags. yabai can skip caching because it only animates windows already
fully on screen. rini's incoming windows are off-strip or on a hidden workspace,
which is exactly the case that returns a sliver.

### Confirmed and worth using

- `SLSHWCaptureWindowList` is gated by the Screen Recording grant, not by window
  ownership. No scripting addition and no SIP disable. Verified working from a
  plain unprivileged connection.
- It is much cheaper per window than ScreenCaptureKit: about 16ms against 38 to
  56ms. Worth using wherever it is sufficient.
- The transform convention is `CGAffineTransformMakeTranslation(-tx, -ty)`, the
  negative of the target origin in top-left space. This matches
  `SLSGetWindowTransform` returning the negated origin, measured above.
- The binding already exists at `src/sys/skylight.rs:495`, so reaching it is
  cheap.
- Captures exclude the drop shadow, which is why yabai calls
  `sls_window_disable_shadow` on its proxies to match
  (`window_manager.c:473`), and it runs `cgimage_restore_alpha` when the source
  window's alpha is not 1.0 (`window_manager.c:521-523`).
- Version-gate it and fall back to ScreenCaptureKit when the array comes back
  null.

### The design this actually implies: a hybrid

Use each API where it wins, rather than choosing one.

- **On-screen windows, captured fresh at switch time.** `SLSHWCaptureWindowList`
  with `window_count = 1` per window, on parallel threads, as yabai does. About
  16ms each and no staleness.
- **Off-strip and hidden-workspace windows, served from a warm cache.**
  ScreenCaptureKit is the only API that returns their full surface, and at 40ms
  plus 14.5ms per window it cannot run at switch time. Refresh these in the
  background on focus and resize events.

This keeps fresh pixels for everything the eye is already looking at, and accepts
staleness only for windows that are currently a 2pt sliver, where staleness is
unobservable.

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
| `sbvs.swift` | captureImage against captureSampleBuffer, head to head |
| `batch.swift` | Whether a batched SLS capture returns per-window images |
| `flags.swift` | Whether capture option flags change the visible-portion clipping |
| `ckthru.swift` | Concurrency ceiling |
| `cksweep.swift` | Cost against set size, 2x against 1x |

Ground truth for window states comes from `rini-cli query diagnostics`, which
reports `is_parked`, `visible_width` and `workspace_name` per window. Do not use
`CGWindowListCopyWindowInfo` with `optionOnScreenOnly` to find hidden windows.
rini parks windows as 40pt slivers, so CoreGraphics counts them as on-screen and
that route silently measures the wrong set.
