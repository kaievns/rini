# Capture-based overlay animation: measured constraints

Research notes for replacing the Accessibility (AX) animation engine with a
capture-based overlay. All numbers below come from spikes in `/tmp/sls-spike`
run on 2026-08-16, against a live 1728x1117 built-in Retina display at 2x
backing scale, with 35 real app windows across 4 rini workspaces.

Status: the approach is viable. The design is a hybrid: capture on-screen
windows fresh through SkyLight at switch time, and serve off-strip and
hidden-workspace windows from a ScreenCaptureKit cache refreshed in the
background.

## Verdict

| Question                                               | Answer                           | Evidence                       |
| ------------------------------------------------------ | -------------------------------- | ------------------------------ |
| Can rini capture a window hidden on another workspace? | Yes, full size, real content     | `sck.swift`, window 45         |
| Can rini capture a window parked at a 2pt sliver?      | Yes, full size, real content     | `sck.swift`, window 28804      |
| Which API works?                                       | ScreenCaptureKit only            | `capdecide.swift`, `sck.swift` |
| Can rini move a foreign window via SLS?                | No, and the calls report success | `tx2.swift`, `mv.swift`        |
| Can rini set a foreign window's alpha?                 | No                               | `tx2.swift`                    |
| Can captures be taken on demand at switch time?        | No                               | `cksweep.swift`                |
| Does lower resolution reduce cost?                     | No                               | `cksweep.swift`                |

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

| State                   | Window        | Requested | Returned  | Content |
| ----------------------- | ------------- | --------- | --------- | ------- |
| Fully visible           | Ghostty 31648 | 859x1081  | 859x1081  | yes     |
| Hidden workspace        | Obsidian 45   | 859x1081  | 859x1081  | yes     |
| Off-strip, 40pt visible | Slack 96      | 859x1081  | 859x1081  | yes     |
| Off-strip, 40pt visible | Kiro 26995    | 1720x1081 | 1720x1081 | yes     |
| Parked, 2pt visible     | Zen 28804     | 859x1081  | 859x1081  | yes     |
| Off-strip, 40pt visible | Chrome 3508   | 1720x1081 | 1720x1081 | yes     |

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
ids returns 1 composited 3456x2170 image of the display, not 19 images, in about
21ms. That composite is correct and full fidelity for on-screen content. It
remains useful for anything that only needs what is currently displayed. It
cannot drive a slide-in, because incoming windows have no pixels yet.

Per-window calls cost about 16ms each, so batching beats them 12 to 1.

### CGWindowListCreateImage is gone

Not deprecated. Unavailable. It is a hard compile error on this SDK: "Please use
ScreenCaptureKit instead". There is no pre-macOS-14 fallback.

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

| Attempt                       | Return code            | Readback held | Pixels changed | Moved     |
| ----------------------------- | ---------------------- | ------------- | -------------- | --------- |
| `SLSSetWindowTransform` plain | 0 `kCGErrorSuccess`    | no, reverted  | 0.0003         | no        |
| `...AtPlacement` placement 0  | 1000 `kCGErrorFailure` | no            | 0.0001         | no        |
| `...AtPlacement` placement 1  | 1000 `kCGErrorFailure` | no            | 0.0003         | no        |
| `...AtPlacement` placement 2  | 1000 `kCGErrorFailure` | no            | 0.0004         | no        |
| inside `SLSDisableUpdate`     | 0 `kCGErrorSuccess`    | no, reverted  | 0.0003         | no        |
| `SLSMoveWindow` legal 100pt   | 0 `kCGErrorSuccess`    | see below     | 0.0004         | no        |
| `SLSMoveWindow` to y = -300   | 0 `kCGErrorSuccess`    | see below     | 0.0003         | no        |
| `SLSSetWindowAlpha` 0.3       | 0 `kCGErrorSuccess`    | n/a           | 0.0003         | no effect |

`SLSMoveWindow` deserves its own note, because it lies in a more convincing way
than the others. After the call, `SLSGetWindowBounds` reports the **new**
position, while `CGWindowListCopyWindowInfo` still reports the **old** one and
no pixel changes. So SLS stores the requested value against our connection and
the window server never applies it. Reading back through SLS confirms a move
that did not happen.

`SLSGetWindowTransform` does work, and returns the negated origin. For window
28809 at origin (-857, 32) it returns `[tx 857 ty -32]`. That explains the
revert: for a foreign window the transform is derived from the real frame rather
than being independent state, so the window server recomputes it and discards
what was written.

**Corrections to earlier notes in this file's history:**

1. `SLSMoveWindow` on a foreign window returns **0 `kCGErrorSuccess`**, not
   error 1000. The earlier note was wrong.
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
from recollection. The relevant code is
`window_manager_animate_window_list_async` in `src/window_manager.c:604-710`.

yabai animates **proxy windows it owns**, never the real windows. The sequence:

1. `SLSNewConnection` for a dedicated animation connection
   (`window_manager.c:607`).
2. Per window, on its own pthread so captures run in parallel
   (`window_manager.c:666`):
   - `SLSGetWindowBounds` for the start frame
   - `SLSHWCaptureWindowList(cid, &wid, 1, (1 << 11) | (1 << 8))`
     (`window_manager.c:521`)
   - `SLSNewWindowWithOpaqueShapeAndContext` to create its own window, then
     `SLSSetWindowOpacity`, `SLSSetWindowResolution(2.0)`, `SLSSetWindowAlpha`,
     `SLSSetWindowLevel`, `SLSSetWindowSubLevel`, then `SLWindowContextCreate`
     and `CGContextDrawImage` to paint the bitmap in
     (`window_manager.c:463-484`)
3. `pthread_join` on all capture threads (`window_manager.c:679`).
4. `scripting_addition_swap_window_proxy_in` (`window_manager.c:685`), which
   asks the injected payload to run
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
`SLSSetWindowTransform` appears in exactly 2 files: `src/misc/extern.h`, which
is only the declaration, and `src/osax/payload.m`, which is the scripting
addition injected into Dock.app and requires partial SIP disable. yabai's own
process never transforms a foreign window. Every transform it applies goes to a
proxy it created itself, which matches the own-window control succeeding here
while every foreign attempt failed.

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
   because `CVDisplayLinkStop` does not wait for an in-flight callback before
   the `Box` is freed.

### Two places rini's situation differs

1. **rini does not need the scripting addition, and must not adopt yabai's
   z-ordering approach.** yabai needs partial SIP disable purely to order each
   proxy directly above its own real window, preserving interleaving with
   unmanaged windows. rini's design only needs one opaque full-screen overlay
   above everything, and setting a level on a window rini owns needs no
   privilege. This is the reason rini can get the same visual result without SIP
   disable, and it should be treated as a constraint on the design rather than
   an accident.
2. **rini cannot use `SLSHWCaptureWindowList` the way yabai does.** yabai only
   ever animates windows that are already fully on screen, so a
   visible-portion-only capture is sufficient for it. rini's incoming windows
   sit off-strip or on a hidden workspace, where that call returns a 40x1081 or
   1x28 sliver, as measured above. rini therefore needs ScreenCaptureKit, which
   costs 40ms plus 14.5ms per window against yabai's roughly 16ms per window in
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
batch."** Measured head to head against `captureImage`, same windows, same
scale, interleaved and repeated 5 times, stopping at the `IOSurface` without
building any image:

```
windows   captureImage   sampleBuffer   result
1              38.9ms         37.5ms    no real difference
2              49.6ms         51.6ms    no real difference
4              72.6ms         88.0ms    captureImage 21% faster
6              92.6ms        121.5ms    captureImage 31% faster
8             129.3ms        150.7ms    captureImage 17% faster
```

The sample-buffer route is slower once past 2 windows. The cost is the capture
session `SCScreenshotManager` spins up per call, not image materialisation, so
no rearrangement of the public API avoids it.

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
is the load-bearing claim and it does not survive rini's layout. The call
returns only the visible portion, and the option flags do not change that,
including yabai's exact flags:

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
  `sls_window_disable_shadow` on its proxies to match (`window_manager.c:473`),
  and it runs `cgimage_restore_alpha` when the source window's alpha is not 1.0
  (`window_manager.c:521-523`).
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

This keeps fresh pixels for everything the eye is already looking at, and
accepts staleness only for windows that are currently a 2pt sliver, where
staleness is unobservable.

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
animation and is replaced by the real window at the end. A slightly stale moving
image is not perceptible.

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

| Spike                                                 | Question it answers                                              |
| ----------------------------------------------------- | ---------------------------------------------------------------- |
| `capbench.swift`                                      | SkyLight capture latency and batching behaviour                  |
| `capshape.swift`                                      | What a batched SkyLight capture actually contains                |
| `capcontent.swift`                                    | Blank buffer against real pixels, measured                       |
| `capdecide.swift`                                     | Visible-portion limit, against rini's own window states          |
| `sck.swift`                                           | Full-size capture of hidden, parked and off-strip windows        |
| `sck2.swift`, `sck3.swift`, `sck4.swift`, `dpy.swift` | The -3811 diagnosis                                              |
| `tx.swift`                                            | Own-window transform control, proving the call is correct        |
| `tx2.swift`                                           | Foreign-window transform and alpha, verified by pixels           |
| `mv.swift`                                            | Whether SLSMoveWindow moves a foreign window (it does not)       |
| `sbvs.swift`                                          | captureImage against captureSampleBuffer, head to head           |
| `batch.swift`                                         | Whether a batched SLS capture returns per-window images          |
| `flags.swift`                                         | Whether capture option flags change the visible-portion clipping |
| `ckthru.swift`                                        | Concurrency ceiling                                              |
| `cksweep.swift`                                       | Cost against set size, 2x against 1x                             |

Ground truth for window states comes from `rini-cli query diagnostics`, which
reports `is_parked`, `visible_width` and `workspace_name` per window. Do not use
`CGWindowListCopyWindowInfo` with `optionOnScreenOnly` to find hidden windows.
rini parks windows as 40pt slivers, so CoreGraphics counts them as on-screen and
that route silently measures the wrong set.

## Phase 1 result: the overlay works

Gating spike for the design, run 2026-08-16. Every question passed. Spikes are
`overlay.swift`, `hold-overlay.swift`, `overlay2.swift` and `levels.swift`.

### Level and coverage

All managed windows sit at CG layer 0, read from the window server rather than
assumed. An overlay at `CGWindowLevelForKey(.screenSaverWindow)`, which is 1000,
covers all of them. Verified from the framebuffer with `screencapture`, not from
`SLSHWCaptureWindowList`, which composites only the windows it is handed and so
cannot answer what is actually on screen.

**The overlay spans the FULL display.** This reverses an earlier decision and
the reasoning for the original is worth keeping, because it is nearly right.
sketchybar sits at layer **-20**, below normal windows, and is visible only
because nothing occupies the top 32pt strip, so an overlay sized to the usable
frame of 1728x1085 leaves the bar live while still covering every window. That
was measured and it works.

It was still wrong. A shorter overlay squashes the captured desktop into a box
the wrong shape, and it leaves a vertical animation invisible in the strip
beneath the bar, which is exactly where a workspace is supposed to slide out of
view. So the overlay covers the whole display and redraws the bar on top of
itself instead. See "The bar is dozens of windows" for what that costs.

The relevant layers on this machine:

```
managed app windows      0
sketchybar             -20
Dock          -2147483624
Notification Center   -2147483601
```

### Focus is never stolen

Tested at 6 levels from `normal` through `CGShieldingWindowLevel`, which is
2147483628. The frontmost application was unchanged before, during and after in
every case. The combination that achieves this is `.borderless` style,
`orderFrontRegardless()`, and `ignoresMouseEvents`.

### Toggle alpha, do not order the window in and out

This is a 30-fold difference and it decides the show and hide path:

```
                      median      min      max
orderFrontRegardless  13.94ms  10.25ms  24.72ms
orderOut              13.07ms  10.09ms  20.25ms
alpha 0 -> 1           0.36ms   0.29ms   8.56ms
alpha 1 -> 0           0.34ms   0.19ms   8.33ms
```

Ordering in or out costs a full frame each way at 60fps. Keep the overlay
permanently ordered in at alpha 0 and toggle `alphaValue`, which is effectively
free. Alpha works here because rini owns this window; it does not work on
foreign windows, as recorded above.

Create the overlay once at startup. First show cost 112ms against a 14ms median,
which is window creation, and it should not be paid per switch.

### Collection behaviour

`[.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenNone]`. Stationary
matters: without it the overlay slides along with macOS's own Space animation.

### Spike gotcha worth remembering

`RunLoop.run(mode:before:)` returns as soon as it handles one input source, so
it cannot hold for a duration. A timed hold needs `run(until:)`. Two coverage
screenshots were captured against an already dismissed overlay before this was
spotted.

## The wallpaper is not reliably a window

The overlay draws the desktop behind the moving strips, so the gaps around
windows look like the desktop rather than a flat colour. That desktop was built
by compositing every window at or below the desktop level through
`SLSHWCaptureWindowList`, which worked until a second display was attached.

Measured on the same machine, same desktop, minutes apart:

```
one display   wid=1056  owner="Wallpaper"  "Offscreen Wallpaper Window"  layer=-2147483625
              wallpaper window on its own  brightness 17.2
              composite of 7 desktop windows      brightness 24.7

two displays  NO window owned by "Wallpaper" exists at all
              composite of 9 desktop windows      brightness  7.8   <- icons on black
```

`ScreenCaptureKit` does not list a wallpaper window either, in either state.

Measured again with one display attached, and the wallpaper IS a window. It is
just not owned by anything called "Wallpaper":

```
-2147483626  Window Server  "Display 1 Backstop"                0,0 1728x1117
-2147483624  Dock           "Wallpaper-EEB523A2-7133-497B-..."  0,0 1728x1117
-2147483603  Finder         desktop icons                       0,0 1728x1117
-2147483602  Window Server  "underbelly"                        0,0 1728x94
-2147483601  Notification Center  Featured / Forecast / Month   widgets
        -20  sketchybar     24 windows across the strip
```

Two consequences, both still live in the code:

- `desktop_backdrop_windows` excludes -2147483624 as `DOCK_LEVEL`, on the earlier
  reading that the Dock lives there. On this system that level holds the
  wallpaper, so the composite route drops the one window it most needs.
- `has_wallpaper` looks for an OWNER name containing "Wallpaper". The owner is
  "Dock" and it is the window NAME that carries the "Wallpaper-" prefix, so the
  flag is always false.

Together those mean the composite route cannot produce a wallpaper here at all:
`is_backdrop_worth_drawing` accepts its wallpaperless output once, before
anything better exists, and rejects it every time after. So every backdrop after
the first animation is the ScreenCaptureKit display render. Left alone for now
rather than fixed in passing, because it changes which route serves the backdrop,
and that is where every black-screen report so far has come from.

Either way the desktop has to be capturable as a DISPLAY, not only as a set of
windows:

```
SCContentFilter(display:excludingWindows:)  excluding every window at layer >= 0
  -> 3456x2234  brightness 23.4   wallpaper, icons, widgets, no app windows
```

This is why `snapshot_service.rs` has a desktop path. It costs the usual
ScreenCaptureKit ~40ms, so it fills a cache in the background and an animation
draws whatever is in hand; the desktop changes rarely enough that a capture a
few seconds old is indistinguishable.

### How this presented

Every vertical workspace switch went black for the length of the animation, with
the desktop icons still drawn and correctly placed. Sampling the same screen
points in a recording, real frame against overlay frame:

```
point(pt)     real RGB        overlay RGB
  600,300     13, 17, 18        0,  0,  0
  900,900     77, 48, 26        0,  0,  0
 1100,400     80, 75, 66        0,  0,  0
```

Pure black, not a dimmed or gamma-shifted wallpaper. The composite simply had
nothing behind its icons.

## Two drawing paths must not share one pool of tile layers

Tile layers are pooled per window and reused across animations, because handing
a layer a bitmap is the expensive part. But the canvas path parents them to the
canvas and positions them in CANVAS coordinates, translating the parent to
animate, while the per-window path parents them to the root and positions them
in OVERLAY coordinates, moving each one.

The reactor lays a workspace switch out over several passes, so both paths fire
for one switch. Measured on a live vertical switch:

```
0:56:52.485  canvas animation, requested=16, tiles=16, travel="0,0 -> 0,2234"
0:56:52.739  overlay animation composition, requested=13, tiles=6, offscreen=7
0:56:53.125  handover mismatch: worst_pt="2621"
```

The later pass re-framed the canvas's own tiles into the wrong coordinate system
while the canvas still had them translated by two workspace rows. Tiles measured
424x500 where the window was 859x1081, and the switch ended with a 2621pt jump
when the overlay lifted.

Fix: while a canvas movement is in flight the per-window path merges its
destinations into the canvas and draws nothing. Plus `reparent`, so a pooled
layer reused by the other path is moved to the right parent rather than
positioned with the wrong arithmetic.

### The animation was running the whole time

Worth recording, because it was reported as "no sliding animation, windows
switch instantly" and the first instinct was to look at the frame clock. The
clock was fine. What was wrong was that it animated a near-black screen holding
half-size tiles.

macOS screen recordings are VARIABLE frame rate: no frame is emitted while the
screen does not change, so a gap in the recording is evidence of a static
screen, and frame timestamps are more informative than frame contents. In the
reported recording the animation showed up as a textbook ease-out decay:

```
2.40-2.53   brightness 43.8   change ~0      real workspace, static
2.5583      brightness 11.3   change 37.4    overlay appears, screen goes dark
2.56-2.88   brightness 11->34 change 37->1   350ms of ease-out = the animation
2.9083      brightness 139.6  change 113.5   overlay lifts, real workspace appears
```

`screencapture -v` records at about 21fps, too coarse to sample a 350ms
animation. Stretching `animation_duration` and taking ordinary screenshots is a
better instrument, and the canvas now reports its own frame count when a
movement finishes:

```
frames=21  elapsed=351ms   duration=350ms   travel="0,1117 -> 0,0"
frames=30  elapsed=501ms   duration=494ms   travel="0,0 -> 0,2234"
frames=87  elapsed=1501ms  duration=1500ms  travel="0,1117 -> 0,0"
```

60fps in every case, and travel proportional to the number of rows crossed.

## The bar has to be captured on its own, at the union's origin

sketchybar is not one window. Measured here: 24 windows at layer -20 across the
strip, most 14pt to 131pt wide, plus more parked at -9999,-9999.

`SLSHWCaptureWindowList` composites into an image covering the UNION of the
windows it is given, so where that union starts matters. Drawn at the overlay's
top-left instead, the bar landed left of the real one — that was the second bar
on screen. Placing it at the union's origin fixes the offset.

The reason to capture the bar at all, rather than clipping its rect out of the
desktop picture the backdrop already holds, is alpha:

```
24 windows, union 0,0 1728x32   ->  3456x64 px, exactly 2x the union
body pixels    18,20,23  a=224   sketchybar's 0xe015171a, premultiplied
alpha          98.3% at 201-254, 1.7% opaque (the glyphs), 0% clear
```

224 of 255 is the 88% the bar's config asks for, and the RGB does not vary with
the wallpaper behind it. These are the bar's own pixels, not a composite, and
the bar's translucency is per-pixel alpha it owns — the one kind of translucency
a capture keeps.

So the strips show THROUGH the bar as they scroll under it. The alternative,
which shipped briefly, was to clip the strip out of the desktop picture and draw
that on top: one bar, aligned by construction, but opaque. Windows sliding up a
vertical switch were then cut off at the bar's lower edge instead of passing
under it, which reads as them disappearing INTO the bar.

The bar is therefore excluded from BOTH desktop capture routes. The composite
route already dropped it, since layer -20 is above the desktop ceiling; the
ScreenCaptureKit render had to be told, because it excluded only layer 0 and
above. While the two disagreed, the strip's appearance depended on which route
served the backdrop for that switch — dark when the composite won, correct when
the render did. That is the whole of the intermittent "black menu bar on
horizontal slides, fine on vertical".

SkyLight reads the framebuffer, so the bar can only be captured while the
overlay is not on top of it. A switch chained onto one still in flight reuses
the picture the first switch took, and a capture that comes back the wrong size
for the strip is rejected by the same `fits` test the tiles use. A failed capture
keeps the previous picture rather than hiding the bar: hiding it let the canvas
show through the menu bar strip, which is worse than a slightly stale bar.

## A window that was resized keeps a usable picture, and vanishes

Three windows were dropped from every single animation:

```
canvas animation, requested=20, tiles=17, missing=0, misshapen=3
  idx=11333 frame="1147x1081" picture="859x1081"
  idx=19914 frame="1147x1081" picture="859x1081"
  idx=20205 frame="1720x1081" picture="859x1081"
```

The overlay is opaque, so a dropped tile is a window-shaped hole for the length of
the animation. The windows appeared to vanish and come back.

The cause was one word in the warm filter. `warm_windows` skipped any window that
already had a "usable" picture, and usable means the picture covers the window it
was taken FROM, not that it fits the window's size NOW. A strip re-fit had widened
these from 859pt to 1147pt, so their 859pt pictures stayed usable, stayed
un-refreshed, and failed the fit test on every animation forever. The resize itself
goes to the Accessibility engine, which does not warm anything, so nothing else
ever corrected them.

`needs_capture` now asks the question the caller means: does the picture fit the
size the layout just gave this window. And a picture of the wrong shape is
stretched onto the frame rather than dropped, because 350ms of a stretched window
is a great deal better than 350ms of a hole. After both:

```
canvas animation, requested=20, tiles=20, missing=0, misshapen=0
```

## A floating window is not part of the strip

Floating windows were in the canvas, so a strip scroll carried them sideways and
snapped them back at the handover. They belong to a workspace but not to its strip:
a scroll must leave them alone, while a switch between workspaces must take them
along.

So a canvas tile can be pinned. `start_canvas_pan` pins whatever the layout engine
calls floating; `start_canvas_switch` pins nothing.

The first attempt lifted pinned tiles out of the canvas and hung them off the
overlay's root, above every strip tile. That is wrong twice over, and both ways were
reported straight away. A floating window is not necessarily in FRONT: this one sits
behind the terminal that overlaps it, so it popped over the strip on every
transition. And a strip window with per-pixel alpha, a terminal at 95%, shows
whatever is behind it, so lifting the floating window out took away what used to
show through and left the backdrop showing instead.

A pinned tile therefore stays in the canvas, keeping the z-position it gets from the
window server's real front-to-back order like every other tile, and is counter-moved
instead: at canvas offset o it is placed at its screen frame plus o, which cancels
the canvas's own movement exactly. Two layer writes per frame at most, since only
floating windows pin.

Verified by pixel, on the left edge of a floating window during a scroll, before the
z-order was corrected:

```
at rest    x=72: lum 86   x=73: lum 75   x=74: lum 28    <- edge between 73 and 74
mid-pan    x=72: lum 52   x=73: lum 36   x=74: lum 27    <- same place
```

The luminance differs because the tile carries the shadow and the window's content
is live video. The edge does not move.

## Nothing may be captured on the way in

The design said it from the start: at switch time rini captures nothing, it
composites bitmaps it already holds. The code had drifted, because a stale or
unfocused picture looks wrong and the cheapest fix was to grab a fresh one first.
Measured cost of that, per press:

```
recaptured on-screen windows, attempts=3, refreshed=3, took_ms=71
recaptured on-screen windows, attempts=3, refreshed=3, took_ms=59
recaptured on-screen windows, attempts=3, refreshed=3, took_ms=67
```

Three SkyLight captures on the main thread, 59ms to 71ms, before the overlay could
be shown, plus a 35ms desktop composite on the canvas path. So a keypress moved
nothing for about a tenth of a second, which reads as software that is thinking
rather than responding, and on a redirection mid-flight it stalls the animation
already running.

Both are gone. The destination is still recaptured, but only mid-flight and only
off the main thread, where it costs no frames and arrives as an event that swaps a
tile's contents without touching its geometry. The desktop is held and refreshed in
the background. Measured after, command to first frame:

```
1:04:44.994967  cmd=MoveFocus(Right)
1:04:44.997095  canvas animation, requested=19, tiles=19, missing=0
```

2ms, against 56ms to 459ms before, with every tile present because the cache is
warmed after each animation instead of during the next one.

The trade is honest and worth stating: the destination now animates with whatever
the cache holds, which for an app that dims when unfocused means it can slide in
dim and correct itself as the off-thread capture lands. That capture measured 280ms
on one occasion, which is most of a flight. Snappiness was the explicit preference.

## One press must move the strip once

A single `MoveFocus` produces several layout passes, and the pan distance was being
read off the windows on each one. Every pass saw different frames, because the
previous pass had already written new ones and macOS had clamped some of them, so
one press retargeted the canvas five times in 110ms:

```
travel="6315,0 -> 0,0"
travel="9471,0 -> 0,0"   chaining, residual="4385,0"
travel="-7749,0 -> 0,0"  chaining, residual="9172,0"     <- and the sign flips
travel="2583,0 -> 0,0"   chaining, residual="1292,0"
travel="1722,0 -> 0,0"   chaining, residual="3645,0"
```

Each retarget is individually continuous, so nothing tears, but the destination
keeps changing and the strip visibly jerks.

The distance now comes from the strip's own scroll offset, which the layout owns:
`strip_scroll_offset(space)` before and after, negated because windows travel
opposite to the viewport. It changes exactly once per press, and it needs no
reference to any window's real frame, so the clamp is irrelevant to it. A pass that
did not move the strip reports zero and starts nothing:

```
1:04:44.994  cmd=MoveFocus(Right)
1:04:44.997  canvas animation, requested=19, tiles=19
1:04:45.174  cmd=MoveFocus(Right)      focus moved within the visible pair, no scroll, no animation
1:04:45.347  cmd=MoveFocus(Right)
1:04:45.351  canvas movement finished, frames=21, elapsed_ms=350
```

Reading it off the windows survives as the fallback for a layout with no strip.

### What the remaining delay is

Worth recording so it is not mistaken for rini. Pressing right three times quickly
produced a fourth animation a second later, and the log names the cause:

```
1:04:45.347  cmd=MoveFocus(Right)
1:04:46.290  Carbon: App front switched (1077)     Electron app, 940ms after the press
1:04:46.403  canvas animation
```

macOS took most of a second to make the app frontmost. rini follows focus, so the
strip moves when the activation lands, not when the key is pressed.

## A strip scroll is one movement, so it has to be one canvas

The per-window path interpolates each tile from its own real frame to its own
layout frame. For a one-column step that is indistinguishable from a pan, because
every window moves by the same vector. For a jump over several columns it is not:
the tiles arrive at different times and overlap, which reads as the strip
telescoping out like an antenna rather than sliding. Pressing again before the
first move finishes makes it worse, because every tile is retargeted
independently from wherever it happens to be.

The canvas path has neither problem by construction. Tiles are assembled at their
strip positions and the VIEWPORT moves, so there is one animated property for the
whole strip and nothing to drift or race. It was already the vertical switch's
mechanism; the horizontal scroll simply was not choosing it.

What stopped it was the test for "is this one movement": every window had to agree
on a movement vector, and windows parked at the macOS clamp cannot. Their real
frame is 40pt off the left edge while their layout frame is thousands of points
away, so their apparent movement is nothing like the strip's, and one of them
disqualified the whole strip. Only windows at least a quarter on screen vote now.
They are never clamped, so they are the honest witnesses.

Measured on three `MoveFocus(Right)` presses 120ms apart, which is faster than the
350ms animation:

```
before:  overlay animation composition, requested=16, tiles=3     per-window
after:   canvas animation, requested=18, tiles=18, travel="-58,0 -> 0,0"
         canvas animation, requested=18, tiles=18, travel="-1722,0 -> 0,0"
                chaining onto an animation already in flight, residual="0,0"
         canvas animation, requested=18, tiles=18, travel="-1722,0 -> 0,0"
                chaining onto an animation already in flight, residual="-98,0"
```

-1722 is exactly two columns of 861pt as one viewport slide, and the third press
picked up 98pt short of the second finishing.

### Why retargeting mid-flight is continuous

Worth writing down, because it is the property that makes rapid presses safe and
it is not obvious from the code. Let `c` be a window's position in the CURRENT
layout, so its screen position is `c - offset`.

```
press 1   d1 = new1 - old1,  from = d1,  to = 0
          t=0:  screen = new1 - d1 = old1                  starts where it was
mid-flight at eased e:  offset = d1(1-e)
press 2   d2 = new2 - new1,  canvas rebuilt so c = new2
          residual = current - to = d1(1-e)
          from = d2 + residual
          t=0:  screen = new2 - (new2 - new1) - d1(1-e)
                       = new1 - d1(1-e)                    exactly the frame before
```

So each press sets a new destination for the same viewport rather than restarting
anything, which is what the sign of `from_offset = delta` buys. Reversing it would
start every chained press on the wrong side.

## A one-point size change sent the whole strip to the Accessibility engine

The overlay animates a picture, so a real size change would stretch that picture
instead of re-rendering the window. Those go to the Accessibility engine, which
resizes for real. The test for "real" was 1pt, and the layout rounds.

Measured on one `MoveFocus` along a 16-column strip. The layout took one window
from 918pt to 917pt wide, which is what a strip re-fit does:

```
idx=104  width: 918.0 ... 917.3475 ... 917.0896 ... 917.0022 ... 917.0
```

`|918 - 917| < 1.0` is false, so `all_translations` was false, so the ENTIRE
layout went to the Accessibility engine. What that costs, from the same recording:

```
612 request=AnimationFrames        per window, per frame, one IPC each
 60 request=BeginWindowAnimation
 60 request=EndWindowAnimation
  0 overlay animations
```

Every window is then written separately and applied on its own app's schedule, so
the strip visibly comes apart: the terminals arrive while the Electron windows are
still in transit, which reads as windows expanding at different speeds from under
each other. The overlay exists precisely to make that impossible.

The threshold is now the one the overlay already uses to decide whether a cached
picture still fits a frame, +-0.5%, so the two agree by construction. Rounding is
no longer a resize. A genuine column resize still goes to the Accessibility
engine, and still tears; nobody has complained about that yet, and fixing it means
choosing between a stretched picture and torn motion.

## macOS will not park a window further off the left edge than 40pt

Not an overlay problem, recorded here because it looks like one. At rest, every
window scrolled off the viewport sits with exactly 40pt showing:

```
6 windows  x=-819   w=859    -819 + 859 = 40
3 windows  x=-1680  w=1720   -1680 + 1720 = 40
2 windows  x=1688   w=1720   1728 - 1688 = 40
```

Exactly 40pt regardless of width, which is the signature of a rule about the
window rather than about the layout. rini asks for the real strip coordinates:
the frame writes in the log carry x values from -12396 to 15848 for a strip
16 columns wide. The window server puts them at 40pt and rini learns the clamped
position back through Accessibility.

The workspace-hide path does better, and how is the clue: it parks windows off the
RIGHT edge, `screen.max().x - 1.0`, and they sit there with 1pt showing. So the
constraint is asymmetric. A window may go almost entirely off the right, but not
off the left.

Inferred, not proven by a direct write test: the clamp keeps a window's left
portion reachable, which is where its titlebar controls are.

## There is one overlay, so it follows the space being animated

The overlay is a single window on a single display. Which display it sits on was
chosen from the ACTIVE display, meaning wherever the menu bar and cursor are, and
that is not necessarily the display whose windows are about to move.

Measured over one session of cmd-tabbing between two windows of the BUILT-IN
display, with the cursor left over on the external one:

```
strip="0,0 1728x32"   167 animations   overlay on the built-in display
strip="0,0 3008x32"    17 animations   overlay on the external display
```

Those 17 animated space 1's windows, whose canvas was built in the built-in
display's coordinates, on an overlay covering the external display. The external
screen showed the built-in's windows sliding around, its own windows disappeared
behind an opaque overlay for the length of the animation, and the built-in's real
windows snapped to their new frames with no animation at all.

Both canvas builders already resolve the screen from the space they are animating,
so they now publish THAT display. `publish_animation_display()` without a space
survives for the debug commands and a config reload, which have no display in
mind.

The other half of the same bug: moving the overlay to another display invalidates
every picture it holds. A display change cleared only the bar, and one frame later
the log showed

```
overlay dressed  backdrop="3008x1692 ScreenCaptureKit"  bar="1728x32"
```

an external display's desktop drawn behind a built-in display's strips. The four
held pictures are one struct now, forgotten as a unit by assignment, so a new one
cannot be left behind.

## A capture can be usable and still be the wrong shape

Two different questions, and the code only asked the first for a long time:

- Does the capture cover the window it was taken FROM? That is
  `Coverage::is_usable`, and both sizes in it are recorded at capture time.
- Does the picture match the frame it is about to be drawn INTO? Nothing asked.

A cached picture routinely fails only the second. A window that was full width
when captured and half width in the new layout has a perfect picture of the
wrong shape, and `contentsGravity` defaults to resize, so it is squashed to
fill. Measured over 1114 logged tiles, 75 were in this state:

```
drawn into  859x1081  but picture covers 1720x1081   squashed to half width
drawn into 1499x1656  but picture covers 1720x1081   squashed and stretched at once
```

`fits_frame` now rejects those. A window left undrawn appears at its destination
when the overlay lifts, which is far less noticeable than a warped one, and the
refresh after an animation captures at the frame the window was sent to, so the
next switch has a correctly shaped picture.

### 1499x1656 is not a frame this display can hold

Worth following up separately. The laptop display is 1728x1117 points, so a tile
frame 1656pt tall cannot belong to it; it fits the external 3008x1692. Windows
homed on the other display are turning up in this display's canvas carrying
their own frames, which is both a stretched tile and a real window being resized
to a size that does not fit the screen it is on.

## What the overlay is NOT doing

Recorded because it took a long time to establish and would otherwise be
re-investigated. Reported as "the windows are scaled", the overlay's tiles were
measured and are not the cause:

- Every tile's rendered rect equals the frame it was given. Checked with
  `convertRect:toLayer:` through the whole chain, which accounts for every
  transform: `asked="4,2266 1720x1081"`, `on_root="4,2266 1720x1081"`.
- The window, its content view, the view's bounds and the root layer are all
  1728x1117 points at backing scale 2. Nothing in the chain scales.
- Tile frames over 1218 logged tiles are only ever 859x1081, 1720x1081, 723x879,
  1499x1656 or 943x1081. None is half-size.
- ~~ScreenCaptureKit fills the buffer it is asked for, at either resolution
  setting.~~ WRONG, and this is the error that cost hours. It was measured
  against `SCScreenshotManager.captureImage`, which rini does not use. The
  sample buffer path underfills at `.nominal`, and that is precisely what drew
  the content at half size in a corner. See "Nominal capture resolution paints a
  quarter of the buffer". The lesson generalises: measure the API the code
  actually calls.
- The backdrop and the bar, which are siblings of the canvas, render 1:1.

A `check_geometry` call now asserts the first point on every animation and logs
only when it fails, so a scale introduced in the layer tree cannot go unnoticed
again.

## Nominal capture resolution paints a quarter of the buffer

The one that caused "the windows are scaled". `SCStreamConfiguration.width` and
`.height` are in PIXELS, and the service asks for the window's point size times
the backing scale. With `captureResolution = .nominal` ScreenCaptureKit renders
the window at its POINT size into that pixel-sized buffer, so on a 2x display
the window fills the top-left quarter and the rest is transparent. Measured on
one window, same buffer each time:

```
window 859x1081 points, buffer 1718x2162 px

  nominal    painted  859x1081 of 1718x2162    50% x 50%
  best       painted 1718x2162 of 1718x2162   100% x 100%
  automatic  painted 1718x2162 of 1718x2162   100% x 100%
```

A layer handed that surface draws the whole buffer, so the window appears at
half size pinned to a corner of a tile that is itself exactly the right size.
Which is precisely what it looked like.

### Why fixing it in the layer is the wrong fix

`contentsRect` set to the painted quarter does make the window the right size,
and it is wrong: that quarter holds 859x1081 pixels being stretched over
1718x2162 backing pixels, so every window renders soft. Measured by eye
immediately. The capture has to produce the pixels; the layer cannot invent
them.

### The two paths do not agree, which is what hid it

`SCScreenshotManager.captureImage` returns a tight, fully painted image at
`.nominal`, so a spike written against the image path measures 100% fill and
proves nothing about the path in use. rini uses `captureSampleBufferWithFilter`.
Only that path underfills, and its sample buffers carry no attachments here, so
there is nothing to read back:

```
CMSampleBufferGetSampleAttachmentsArray -> NO attachments
```

The display filter used for the desktop backdrop does NOT underfill at
`.nominal`, which is why the wallpaper always looked right and only windows were
affected.

### What ruled the layer out

Recorded because it took several wrong turns. Four squares of identical size,
each given the same surface and a coloured background, on a 2x display:

```
baseline, unit contentsRect        75% of the background still showing
contentsRect 0,0 0.5x0.5          background down to 104 pixels: fills, but soft
contentsGravity resize, explicit  same as baseline, so gravity is not the lever
surface inside a child layer      same as baseline, so it is not the parent
```

`contentsScale` is not the lever either: 1.0 and 2.0 draw identically. Every
layer property reads correctly, which is why the layer tree looked innocent for
so long. Tile layers were never wrong: `convertRect:toLayer:` matched the
requested frame on every tile, and colouring each tile's background showed
rectangles at exactly the right sizes and positions.

## A picture is not the window, and the differences show at the handover

Three separate reports, one shape: the tile is a picture taken at a different
moment and in a different context than the real window, so anything the app
renders differently shows as a flicker when the overlay lifts.

### Stale content

The cache is refreshed after an animation finishes, so a picture shows the
window as it was at the END of the previous switch. Off screen that is
invisible. For the window just being typed in it is not: it animates out holding
content from whenever the workspace was last entered.

Fixed for the windows that are on screen, by recapturing them through SkyLight
at switch time, frontmost first and capped at three. Measured: one window, 32ms
to 35ms once warm, against the roughly 400ms that capturing all eighteen used to
cost.

Not fixed for the window being switched INTO. SkyLight reads the framebuffer and
the destination workspace's windows are not on screen yet when the animation
starts.

### Unfocused rendering

Because the destination cannot be recaptured, its picture is whatever it last
had, and if that was taken while the app was unfocused the tile slides in dimmed
and snaps to focused at the handover. Ghostty greys out noticeably when
unfocused.

Fixing it needs a capture after focus has landed, which is either one SkyLight
capture mid-animation, once the destination is on screen behind the overlay and
costing a frame or two of jank, or accepting that the first arrival is wrong.

### Translucency is three things, and only one of them is lost

An earlier version of this section claimed translucency is flattened at capture,
on the strength of a histogram reading 100% opaque. That histogram sampled every
fourth pixel and rounded, so it missed the corners. Corrected below.

Sampling EVERY pixel of a `.best` capture of a 90%-opacity terminal:

```
kCGWindowAlpha                       1.000
every pixel   99.9764% opaque, 0.0064% partial, 0.0172% clear
top-left corner, row 7, columns 10-11        1, 67
centre pixel                                   255
```

**Per-pixel alpha the window owns is preserved.** The corner ramp is right
there: alpha 0 across the rounded corner, climbing through 1 and 67 as the curve
ends. The non-opaque share is small only because four corners are a small part
of a 2294x2162 image. Rounded corners, chromeless windows and genuinely
transparent regions all come through, given `shouldBeOpaque = false` and a BGRA
pixel format, both of which the service already sets.

**Window-level alpha is not in play here.** `kCGWindowAlpha` is 1.000. If a
window were dimmed below 1.0, captured pixels would come back scaled by it and
the factor could be divided back out, which is what yabai's
`cgimage_restore_alpha` does.

**Blur-behind material is the one that is lost**, and it is what this terminal's
`background-opacity = 0.9` actually uses: the centre pixel is fully opaque, so
the 90% is not in the window's own pixels at all. The server samples what is
behind the window and blurs it at composite time, and no per-window capture on
any API can contain that.

Measured on Outlook's vibrancy sidebar, sampling the same patches through all
three routes with the window on screen and unoccluded:

```
patch        SkyLight      ScreenCaptureKit   the screen
50% x 80%    52, 40, 33    52, 40, 33         52, 40, 33     opaque, identical
25% x 55%    84, 84, 83    84, 84, 83         55, 59, 53     vibrancy
75% x 35%    79, 70,141    79, 70,141         79, 70,141     opaque, identical
50% x 95%    82, 82, 81    82, 82, 81         62, 56, 52     vibrancy
90% x 65%    33, 33, 33    33, 33, 33         33, 33, 33     opaque, identical
```

The two per-window routes agree with each other EXACTLY and both differ from the
screen only in the vibrancy regions, where they return a flat neutral grey about
25 units too bright and missing the colour cast of the wallpaper behind. Opaque
regions are pixel-identical everywhere. So no choice of capture API changes
this. The same comparison on a terminal over a dark background differs by 3 to 4
units, which is why a sidebar shows this and a terminal barely does.

Masking an `NSVisualEffectView` to the fractional-alpha regions is the textbook
fix and does not work here: the capture is fully opaque exactly where the blur
belongs, so there is no mask to derive.

What DOES contain the blur is a display capture cropped to the window, which is
the third column above, and for an unoccluded on-screen window it is pixel-exact
including the shadow. It cannot run at switch time, being a ScreenCaptureKit
call, but it would fit the background refresh, at the cost of baking whatever
was behind the window into the picture. yabai accepts flat translucency on the
bet that nobody notices in 200ms, which over a calm background is the right
call.

### Plain transparency IS capturable, so the blur can be traded away

Measured on two Ghostty windows, one launched with `--background-blur=false` and
one with the configured `background-blur = macos-glass-regular`, both at
`background-opacity = 0.9`:

```
blur off   centre pixel alpha 230   99.16% of pixels partial
blur on    centre pixel alpha 255   99.98% of pixels opaque
```

230 is 0.9 of 255. With the material disabled the app draws its background with
real per-pixel alpha in its own surface, which every capture route preserves,
and a tile drawn over the desktop backdrop then reproduces the real appearance.

So the blur discrepancy is a choice rather than a wall: an app configured for
plain transparency animates correctly, and one configured for a system material
cannot. Worth knowing before building anything elaborate to approximate the
material.

### A tile is pixel-identical to its window, except for the shadow

Worth measuring before adding anything to a capture, because the answer was that
nothing is missing. Slack, 1720x1081, opaque, on the built-in display, comparing
the tile capture against the same rect of the screen at native pixels:

```
region              pixels     mean diff   worst
outer ring (2px)     22024       0.201     162
edge band (8px)      66072       0.000       0
corner box (24px)     2304      12.472     162
interior           7346880       0.000       0
```

The interior and the 8px edge band are identical to the value. There is no
border, hairline or outline to add: whatever chrome the window draws is in its own
surface and comes back in the capture. The corner boxes differ only because a
tile's corners are TRANSPARENT, being the window's own rounded corners, so the
comparison there is between a tile pixel and whatever the screen has behind the
window. On screen that transparency shows the desktop, and in the overlay it shows
the backdrop, which is a picture of the same desktop.

### Shadows are never in the surface

On any API. The window server generates a shadow at composite time from the
window's shape, so a clone has none.

Asking SCK for one is a bad trade, measured. `ignoreShadowsSingleWindow = false`
does not grow the image, because the buffer is whatever `width` and `height` say.
It fits the window PLUS its shadow into that buffer instead, so the content ends
up inset and smaller:

```
Slack 1720x1081, buffer asked 160pt larger than the window on both axes
content starts (from each buffer edge): left 68px  top 52px  right 252px  bottom 268px
```

Every tile would then need the `contentRect` attachment to know its own inset,
and the sample buffers rini captures through carry no attachments at all.

So the shadow is drawn by Core Animation instead, from the shape of the real one.
Read out of that same shadow-inclusive capture, as a share of black at distances
from the window edge:

```
3.5pt  0.180      the peak, right at the edge
  7pt  0.129
 14pt  0.059
 17pt  0.035
```

The bottom reaches about twice as far as the top, which is a downward offset. That
fits `shadowOpacity = 0.4`, `shadowRadius = 9`, `shadowOffset = (0, 5)`, with
positive y meaning down because the tiles hang off a flipped view. `shadowPath` is
an explicit rounded rect at a 10pt corner radius, measured from where a capture's
own alpha starts along its top row: transparent for the first 18px to 20px at 2x.
An explicit path rather than letting Core Animation derive one from the contents
alpha, which is exact for an oddly shaped window but recomputed whenever the
contents change.

`masksToBounds` had to come off the tile layers to allow this: it clips the layer's
own shadow away. Nothing was lost, because Core Animation resizes contents to the
layer's bounds, so a picture cannot spill regardless.

Verified mid-flight, luminance across a tile boundary where one tile's shadow falls
on the tile behind it:

```
x=1080..1125   lum 32 -> 27 over 22pt, smooth      the shadow
x=1126         lum 14, hard step                   the next tile's content
```

16% darkening at the edge against the 18% measured on a real window.

### A layer shadow covers the whole layer, which is not what a window shadow does

The first version put the shadow on the tile itself, and that is wrong for any
window with per-pixel alpha. Core Animation draws a shadow behind the WHOLE layer,
including under its own area. Under an opaque window that is invisible. Under a
terminal at 95% it shows through the glass as a wash across the entire window,
which was reported immediately and is visible at a glance with the terminal set to
`background-opacity = 0`: the window's background goes translucent-dark rather than
showing the desktop.

The window server does not do this. It clips a window's shadow to the outside of
its shape, precisely because windows can be translucent.

Measured in a spike, two arrangements side by side over white, each under a 50%
grey stand-in for a translucent window:

```
shadow on the tile itself   interior 0.45 grey     washed dark
masked caster behind it     interior 0.62 grey     clean
the window's own colour over white would be 0.55
```

So a tile is two layers now: the picture, and a caster behind it carrying nothing
but the shadow, masked by a `CAShapeLayer` whose path is the mask's own rect plus
the window's rounded rect, wound even-odd, so only the ring outside the window is
drawn. `SHADOW_REACH` is 40pt, comfortably past the 17pt where the measured ramp is
spent, so the ring never clips the blur into a straight edge.

The shape is rebuilt only when a tile's size changes, which is once per animation:
a movement changes where a tile is, not how big it is.

Verified against the screen, signed difference in the 4pt gap at a window's left
edge, positive meaning the overlay is darker:

```
shadow on the tile      +2.1     gap AND under the window
masked caster           +1.2     gap only
```

### The outline is captured, but it can be the wrong one

Asked for alongside the shadow. Nothing is missing from the capture: a tile compared
against the same rect of the screen at native pixels gives

```
edge band (8px)   mean 0.000    identical to the value
left edge, pixel by pixel:  tile 21,23,26  screen 21,23,26   diff 0
```

But a window's border depends on focus. Profiled across the 2pt gap between two
Ghostty windows, one focused and one not, at 2px per point:

```
lum 17-18   the focused window's interior
lum 65      its 1pt border                  <- bright
lum 1-23    the gap: shadow, then wallpaper
lum 42      the unfocused window's border   <- dim
lum 13      its interior
```

65 against 42 of 255, on a 1pt line, and none of it is a size change. The ordinary
warm only recaptures a window whose picture no longer FITS its frame, so a capture
taken while a window was unfocused stayed forever, and its tile popped from the dim
border to the bright one when the overlay lifted. That is the flicker that prompted
the request in the first place; the shadow was only half of it.

A focus change now asks for a fresh picture of BOTH windows, the one gaining focus
and the one losing it, straight to the capture service with no size test in the way.
Background work, so it costs nothing on the way into an animation.

## A render of the wrong display, drawn at its own size

The wallpaper appeared to zoom in during some animations and not others. Two
compounding causes, both about a desktop render outliving the display it was taken
of:

- `SnapshotService::set_scale` was the only invalidation, and it acts on the backing
  scale. Both displays here are 2x, so moving the overlay between them invalidated
  nothing, and a render already in flight for one display landed as the cached
  desktop for the other.
- `capture_backdrop` size-checked the SkyLight composite but handed the cached
  ScreenCaptureKit render over unchecked.

The backdrop layer is sized from the picture rather than from the overlay, which is
what keeps it in register with the real desktop. Given a 3008x1692 render of the
external display, that lays the wallpaper out at 3008x1692 inside a 1728x1117
overlay, so only its top-left corner is visible: the wallpaper looks zoomed in until
the right render arrives, which is exactly the intermittence that was reported.

`spans_display` now guards both routes, and a display change invalidates whatever is
in flight.

Two things this rules out, both measured while chasing it. The desktop capture fills
its buffer at nominal resolution, unlike a window capture: painted to 100% x 100%
through `captureSampleBuffer`, the same API rini uses. And nominal against best for a
display capture is identical to the value, mean 0.000.

### Keep alpha premultiplied end to end

These captures are premultiplied. Dark halos around a tile's edges mean it was
premultiplied twice, light halos mean straight alpha was treated as
premultiplied. That fringe is the fastest way to locate an alpha bug.

## Three numbers the design rests on

Recorded here because they used to live in source comments, and they are the
whole argument for three decisions that otherwise look arbitrary.

**Cross-app tear, 100px to 150px.** Measured between neighbouring windows mid
scroll, with the old engine writing `AXPosition` to every animating window on
every frame. Each write is a synchronous request into a different process and
those processes answer at their own speeds, so the windows never landed
together. Per-app batching reduced it and could not remove it. This is why the
overlay draws pictures instead of moving real windows: one composited frame
cannot tear.

**Stretching a clipped capture, 13.5pt out.** Accepting a capture that covered
92% of its window and stretching it to full size put the animation 13.5pt off
vertically on a live switch, which read as the whole animation being misaligned
and jumpy. Hence `MIN_USABLE_COVERAGE` at 0.995, which is strict enough to
reject anything genuinely clipped while still absorbing a pixel or two of
rounding.

**Manual rasterisation, over 800MB resident.** A raw window server window cannot
have its layer tree bound here, since `SLSSetWindowLayerContext` fails with
`kCGErrorFailure`, which leaves a manual `renderInContext` fallback. That
fallback rasterises every tile's `IOSurface` into CPU memory on every frame and
measured above 800MB resident for a single animation, peaking near 856MB.
Reusing one context did not help, which is what proved the cost was
rasterisation rather than allocation. A layer-backed `NSWindow` composites on
the GPU for about 48MB.

## The test suite runs single-threaded on purpose

Several tests reach macOS frameworks that initialise lazily, and initialising
them from more than one thread at once aborts the whole process:

```
objc[75992]: Cannot form weak reference to instance of class
SLSWindowManagementFallbackBridge. It is possible that this object was
over-released, or is in the process of deallocation.
```

Measured rates, 40 parallel runs each: none at all before the overlay work, and
a few percent after it. Narrowing it was misleading, because skipping a test
changes timing as well as coverage, so two different "confirmed" causes both
turned out to be sampling noise. Single-threaded runs never reproduced it across
88 runs.

`RUST_TEST_THREADS = "1"` in `.cargo/config.toml` settles it. The suite goes
from about 0.4s to 1.25s, which is not a trade worth thinking about. The tests
that build capture fixtures also avoid `IOSurface` entirely for the same reason
and use a CPU bitmap instead.

### A clipped destination needs ScreenCaptureKit, not SkyLight

The recapture below cannot use SkyLight when the destination is mid-slide,
because SkyLight reads the framebuffer and returns only what is visible.
Measured while focusing an adjacent window on a strip:

```
destination recapture rejected, idx=54, covered="40x1081", wanted="1147x1081"
```

40pt of a 1147pt window, correctly rejected as a sliver. A workspace switch does
not hit this, because the destination workspace's windows are already at their
final positions and simply hidden, which is why the vertical case worked first
time and focusing an adjacent window still arrived unfocused.

So a clipped destination is routed to ScreenCaptureKit instead, which returns
the window's own surface at full size whatever its visibility. That path is
already asynchronous, and a landed capture is now applied straight to the tile
if an animation is still running, which is how it reaches the screen before the
handover.

### Both drawing paths have to dress the overlay

Only the canvas path set the backdrop and the bar. Focusing a window on a strip
goes through the per-window path, which set neither, so a horizontal slide
showed whatever those layers happened to hold: nothing at all until some
vertical slide had filled them, and the last picture afterwards. That matches
the report exactly, of horizontal slides being almost black until a vertical
slide, then working for a while.

The per-window path now dresses the overlay too, reusing the held picture rather
than capturing. A desktop composite measures 13ms to 36ms for 6 windows, which
is a frame or two of lag on every window focus change, while re-applying a
picture already in hand is a pointer assignment. The background refresh keeps
that picture current.

### Cached surfaces have to be marked in use

Suspected cause of the other half of the same report, that it works for a few
minutes and then goes black again. An `IOSurface` whose `CVPixelBuffer` has been
released is eligible to have its backing store reclaimed, and a layer still
holding it then draws nothing. The overlay sits at alpha 0 between animations,
so nothing composites those surfaces for minutes at a time.

`increment_use_count` on every cached capture prevents it. This one is reasoned
rather than reproduced: purging happens on the system's schedule and did not
reproduce inside a test session.

### Recapturing the destination once it is on screen

The window being switched INTO cannot be recaptured when a movement starts,
because the destination workspace's windows are not on screen yet. Its picture
is therefore whatever it last had, and if that was taken while the app was
unfocused the tile slides in dimmed and snaps to focused at the handover.
Ghostty greys out noticeably, so this was the most visible flicker left after
the sizing fixes.

Fixed by recapturing it at 12% progress, by which point the reactor has shown
the workspace and moved focus, so SkyLight returns the focused rendering.

On the main thread this cost too much. Measured over a 494ms flight, capture
times of 38ms to 179ms dropped the frame count from 30 to as low as 26. Moving
it to its own thread removes that entirely: 17ms to 56ms of capture, and 30 of
30 frames on every switch. `SLSHWCaptureWindowList` is safe off the main thread,
which yabai relies on as well, capturing on a pthread per window at
`window_manager.c:666`. The picture arrives back as an event a few frames later
and replaces contents only, never geometry, so a late arrival cannot disturb the
movement.

This does nothing for blur, which no capture contains. See the section above.
