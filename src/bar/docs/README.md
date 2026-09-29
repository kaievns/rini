# `bar` — the menu bar across the top of every display

One 32pt strip per display. On the left, where you are: every workspace's numeral, the shown one's
applications, then the focused window's application and title. On the right, the menu extras and the
time. It replaces a sketchybar setup (`~/.config/sketchybar`) and keeps its look, which was built to
the Okibi 燠火 design system. The requirements are in `specs/bar.md`.

## What it owns

| | |
|---|---|
| **What each bar shows** | `domain/model.rs`: `BarInput` in from the reactor, `BarModel` out, `Action` back |
| **Where each piece goes** | `domain/layout.rs`: spans by ink, the underline, and the hit test |
| **How each piece is set** | `domain/style.rs` and `domain/palette.rs` |
| **Which menu extras are drawn** | `domain/extras.rs`: vitals, tray and skipped, the twin-block rule, ink columns |
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
which no font here can. A thread pictures every extra in one batched capture, compares, and sends
only what changed. It stops while a flight runs. See `menu-extras.md`.

## Measured

The spacing was read off the old bar's pixels on 2026-09-29, from a 2x capture of the built-in
display, as ink runs above a luma threshold. Ink to ink, in points:

| between | measured | used |
|---|---|---|
| screen edge and the first numeral | 17.5 | 16 |
| numerals | 21.5 | 21 |
| the shown numeral and the lit glyph | 15.5 | 16 |
| the lit glyph and the next, then glyphs | 11, then 4.5 to 5 | 10, then 4 |
| the last numeral and the divider | 17.5 | 18 |
| the divider and the application | 13.5 | 13 |
| the application and the dot, the dot and the title | 15.5, 5.5 | 13, 6 |
| tray icons | 12.5 to 17, mean 14.4 | 15 |
| either side of the chevron | 22, 16 | 16, 16 |
| the vitals | 17.5, 14 | 16 |
| the battery, the divider and the date | 10.5, 11 | 10, 10 |
| the date and the time | 15.5 | 14 |
| the time and the screen edge | 27 | 27 |

The measured figures run up to 1.5pt wide of the tokens because anti-aliased edges fall below the
threshold. Where the old bar was uneven (the vitals, the chevron) the tokens are even on purpose.

Vertically, digit ink sat at 12 to 22pt from the top for the 14pt numerals and 11 to 20pt for the
12-13pt text, which is where the baselines in `domain/layout.rs` come from.

## Not in this iteration

- The vitals' popups: Wi-Fi details, battery details, the volume slider. A click on a vital does
  nothing.
- A click on a tray icon does nothing. The owning app's `AXExtrasMenuBar` lists each extra with
  `AXPress` enabled, which might open its menu; not tried. See `menu-extras.md`.
- The Wi-Fi network's name: macOS 26 hides it from every unprivileged route.
- The layout glyph the old bar reserved space for. It never drew, because the dump it read has no
  layout mode, and rini has one layout.
