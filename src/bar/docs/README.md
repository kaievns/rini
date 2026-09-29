# `bar` — the menu bar across the top of every display

One 32pt strip per display. On the left, where you are: every workspace's numeral, the shown one's
applications, then the focused window's application and title. On the right, the menu extras and the
time. It replaces a sketchybar setup (`~/.config/sketchybar`) and keeps its look, which was built to
the Okibi 燠火 design system. The requirements are in `specs/bar.md`.

## What it owns

| | |
|---|---|
| **What each bar shows** | `domain/model.rs`: `BarInput` in from the reactor, `BarModel` out, `Action` back |
| **Where each piece goes** | `domain/layout.rs`: spans by ink, the underline, and the hit test; `domain/placement.rs`: where each picture goes so its ink lands on its span |
| **What each piece says, and what a click does** | `domain/pieces.rs` |
| **How each piece is set** | `domain/style.rs` and `domain/palette.rs` |
| **The two movements** | `domain/motion.rs`: the fold's states and the fades' timing |
| **Which menu extras are drawn** | `domain/extras.rs`: vitals, tray and skipped, the twin-block rule, which display's, ink columns, cutting a capture apart, the change test, the pictures kept, and the tick |
| **The words** | `domain/format.rs`, and `domain/glyphs.rs` with its table `domain/app_glyphs.tsv` |
| **The windows** | `platform/actor.rs` on the main thread, `platform/panel.rs`, `platform/text.rs` |
| **The menu extras' pictures** | `platform/menu_extras.rs`, a thread of its own |

The reactor's half is `src/app/reactor/bar.rs`. It fills a `BarInput` from its stores after every
batch of events and sends the model on only when it changed.

## The shape that matters

**Told, never asking.** The old bar learned about each change by forking `sh` and
`sketchybar --trigger`, then forked `rini-cli query diagnostics` and parsed the whole dump. This bar
is sent a model, and only a model that differs from the last one, so a burst of events that changes
nothing on the bar costs it nothing.

**Live over flights.** The flight overlay covers the whole display at level 18. The bar sits above
it, so it is never in a capture and never frozen under the overlay. The old bar sat at -20, and every
flight pictured it and redrew the picture on top of itself.

**Laid out by ink.** Every gap is from one piece's ink to the next's, so a "1" and a "2" get the same
air and icons with different margins space evenly. See `domain/layout.rs`.

**The menu extras are pictures.** macOS's own Wi-Fi, speaker and battery icons carry the levels,
which no font here can. A thread pictures every extra in one batched capture and compares. It sends
them on only when one changed, and only a changed one carries a new picture, so only its layers are
redrawn. No capture starts once the bar hears of a flight; one already under way runs to its end.
See `menu-extras.md`.

## Measured

The spacing was read off the old bar's pixels on 2026-09-29, from a 2x capture of the built-in
display, as ink runs above a luma threshold, and this bar was measured the same way against it. Ink
to ink, in points:

| between | old bar | token |
|---|---|---|
| screen edge and the first numeral | 17.5 | 18 |
| numerals | 21.5 | 22 |
| the shown numeral and the lit glyph | 15.5 | 16 |
| the lit glyph and the next, then glyphs | 11, then 4.5 to 5 | 11.5, then 5 |
| the last glyph and the next numeral | 17 | 17.5 |
| the last numeral and the divider | 17.5 | 18 |
| the divider and the application | 13.5 | 14 |
| the application and the dot, the dot and the title | 15.5, 5.5 | 16, 6 |
| tray icons | 12.5 to 17, mean 14.4 | 15 |
| either side of the chevron | 22, 16 | 16, 16 |
| the vitals | 17.5, 14 | 16 |
| the battery, the divider and the date | 10.5, 11 | 10, 11.5 |
| the date and the time | 15.5 | 16 |
| the time and the screen edge | 27 | 27 |

A text token is the old figure plus half a point: AppKit's device-metric ink runs about that far past
the pixels the threshold keeps, so the first render, spaced to the old figures, measured half a point
tight. Two are not: the time's 27 to the screen edge measured 27 on both bars, and the battery's 10
to the divider is an icon's ink, read from pixels. Tray icons are spaced by ink read from their own
pixels too, so their token is the figure. Where the old bar was uneven (the vitals, the chevron) the
tokens are even on purpose.

Vertically the old bar's digits sat 10 to 20pt from the top and its 12-13pt text 12 to 21pt, which
is where the baselines in `domain/layout.rs` come from. An earlier reading had them upside down, 12
to 22 and 11 to 20: its scan counted pixel rows from the bottom, which the ember underline, found at
the top, gave away.

## Drawing

One `NSPanel` per display, at level 20: over the flight overlay at 18, under notification banners at
21 and the menu bar at 24. It is non-activating, so a click never activates rini. It is ordered out
rather than faded, because an alpha-0 window still takes clicks. `FullScreenNone` keeps it off a
native fullscreen space. A panel hides whenever its app deactivates unless told not to, so
`hidesOnDeactivate` and `canHide` are off. A panel is made the first time a model names its display,
which costs about 112ms, and kept; a display that leaves the model has its bar ordered out.

```text
root         the ground: n1 at 0.88
├── one layer per piece: a picture of its string, a hairline, or a vital's picture
├── tray     masks to its bounds, from the first tray icon to the chevron's ink
│   └── one layer per tray extra
└── underline
```

`domain/pieces.rs` says what each piece shows, `domain/placement.rs` where its picture goes, and
`domain/motion.rs` how the two movements run. `platform/panel.rs` sets the layers and
`platform/text.rs` draws the words.

**Every string is a picture.** Each is drawn once per string, face and colour into a bitmap of its
ink plus a 2pt margin, and kept while some bar still says it. A `CATextLayer` cannot say where its
ink is, and the layout places by ink. A layer is touched only when its picture or its place changed,
and each update is one transaction with implicit animation off.

**Measured by ink.** `boundingRectWithSize:options:` with `UsesDeviceMetrics` and without
`UsesLineFragmentOrigin` gives the ink from the pen on the baseline, y up. Drawn with
`drawWithRect:options:` from the same pen into a 2x bitmap, the scanned ink matched the measurement
to within its anti-aliased edge, one pixel. Measured 2026-09-29, in points:

| string | face | ink x | ink width | ink height |
|---|---|---|---|---|
| `1` | Ioskeley Mono Term Medium 14 | 1.484 | 3.808 | 9.660 |
| `2` | Ioskeley Mono Term Medium 14 | 1.162 | 6.104 | 9.772 |
| `Tue 29th` | Ioskeley Mono Term 13 | 1.027 | 60.268 | 9.724 |
| `:ghostty:` | sketchybar-app-font 14 | 0 | 11.410 | 13.902 |

The token's typographic width was 19.754 against 11.41 of ink, so the ligature forms through an
`NSAttributedString` with default attributes.

**Placed on whole pixels.** Baseline text keeps its baseline on a whole pixel, so numerals of
different ink heights share one. Everything else is snapped to the pixel grid, at most a quarter
point from its span. Vitals and tray icons are the watcher's pictures, placed so their ink starts on
the span and centred on the bar. About 33pt tall, they hang half a point over each edge, with their
ink inside.

**Colour.** The bitmaps are device RGB, which took the palette's sRGB bytes unchanged: ember drew as
`ff7c50` in a device RGB and an sRGB bitmap alike. Each picture is then tagged sRGB, so it is
matched to the display the way the ground's colour is.

**The two movements.** Unfolding fades the newly shown glyphs in, 14/60s each, staggered 0.035s from
the left, each held transparent until its turn. Folding fades them out from the right and only then
folds. A click during either fade turns it round: each glyph goes on from the opacity on screen,
held there until its turn, where starting from the far end would blink it first. Closing the tray
slides the tray layer's bounds a whole tray's width to the right in 0.2s, eased out, so the icons go
into the chevron and are clipped there; opening slides them back. The chevron does not move, and its
glyph changes at once.

**At rest** the actor holds one timer, for the minute. A second runs only while a fold fades out.
Every wake reads the clock, one `localtime_r`, and redraws only if its minute moved, so a wake that
crosses the boundary cannot leave the time a minute behind, and a bar coming back up is drawn with
the time it woke to.

**A missing font** is stood in for by the system's monospaced face at the same size and weight, and
an application's glyph by its first letter. Each missing face is logged once.

## Not in this iteration

- The vitals' popups: Wi-Fi details, battery details, the volume slider. A click on a vital does
  nothing.
- A click on a tray icon does nothing. The owning app's `AXExtrasMenuBar` lists each extra with
  `AXPress` enabled, which might open its menu; not tried. See `menu-extras.md`.
- The Wi-Fi network's name: macOS 26 hides it from every unprivileged route.
- The layout glyph the old bar reserved space for. It never drew, because the dump it read has no
  layout mode, and rini has one layout.
