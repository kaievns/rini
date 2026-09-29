# Picturing the menu extras

The bar draws macOS's own status items: the Wi-Fi fan, the speaker and the battery beside the clock,
and every third-party extra in the tray. It does not draw glyphs of its own for them, because the
system's icons carry the levels (signal bars, speaker waves, charge and the charging bolt) and no font
on this machine has those shapes. See `~/.config/sketchybar/docs/okibi-bar.md`, "Vital icons are
macOS's own", for the three font families tried and rejected.

Everything below was measured on 2026-09-29, macOS 26.7 (25G229), one 1728pt built-in display, with
the menu bar auto-hidden (`_HIHideMenuBar = 1`). Probe sources are not kept.

## Finding them

Every extra is a window at the status level (25) owned by the **Control Center** process, including
the third-party ones. `CGWindowListCopyWindowInfo` finds them only with `kCGWindowListOptionAll`:
13 to 14 windows, all at `y = -33` while the menu bar is hidden and all reported off-screen.
`kCGWindowListOptionOnScreenOnly` returns none of them. The listing costs 2.4 to 5.9ms at the
median, and 15 to 72ms the first time.

`kCGWindowName` is the module for Apple's own (`Clock`, `BentoBox-0`, `Sound`, `WiFi`, `Battery`,
`AudioVideoModule`), and for third-party extras either a bundle id or the anonymous `Item-0`. On the
day this was measured all eight were anonymous. The window server's `Menubar` window is at level 24,
owned by `Window Server`, and is not an extra. Every window at level 25 was Control Center's, so the
bar lists level 25 alone and leaves the owner to `kind` in `src/bar/domain/extras.rs`.

The owning application is not in anything the window server says: every one reports Control Center's
pid, and its only property is the title. The owning app's Accessibility `AXExtrasMenuBar` does name
it. Each child's centre x equals the window's centre x, and the sweep costs 295ms over the 12 apps
that have extras. The bar does not use this yet.

**Which display's.** The bar keeps the windows whose centre x is within the main display's width
(`CGMainDisplayID`), then selects. That rule is inferred, not measured: only the built-in display
was attached. Two things are unchecked. One is whether a second display's menu bar carries its own
set of extras. The other is a display above or below the main one: it shares the main display's x
range, so its extras would pass the rule too.

## Capturing them

| call | status-item window | cost |
|---|---|---|
| `SLSHWCaptureWindowList`, any options | **NULL** for every one | ~15ms |
| `SLSCaptureWindowsContentsToRectWithOptions(cid, &wid, 1, CGRectNull, 1 << 8, &image)` | real pixels at 2x | 17.5ms median |
| the same with every window in one call | one composite of the union, transparent ground | 16 to 50ms median |
| ScreenCaptureKit, unsigned probe | `SCStreamError -3811` | ~40ms |

`SLSHWCaptureWindowList` captures only the part of a window that is on screen, as the animation
engine found (`src/animation/docs/capture-overlay-research.md`), and a hidden menu bar's items are
all off screen. `SLSCaptureWindowsContentsToRectWithOptions` is what sketchybar 2.24 calls. Its third
argument is a real count: sketchybar declares it `bool` and passes a 64-bit array, which works only
because `true` is 1. It returns a `CGError`, where sketchybar declares `void`: 0, 1000 when no image
was made, and 1001 with nowhere to put it, read from its disassembly. One branch returns 0 with an
empty image, so the bar refuses a picture that is not the union at 1x or finer (`Composite::new`).

One batched call per pass is what the bar does. WindowServer CPU for batched captures flat out was
about 0.8ms a batch of 14, against 2 to 4ms per single capture. Each call blocks for one to three
frames, so the captures run on their own thread and never on the main one.

Flag `1 << 11` crops each picture to its opaque bounds, which loses where the icon sits vertically;
the bar reads the ink from the pixels instead.

## A pass

Once a second the `bar-extras` thread lists the status windows, keeps the main display's, selects,
captures the selection in one call and cuts it apart. The composite is the union of the selection's
bounds at one scale: 892 x 66 pixels for 12 extras over 446 x 33pt at 2x, every one cut out whole.
Its pixels are read once into an RGBA bitmap of the bar's own. That gives each extra's columns, and
so its ink, and a hash of its pixels. An extra with no ink is left out.

The extras are sent only when the windows, their kinds or their order differ from the last send, or
any hash does. The first pass always sends; a failed capture sends nothing, and the next tick tries
again. Paused, the thread waits on its channel and neither lists nor captures.

Medians over 10 passes, with sketchybar still capturing its own copies, at a load average of 9 to
11:

| run | listing and selection | capture | cutting, ink and hashes |
|---|---|---|---|
| debug build | 2.4ms | 22ms, 35ms at worst | 4.6ms |
| debug build | 3.8ms | 29ms, 42ms at worst | 4.3ms |
| optimised build, other builds compiling | 5.9ms | 50ms, 67ms at worst | 5.2ms |

The cutting is fetching pixels, not scanning them: drawing the same capture a second time costs
0.04ms, and every extra's columns and hash together 1.1ms unoptimised.

What the pictures hold, in points:

| extra | picture | ink |
|---|---|---|
| `AudioVideoModule`, the camera pill | 48 | 40 |
| eight `Item-0` | 32 to 38 | 11.5 to 20 |
| `Battery` | 42 | 25.5 |
| `WiFi` | 38 | 17 |
| `Sound` | 38 | 13 |

No hash changed over the 10 passes, and the eleven extras present in two runs a minute apart hashed
the same in both.

## Knowing when they change

Nothing tells. Across three minute boundaries the clock extra redrew and one third-party extra
redrew three times in ten seconds, and no SkyLight notification fired for any of their windows
(every event number from 100 to 2000 was registered, and a window of the probe's own produced its
events). The window-closed event does fire when an extra's window is replaced: the audio/video pill
is recreated every one to two minutes.

So the bar pictures them on a timer and compares, and redraws only what changed. They rarely do:
over four minutes only the clock, one app's extra and the pill changed.

sketchybar pictured every alias once a second, one capture each, about 13 blocking captures a second
on its own main thread. The bar makes one, on its own thread.
