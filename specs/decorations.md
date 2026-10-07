# Borders and the focus ring

What rini draws around a window that macOS does not: niri's focus ring. macOS draws each window's
own hairline and shadow; rini draws those only on the overlay's tiles, as `animation.md` requires.

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
- The ring MUST be on unless `layout › focus-ring { off; }` turns it off: 4pt wide, `#7fc8ff`
  active and `#505050` inactive, niri's ring.

> **Reported 2026-10-06.** Kai's decision D1: the focus ring on by default, as niri's, width 4,
> active `#7fc8ff`, inactive `#505050`, taken knowing it covers part of a neighbour (below).

- The ring MUST lie outside the window and take no space from it: the layout is the same with the
  ring on or off. So it covers what is beside the window: with `gaps 3`, a 4pt ring covers 1pt of
  the neighbouring window and runs 1pt past the gap at the display's edge, and with rini's default
  `gaps 0` it covers 4pt. A ring at the display's edge MUST NOT be drawn on the next display.
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

Not yet met: no ring is drawn at rest. Between flights the windows wear none, so a ring shows only on
the overlay's tiles while a flight runs, and disappears at the lift.

## Where it lives

The ring's settings are `src/decorations/settings.rs`, decoded from `layout › focus-ring` in
`src/app/config/kdl/layout.rs` and `src/app/config/kdl/apply.rs`. Who wears it, its paint by focus
and its colours are `src/decorations/domain/style.rs` and `src/decorations/domain/paint.rs`; its
band, width and corner, `src/decorations/domain/shape.rs`; the layer it is drawn on,
`src/decorations/platform/layers.rs`. The reactor decides each window's ring in
`src/app/reactor/decorations.rs`, from each workspace's active window
(`src/workspaces/engine.rs`). When a tile's ring turns is `src/transitions/domain/lifecycle.rs`,
carried out by `src/transitions/platform/engine.rs` on the overlay
(`src/animation/platform/overlay.rs`).
