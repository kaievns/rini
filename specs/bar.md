# The bar

rini draws its own menu bar: one strip across the top of every display saying where you are, what
you are in, and the time. It replaces a sketchybar setup, and keeps that setup's look.

> **Reported 2026-09-29.** "i have this sketchybar setup that i use. i have it the way i want it more
> or less, but i don't like the sketchibar itself and how i have to manage its configuration, it's
> very laggy, inconsistent, glitches a lot, and worst of all it keeps contesting resources for
> animations with rini and makes rini animations choppy... bake a rini based clone, that would be
> hopfully more stable, reliable and faster, and just be integrated with rini out of the box." The
> sketchybar bar held 89 items on two displays, and sketchybar charges WindowServer for every item
> whether it draws or not. It learned each change by forking `sh` and `sketchybar --trigger` per
> event, then `rini-cli query diagnostics`. It pictured its menu extras about 13 times a second on
> its own main thread. And every rini flight captured the bar and redrew a frozen copy of it.

## Showing it

- There MUST be one bar per display, 32pt tall, across the top. `[settings.bar] enabled = false`
  turns every bar off.
- The bar's ground MUST be the desktop behind it, blurred, under the ground's colour. It MUST be a
  still picture, taken again when the display or the wallpaper changes and never while a flight
  runs, so nothing is blurred again as windows move. Windows passing under the bar are not seen
  through it.

> **Reported 2026-09-29.** "make the bar background to use blur". Until then the wallpaper showed
> through the 88% ground in sharp detail. The first answer had the window server blur the desktop
> behind the bar live.
>
> **Asked 2026-09-29, after that.** "can you just take blurred background picture and use a static
> behind the menu bar?" A live blur is done again on every frame something moves behind the bar,
> and the flight overlay moves behind it on every frame of a flight.
- No window MAY sit under the bar on any display. The band MUST be kept clear on every display, not
  only on the one whose notch macOS already keeps clear. A per-display gap that made room for the old
  bar on the external display is no longer needed, and left in place it doubles the band.
- The real macOS menu bar, revealed from the top edge, MUST draw over the bar.
- The bar MUST NOT be tiled, focused, counted as a window, or offered by a switcher.
- A display that sleeps MUST keep its bar. A display that is plugged in gets one; one unplugged
  loses it.
- A display whose resolution or arrangement changes MUST have its bar follow it at once.
- The bar MUST NOT be drawn over a native fullscreen space.

## What it costs

- The bar MUST NOT ask the window manager for anything. It is sent what it shows, when that changes,
  and a burst of events that changes nothing on it MUST cost it nothing.
- At rest the bar MUST do no work but the minute tick and picturing the menu extras.
- The bar MUST stay live over a flight. It is drawn above the flight overlay rather than pictured by
  the overlay and redrawn frozen.
- The bar MUST NOT picture menu extras while a flight runs or settles.

## Where you are

- Every workspace MUST have a numeral on every display. The workspace a display shows is ember,
  heavier, and underlined together with its applications. A workspace holding windows ON THAT
  DISPLAY is bright, and an empty one is faint.
- A click on a numeral MUST show that workspace on the display the numeral is on, whichever display
  has focus.
- The shown workspace's applications MUST each have a glyph, the focused window's first and bright.
  A click on a glyph MUST focus that application's window.
- Past five applications the rest MUST fold behind a count ("+3"). A click on the count unfolds them;
  a click on the "−" that replaces it folds them again.

## What you are in

- The focused window's application and title MUST be shown on the display the window is on, and on
  no other. A title is cut at 48 characters. With nothing focused, nothing is shown.

## The time and the menu extras

- The time MUST read 24-hour `HH:MM` and change on the minute, whatever else wakes the bar first. A
  bar that comes back up MUST show the time it came back to. The date reads like "Tue 29th".
- A change of the system clock or time zone MUST show on the bar at once.
- Wi-Fi, sound and battery MUST be macOS's own icons, so they show what macOS shows: signal, level,
  charge, and charging.
- Every other menu extra MUST be drawn in the tray, in menu-bar order, with the app's own icon. The
  tray MUST be open to begin with, and the chevron beside it opens and closes it.
- Left out on purpose: the clock extra (the time is drawn as text), Control Center's own button,
  Now Playing, Apple's other extras, and the print queue and DisplayLink when macOS names them. macOS
  names third-party extras only in some sessions, so those two show in the others.
- Icons MUST be evenly spaced by their ink, not by their pictures, whose margins differ from one
  extra to the next.

## Motion

- Nothing on the bar MAY move at rest. Changes are cuts, with two exceptions, both asked for:
  unfolding the glyphs fades them in, and the tray slides into and out of its chevron.
- A click during either movement MUST turn it round from where it is on screen, never cut it back
  to where it started.

## Where it lives

`src/bar/`: `domain/model.rs` decides what each bar shows, `domain/layout.rs` where each piece goes,
`domain/extras.rs` which menu extras are drawn, `domain/style.rs` how each piece is set.
`platform/actor.rs` owns the bars on the main thread and `platform/menu_extras.rs` pictures the
extras. The reactor's side is `src/app/reactor/bar.rs`. Findings are in `src/bar/docs/`.
