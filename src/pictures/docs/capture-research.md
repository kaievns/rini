# Capture research: what each route costs and returns

Measured constraints on picturing windows, the desktop and the bar for the overlay: what each capture
route costs, what it returns, and what that forced on the design. The early numbers come from spikes
in `/tmp/sls-spike` run on 2026-08-16, against a live 1728x1117 built-in Retina display at 2x backing
scale, with 35 real app windows across 4 rini workspaces; later sections date their own. The code is
`src/pictures/`. How the overlay draws what these routes return is
`src/animation/docs/capture-overlay-research.md`.

Status: the approach is viable. The design is a hybrid: capture on-screen
windows fresh through SkyLight at the press, as they land, as their look turns
and before the lift, and again at rest once a change of focus has turned their
look; serve off-strip and hidden-workspace windows from a cache ScreenCaptureKit
warms in the background.

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

## What a capture costs in a press

### Frame zero, a kept filter and a stream

Measured 2026-10-02 in the runs of "Sequencing a focus change behind the overlay"
(`src/animation/docs/capture-overlay-research.md`, branch `scratch/perf-timing`): load average 6 to 11
with the endpoint agents busy, so the absolute numbers are a loaded machine's; the comparisons hold.
Times in ms, p50/p99.

**Frame zero:** one batched SkyLight call of the on-screen windows, 26-33 / 55-76, against 71 / 163
for one call per window (three windows). A crop of the batched picture is identical to the window's
own capture (0 to 0.02% of pixels differ) unless windows overlap: a floating window under two tiled
ones put 60% of the pixels off.

**No repaint notification.** `WindowUpdated` (723), registered with every window asked for through
`SLSRequestNotificationsForWindows`, delivered nothing in 40s across 128 windows while five windows
changed, among them a parked VS Code seven times. Parked windows do repaint.

**A ScreenCaptureKit filter can be kept.** One window list, then each window captured through its
kept `SCContentFilter`: 22.8ms against 83 for a fresh list plus 26 for the capture, and the same
picture in 61 of 63 windows, 62 of 63 after four strip moves; the misses were windows whose content
was changing.

**A low-rate `SCStream` costs nothing while a window is still and a lot while frames flow.** Four
streams on still windows delivered one frame each in 15s. Sixteen streams at 2fps delivered 855
frames with pixels in 30s, of which 90 differed from the one before, and held the WindowServer at
43% CPU against 9.5% without them. A frame's arrival is not a change signal; comparing it is.
SkyLight captures were not slowed by the streams.

### A SkyLight capture costs what the time since the previous one says

Measured 2026-10-03 on an idle machine with the `tools/focuslab/` probes (`spacing.swift`,
`coldsize.swift`, `gpuwarm.swift`, `qos.swift`, `keepalive.swift`, `par.swift`; see
`tools/focuslab/README.md`). It is the time since the last capture, not the picture's size:

```
gap since the last capture   0     5     10    20    35    60    120   300   800 ms
capture, p50                 15.9  15.7  16.8  21.7  24.1  29.9  39.1  43.0  43.3 ms
```

4.9 and 7.4 Mpx cost the same cold (39-47) and warm (21-23). A 20ms CPU spin in the calling
process before a cold capture cut it to 33; a 20ms GPU burst, the caller's QoS, a 1pt animating
window and tiny keep-alive captures did nothing. Calls on separate `SLSNewConnection`s still run
one at a time. The frame-zero capture is always the cold one: 27-35 in a press.

### What the prototype's journal showed of a focus change

Measured 2026-10-03 with the handover prototype on branch `scratch/handover`, from its transitions
journal of 225 flights between 19:34 and 20:40 (`docs/data/2026-10-03-focus/transitions-1934-2040.log`
on that branch; a flight is a group with a `proto_capture_flight` mark).

**The focus report often comes before the watch.** The reactor's focus report (`ws_focus`) came
before the capture worker started its flight (`proto_capture_flight`) in 136 of the 225 flights. The
prototype dropped a report that came before its flight, and 135 of those 136 had no focus capture
(`why=focus`); of the other 89, 42 had one. A `Change` watch created within a second of the report
starts armed (`src/pictures/domain/watch.rs`).

**A refresh behind the strip ahead of the focus capture holds it up.** With the windows behind the
strip refreshed from the start of a flight (`bg=1`), a refresh held up the focus capture for 31ms in
one flight of a smoke run on the comms workspace at 19:36, recorded in the prototype's working notes
(`STATE.md` on that branch). The next build refreshed them only once the focus capture was done
(`bg=2`), which is the `Live` watch's `after`.

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

This is the SkyLight SPI yabai uses. It works and it is fast, but it returns only
the part of the window inside a display's bounds. A window with 40pt showing
returns a 40x1081 image. A window on a hidden workspace returns 1x28, a parked one
2pt wide.

Covering windows do not matter. Measured 2026-10-02: Zoom sat behind two tiled
windows that covered nearly all of it, and the capture returned the whole Zoom
window, current content included, with neither covering window in it. The screen
in the same rect showed the two windows. The capture is the window alone, like
`CGWindowListCreateImage` with `OptionIncludingWindow` (0.6% of pixels differ): a
translucent Ghostty came back with its own per-pixel alpha (3.9% of pixels
opaque) and nothing from behind it, and no blur-behind or hairline (see
"Translucency is three things" and "The hairline is composited outside every
capture").

Batched behaviour is worth recording because it surprised us. Passing 19 window
ids returns 1 composited 3456x2170 image of the display, not 19 images, in about
21ms. That composite is correct and full fidelity for on-screen content. It
remains useful for anything that only needs what is currently displayed. It
cannot drive a slide-in, because incoming windows have no pixels yet.

Per-window calls cost about 16ms each, so batching beats them 12 to 1.

**SkyLight in parallel** (2026-10-02, 10 rounds): calls are served one at a time.
One call 27ms; two at once 48ms for both, four 122ms, eight 205ms, so threads
buy nothing. A call issued behind four ScreenCaptureKit captures took 204ms, and
those captures 131ms each; beside a framed capture it took 32ms. During an
overlay slide it took 22.7ms median (45.8 p99), so an animation does not slow
it; queued ScreenCaptureKit work does.

**No option bit makes a material transparent.** All 32 bits, alone and added to
yabai's, on Outlook, whose vibrancy covers 20% of the window and differs from the
screen there by 33 levels: none changed a pixel in those regions. Bit 0 returns
a larger image (12px wider, 76px taller at 2x), bit 9 a 1x image, bits 16 and 17
nothing. A blur-behind region can only be found by comparing against a capture
of the window with what is below it.

**A batched call composites in the order given, front-most first, and carries the blur of whatever
it composites behind a window.** Outlook's vibrancy regions against the screen, mean difference in
levels (2026-10-02):

```
Outlook alone                                  24.9    flat grey, as every per-window route
desktop windows first, then Outlook            27.3    the wallpaper drawn over Outlook: no Outlook at all
Outlook, then the desktop windows               0.5    the real blur, matching the screen
Outlook, then every other window, then desktop 21.4    windows parked or hidden at the edges composited in
window plus below (CGWindowListCreateImage)     0.0
```

The desktop windows are the six at a negative window level (wallpaper, icons, widgets). The same
call with the desktop windows alone is a picture of the desktop, icons and widgets included, in
42ms.

**A sidebar that samples the window's own content captures whole.** Messages'
macOS 26 glass sidebar came back identical to the screen through every route:
its material blurs the window's own pixels, which are in the surface. Only a
material that samples what is behind the window is lost.

### CGWindowListCreateImage is gone from the SDK

Deprecated since macOS 14 and removed from the SDK in 15: a call from Swift is a hard
compile error, "Please use ScreenCaptureKit instead". It is still in the dylib, and
`objc2-core-graphics` still binds it. rini no longer calls it: it waits on the main connection's
last Core Animation transaction, and `SLSCaptureWindowsContentsToRectWithOptions` on rini's read
connection returns the same two composites, the window with its framing ("The hairline is
composited outside every capture") and the window with what is below it ("A composite of the window
and what is below it carries the blur"), pixel for pixel: with bits 11 and 8 for a window wholly on
a display, with none for one that is not ("The read connection answers as the main one does" in
`src/windows/docs/window-server-reads.md`).

It must never be called inside a ScreenCaptureKit completion. On modern macOS it is
proxied through the same capture machinery, so a call from the delivery queue deadlocks
its own reply until a ~20s timeout, and every capture in the process serialises behind it
(measured as half-minute window switches). `SLSCaptureWindowsContentsToRectWithOptions` reaches the
same proxy in its disassembly and `SLSHWCaptureWindowList` does not
(`docs/data/2026-10-08-skylight-lock/callgraph.txt`); whether the first takes it at run time is not
measured, so no capture of either is made there. The engine's one framed capture, of a window that has just
opened, runs on the `transitions` thread as it composes; the service's SkyLight calls run on its
own thread.

The system tool inherited this. `screencapture -l <windowid>` fails with "could
not create image from window" even for a fully visible window.

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

- `SLSHWCaptureWindowList` requires the Screen Recording grant, not window
  ownership. No scripting addition and no SIP disable. Verified working from a
  plain unprivileged connection.
- It is much cheaper per window than ScreenCaptureKit: about 16ms against 38 to
  56ms. Worth using wherever it is sufficient.
- The transform convention is `CGAffineTransformMakeTranslation(-tx, -ty)`, the
  negative of the target origin in top-left space. This matches
  `SLSGetWindowTransform` returning the origin with its sign flipped ("Window server mutation
  does not work for foreign windows" in `src/animation/docs/capture-overlay-research.md`).
- The binding already exists in `crates/rini-skylight-sys/src/lib.rs`
  (`SLSHWCaptureWindowList`), so reaching it is cheap.
- Captures exclude the drop shadow, which is why yabai calls
  `sls_window_disable_shadow` on its proxies to match (`window_manager.c:473`),
  and it runs `cgimage_restore_alpha` when the source window's alpha is not 1.0
  (`window_manager.c:521-523`).
- Check the OS version and fall back to ScreenCaptureKit when the array comes back
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

**Warm cache, refreshed in the background.** At switch time the flight engine,
on the `transitions` thread, captures nothing itself. It composes from bitmaps the
cache holds, but for the windows on screen at the press, which one batched
SkyLight call on the pictures service's thread pictures while the overlay waits
for it at most 80ms ("Nothing is captured on the main thread on the way in"
below). This matches the original instinct that only on-screen and incoming
windows need fresh pixels, and the rest can be refreshed occasionally: through
ScreenCaptureKit for the windows off the strip, and through SkyLight for a window
on screen whose look a change of focus turned, which keeps its blur only that way
("The schedule" in `README.md`).

**Staleness is acceptable for a window that was off screen.** The bitmap is on
screen for the duration of one animation and is replaced by the real window at the
end. A slightly stale moving image of a window that comes in from a park is not
perceptible. One on screen is: seen through a translucent window, its content
changes while the flight runs, which is why the windows on screen are pictured at
the press and kept live behind the strip (Kai, 2026-10-03, "The pictures a flight
starts with" in `specs/animation.md`).

**rini needs the Screen Recording grant.** It did not have it when this was
written. (The rift-inherited overview overlay that first carried a
ScreenCaptureKit pipeline has since been removed;
`src/pictures/platform/screencapturekit.rs` is the pipeline now.)

**Capture on a thread of its own.** At 14.5ms per window, refreshing even a few
windows would blow several frames on the `transitions` thread, which composes a
flight, or on the main thread, which runs the renderer. Every capture runs on the
`pictures` thread.

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

Two consequences followed, and both have since been fixed in `pictures`' `domain/backdrop.rs`
and `platform/desktop.rs`:

- `desktop_backdrop_windows` excluded -2147483624 as `DOCK_LEVEL`, on the earlier
  reading that the Dock lives there. On this system that level holds the
  wallpaper, so the composite route dropped the one window it most needs. The
  ceiling is now `DESKTOP_CEILING` (-2147483600) and everything at or below it
  counts as desktop, the Dock-owned wallpaper included.
- `has_wallpaper` looked for an OWNER name containing "Wallpaper". The owner is
  "Dock" and it is the window NAME that carries the "Wallpaper-" prefix, so the
  flag was always false. Both keys are now checked.

Together those meant the composite route could not produce a wallpaper here at all,
and a wallpaperless composite was then accepted only when no desktop was drawn
yet. So every backdrop was the ScreenCaptureKit display render, and the
composite route was dead code in practice.

That held until a display change started clearing the held pictures, which reset
"we already have a good one" and let the wallpaperless composite through. The whole
background then went almost black for the length of every animation, which is the
symptom this section predicted.

Both halves are fixed. `is_desktop_layer` includes -2147483624, and the wallpaper is
recognised by window NAME as well as owner name. Verified: the composite now picks up
7 windows including `Dock / Wallpaper-`, and its mean brightness is 31.16 against
29.85 for the same region captured as a display. The remaining 1.3 is the widgets'
vibrancy, which no capture reproduces.

A composite without its wallpaper is no longer refused at all. The overlay lets go of
the desktop at every lift, so a flight is never dressed over one it could keep, and
a refused composite would leave the bare black window. Where the wallpaper is not a
window, as with two displays above, a flight before its display's first render lands
draws the icons on black.

### Which route serves, and why the render wins

The composite is cheaper and synchronous, so it looked like the natural primary.
Compared pixel for pixel against the display render of the same desktop, it is not:

```
mean |difference| per cell, 12 by 8 grid
row 0 (top 140pt)   17.0  7.2  1.1  18.5  9.3  12.2  26.1  1.4  3.6  10.8  7.3  7.4
rows 1 to 7          0.0  0.0  0.0   0.0  0.0   0.0   0.0  0.0  0.0   0.0  0.0  0.0
best alignment: dx 0 dy 0
```

Identical everywhere below the top band, and up to 26 of 255 inside it. That band is
the menu bar strip and Notification Center's widgets, whose vibrancy no capture of a
window's own surface can reproduce. It is also exactly the band that shows through
the bar, so a composite there changes the moment the overlay appears and the
background visibly flickers.

The render is the compositor's own output, so it matches by construction. The order
is therefore: cached render if it spans this display, composite only when no render
has landed yet, which is the first switch after starting or after moving to another
display. Observed over five animations from a cold start:

```
1 backdrop="1728x1117 SkyLight"            the first switch
4 backdrop="1728x1117 ScreenCaptureKit"    every switch after
```

Reusing the previous picture instead of asking again is what kept the cold-start
composite in place forever, so the render never got its turn. The pictures service now
renders the desktop whenever the display changes, and again at the quiet after a flight
that found it stale, and a transition is dressed with the render it holds; only a
transition before the first render of its display lands pays for a composite.

Either way the desktop has to be capturable as a DISPLAY, not only as a set of
windows:

```
SCContentFilter(display:excludingWindows:)  excluding every window at layer >= 0
  -> 3456x2234  brightness 23.4   wallpaper, icons, widgets, no app windows
```

This is why `screencapturekit.rs` has a desktop path. It costs the usual
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

## The bar has to be captured on its own, at the union's origin

This is about a bar under the overlay, which rini's own is not ("Level and coverage" in
`src/animation/docs/capture-overlay-research.md`).

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

The cause was one word in the warm filter. It skipped any window that
already had a "usable" picture, and usable means the picture covers the window it
was taken FROM, not that it fits the window's size NOW. A strip re-fit had widened
these from 859pt to 1147pt, so their 859pt pictures stayed usable, stayed
un-refreshed, and failed the fit test on every animation forever. The resize itself
then went to the Accessibility engine, which warmed nothing, so nothing else ever
corrected them.

`needs_capture` now asks the question the caller means: does the picture fit the
size the layout just gave this window. And a picture of the wrong shape is
drawn onto the frame rather than dropped, because 350ms of a misshapen window is
a great deal better than 350ms of a hole; it is cropped, with the window's ground
where it ends ("Resizes through the overlay" in
`src/animation/docs/animation-smoothness.md`). After both:

```
canvas animation, requested=20, tiles=20, missing=0, misshapen=0
```

## Nothing is captured on the main thread on the way in

The design said it from the start: at switch time the main thread captures nothing,
it composites bitmaps it already holds. The code had drifted, because a stale or
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

Both are gone from the main thread. The desktop is held and refreshed in the
background. Measured once they had gone, before any press waited for a capture,
command to first frame:

```
1:04:44.994967  cmd=MoveFocus(Right)
1:04:44.997095  canvas animation, requested=19, tiles=19, missing=0
```

2ms, against 56ms to 459ms before, with every tile present because the cache is
warmed after each animation instead of during the next one.

A press now waits for one capture on purpose, off the main thread: the windows on
screen at the press are captured in one batched SkyLight call on the pictures
service's thread, and the overlay waits for them at most 80ms, then again as they
land behind it, each arriving as an event that swaps a tile's contents without
touching its geometry. The overlay is up, frames sent, 53.4ms after the press at
the median and 71.9 at p90 ("Where a moving focus change spends its time" in
`src/animation/docs/capture-overlay-research.md`).

The trade: a window not on screen at the press, the destination of a switch or a
column coming in from its park, flies what the cache holds, its focused look when
it gains focus, until its landing capture is swapped in. For an app that dims when
unfocused, a cached picture taken unfocused slides in dim. The off-thread capture
that once corrected it measured 280ms on one occasion, most of a flight; the
end-state watches take each window as it lands.

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

## Nominal capture resolution paints a quarter of the buffer

The one that caused "the windows are scaled". `SCStreamConfiguration.width` and
`.height` are in PIXELS, and the service asks for the window's point size times
the scale it captures at, 2x. With `captureResolution = .nominal` ScreenCaptureKit renders
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

The guard is `content_reaches_edges`: seven samples along the right and bottom
edges, inset 4px to clear rounded corners, painted meaning alpha above 8
(`PAINTED_ALPHA`), both edges required since content along one does not rule
out underfill in the other direction. An underfilled capture is rejected, not
cached: its coverage would claim the full buffer, pass every fit test, and draw
the window small in a corner on black. Mid-resize the same underfill has another
cause: a 1147pt window requested at 572pt (the layout's intended size) came back
aspect-fitted into a corner, and the tile drew it at two-thirds size on black.
The buffer is therefore sized from the window's frame at enumeration time;
`target.size` only decides whether to capture. The check locks the
`CVPixelBuffer`, not the IOSurface: creating or locking IOSurfaces from parallel
test threads raced SkyLight's lazy initialisation and aborted the suite in 3% to
8% of runs ("Cannot form weak reference to instance of class
SLSWindowManagementFallbackBridge"), so the tests use plain closures and 1x1
`CGImage` bitmaps.

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

Recapturing the windows on screen through SkyLight at switch time, frontmost first
and capped at three, fixed it for a while: one window, 32ms to 35ms once warm, against
the roughly 400ms that capturing all eighteen used to cost. It held every press back
by 59ms to 71ms and went ("Nothing is captured on the main thread on the way in"). The cache is now
refreshed at the quiet after each flight, a flight pictures its windows on screen at the
press and as they land and its focus target once its focus changes, and a warm recaptures a
picture older than `PICTURE_STALE_AFTER` ("Snapshot staleness").

The window being switched INTO cannot be recaptured as the movement starts: SkyLight
reads the framebuffer and the destination workspace's windows are not on screen yet.

### Unfocused rendering

Because the destination cannot be recaptured, its picture is its focused look when
the service kept one, else whatever it last had, and if that was taken while the app
was unfocused the tile slides in dimmed and snaps to focused at the handover. Ghostty greys out noticeably when
unfocused.

It takes a capture after focus has landed: mid-flight, off the main thread, once the
destination is on screen behind the overlay ("Recapturing the destination once it is on
screen").

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

What DOES contain the blur is a capture of the window together with what is
below it, which is what the third column shows. The route this section first
pointed at was a ScreenCaptureKit display capture cropped to the window; the
route actually used is cheaper, synchronous, and immune to occlusion — see "A
composite of the window and what is below it carries the blur" below. yabai
accepts flat translucency on the bet that nobody notices in 200ms; with blurred
materials now on most windows' sidebars and toolbars, that bet stopped paying.

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

### A composite of the window and what is below it carries the blur

Measured 2026-09-28 with a titled window holding a `sidebar` visual-effect view,
over a window of bright vertical stripes, 420x260pt at 2x:

```
CGWindowListCreateImage option                  result                     cost
IncludingWindow (the window alone)              flat grey, opaque          1.6ms
OnScreenBelowWindow | IncludingWindow           the real blur              15.9ms
OnScreenOnly (the screen)                       the real blur, identical
```

The same composite with an opaque window placed ABOVE the blurred one, over half
of it, came back without that window: the option takes the target and everything
below it, so an overlapping window, the switcher's popup and rini's own overlay
at level 18 are all left out. At 1720x1081pt the composite cost 13.8ms — the cost
is fixed overhead, not area.

The composite has no transparency: the rounded corners came back filled with the
stripes behind them. So it is merged with the window's own capture
(`pictures::domain::translucency`): the window's own pixel wherever it is not
fully opaque — corners, and real per-pixel transparency, which captures
correctly on its own (see the next section) — and the composite's wherever it
is. Opaque content is pixel-identical between the two, so the merge only
changes the blurred areas, and whether it changed anything is what marks a
picture as carrying its blur.

It is only taken for a window wholly on a display, and never for every tile at
the start of a flight: at 14ms a window that would delay the flight.

**And it is only kept if the window is still where it was once the capture
returns.** The composite is the window and whatever is below it in a RECT. Found
on the first live run: a workspace switch that raced a framed capture parked Messages
between reading its frame and taking the composite, so the rect held the VS Code
window that had just moved back under it, and the merge replaced every opaque
pixel of Messages with VS Code. Far worse than grey. The frame is read again
after the capture, and a composite of a window that moved or left the display is
thrown away (`stayed_wholly_on_a_display`).

**The window's own framed capture needs the same check.** The window server
returns the full requested rect whatever is on screen, with the part off every
display transparent (a parked window reads alpha 0, "The hairline is composited
outside every capture" below), so the image's size says nothing about how much of the window it
holds. Found on the first live run of the mid-flight focus refresh: a strip pan
moves the real windows while the flight flies, and a window read as wholly on
screen was captured half past the edge, or already parked, then cut onto its
tile as a half or fully missing window. A framed capture now covers the window
only if it stayed wholly on one display through both captures
(`framed_coverage`). Otherwise it holds nothing to
draw. The capture of a new window at its spawn frame has the same exposure, for a
window partly off the display.

One exposure is left as it was: the engine's framed captures name a window by the
server id folded into its `WindowId`. For the rare window Accessibility reports
without a server id, rini falls back to a counter there, which could name another
window. The composite runs in the framed capture (`capture_via_framed`), which only
the flight engine's capture of a new window at its spawn frame uses. The picture
bakes in whatever was behind the window at that moment, which differs from the
live blur only once the window has moved over something else.

A picture carrying its blur is not replaced by a same-sized capture of the
window alone, which would be grey again (`keeps_blur`). The cost is that a
blurred window's picture only refreshes while the window is on screen.

### A tile is pixel-identical to its window, except for the shadow and the hairline

Slack, 1720x1081, opaque, on the built-in display, comparing the tile capture
against the same rect of the screen at native pixels:

```
region              pixels     mean diff   worst
outer ring (2px)     22024       0.201     162
edge band (8px)      66072       0.000       0
corner box (24px)     2304      12.472     162
interior           7346880       0.000       0
```

The interior and the 8px edge band are identical to the value: whatever chrome
the window draws is in its own surface and comes back in the capture. The corner
boxes differ only because a tile's corners are TRANSPARENT, being the window's
own rounded corners, so the comparison there is between a tile pixel and whatever
the screen has behind the window. On screen that transparency shows the desktop,
and in the overlay it shows the backdrop, which is a picture of the same desktop.

The outer ring's 0.201 mean diff was originally dismissed as noise. It is not:
it is the window server's hairline outline, missing from every surface capture.
See "The hairline is composited outside every capture" below.

### The hairline is composited outside every capture

macOS composites a 1pt hairline over every window's outermost point, and no
surface capture carries it — not ScreenCaptureKit's per-window filter, not its
display filter cropped to the window rect, not `SLSHWCaptureWindowList` under
any option bit probed (0, 1<<4..1<<13, 1<<16, 1<<19). Like the shadow, it is
drawn at composite time outside the app's surface.

It can be synthesised as a constant: white at 20% over the window's own edge
pixel, 1pt wide (two pixels at 2x), 18% on an unfocused window. Measured
2026-10-02 in dark mode on 27 window edges across 11 applications, the
window-only `CGWindowListCreateImage` (hairline composited) against the SkyLight
capture of the same window (no hairline):

```
window                 edge pixel     with hairline    0.20 white over it
Kiro                    0, 0, 0       51, 51, 51       51
Outlook                69,69,68      106,106,105      106
Zen                    76,76,76      112,112,112      112
Chrome                 17,18,23       65, 65, 69       65
Ghostty (translucent)  18,19,21 a243  65, 66, 67 a245  65
```

The blend fits every focused edge to 0.3 levels rms and every unfocused one to
0.1; a constant colour misses by 59. An earlier fit from single screenshots
put it at 0.25 and failed on Outlook and on a translucent terminal; the
window-only capture against SkyLight is the comparison that isolates the
hairline. Light mode is not measured.

Two calls composite framing into their output: the obsoleted
`CGWindowListCreateImage` (alive in the dylib, deprecated since 14.0, removed
from the SDK in 15.0 — the crate still binds it), and
`SLSCaptureWindowsContentsToRectWithOptions` with bits 11 and 8, which rini takes it with, the same
pixels for a window wholly on a display (`src/windows/docs/window-server-reads.md`). Behaviour,
measured on the first:

- A rect-bounded call (window's own bounds) returns exactly the window rect,
  hairline composited opaque on the outermost point of every edge, 16-24ms
  after warmup. A null rect pads the image with the shadow instead (asymmetric
  margins), so the rect-bounded form is the useful one.
- It only renders windows that are actually composited: a parked window (a
  1pt sliver at the display edge) returns a fully transparent image of the
  right size. A window with only a couple of points visible has been seen to
  return full content WITH the hairline, so visibility is necessary but the
  exact threshold is the compositor's business.
- The hairline rides INSIDE the window bounds: the capture's outermost pixel
  pair at 2x is the hairline over the app's own edge pixel, which is exactly
  what the screen shows at rest.
- The framing is TWO lines, not one. Just outside the bounds sits a near-black
  outline (measured rgb 0,0,0 at alpha 0.95 in the shadow margin of a null-rect
  capture), and only past it does the soft shadow begin. A tile wearing only
  the inner hairline had the caster's shadow bleeding straight against it, so
  the border read darker mid-flight and snapped at the handover. The renderer
  therefore draws the outline too, 0.5pt of black at 0.9 (the handover
  prototype's value) in the half point outside the window ("The hairline and the shadow, as drawn"
  in `src/decorations/docs/README.md`).

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
and the one losing it, at focus priority: no size test in the way, and through a
flight's hold. Background work, so it costs nothing on the way into an animation.

## A render of the wrong display, drawn at its own size

The wallpaper appeared to zoom in during some animations and not others. Two
compounding causes, both about a desktop render outliving the display it was taken
of:

- A change of backing scale was the only invalidation. Both displays here are 2x,
  so moving the overlay between them invalidated nothing, and a render already in
  flight for one display landed as the cached desktop for the other. A render now
  belongs to the display it was asked for: the capture schedule refuses one of a
  display the overlay has left, and renders the new display's after it. The first
  fix bumped one revision every capture carried, which dropped every window
  capture in flight too; a window's picture belongs to no display, and with two
  displays attached each flight on the other one threw them away (2026-09-29:
  the first layout passes after a restart asked for 51 pictures, and 14 windows
  had one 11s later). The second asked for the new display's render before it
  invalidated the old one, so the render it asked for was the one thrown away and
  every flight after a display change fell back to the composite on the main
  thread, until 2026-10-04. The pictures service keeps the desktop render and the
  bar picture of one display, replaced together when the display changes, and a
  transition reads them only for the display it flies on: a display change used
  to clear only the bar, and the overlay drew an external display's 3008x1692
  desktop behind a built-in display's strips on a 1728x1117 overlay. A transition is
  dressed with the render the service holds; without one it composites the desktop
  on the `transitions` thread, 13ms to 36ms, a frame or two of lag, and draws it with
  or without its wallpaper, since the lift lets go of the backdrop and leaves nothing
  to prefer; a composite that does not span the display leaves the backdrop empty.
  Re-applying a held render costs nothing.
- The composite was size-checked, but the cached ScreenCaptureKit render was handed
  over unchecked.

The backdrop layer is sized from the picture rather than from the overlay, which is
what keeps it in register with the real desktop. Given a 3008x1692 render of the
external display, that lays the wallpaper out at 3008x1692 inside a 1728x1117
overlay, so only its top-left corner is visible: the wallpaper looks zoomed in until
the right render arrives, which is exactly the intermittence that was reported.

`spans_display` now guards both routes: a render is kept only if it spans its display,
and a composite is drawn only if it does. A display change refuses whatever render is
in flight.

Two things this rules out, both measured while chasing it. The desktop capture fills
its buffer at nominal resolution, unlike a window capture: painted to 100% x 100%
through `captureSampleBuffer`, the same API rini uses. And nominal against best for a
display capture is identical to the value, mean 0.000.

### Keep alpha premultiplied end to end

These captures are premultiplied. Dark halos around a tile's edges mean it was
premultiplied twice, light halos mean straight alpha was treated as
premultiplied. That fringe is the fastest way to locate an alpha bug.

## A clipped destination needs ScreenCaptureKit, not SkyLight

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

## Cached surfaces have to be marked in use

Suspected cause of the other half of the same report, that it works for a few
minutes and then goes black again. An `IOSurface` whose `CVPixelBuffer` has been
released is eligible to have its backing store reclaimed, and a layer still
holding it then draws nothing. The overlay sits at alpha 0 between animations,
so nothing composites those surfaces for minutes at a time.

The first fix was `increment_use_count` on every cached capture. It is gone, and
nothing replaced it directly, because `own_copy` below covers this case too: a
surface rini allocated itself is not the pool's to reclaim. The reclaim half was
always reasoned rather than reproduced — purging happens on the system's schedule
and did not reproduce inside a test session.

A use count would have kept a surface from being reclaimed, not from being reused.
ScreenCaptureKit's sample buffers come from its own pool: once the completion
returns, the surface is recycled for the next capture, and a layer still holding
it draws whatever landed there next. Two same-size Chrome windows on different
workspaces swapped pictures that way. `own_copy` therefore memcpys every capture
into an IOSurface this process owns, one copy per capture, off the main thread.

## Recapturing the destination once it is on screen

The window being switched INTO cannot be recaptured when a movement starts,
because the destination workspace's windows are not on screen yet: what its tile
flies is "Unfocused rendering" above. Ghostty greys out noticeably, so a dimmed
destination snapping to focused at the handover was the most visible flicker left
after the sizing fixes.

Fixed by recapturing it mid-flight, by which point the reactor has shown the
workspace and moved focus, so SkyLight returns the focused rendering: once its focus is
reported, until its look changes. The window being left is pictured again by the last
capture before the lift. When and by which route is "Focus changing during a flight" in
`src/transitions/docs/coordination.md`.

On the main thread this cost too much. Measured over a 494ms flight, capture
times of 38ms to 179ms dropped the frame count from 30 to as low as 26. Moving
it to its own thread removes that entirely: 17ms to 56ms of capture, and 30 of
30 frames on every switch. `SLSHWCaptureWindowList` is safe off the main thread,
which yabai relies on as well, capturing on a pthread per window at
`window_manager.c:666`. The picture arrives back as an event a few frames later
and replaces contents only, never geometry, so a late arrival cannot disturb the
movement.

The watches' batched capture (`capture_on_screen`) carries a material window's
blur too, composited over the windows behind it and the desktop; see "A batched
call composites in the order given, front-most first, and carries the blur of
whatever it composites behind a window".

## The hairline

The long-standing "windows go flat and flicker" complaint took three attempts
because the border's identity was misdiagnosed twice:

1. A config-driven drawn border (bronze, mirroring the bordersrc) mismatched
   reality in the other direction — a redrawn border flickers against the
   real one at the handover just as visibly as no border did.
2. Companion tiles for JankyBorders' border windows — correct mechanism, wrong
   target: the `borders` process turned out not to be running at all
   (`bordersrc` exists, nothing draws it). Live enumeration showed zero
   border windows. The companions are gone: rini carries no workaround for
   another app's tooling ("What rini is" in `specs/README.md`).
3. **The border that actually flickers is macOS's own window outline**: a
   1pt hairline the window server composites over every window's outermost
   point, present at rest on every window, absent on every tile mid-flight,
   because it is drawn outside the app's surface exactly like the shadow.

A fourth attempt drew it as a constant (a 1pt white stroke at 0.25 alpha focused, 0.16 unfocused,
fitted from single screenshots): pixel-exact on opaque windows, wrong by 2x on translucent ones.
So the real composited pixels were cut out of a framed capture instead, the one capture that
composites the framing, as a band one point either side of the window's edge that the tile wore.
It only had a hairline for a window composited when it ran, so a tile had no outline until a capture
after a flight had run: the 53-level tail at the handover ("The handover, measured" in
`src/animation/docs/capture-overlay-research.md`). Refitted as a blend over each window's own edge
pixel instead, a constant does match: white at 20% focused and 18% unfocused, within 0.3 levels rms
over 27 edges in 11 apps, the translucent terminal included ("The hairline is composited outside
every capture" above; dark mode only). The renderer draws that white line and the outline outside it on
every tile ("The hairline and the shadow, as drawn" in `src/decorations/docs/README.md`).

## Snapshot staleness

For a window that was off screen, staleness is accepted by construction
("Consequences for the design" above), and the worst case, the destination's focus
appearance, is patched mid-flight (the focus target's change watch, and its focused
look when it starts parked). A window on screen is pictured at the press and as it
lands, and one behind the strip is kept live ("The pictures a flight starts with" in
`specs/animation.md`). What that does not cover: content that changed while parked. Terminal output, chat,
anything live — warmed only at the quiet after a flight and at layout
passes, and pictured at a press only while it is on screen, so a window that
repainted itself while hidden is stale until the next switch touches it. Capturing at switch time instead is ruled out by
measurement: 4 windows cost 94.5ms against a 180ms budget. Warming DURING the
flight is ruled out too: see "Capture work in flight" in `src/transitions/docs/coordination.md`.

Options, in order of expected value:

1. **Low-rate `SCStream` for likely targets.**
   `SCContentFilter(desktopIndependentWindow:)` works for off-screen windows,
   and a persistent stream (unlike one-shot `SCScreenshotManager` calls)
   delivers frames only when the content actually changes. A small pool
   covering adjacent workspaces keeps the cache continuously fresh with zero
   work at switch time. Wants a budget (frontmost window of workspace n±1),
   not every window. Measured since: sixteen streams at 2fps held the window server at 43% CPU against
   9.5% without them, and a frame's arrival is not a change signal ("What a capture costs in a
   press").
2. **Change-signal-driven warming.** Use signals already flowing — AX title
   changes, the CGS window events `window_notify` subscribes to — as "picture
   is dirty" triggers. Misses silent repaints. The window server's own
   `WindowUpdated` (723) delivered nothing in 40s across 128 windows while five
   changed, so it is not one of them.
3. **Aged-cache sweeps.** A `Picture` is timestamped (`taken`) and a warm
   (`wants_warm`) re-captures anything older than `PICTURE_STALE_AFTER`; the
   engine warms every window it flew at the quiet after the lift. A sweep of nearby workspaces on a slow idle timer is not
   done; it would break the "nothing polls" principle deliberately.
4. **Recapture more windows mid-flight.** Done for the windows on screen, through
   SkyLight alone, while the pictures service holds the rest of its work off: the
   landing watch takes each end window as it lands, the change watch the focus
   target, the live watch the floating windows behind the strip every 16ms, and the
   final ask every end window 110ms before the end ("The watches" in
   `src/pictures/docs/README.md`). What stays ruled out is ScreenCaptureKit and
   framed capture work in flight: with warms, the desktop render and other framed
   captures queued behind it, the old framed focus refresh took 203ms median
   against 16-24ms at idle ("Capture work in flight" in
   `src/transitions/docs/coordination.md`). A window parked off screen has nothing
   on screen to capture, which is what options 1 to 3 are for.
