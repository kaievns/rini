# `overview` — every workspace of a display at once, while its key is held

The overview zooms one display out to every one of its workspaces, a row each, stacked top to
bottom in the stack's order. Each row is its strip at one depth, aligned on the window a switch to
that workspace focuses. The requirements are `specs/overview.md`.

This feature is the scene and nothing else: pure geometry over `rini-core`'s ids in rini-geometry's
`Overlay` space. It names no other feature and has no `platform/`, as `layout` has none. The
reactor fills its input from the layout, and the features that draw it read its `Scene` directly.

## What it owns

| | |
|---|---|
| `domain/scene.rs` | `OverviewInput` in, `Scene` out (`lay_out`): one `RowScene` per workspace, its band, the window it is aligned on, an empty one's outline, and a `SceneTile` per window with where it is drawn, where its row lays it out (`in_row`) and its `Holder`; the `Selection` the active ring is on |
| `domain/depth.rs` | The depth every row is drawn at (`depth`, between `DEEPEST` and `SHALLOWEST`) and each row's top down the display, centred when the stack fits and scrolled the least that keeps the selected row in view when it does not (`row_tops`) |
| `domain/stacks.rs` | The columns that collapse into a stack at either edge (`edge_stacks`): the walk out from the pivot column, the top card's place, the cards behind it, and how many are drawn |
| `domain/changes.rs` | What changed between two scenes (`changes`): each thumb that moved, changed holder or changed row, each row's pan, the stack's scroll, and the selection's move |
| `domain/fixture.rs` | Tests only: strips laid out as the layout lays them, on the built-in display |

## The shape that matters

**One axis.** Every row's current window is centred on the tiling area's centre x. The centre is
the one place every row can put its current window whatever its strip's scroll, and it keeps the
selection still while a press pans its row under it. A row reads its anchor off the current
window's frame, so the real strip's own scroll moves no thumb. A row whose current window floats,
or that has none, keeps its strip where it shows.

**A row is the usable frame at the scene's depth.** Tiles, floating windows and an empty row's
outline all map through the same transform, so a floating window keeps its place on the display
and a stacked column is drawn as the strip draws it.

**Stacks are the switcher's turned on their side.** The constants are the overview's own rather
than read from `switcher`, which is downstream of every feature that could draw this. The pivot,
the current column or with none the one nearest the axis, never stacks.

**A pan is read off one window on the strip in both scenes.** The window the row is aligned on
after, else the one it was aligned on before, else any window in both. A move along the strip keeps
the moved window on the axis, so it pans nothing; the columns it passes change places beneath it.

## Reading order

`domain/depth.rs` → `domain/stacks.rs` → `domain/scene.rs` → `domain/changes.rs`.

## Known debt

Nothing reads the scene yet: the input mode, the reactor's wiring and the drawing are the steps
`specs/overview.md` marks not yet met.
