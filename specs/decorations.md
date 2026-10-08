# Borders and the focus ring

What rini draws around a window that macOS does not: niri's focus ring and border, on the overlay's
tiles through a flight and in windows of its own at rest. macOS draws each window's own hairline and
shadow; rini draws those only on the overlay's tiles, as `animation.md` requires.

> **Reported 2026-10-06.** "full niri border implementation active/inactive". macOS draws neither,
> and rini refused niri's `focus-ring` with "rini draws no focus ring" and its `border` with "rini
> draws no borders; macOS draws the windows".

## Who wears the ring

- Each workspace's active window MUST wear the ring, on every display, and no other window MUST. A
  workspace's active window is the one focus lands on when the workspace takes it, the window focused
  there last, as niri's is; on the workspace holding the focused window it is that window, a floating
  one included. niri draws its ring the same way.
- The ring MUST be drawn in `active-color` on the display holding focus and in `inactive-color` on
  the others. A display holds focus while a managed window on it is focused; with none focused, every
  ring MUST be the inactive one. A window rini does not manage, a sheet or another application's
  panel, wears no ring, and while one is focused no display holds focus.
- The ring MUST be on unless `layout › focus-ring { off; }` or a window rule turns it off: 2pt wide, `#7fc8ff`
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

## The border

- While `layout › border` is on, every window rini manages, tiled or floating, MUST wear the border
  unless a window rule turns it off for that window, in `active-color` while it holds focus and in
  `inactive-color` otherwise, as niri paints it. A window rini does not manage wears none.
- The border MUST be off unless `layout › border` turns it on, and a `border` block that writes
  neither `on` nor `off` turns it on, as in niri (`config.md`). It is niri's: 4pt wide, `#ffc87f`
  active and `#505050` inactive.
- A tiled window MUST give the border its width out of its own place in the strip: the strip is laid
  out the same with borders on or off, and the window is laid out inside its place by the border's
  width on every side, so the border fills that place and covers nothing beside it. Its limits count
  the border too: a window that needs 400pt gets a place 408pt wide under a 4pt border. Outside the
  window, a border would cover its neighbour less the gap, as the ring does, and every window wears
  one. niri draws its border inside the tile the same way.
- A floating window MUST keep its frame and wear the border outside it. Its frame is the user's or
  the app's, which a border must not resize; niri grows the floating tile instead.
- The border's inner edge MUST be the window's own corner, on its continuous curve, and its outer
  edge that corner grown by its width, as the ring's; its width MUST be whole device pixels, never
  below one.
- With both on, the ring MUST lie outside the border and the border be drawn in front of it, as
  niri stacks them; the ring is drawn under the border in to the window's edge, so the two meet at
  no seam.

## Window rules

- A `window-rule`'s `focus-ring` and `border` MUST set the ring and the border of each window the
  rule applies to, part by part over `layout`'s, every rule that applies in file order, a later one
  overriding an earlier one: a later `off` beats an earlier `on` and the reverse, as in niri. Within
  one rule, `on` and `off` both written MUST be refused, as in `layout`. A rule's block MUST NOT turn
  its band on by being written; only its `on` does, as in niri, where only `layout › border` has
  that exception (`config.md`).
- The rules MUST apply continuously, as niri's do: when a window's focus, whether it floats or its
  title changes, what it wears MUST follow, on its tiles and at rest, with no reload. A rule matching
  `is-active` or `is-focused` recolours the window focus moves to and the one it left, each at the
  time its hairline turns. rini paints one window active, the one holding focus, so `is-active` and
  `is-focused` match the same window.
- Under `is-active` or `is-focused`, on a `match` or an `exclude` line, a rule MUST set colours
  only. A border's `on`, `off` or `width` there MUST be refused: a tiled window gives its border's
  width out of its place in the strip, so a border that changed with focus would lay the strip out
  again on every focus change, at 2-60ms a size write plus a flight (`windows.md`). A ring's `on`,
  `off` or `width` and `geometry-corner-radius` there are refused as not yet supported: the tile's
  bands and corner are laid when it is staged, and only its paint turns during a flight.
- A rule that gives a tiled window another border width MUST lay the strip out again as soon as it
  applies, a title change included, so the window lands inside its place with the new width.
- `geometry-corner-radius` MUST round the window's ring and border, on its tiles and at rest, ahead
  of the radius the window server reports, as niri's does without `clip-to-geometry`. The hairline,
  the shadow and a clipped picture MUST keep the window's own corner: on a tile they draw what macOS
  draws at rest, and a corner that differed between the two would show at the lift. Four radii MUST
  be refused unless equal: rini rounds every corner of a band alike, and the window server reports
  no window with unequal corners.
- A rule that says only what a window wears MUST NOT change what the window opens with: with no
  other rule applying, a new window still opens where its application's windows last were.
- At open a window holds no focus and does not float, as niri reads it, so a rule matching either
  sets nothing the window opens with.
- `draw-border-with-background false` MUST be accepted, since rini draws the bands hollow;
  `true` is refused as not yet supported, since it needs a fill under another application's window.
- niri's `is-active-in-column` MUST be refused: a rini column keeps no window of its own that focus
  lands on, since focus entering a column lands on the row it came from. `is-urgent` is refused as
  not yet supported until rini reads an application asking for attention.

## Through a flight

- Each tile MUST wear the ring and the border its window wears, drawn in front of the tile's
  picture, its hairline and its outline. The tile's outline stays drawn under them, as macOS's own
  outline lies under a ring at rest.
- When focus moves during a flight, each tile's ring MUST turn with its hairline, at the time
  "Focus changing during a flight" in `animation.md` gives it. The window gaining focus on its own
  display wears no ring until its turn and then the active one; the window losing it wears the active
  ring until its turn and then none. When focus moves to another display, each keeps its display's
  ring, and only the paint turns at each one's time: the window losing focus to the inactive, the one
  gaining it to the active. Each tile's border turns its paint at the same time as its hairline,
  by its own focus alone.

## At rest

What this section requires of the ring it requires of the border, and of both together: a window
wearing either is drawn by one rest window holding both.

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

The ring's and the border's settings are `src/decorations/settings.rs`, decoded from
`layout › focus-ring` and `layout › border` in `src/app/config/kdl/layout.rs` and
`src/app/config/kdl/apply.rs`. Who wears the ring, the paint of each by focus and their colours are
`src/decorations/domain/style.rs` and `src/decorations/domain/paint.rs`; what the window rules give
each window, `src/decorations/domain/rules.rs`, matched through `src/windows/domain/rules.rs` and
decoded with each `window-rule` in `src/app/config/kdl/window_rule.rs` and `apply.rs`; each band, its width and
corner, how far the two reach, and their rest window's frame, `src/decorations/domain/shape.rs`; the
layers they are drawn on, `src/decorations/platform/layers.rs`. A tiled window inside its place in
the strip, the border's width in, is `src/layout/domain/tile.rs`, read by
`src/layout/domain/scrolling.rs` with each window's width, which the reactor hands
`src/workspaces/engine.rs` before every pass and after any batch of events that changed one. The reactor decides each window's ring and what
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
