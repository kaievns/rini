# The strip

One horizontal strip of columns per display. A column holds one or more windows stacked vertically.
The strip scrolls left and right under a fixed viewport; it never pans within a single column.

## Columns and widths

- A column MUST be at least 1pt wide and MUST NOT be wider than the viewport. Wider than the viewport
  would leave a region nothing can ever scroll to, because scrolling moves between column starts.
- The windows in one column agree on its width pessimistically: the largest minimum, the smallest
  maximum, and the larger of two locks. A column whose windows demand more than they allow MUST take
  the minimum — a window clipped at the screen edge is recoverable, one too small to use is not.
- A window reporting a maximum of 0 has NO maximum. macOS reports 0 for an unconstrained window, and
  reading it literally collapses the column.
- A locked width raises a column's floor and MUST NOT cap it. A non-resizable window in a wider column
  sits at its own size with space beside it, rather than shrinking the column and dragging its
  neighbours along.
- The inner gap comes out of the columns, not from between them, so two half-width columns plus the gap
  between them add up to exactly the viewport.

## Full width and height

- `ctrl-F` toggles a window to the full usable space and back. The window STAYS on the strip with
  everything else; it is not a separate mode and there is no fullscreen state.
- A full-width column occupies the whole viewport, gap included. It MUST NOT leave a band of
  background down its side.
- When the window is in a column stack, `ctrl-F` MUST pull it out of the stack and maximise both width
  and height. Pressing again MUST put it back where it was, if that place still exists, and the move
  MUST be animated both ways.
- A full-width window MUST come back full-width after a restart.

> **Reported 2026-09-22.** "Most of the windows/apps that were in full-size mode don't come back as
> full sized and respawn as the default 1/2 size... we already fixed it like 3 times in the past."
> Measured in the live layout file: 43 of 75 saved slots had `width: None`. A save that could not read
> a window's width was writing `None` over the remembered value, so every save while a window was
> unreadable erased it. "Could not read" and "is not full width" are now different answers and only
> the second one clears the record. Three earlier fixes had each addressed a path that produced the
> symptom without addressing the overwrite.

> **Reported 2026-09-22.** "Is fullscreen a separate state in rini? How is it different from
> full-width/height?" It was a separate state, and that was the defect: two ways to be maximised, one
> of which left the strip. Deleted. `ToggleFullscreenWithinGaps` is the only maximise, and it keeps the
> window on the strip.

## Folding

- `ctrl-,` folds and unfolds toward the column on the LEFT. `ctrl-.` does the same toward the column on
  the RIGHT. The two MUST be symmetric.
- Folding MUST act on the focused window. It MUST NOT switch focus to another window and fold that one
  instead.
- A folded window MUST NOT be squashed to its title bar. A column's stored heights are either
  deliberate pixel heights from a vertical resize or the equal ratios a freshly folded column starts
  at, and the two MUST NOT be conflated.

> **Reported 2026-09-22.** "The title collapse was not fixed — I tried a fresh new Ghostty terminal,
> it squashed the window on the left to 0 again. Also trying to toggle unfold worked weirdly: I was in
> a folded window at the bottom and pressed ctrl-, again, it switched me to the window on top and
> unfolded that one rather than the one I was focused in. The ctrl-. still worked just fine." Two
> defects in one report: the height-weight conflation, and folding acting on the column's first window
> rather than the focused one.

> **Reported 2026-09-22.** "Folding in/out works on the window on the left, and I want a symmetric
> functionality that lets me do the same to the window on the right." Both directions are now one
> operation with a side argument.

## Off-strip windows

- A window may be taken off the strip ("floating"). It keeps a position per workspace.
- A floating window whose stored position is off every attached screen MUST be centred rather than
  restored where it cannot be seen.
- macOS taking a window into its own fullscreen space MUST take that window off the strip and out of
  rini's management entirely, and returning MUST put it back.

> **Reported 2026-09-22.** "When macOS takes a window fullscreen it should go off-strip and stop being
> managed by rini."

## Where it lives

`src/layout/domain/scrolling.rs` is the layout, `src/layout/domain/constraints.rs` the widths,
`src/layout/domain/strip.rs` the geometry, `src/workspaces/engine/commands/` the commands.
Measurements are in `src/layout/docs/strip.md`.
