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
| `domain/strip.rs` | Where the strip sits and how wide its columns are: `anchor_x`, `column_starts`, `gap_share`, `reveal_offset` |
| `domain/constraints.rs` | `column_ratio` for how wide a column asks to be, `solve_axis_lengths` for row heights, and `clamp_to_constraints` for what a window will accept |
| `domain/preset_width.rs` | Which width the next `ctrl-R` asks for, and whether that width is the maximise mode |
| `domain/area.rs` | The tiling rect: the usable frame minus the outer gaps |
| `domain/boundary.rs` | What a swipe means once the strip has run out: which workspace step a strip edge implies, and the inversion flag that swaps it |
| `settings.rs` | Ratios, gaps, presets, alignment, navigation style |

## The shape that matters

**A column is the unit, not a window.** A column holds one or more windows stacked
vertically. Widths are per column; heights are per row within one.

**Two meanings of `height_weights`.** Equal ratios in a column nobody has resized, and
desired pixel heights after a deliberate vertical resize. `Column::height_overridden`
says which, and conflating them collapsed a folded window to its title bar.

**Reservation and frame must agree.** `calculate_layout` decides a column's width
first, then assigns frames inside it. Both clamp to the window's limits through
`clamp_to_constraints`; when only one did, the next column was laid out on top.

**Maximize stays in the strip.** There is one maximize mode and it keeps its
strip-relative x, so the column still scrolls. The second mode rift had is deleted.

**Maximized is a width, and also a mode.** It is the widest entry in the preset cycle,
so `ctrl-R` reaches it and a step off it unmaximizes. It cannot be stored as a ratio
because `max_column_width_ratio` is below 1.0 on purpose, which is why every rule that
asks how wide a column is has to ask `column_ratio` rather than adding up the offset.

## Reading order

`domain/strip.rs` (small, pure, tested) → `domain/constraints.rs` →
`domain/scrolling.rs::calculate_layout`.

## Detail

- [`strip.md`](strip.md) — every rule the strip enforces and the failure each one
  answers: column width, navigation, folding, maximizing, parking
