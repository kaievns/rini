# `overview` — every workspace of a display at once, while its key is held

The overview zooms one display out to every one of its workspaces, a row each, stacked top to
bottom in the stack's order. Each row is its strip at one depth, aligned on the window a switch to
that workspace focuses. The requirements are `specs/overview.md`.

This feature is the scene and nothing else: pure geometry over `rini-core`'s ids in rini-geometry's
`Overlay` space. It names no other feature and has no `platform/`, as `layout` has none. The
reactor fills its input from the layout and hands each scene to the flight engine, and the features
that draw it read its `Scene` directly: `animation`'s overview choreography
(`src/animation/domain/choreography/overview.rs`) turns it into the overlay's score, and
`transitions`' engine plays that on its own lifecycle (`src/transitions/domain/lifecycle/overview.rs`,
`src/transitions/platform/engine/overview.rs`).

## What it owns

| | |
|---|---|
| `domain/scene.rs` | `OverviewInput` in, `Scene` out (`lay_out`): one `RowScene` per workspace, its band, where its strip starts and where the display's top-left is drawn for its floating windows, the window it is aligned on, an empty one's outline, and a `SceneTile` per window with where it is drawn, where its row lays it out (`in_row`) and its `Holder`; the `Selection` the active ring is on |
| `domain/depth.rs` | The depth every row is drawn at (`depth`, between `DEEPEST` and `SHALLOWEST`) and each row's top down the display, centred when the stack fits and scrolled the least that keeps the selected row in view when it does not (`row_tops`) |
| `domain/stacks.rs` | The columns that collapse into a stack at either edge (`edge_stacks`): the walk out from the pivot column, the top card's place, the cards behind it, and how many are drawn |
| `domain/changes.rs` | What changed between two scenes (`changes`): each thumb that moved, changed holder or changed row, each row's pan, the stack's scroll, and the selection's move; the flight engine logs each scene it plays by it |
| `domain/fixture.rs` | Tests only: strips laid out as the layout lays them, on the built-in display |

## The shape that matters

**One axis.** Every row's current window is centred on the tiling area's centre x. The centre is
the one place every row can put its current window whatever its strip's scroll, and it keeps the
selection still while a press pans its row under it. A row reads its anchor off the current
window's frame, so the real strip's own scroll moves no thumb. A row whose current window floats,
or that has none, keeps its strip where it shows.

**A row is the usable frame at the scene's depth.** Tiles, floating windows and an empty row's
outline all map through the same transform, so a floating window keeps its place on the display
and a stacked column is drawn as the strip draws it. A row's `strip` is where its first column
starts: every tiled thumb sits at its window's place on the strip from there, so the overlay draws
a row as one container that a press along it moves, and no thumb in it, and the real strip's own
scroll moves neither.

**Stacks are the switcher's turned on their side.** The constants are the overview's own rather
than read from `switcher`, which is downstream of every feature that could draw this. The pivot,
the current column or with none the one nearest the axis, never stacks, and no card covers it: a
top card whose 16pt inset would bring it nearer the pivot than the row's gap beside it is pushed
out to that gap and sits partly past the edge. Wide columns on a display with one or two rows hit
this; at depth 0.5 a full-width neighbour's card would otherwise cover 424pt of the selection.

**A pan is read off one window on the strip in both scenes.** The window the row is aligned on
after, else the one it was aligned on before, else any window in both. A move along the strip keeps
the moved window on the axis, so it pans nothing; the columns it passes change places beneath it.

## Reading order

`domain/depth.rs` → `domain/stacks.rs` → `domain/scene.rs` → `domain/changes.rs`.

## Known debt

**The ring thins as the overview zooms.** A thumb's ring and border are drawn at their width in the
strip's points, so the zoom out draws them thinner as it goes, down to the depth's share of their
width: a 1pt ring is 0.22pt on screen at depth 0.22. While held they are dressed again as wide on
screen as at rest (`Command::Widen`), and narrowed again as the zoom in begins: at rest and while
held they read at their width, but they thicken at once as the zoom out lands and thin at once as
the zoom in begins. Keeping them at their width through the zoom needs each band's width to follow
the zoom's track.

**A click reaches the window under the overview.** The overlay ignores the mouse, so a click
inside the overview reaches whichever real window is under the pointer behind it
(`specs/overview.md`, "Keys inside it", not yet met).

**An overview opened while a flight runs waits for it to lift** rather than taking the flight's
tiles from where they are drawn: up to the flight's length, 0.35s by default.

**Focus stops following the mouse in the reactor, not in the tap.** The reactor ignores the
pointer entering a window while the overview is open, and never asks the tap to stop following it:
the tap's switch for it, `Request::SetFocusFollowsMouseEnabled`, changes the event mask when
focus-follows-mouse is configured, and a new mask replaces the tap
(`rebuild_event_tap_mask_if_needed`), which closes the overview (`KeyRouter::tap_replaced`). A menu
opening or closing while it is open still sends that request (`update_focus_follows_mouse_state`),
and can close it that way.
