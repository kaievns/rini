# Borders and the focus ring

What rini draws around a window that macOS does not: niri's focus ring, on the overlay's tiles
through a flight and in windows of its own at rest. macOS draws each window's own hairline and
shadow; rini draws those only on the overlay's tiles, as `animation.md` requires.

> **Reported 2026-10-06.** "full niri border implementation active/inactive". macOS draws no focus
> ring, and rini refused niri's `focus-ring` with "rini draws no focus ring".

## Who wears the ring

- Each workspace's active window MUST wear the ring, on every display, and no other window MUST. A
  workspace's active window is the one focus lands on when the workspace takes it, the window focused
  there last, as niri's is; on the workspace holding the focused window it is that window, a floating
  one included. niri draws its ring the same way.
- The ring MUST be drawn in `active-color` on the display holding focus and in `inactive-color` on
  the others. A display holds focus while a managed window on it is focused; with none focused, every
  ring MUST be the inactive one. A window rini does not manage, a sheet or another application's
  panel, wears no ring, and while one is focused no display holds focus.
- The ring MUST be on unless `layout › focus-ring { off; }` turns it off: 2pt wide, `#7fc8ff`
  active and `#505050` inactive, niri's ring but for its width, which is 4pt in niri.

> **Reported 2026-10-06.** Kai's decision D1: the focus ring on by default, as niri's, active
> `#7fc8ff`, inactive `#505050`, taken knowing it can cover part of a neighbour (below). Amended by
> Kai 2026-10-08: "make the default ring size 2 not 3", so 2pt rather than niri's 4.

- The ring MUST lie outside the window and take no space from it: the layout is the same with the
  ring on or off. So it covers what is beside the window less the gap: with `gaps 3`, a 2pt ring
  stays in the gap, and with rini's default `gaps 0` it covers 2pt of the neighbouring window. A
  ring at the display's edge MUST NOT be drawn on the next display.
- The ring's inner edge MUST be the window's own corner, on its continuous curve, and its outer edge
  that corner grown by the ring's width; a square window wears a square ring. Its width MUST be
  whole device pixels, never below one, as niri rounds it.

## Through a flight

- Each tile MUST wear the ring its window wears, drawn in front of the tile's picture, its hairline
  and its outline. The tile's outline stays drawn under it, as macOS's own outline lies under a ring
  at rest.
- When focus moves during a flight, each tile's ring MUST turn with its hairline, at the time
  "Focus changing during a flight" in `animation.md` gives it. The window gaining focus on its own
  display wears no ring until its turn and then the active one; the window losing it wears the active
  ring until its turn and then none. When focus moves to another display, each keeps its display's
  ring, and only the paint turns at each one's time: the window losing focus to the inactive, the one
  gaining it to the active.

## At rest

- Between flights the ring MUST be drawn around its window by rini itself, in a window of rini's own
  that sits directly above the window it decorates, with nothing of another application's in
  between, and takes no click: a press on the ring reaches the window under it, and the window's own
  edges stay resizable through it.
- That window MUST be back directly above its window within two frames of another application
  raising its window. Until then the raised window's shadow, and any window it rose past, may show
  over the ring.
- Drawing or moving it MUST NOT change which application is in front or which window is key.
- The ring MUST follow its window wherever the window server moves it, whoever moves it: a title-bar
  drag, an edge drag, an application resizing itself, and rini's own writes, with or without a
  flight, and MUST NOT be hidden while it does.
- The ring MUST NOT be drawn around a window rini has parked, judged by rini's layout rather than
  the window's frame: macOS leaves up to 52pt of a parked window on screen, and a ring around that
  sliver would draw a stripe at the display's edge. rini parks a window and brings one back by
  moving it, so the ring goes or comes with that move, or at the lift that ends the flight, and never
  before it.
- The ring MUST NOT be drawn while Mission Control is up, around a window in native fullscreen or
  entering or leaving it, around a window the window server has ordered out (minimised, its
  application hidden), or around a window on a Space its display is not showing.
- At rest the window holding focus is the one macOS has made key, so the ring turns when macOS
  repaints its windows' focus, not when rini decides to move it. A change that moves no key window
  turns it at once: a selection on a display not holding focus, a config reload.
- A lift MUST show no change in the ring: the ring at rest is drawn by the same layers as the one
  on the tile, and every rest window on the display is put where its window is, in the look it has
  then, before the overlay hides. A border drawn differently on the two sides of the handover
  flickers at the lift as visibly as no border did (item 1 of "The hairline" in
  `src/pictures/docs/capture-research.md`). Kai's words for the one module are R2's Reported line,
  under "The window's hairline" in `animation.md`.
- A ring at rest the overlay covers MUST keep painting, as the window does (`animation.md`): the
  overlay is holed over the ring as well as over the window.

## Where it lives

The ring's settings are `src/decorations/settings.rs`, decoded from `layout › focus-ring` in
`src/app/config/kdl/layout.rs` and `src/app/config/kdl/apply.rs`. Who wears it, its paint by focus
and its colours are `src/decorations/domain/style.rs` and `src/decorations/domain/paint.rs`; its
band, width and corner, and its rest window's frame, `src/decorations/domain/shape.rs`; the layer it
is drawn on, `src/decorations/platform/layers.rs`. The reactor decides each window's ring and what
holds its band at rest in `src/app/reactor/decorations.rs`, from each workspace's active window
(`src/workspaces/engine.rs`). When a tile's ring turns is `src/transitions/domain/lifecycle.rs`,
carried out by `src/transitions/platform/engine.rs` on the overlay
(`src/animation/platform/overlay.rs`), which is holed over each band
(`src/animation/domain/motion/holes.rs`). At rest, which windows show a band, which rest window draws
each and which are ordered again are `src/decorations/domain/rest.rs`, carried out by the main-thread
actor in `src/decorations/platform/rest.rs`, which the renderer
(`src/animation/platform/renderer.rs`) calls before each lift. Its windows follow the window server's
frames and orders (`src/displays/platform/window_notify.rs`), and are ordered with the level and
sub-level calls in `src/windows/platform/window_server.rs`; no listing of windows names them
(`src/windows/domain/listing.rs`). What was measured is "At rest" in `src/decorations/docs/README.md`.
