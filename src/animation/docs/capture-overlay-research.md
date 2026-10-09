# The capture overlay: measured constraints

How rini draws a transition: one opaque window of its own above every managed window, holding a
picture of each window that moves, while the real windows are placed once underneath. Why it is
built that way, what it does and does not do, and what was measured about the handover. The early
spikes ran on 2026-08-16 in `/tmp/sls-spike` against a 1728x1117 built-in display at 2x; later
sections date their own. What the captures it draws cost and return, the spikes' list included
("Reproducing"), is `src/pictures/docs/capture-research.md`.

## Window server mutation does not work for foreign windows

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

`SLSGetWindowTransform` does work, and returns the origin with its sign flipped. For window
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
3. **`CVDisplayLink` for the tick.** This is item 39 in the backlog. The old
   `sys/display_link.rs` binding was deleted unused; its `Drop` was unsound
   (`CVDisplayLinkStop` does not wait for an in-flight callback before the `Box`
   is freed), so a new binding must not copy it.

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
   1x28 sliver, as measured in `src/pictures/docs/capture-research.md`. rini
   therefore needs ScreenCaptureKit, which costs 40ms plus 14.5ms per window
   against yabai's roughly 16ms per window in
   parallel. That difference is exactly why rini needs a warm cache and yabai
   does not.

## Phase 1 result: the overlay works

Go/no-go spike for the design, run 2026-08-16. Every question passed. Spikes are
`overlay.swift`, `hold-overlay.swift`, `overlay2.swift` and `levels.swift`.

### Level and coverage

All managed windows sit at CG layer 0, read from the window server rather than
assumed. The overlay sits at level 18 (`OVERLAY_LEVEL`): above every managed
window, below utility panels (19), the Dock (20), notification banners (21) and
the menu bar (24), all read from the window list on this machine. At
`NSPopUpMenuWindowLevel` (101) it blotted out notification banners for every
animation; `screenSaverWindow` (1000) was the first working level and worse still. Verified from the framebuffer with `screencapture`, not from
`SLSHWCaptureWindowList`, which composites only the windows it is handed and so
cannot answer what is actually on screen.

**The overlay's coordinates span the FULL display; its window starts below the
bar.** An earlier version simply sized the whole overlay to the usable frame, and
that was wrong twice over: a shorter overlay squashes the captured desktop into a
box the wrong shape, and it leaves a vertical animation invisible in the strip
beneath the bar, which is exactly where a workspace is supposed to slide out of
view.

Both come from confusing the window's rect with the coordinate system. The
overlay keeps the FULL display as its coordinate frame — the backdrop, the tiles
and the strip surface are all placed in full-display coordinates and register 1:1
with the real desktop, unchanged. Only the window is shorter: it starts at the
bar's lower edge (`window_rect`), and the content layer is lifted by the band
(`canvas_rect`) inside a clipping view, so the reserved top band is drawn and
then clipped rather than squashed. The band comes from the reactor as
`SetDisplay { top_band }`, which is the bar's reserved height, or 0 with the bar
off (`bar { off; }`). Every `SetDisplay` measures the window where AppKit has it,
so one macOS moved when a display came or went is put back by the next, even when
the display itself did not change. See "Placed from where its window is" in
`src/bar/docs/README.md`.

rini's own bar is at level 20, above the overlay (18), so it stays live through a
flight and nothing pictures it; the engine looks for a bar UNDER the overlay only
while `bar { off; }`, where it is another app's. See
`src/bar/docs/README.md`.

The relevant layers on this machine:

```
managed app windows      0
sketchybar             -20
Dock          -2147483624
Notification Center   -2147483601
```

The overlay window has a black background colour, which is what fills a gap
between columns; left with no background it rendered as AppKit's bare grey slab.
It is NOT flagged opaque, and the distinction matters: `isOpaque` is a promise to
the window server that nothing behind the window contributes to what is
composited, and that promise is kept for windows ABOVE it as well. Combined with
the alpha-toggle decision below, which leaves the overlay ordered in at alpha 0
between flights, it truncated the backdrop for every window above level 18 — the
switcher's blurred backing blurred an empty black slab and looked like it was
not blurring at all. The black background is what draws black; opaque never was.

Its content view is a
flipped `NSView` (`isFlipped`), so the layer tree agrees with CoreGraphics
coordinates; `setGeometryFlipped` on a view-backed layer is silently
ineffective, and on the backdrop layer it flips the contents too and drew the
captured desktop upside down.

### Focus is never stolen

Tested at 6 levels from `normal` through `CGShieldingWindowLevel`, which is
2147483628. The frontmost application was unchanged before, during and after in
every case. The combination that achieves this is `.borderless` style,
`orderFrontRegardless()`, and `ignoresMouseEvents`.

### Toggle alpha, do not order the window in and out

This is a 30-fold difference and it decides the show and hide path. The cost of
the choice is that the overlay is permanently in the window stack, so everything
it tells the window server about itself applies while it is invisible — see the
opacity note above:

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
animation. Stretching the movement with `animations › slowdown` and taking ordinary
screenshots is a
better instrument, and the canvas now reports its own frame count when a
movement finishes:

```
frames=21  elapsed=351ms   duration=350ms   travel="0,1117 -> 0,0"
frames=30  elapsed=501ms   duration=494ms   travel="0,0 -> 0,2234"
frames=87  elapsed=1501ms  duration=1500ms  travel="0,1117 -> 0,0"
```

60fps in every case, and travel proportional to the number of rows crossed.

## The overlay has to draw everything it covers

The per-window path drew only the windows it was asked to animate. The overlay is
opaque and spans the display, so every window it was NOT asked about was replaced by
the backdrop — which is the desktop, wallpaper and icons and nothing else.

That is invisible when a switch moves everything on screen, and catastrophic when a
layout pass moves one window. The trigger was mundane: a floating System Settings
window oscillated between x = 502 and x = 503 on every space-state refresh.

```
0:51:00.689  overlay animation composition, requested=1, tiles=1
0:51:00.691  overlay dressed, path="per-window", backdrop="1728x1117", bar="1728x32"
```

A frame pulled out of a screen recording during those 350ms shows the wallpaper, the
desktop icons, the widgets, the bar, and the single Settings window. Slack, the
terminal, the browser — every other window on the display — are simply gone, and back
a third of a second later. This is what "windows disappear and then reappear when I
click on things" was.

Two fixes, both needed:

- The reactor now sends the windows a pass leaves alone as well, with `from == to`, so
  the overlay reproduces the whole screen. They are drawn, never placed: asking an
  application to move a window to where it already is costs an Accessibility round
  trip and invites another layout pass.
- A pass where no window moves 2pt or more, and the layout resizes no window's
  slot by 2pt or more, is not animated at all. Animating a one-point move buys
  nothing and costs 350ms of frozen pictures over the whole display. The resize
  is the slot's, not the window's: an app snapped a few points off its slot is
  written the slot again, and that is no resize.

## The stack has to be the one the overlay hands back to

Tile order came straight from `front_to_back_depths()`, sampled when the animation
starts — which for a focus change is BEFORE the raise. rini asks for the animation
first, deliberately, so the first frame can draw the windows where they still are; the
raise lands milliseconds later.

So the tiles were stacked in the pre-switch order. With a floating window frontmost —
System Settings, just used — the window sliding in was drawn UNDERNEATH it, and the
stack corrected itself the instant the overlay lifted. Photographed mid-animation: the
Settings panel sits over the arriving Chrome window, and 350ms later it is behind it.

`z_group::stack` puts the window that is gaining focus at the front and shifts everything
else back one, so the stack the overlay draws is the stack the real windows will be in
when it hands over. The focus target comes from `LayoutEngine::focused_window()`, which
is the layout's own answer to where focus is going, rather than the window server's
answer to where it has been.

## The strip is one z-order group

macOS raises the window that was clicked. The strip is a single surface, so that is the
wrong unit: clicking one half of a 50/50 pair lifts it over a floating window and
leaves the other half behind it, even though the two sit side by side on screen.
Measured with System Settings floating over two Ghostty terminals:

```
after clicking the LEFT terminal (idx 90):
  0  wsid 90     Ghostty            4,32     <- clicked, in front
  1  wsid 5830   System Settings  383,126    <- floating, sandwiched
  2  wsid 89     Ghostty         -857,32
  3  wsid 91     Ghostty          865,32     <- its 50/50 partner, behind Settings
```

Settings spans x = 383 to 1106 and idx 91 spans 865 to 1724, so the overlap is real
and visible. The rule is in `motion::z_group` for the tiles and in `windows::domain::raise_order`
for the real windows: focusing any window on the strip puts
the whole strip in front of everything off it, and focusing something off the strip
puts ITS APPLICATION in front of the whole strip — not every off-strip window, which
is what a two-value group once did. Within a band the window server's own order is
kept, since windows in one band really can overlap.

It decides two separate things:

- **Tile order.** `stack` puts the focus target first and draws three bands —
  lifted off-strip windows, the strip, the off-strip windows behind it — with a
  stride between bands wide enough that no window count can make them interleave. Without it, the animation drew the floating
  window over the columns sliding past underneath.
- **Real order.** `regroup_tiled` (`src/windows/domain/raise_order.rs`) returns the strip windows back to front when a
  floating window is INSIDE the strip — something on the strip in front of it and
  something on the strip behind it — and nothing at all when the order already obeys the
  rule — the common case, and it must cost nothing, because
  putting it back costs one Accessibility raise per window on screen. Only on-screen
  windows are judged and raised (`strip_group_to_lift_for` in `src/app/reactor/mod.rs`),
  and the judgment runs twice per focus change: once with the raise the focus move
  issues, and again after the layout pass has been applied (`regroup_after_layout`),
  because nothing raises a column when it scrolls back into view — the engine's raise
  list for a focus move is the target column alone (`scrolling.rs` `move_focus`).
  Without the second pass a parked column left behind a floating window came into
  view behind it and sat between the focused column and the floating window, the
  sandwich seen after keyboard navigation with System Settings floating. The second
  pass is skipped when the first already raised everything it would.

  The rule was "something off the strip is in front of any strip window", which was a
  generalisation from that measured sandwich and was wrong. macOS raises a newly opened
  window, so it is frontmost with the whole strip behind it, and that matched: opening a
  System Settings window made it appear and then instantly drop behind the columns, so it
  had to be cmd-tabbed back to. A floating window in front of the whole strip is the point
  of floating; only a floating window with strip windows on BOTH sides is a broken order.
  Three tests hold it, one of them `a_newly_opened_floating_window_is_left_in_front_of_the_strip`
  at the reactor level.

  Raising the WHOLE strip, parked columns included, was tried and measured on
  2026-09-15: 19 windows across 10 apps took seconds (one activation wait per app,
  serialized by the raise mutex in `app.rs`), every raise echoed as a focus report,
  and echoes past the 400ms `RaiseEcho` window read as the user moving, so the strip
  scrolled to random parked windows and regrouped again — 13 regroup rounds for one
  focus move before the order settled. On-screen only keeps a regroup to 2-3 raises,
  inside the echo window.

The windows off the strip are LEFT OUT of the regroup raise rather than raised first.
Everything raised in one sequence lands in front of everything not raised, but the
order within a sequence is up to whichever app answers first, so raising both groups
together would leave exactly the interleaving this is fixing.

### Both routes into focus need it, for different reasons

A keyboard focus move raises through `handle_layout_response`, and the engine's raise
list is only ever the window it landed on — measured, `MoveFocus(Right)` raises
exactly one window. So the list is EXTENDED with the strip group.

A click is raised by macOS, not by rini, and produces only a
`WindowServerFocusChanged`. Nothing else in that path issues a raise, so it gets one
of its own.

Verified on the same desktop that produced the trace above. After a focus move the
50/50 pair sits at depths 0 and 1 with Settings below the strip entirely; after an app
activation the whole strip occupies depths 0 to 12 and Settings sits at 13.

One gap, left alone deliberately: `rini-cli execute window focus --window-id` on a
window the engine ALREADY considers focused returns early from the focus handler, so
it does not regroup. Nothing a user does reaches that path — a click always changes
the engine's idea of focus first.

### Rejected: filtering the layout's raise lists by group

Before extending the list, the opposite was tried: filtering the focused window's
group OUT of raise lists that mixed both. No such list exists. Every raise list the
engine produces comes from `visible_windows_in_layout` or `move_focus`, which both
walk the strip tree, and floating windows are not in it — a floating window can only
reach a raise list as the `focus_window`. Two attempts to write a test that
distinguished the filter from its absence both passed without it, which is what
proved the point. Removed rather than kept as untestable insurance.

## A floating window is not part of the strip

Floating windows were in the canvas, so a strip scroll carried them sideways and
snapped them back at the handover. They belong to a workspace but not to its strip:
a scroll must leave them alone, while a switch between workspaces must take them
along.

So a tile can be pinned: `surface_travel` starts and ends a pinned window where the
final viewport shows its frame, so a strip movement carries it nowhere. A pan ends
at viewport (0, 0), where that is the frame itself. A pan pins whatever the layout
engine calls floating; a switch pins nothing and moves the floating container by
the strip's own travel (`FlightPlan.floating_travel`).

The first attempt lifted pinned tiles out of the canvas and hung them off the
overlay's root, above every strip tile. That is wrong twice over, and both ways were
reported straight away. A floating window is not necessarily in FRONT: this one sits
behind the terminal that overlaps it, so it popped over the strip on every
transition. And a strip window with per-pixel alpha, a terminal at 95%, shows
whatever is behind it, so lifting the floating window out took away what used to
show through and left the backdrop showing instead.

A pinned tile therefore keeps the z-position it gets from the window server's real
front-to-back order like every other tile. That survives the move to containers:
floating tiles live in their own containers, but `stack` still orders tiles by
the window server's order WITHIN a band, which is what this finding needs.

Verified by pixel, on the left edge of a floating window during a scroll, before the
z-order was corrected:

```
at rest    x=72: lum 86   x=73: lum 75   x=74: lum 28    <- edge between 73 and 74
mid-pan    x=72: lum 52   x=73: lum 36   x=74: lum 27    <- same place
```

The luminance differs because the tile carries the shadow and the window's content
is live video. The edge does not move.

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

The distance now comes from where the layout says the strip starts on screen:
`strip_origin(space)`, the anchor less the scroll offset, before and after. It
needs no reference to any window's real frame, so the clamp is irrelevant to it.
The scroll offset alone, sign flipped, was used first and missed a change of anchor:
with `center-focused-column "never"`, centring a column and then moving it
cleared the centring, so every window moved 576pt on a 1728pt display while the
offset stayed at 1152, and the move was drawn as the finished layout at frame zero.
A pass that did not move the strip reports zero and starts nothing:

```
1:04:44.994  cmd=MoveFocus(Right)
1:04:44.997  canvas animation, requested=19, tiles=19
1:04:45.174  cmd=MoveFocus(Right)      focus moved within the visible pair, no scroll, no animation
1:04:45.347  cmd=MoveFocus(Right)
1:04:45.351  canvas movement finished, frames=21, elapsed_ms=350
```

Reading it off the windows survives as the fallback for a layout with no strip.

### The offset is honest, and it still moved eight times per press

Taking the distance from the layout fixed the arithmetic but not the count, because
the offset follows the layout's SELECTION and the selection was being moved by
rini's own raises.

A raise walks every window of the workspace, so the window server's z-order matches
the strip order. macOS reports a focus change for each window it touches on the way,
and `WindowServerFocusChanged` was taken at face value: each report moved the
selection to that window and the strip scrolled to it. One `MoveFocus(Right)` press,
with a floating window focused beforehand, measured on 18 windows:

```
0:27.628  cmd=MoveFocus(Right)
0:27.631  strip movement  now=12548  focused=idx 58     <- the real target, no movement
0:27.696  strip movement  now=1722   focused=idx 68     <- raise echo
0:27.734  strip movement  now=14270  focused=idx 92     <- raise echo
0:27.855  strip movement  now=0      focused=idx 71     <- raise echo
0:27.869  strip movement  now=9104   focused=idx 119    <- raise echo
0:27.909  strip movement  now=9965   focused=idx 89     <- raise echo
0:27.936  strip movement  now=12548  focused=idx 58     <- back where it started
```

Eleven `RaiseCompleted` events, all `sequence_id: 11`, all from that one press. Seven
scroll targets in 276ms, up to 14,270pt apart, ending on the offset it started from:
the press moved the strip nowhere and swept it across the whole workspace six times
to get there. Coalescing cannot fix this — 276ms is longer than any window that
would still feel immediate — and the retargeting itself was working correctly. The
destinations were garbage.

`RaiseEcho` records the windows each raise is about to touch, minus the one that is
meant to end up focused, and focus reports for those are dropped for 400ms. The
target's own report is never dropped, so the press still lands. Costs: a click that
lands on a just-raised window within 400ms of a keystroke does not scroll the strip
to it.

### Late reports from raises a newer one replaced

Pressing right three times quickly produced a fourth animation a second later:

```
1:04:45.347  cmd=MoveFocus(Right)
1:04:46.290  Carbon: App front switched (1077)     Electron app, 940ms after the press
1:04:46.403  canvas animation
```

This was first filed as macOS being slow and not rini's doing. It is rini's doing.
The raise manager runs raise sequences one at a time, and each waits up to 250ms for
its app to report back (`TIMEOUT_DURATION` in `src/windows/domain/raise.rs`). In a
burst, one sequence queued per press, and each ran late. macOS then reported the old
target focused, after the newer raise had replaced it in `RaiseEcho`, so rini read it
as the user moving and the strip flew back to it.

Measured 2026-09-29, replaying a reported burst as real Ctrl-J/L key events (Right x13,
then Left x6, 60-570ms apart) on nine windows:

```
2:30:56.214  report for 14908:5556, mid Right run    strip -1787 -> 861   (back 3 columns)
2:30:57.499  report for 2101:65, mid Left run        strip   813 -> 0     (against the run)
2:30:59.061  report for 14908:5556, 1.2s after the   strip  4593 -> 861   (3732pt back)
             last press
```

Two changes. First, a newer focus raise cancels a running raise that focuses and drops
every waiting one that focuses, so a window the presses moved past is never raised.
Dropping only a waiting raise the newer one covered was tried first and rarely fired:
each press raises only the windows on screen at its own scroll position, so consecutive
raises cover different windows. Second, in `RaiseEcho`, every window rini's raises
touched or focused stays an echo until the raise manager reports it has nothing
running or waiting (`RaisesIdle`), plus the 400ms cascade, unless it is the newest
target.

A fixed window was tried first. Echoes from replaced raises lasted 1s after the
replacement. Replayed on the same burst, the strip stopped turning back mid-burst, but
sequences were still timing out 2.3s after the last press (sequence 19 at 2:42:27.063)
and two of their reports moved it after the burst. Any fixed window is a guess at how
long the queue takes to drain; the raise manager knows.

The idle report carries how many requests the manager had received, and the reactor
ignores one that predates its latest raise. The cost: a click on a window rini just
raised, before its raises finish, focuses it without the strip following.

## A workspace switch is not a strip movement

Switching to a window in another workspace that its own strip had scrolled away from
moved diagonally, or slid vertically and then panned sideways. Both shapes are one
bug, and it is not in the switch: `travel()` has always returned `from.x == to.x ==
0`, so the switch itself only ever moves in y.

The strip's position was remembered per SPACE, while `strip_origin(space)` answers for
whichever workspace is currently ACTIVE. So the first layout pass after a switch
compared the destination workspace's position against the departed one's and reported
the difference as a movement no window had made. Measured in a test on two
workspaces, six columns: **5529.6pt of phantom horizontal pan** for a switch that
moved nothing sideways.

- Landing after the slide, that pan is the "sideways" half.
- Landing while the slide is still in flight, chaining folds it into the residual and
  the whole thing goes diagonal.

Two changes. The position is now keyed by `(space, workspace)`, because every workspace
has its own strip and two of them are not comparable. And a switch CLAIMS the
destination's position as it builds the canvas, because the destination row is drawn at
that scroll already — the strip arrives with the target window in view, so revealing
it afterwards is not something left to do.

The claim is exactly the position the rows were drawn from, so a genuine scroll after
the switch still animates. What the claim would hide is a destination row that was
never scrolled to the target in the first place: `lands_in_view` checks for that at
build time and logs it, since the target would then simply never be revealed.

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
disqualified the whole strip. The fix at the time was to let only windows at least a
quarter on screen vote. That classifier was later replaced outright: the reactor
reads where the layout says the strip starts (`take_strip_movement`), which says how
far the strip travelled without consulting any window.

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

The canvas was dissolved into per-tile animations for a while and is back as one
container per rigid piece (`src/animation/docs/animation-smoothness.md`, "The overlay engine").

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
no longer a resize. A genuine column resize still went to the Accessibility
engine, and tore, until resizes rode the overlay too ("Resizes through the overlay" in
`animation-smoothness.md`).

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

### So off-strip columns are parked, not written at their strip coordinate

The strip coordinate is never sent for a column with nothing on screen. It gets the
same corner park the workspace-hide path uses, which macOS honours, and the result on
the same desktop is 1pt instead of 40:

```
before:  6 windows x=-819  (40pt showing)   3 x=-1680 (40pt)   2 x=1688 (40pt)
after:  13 windows x=1727, y≈1080          (1pt, bottom-right corner)
```

Three things make this safe, and each one was a reason it could not have been done
before the overlay existed:

- **The strip's geometry is untouched.** `calculate_layout_for_workspace` still
  answers with real strip coordinates, and that is what the animation canvas is built
  from. Only the physical placement changes.
- **The windows are still captured.** Verified after the change: `tiles=17,
  missing=0`. A corner-parked window is a full-size window that happens to be off
  screen, and ScreenCaptureKit re-renders it regardless.
- **They are NOT resized.** A stub-sized window would invalidate its cached picture,
  and every park and unpark would cost an application relayout. Moving a full-size
  window achieves the same 1pt result with neither.

The park corner records which side the column was on, so `entry_frame` can bring it
back in from that edge. Without that, a window returning to the strip animates from
its real frame and flies in diagonally from the bottom of the display.
`calculate_hidden_position_multi` may flip the corner to keep the park off a second
display, and then the side is lost and the entry is guessed from the destination.

A column with ANY part on screen keeps the position the layout gave it. Measured
after the change: one column sat at x = 1726 with 2pt showing, which is the strip
genuinely continuing past the edge rather than a parked window.

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

## What the overlay is NOT doing

Recorded because it took a long time to establish and would otherwise be
looked into again. Reported as "the windows are scaled", the overlay's tiles were
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
  quarter of the buffer" in `src/pictures/docs/capture-research.md`. The lesson generalises: measure the API the code
  actually calls.
- The backdrop and the bar, which are siblings of the canvas, render 1:1.

A `check_geometry` call used to assert the first point on every animation and log
only when it failed. It was a canvas-era safeguard and went with the canvas
(`d6c4ffe`); nothing checks this at runtime now, so a scale introduced in the
layer tree would again have to be caught by eye.

## Shadows are never in the surface

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
macOS draws two shadows — the key window's is much heavier — so both were read
out of shadow-inclusive framed captures (`CGWindowListCreateImage`, null rect,
whose margins hold the shadow as premultiplied alpha), as a share of black at
distances from the window's side edge at mid-height:

```
            unfocused   focused
  2pt         0.200      0.330     the peak, right at the edge
  7pt         0.129      0.255
 14pt         0.055      0.169
 21pt         0.020      0.102
 28pt         0.004      0.055
 42pt             0      0.008
```

Below the window the focused shadow peaks at 0.56 and is still at 0.08 by 42pt;
above it barely reaches 0.11 — a strong downward offset, roughly half again the
unfocused bias. Unfocused fits `shadowOpacity = 0.4`, `shadowRadius = 9`,
`shadowOffset = (0, 5)`; focused is 1.65x darker at the edge with a 1.5x longer
falloff, fitting `shadowOpacity = 0.65`, `shadowRadius = 14`,
`shadowOffset = (0, 12)`. Positive y means down because the tiles hang off a
flipped view. `shadowPath` is
an explicit rounded rect at the tile's corner, the one its hairline rounds to:
the window's radius as the window server reports it, else 16pt ("The corner,
measured" in `src/decorations/docs/README.md`). An explicit path rather than letting Core Animation derive
one from the contents alpha, which is exact for an oddly shaped window but
recomputed whenever the contents change.

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
the window's rounded rect, wound even-odd, so only what lies outside the window is
drawn. `SHADOW_REACH` is 70pt: the focused style (radius 14, offset 12) spends
its ramp by about 54pt (three radii plus the offset), so the 40pt reach that fit
the unfocused style clipped it into a straight edge.

`CGPath::with_rounded_rect` with a zero radius or a zero dimension emits a plain
rect, whose element structure differs from a rounded rect's; a `shadowPath` or
mask `path` animation between the two does not interpolate, it cuts. An entrance
growing from zero width popped its shadow this way. The silhouette, the mask's hole
and the outline are now rini's own path (`shape::rounded_rect`), the same 18
segments at every size and radius, zero included, so none is floored. The caster's resize
animations use the same key prefix (`rini.tile`) as the plain move: a move leg
retargeted into a resize otherwise left its old position animation fighting the
resize's, and the shadow tore away from its tile.

The shape is rebuilt only when a tile's size or corner changes, which is once per
animation: a movement changes where a tile is, not how big it is, and the corner
changes only when a pooled tile is dressed for a window of another radius.

Verified against the screen, signed difference in the 4pt gap at a window's left
edge, positive meaning the overlay is darker:

```
shadow on the tile      +2.1     gap AND under the window
masked caster           +1.2     gap only
```

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

### Both drawing paths have to dress the overlay

Only the canvas path set the backdrop and the bar. Focusing a window on a strip
goes through the per-window path, which set neither, so a horizontal slide
showed whatever those layers happened to hold: nothing at all until some
vertical slide had filled them, and the last picture afterwards. That matches
the report exactly, of horizontal slides being almost black until a vertical
slide, then working for a while.

The per-window path now dresses the overlay too, with the render of the desktop
the pictures service holds. A desktop composite measures 13ms to 36ms for 6
windows, which is a frame or two of lag on every window focus change, while
re-applying a picture already in hand is a pointer assignment. The service renders
the desktop again at a release when it is missing or stale; until the first render
of a display lands, a transition there composites it on the `transitions` thread
("Which route serves, and why the render wins" in
`src/pictures/docs/capture-research.md`).

## Sequencing a focus change behind the overlay

Measured 2026-10-02 with a scratch sequencer (branch `scratch/perf-timing`) that holds rini's raise and
frame writes and re-issues them in a chosen order, animations off, the overlay held up for 800ms.
Interleaved ABBA, 64 presses per order, load average 6 to 11 with the endpoint agents busy, so the
absolute numbers are a loaded machine's; the comparisons hold. Times in ms, p50/p99.

**A window covered by the overlay may stop painting.** Ghostty under the overlay never reached its
final focused look within 450ms (0 of 24); uncovered it did every time. Chrome kept painting but
took 181 against 157. Either change below restores both to their uncovered times, 24 of 24:

```
                        Ghostty final look   Chrome final look   (ms after the focus call)
uncovered               153 / 256            159 / 208
overlay                 never                181 / 245
overlay + 1pt hole      159 / 193            157 / 196
overlay + 8pt hole      155 / 184            159 / 215
overlay at alpha 0.99   165 / 278            153 / 188
```

The hole is a 1pt transparent square in the overlay over the new window's final frame, so the
window server counts the window as visible.

**Orders**, parked target, overlay with the hole, frame-zero pictures from one batched SkyLight call:

```
order                                   focused    placed    old pic    old pic before   new pic   all three
                                                                        it moved
B   frames, then focus                  410/621    200/388   559/579     4/20            428/630   565/638
PAR focus and frames together           339/634    250/489   525/548     7/20            419/626   530/635
R   focus, frames on RaiseCompleted     294/465    379/615   294/391     8/20            404/632   454/642
RW  focus, frames on the focus report   273/438    376/639   377/440    10/20            385/630   440/654
K   new window's frame, focus, the      345/546    395/529   410/573    20/20            403/605   472/607
    rest on RaiseCompleted
```

Only K leaves the old window on screen until it has repainted as inactive, so only K can always take
the old window's end-state picture with SkyLight. The raise-first orders focus 50-70ms sooner but
move the old window first half the time. "All three" pairs old picture, new picture and placement
from different presses, so it is an estimate.

**Queued ScreenCaptureKit work slows every step.** With four captures in flight at the press, the
frame-zero capture took 92-97 against 26-33, the overlay came up at 176-180 against 84-86, focus
landed 100-165 later and the windows were placed 140-176 later. A capture already issued cannot be
stopped, so the queue has to stop issuing before a focus change.

**rini's own work before the raise is 2.4 / 4.9 / 12.6** (p50 / p90 / p99, 506 presses of normal use,
2026-10-03): the press to the raise's dispatch. The make-key call goes out at 6.5 / 15.6 / 32.2.

## Where a moving focus change spends its time

Measured 2026-10-03 on an idle machine with an outside probe (`tools/focuslab/focuslab.swift`,
see `tools/focuslab/README.md`): a 1ms poller on `_SLPSGetFrontProcess`, `SLPSGetKeyFocusProcess` and
`SLSGetWindowBounds`, and an `SCStream` of the titlebar band at 120fps timed by each frame's
`displayTime`. Raw data in `docs/data/2026-10-03-focus/`, one JSON line per press. What each
SkyLight capture in it costs is "What a capture costs in a press" in
`src/pictures/docs/capture-research.md`.

**A focus change with nothing moving is at the floor.** Two half-width windows side by side, the
focused look visible on screen, ms after the input, p50 / p90:

```
                                      Chrome          Ghostty
native click                          69 / 77         62 / 66
rini key press                        52 / 60         53 / 66
the make-key call itself, no rini     47 / 68         47 / 54
```

The window server changes the front process 1.1-1.6ms after the make-key call, with or without
captures running; the rest is the app activating and repainting.

**The window server has a written frame at once.** For seven apps, a parked window's AX position
write took 0.4-0.8ms and `SLSGetWindowBounds` showed the new frame 0.04-0.1ms after it returned,
`CGWindowList` about 1ms after. During a flight it is different: an app still activating applies the
write late. Ghostty's write returned at 42-47ms and the server showed the move at 62-75ms.

**The breakdown**, strip focus changes on the comms strip, VS Code to Ghostty and back, the two
builds' flags interleaved in blocks of six in one run, 24 presses each, ms after rini takes the
press, p50 / p90:

```
                                  before          after
frame-zero capture starts         17.6 / 31.6      5.7 / 11.3
frame-zero pictures ready         70.1 / 83.5     53.0 / 71.5
overlay up, frames sent           71.5 / 84.2     53.4 / 71.9
target write returns              78.8 / 97.5     58.4 / 75.9
rini sees it landed              105.9 /119.4     76.9 / 99.0
focused picture captured         123.3 /140.7    104.9 /121.4
```

rini's own journal gives every row but the target write returns, which only the prototype marked:
`tools/focuslab/journal.py summary` per flight, and `journal.py probe` joined to a probe's presses
(`tools/focuslab/README.md`). What each row measures there is "Reading it" in
`src/transitions/docs/journal.md`.

What moved it: the frame-zero capture starts before the flight is composed; pictures are regions of
the batched composite drawn through `contentsRect` (a crop handed to Core Animation cost 2-5ms of
main-thread decode per quarter window, a standalone copy 14ms per flight); a background window
wholly behind an opaque one at frame zero is captured after the overlay is up, not in a second
frame-zero call; landing is read from `SLSGetWindowBounds` and watched from the moment the frames
are sent. Making the window key before the raise's Accessibility calls changed nothing measurable.

**The focused look can fly from frame zero.** After every flight the off-screen warm-up recaptures the
window just left, unfocused, so a parked target's tile showed the unfocused look until its landing
capture. Keeping each window's last focused picture, dropped once a newer capture shows different
content below the titlebar, gives the tile the focused look at overlay-up, 48ms.

## The handover, measured: an end-state mirror

Measured 2026-10-02 with a scratch prototype (branch `scratch/handover`, files `proto.rs` and the
`lab_*` hooks), against the deployed engine, interleaved ABBA, animations on, 64 presses per
condition on the strip and 32 per swap or resize condition.

**The metric:** the overlay window's own picture (one SkyLight call on the overlay) just before it
lifts, against the screen just after, over every window that ends wholly on screen. Interior is
inset 4pt; the edge band is the 1pt either side of the window's outline. Mean RGB difference, p50/p99:

```
                                  deployed engine            prototype
strip, focused window   interior  0.35 / 4.28                0.26 / 3.39
                        edge in   4.79 / 53.13               0.37 / 2.67
                        edge out  4.30 / 17.54               1.56 / 3.10
strip, other window     interior  4.14 / 8.15                0.79 / 1.51
                        edges     16.6-17.7 / 33.7-33.9      1.08-2.26 / 1.23-2.49
swap, focused window    edges     0.84 / 4.46                0.34 / 1.37
swap, other window      edge in   5.68 / 24.78               5.30 / 6.07
resize, focused window  interior  0.11 / 14.23               1.57 / 1.66
```

This metric, sampled the same way, is `src/pictures/domain/handover.rs`. `animations { journal {
measure-handover; } }` measures every lift with it, and `tools/focuslab/journal.py handover` prints
these rows from the journal, the strip's as `pan` (`src/transitions/docs/journal.md`, "The handover,
measured").

What each part of the prototype fixed:

- **A constant hairline and outline replace the harvest.** A 1pt line of white at 20% (18%
  unfocused) inside the edge, and a 0.5pt stroke of black at 90% half a point outside it, drawn
  as layers over every tile. The harvest left a tile with no outline whenever it had not run,
  which is the 53-level tail. A border layer at a half-point origin landed one pixel too far out,
  so the outline is a `CAShapeLayer` stroke; both have to be set after the tile's frame, or a pooled
  layer's autoresizing carries the old size, and both follow a resize on its own timing.
- **Every window that ends on screen gets a 1pt hole in the overlay** over its final frame, not only
  the focused one: the window losing focus is occluded too, and repainted as unfocused only at the
  lift.
- **The end-state mirror.** From the overlay up until the lift, the windows that end wholly on
  screen are captured through SkyLight as they land, so the tile the lift ends on shows the window
  where it ends rather than a picture taken before it got there. The build measured here captured
  them back to back and sent every picture on every pass. The one kept captures each window once it
  is where it ends, the focus target again until its look changes and every one once more 110ms
  before the clock ends, and swaps a picture in only when its band print differs from the last one
  delivered (`src/pictures/domain/watch.rs`, "The lifecycle" in
  `src/transitions/docs/coordination.md`).
- **Tiles snap to the real frame.** A strip pans by a fractional vector and the window server rounds
  each window on its own, so a tile in a rigid group ended a point from its window (861 against 860
  wide). When a window is pictured where it landed, its tile is moved onto the real frame while
  still in motion.
- **A window counts as landed when its frame is within 20pt of its target**, and the crop uses the real
  frame: a terminal shrinking to half width sat at 862 against 860 requested before settling. The build
  measured here waited for the frame to be stable between two polls; the one kept captures the window
  at the first `SLSGetWindowBounds` read within the 20pt and crops it at that frame, reading every 1ms
  until then, which waits out a write an app applies late ("Where a moving focus change spends its
  time").
- **A resize places the real windows at frame zero.** The deployed engine applies them at half the
  flight, which left the app no time to render before the lift; the mirror's picture arrived 50ms
  after it.
- **Fresh frame zero:** one batched SkyLight call of the strip windows on screen, 16 / 84ms on the
  main thread in the build measured here; the one kept makes it on the pictures thread. The cached pictures it replaces differed from the screen by 2.3 / 8.1 levels, 2% / 8%
  of pixels changed visibly, at an age of 1.6s p50.

Cost: no measurable CPU. Over 16-press sweeps the WindowServer ran 42-46% and rini 14.6-16.4% with
or without the prototype. A 400ms flight was no better than 350ms. In the build measured here grows
lifted at about 700ms because of the frame-zero reveal hold; the one kept holds a grow only for its
repaint, up to 300ms from the show, and flies a shrink at once on its picture from the press, kept to
the lift ("A grow waits for its repaint" in
`src/transitions/docs/coordination.md`).
