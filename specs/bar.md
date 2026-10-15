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

- There MUST be one bar per display, 32pt tall, across the top. `bar { off; }` in the config turns
  every bar off.
- With every bar off, a flight MUST keep visible the bar another application draws under the
  overlay, such as sketchybar's: it pictures that bar and draws the picture over itself. With rini's
  bar on it MUST NOT picture one: rini's bar sits above the overlay, and nothing is under it.

> **Asked 2026-10-15.** Kai, on whether the path that pictures another application's bar goes: "i
> want to have an option to disable the rini's menubar for those who want to run the native one or
> sketchybar". `bar { off; }` is that option, and a flight that hid the other bar would leave it
> half usable. This is the one exception to the batteries-included rule ("What rini is" in
> `specs/README.md`).
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
- A bar MUST stay across the top of its display whatever else moves its window. macOS moves windows
  itself when a display comes or goes, and the bar MUST be back at the top at once.

> **Reported 2026-10-01.** "can you have a look at the menubar? it got stuck in the middle of the
> built-in screen when i connected my external monitor in the office". The external was plugged in,
> dropped 30 seconds later and came back 18 seconds after that. Neither display's bounds changed,
> yet the built-in's bar was left 832pt down its 1117. The bar moved its window only when its
> display's bounds changed, so nothing ever put it back.
- The bar MUST NOT be drawn over a native fullscreen space.

## What it costs

- The bar MUST NOT ask the window manager for anything. It is sent what it shows, when that changes,
  and a burst of events that changes nothing on it MUST cost it nothing.
- At rest the bar MUST do no work but the minute tick and picturing the menu extras.
- With no input for a minute, the bar MUST picture the menu extras at most every 30 seconds, and
  MUST picture them again within a second of any input. A wake, a change of the clock or time zone,
  a display change and the end of a flight MUST still be pictured within a second.

> **Reported 2026-10-06.** "any optimisations worth doing on the menu bar at this point?" The
> extras were pictured every second whatever, and each capture cost replayd and tccd about 20ms of
> CPU besides rini's 4ms. With the display kept on overnight, the bar captured every second for
> up to 15 hours.
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

## A config that cannot be used

- While the config file cannot be used, every bar MUST say so: the error glyph and the word, `✕ config`,
  in the design system's error colour, opening the right zone. It MUST go as soon as the file is read
  and can be used, and it MUST NOT be a button. `config-notification { disable-failed; }` turns it off.

> **Reported 2026-10-01.** Kai's decision on the switch to KDL: a failed reload shows a marker in
> rini's bar until the next good load, as niri shows an on-screen error, with niri's
> `config-notification { disable-failed; }` to turn it off. Before, a failed reload said nothing at
> all, so a typo looked like "my change did nothing". The glyph is there because the design system
> refuses a status carried by colour alone.

## Motion

- Nothing on the bar MAY move at rest. Changes are cuts, with two exceptions, both asked for:
  unfolding the glyphs fades them in, and the tray slides into and out of its chevron.
- A click during either movement MUST turn it round from where it is on screen, never cut it back
  to where it started.

## Where it lives

`src/bar/`: `domain/model.rs` decides what each bar shows, `domain/layout.rs` where each piece goes,
`domain/extras.rs` which menu extras are drawn, `domain/style.rs` how each piece is set.
`platform/actor.rs` owns the bars on the main thread and `platform/menu_extras.rs` pictures the
extras. The reactor's side is `src/app/reactor/bar.rs`. Findings are in `src/bar/docs/`. Whether a
flight pictures another application's bar is decided in `publish_animation_display_for`
(`src/app/reactor/mod.rs`), which passes `!bar.enabled` to the pictures service, and
`Schedule::set_display` and `arm_bar` (`src/pictures/domain/schedule.rs`) act on it.
`the_overlay_pictures_a_bar_only_when_rini_draws_none` (`src/app/reactor/tests/displays.rs`) and
`no_bar_is_pictured_while_rini_draws_its_own` (`schedule.rs`) hold it.
