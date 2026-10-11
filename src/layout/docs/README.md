# `layout` — the scrolling strip

Pure geometry over window ids and frames. It has no `platform/` because it touches
nothing outside itself: no macOS, no workspaces, no spaces. Given a screen rect, a set
of columns and each window's size limits, it says where every window goes.

This is rini's core domain. It is also the only feature that would be reusable in
principle, and deliberately is not reused — see "Why this shape" in
[`docs/architecture.md`](../../../docs/architecture.md).

## What it owns

| | |
|---|---|
| `domain/scrolling.rs` | `ScrollingLayoutSystem`: the columns, the selection, the scroll offset, and `calculate_layout`. The one layout system there is |
| `domain/strip.rs` | Where the strip sits and how wide its columns are: `anchor_x` and `centres`, `column_starts`, `gap_share`, `reveal_offset`, `offset_keeping_right_edge`, and `place_share` and `place_x` for the place a width change by key keeps a column at |
| `domain/edge_drag.rs` | One report of a mouse drag (`Drag`), which edge of a window it moves, read off the frame it started from and the one it has now, the width the drag asks of its column, and the height a stacked edge trades with the window across it |
| `domain/constraints.rs` | `column_ratio` for how wide a column asks to be by its `ColumnKind`, `column_limits` for what its windows' slots allow, `column_width` for how wide that lays it out, `holds_viewport` for the columns held to the viewport, which no maximum narrows, and `ratio_of_column_width`, its inverse, for the ratio a reported width was laid out at, `solve_axis_lengths` for row heights, and `clamp_to_constraints` for what a window will accept |
| `domain/tile.rs` | A tiled window and its slot, the window and the border it wears: the border each window wears (`Borders`), the window inside a slot (`window_in`), the slot of a reported frame (`slot_of`), and a slot's limits (`slot_limits`) |
| `domain/preset_width.rs` | Which width the next `ctrl-R` asks for, and whether that width is the whole viewport, past the bounds |
| `domain/area.rs` | The tiling rect: the usable frame minus the outer gaps |
| `domain/boundary.rs` | The ends of the strip and of the workspace stack: `strip_edge`, the left/right end a command stops at rather than crossing displays, and `workspace_stack_direction`, the end a `next`/`prev` step runs into |
| `settings.rs` | Ratios, gaps and struts, presets, where focus sits (`center-focused-column`), insertion |

## The shape that matters

**A column is the unit, not a window.** A column holds one or more windows stacked
vertically. Widths are per column; heights are per row within one.

**Two meanings of `height_weights`.** Equal ratios in a column nobody has resized, and
desired pixel heights after a deliberate vertical resize. `Column::height_overridden`
says which, and conflating them collapsed a folded window to its title bar.

**Reservation and frame must agree.** `calculate_layout` decides a column's width
first, the slot it reserves, then lays each window out inside its slot, the
border's width in (`tile::window_in`), and clamps it there to the window's limits
through `clamp_to_constraints`, as the column's width clamps to them with the
border counted (`tile::slot_limits`); when only one clamped, the next column was
laid out on top.

**Maximize stays in the strip.** There is one maximize mode and it keeps its
strip-relative x, so the column still scrolls. The second mode rift had is deleted.

**Maximized is a mode; 100% is also a width.** `ctrl-F` maximizes and remembers where
the window came from. The preset cycle's 1.0 is a width held only to the viewport, the
whole column and its stack kept, and a step off a maximized window unmaximizes it. A
maximized column cannot be stored as a ratio because `max-column-width`, which bounds
every ratio but an unbounded one, is below 1.0 on purpose, which is why every rule that
asks how wide a column is has to ask `column_ratio` rather than adding up the offset.

**A maximized window is alone in its column.** Maximizing a stacked window pulls it
out, and a window that comes to share a maximized one's column ends that full width
where it is.

## Reading order

`domain/strip.rs` (small, pure, tested) → `domain/constraints.rs` →
`domain/scrolling.rs::calculate_layout`.

## Detail

- [`strip.md`](strip.md) — every rule the strip enforces and the failure each one
  answers: column width and maximizing, navigation, joining and folding, the windows'
  own limits, mouse resize, parking
