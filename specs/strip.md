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
- Changing the gap between windows MUST leave the focused column where it was on screen, with the
  columns around it re-spaced by the new gap.

> **Found 2026-09-30, while acting on a report.** "can you check the gap between windos is 50/50
> configurations? i think it's smaller than other gaps". It was: 2pt between windows against 4pt at
> the edges. Raising it to 4 in the config left a full-width column two columns along the strip at
> left 6 / right 2, because the scroll position is kept in points and each column's start includes
> the gaps before it.
- `ctrl-R` steps through the configured preset widths in order and wraps: a third, a half, two thirds,
  full, then a third again. A column at a width that is no preset — what the incremental resize keys
  leave behind — MUST join the cycle at the first preset above it rather than at the start.
- The configured widths MAY be any ratios in `(0, 1]`; anything else is ignored. A ratio of 1.0 means
  full width, which is the maximise mode rather than a ratio.

## Full width and height

- `ctrl-F` toggles a window to the full usable space and back. The window STAYS on the strip with
  everything else; it is not a separate mode and there is no fullscreen state.
- A full-width column occupies the whole viewport, gap included. It MUST NOT leave a band of
  background down its side.
- When the window is in a column stack, `ctrl-F` MUST pull it out of the stack and maximise both width
  and height. Pressing again MUST put it back where it was, if that place still exists, and the move
  MUST be animated both ways.
- A full-width window MUST come back full-width after a restart.
- **Full width is the widest of the preset widths, not a state outside them.** `ctrl-R` MUST size a
  full-width window like any other, and MUST reach full width when the cycle comes round to it — even
  though full width is above `max_column_width_ratio`, which deliberately stops the incremental resize
  keys from crawling to 100%.
- **Being given a width is what ends full width.** A window sized by `ctrl-R` or by a resize key MUST
  stay where it is and MUST NOT be returned to the stack `ctrl-F` pulled it out of. It becomes an
  ordinary column with nowhere to return to, and the next `ctrl-F` MAY maximise it afresh.
- Growing an already-full-width column MUST do nothing: there is nothing wider than the viewport, and
  a "grow" that lands on `max_column_width_ratio` would make the window smaller. Shrinking one MUST
  end full width and start from the width it HAS, not the width it had before being maximised.
- **A window the user zooms from its title bar MUST become the full-width column**, the same one
  `ctrl-F` makes, whatever size the app zooms it to: the screen, or its content as Safari and Finder
  do. It MUST end exactly at rini's full-width frame and stay there when it is clicked again. Zooming
  it again from its title bar MUST restore the width and the stack it had, whatever size the app
  reports on the way back. The two are one toggle, so `ctrl-F` MUST undo a zoom and a zoom MUST undo
  `ctrl-F`.
- One double-click MUST toggle once. The frames the app reports after its zoom MUST NOT become the
  column's width, and MUST NOT keep rini's frame off the window.
- Only a double-click on that window's own title bar makes its resize a zoom, and only for 1 second
  after the click. A double-click in a window's content or on another window, a drag that starts after
  the double-click, native fullscreen, and a floating window MUST each be handled exactly as with no
  double-click at all. `ctrl-F` or `ctrl-R` pressed after a double-click MUST keep the width it gave.

> **Reported 2026-09-30.** "when i double tap a window title to maximise it, rini doesn't pick up the
> change and when i click to the window again it gets resized back to original rini's size. it should
> just go the normal rini full-size window". The zoom reached rini as ordinary resize reports: either
> adopted as a column width, clamped to `max_column_width_ratio`, or swallowed as a late echo of
> rini's own last write. Either way the next click's layout pass wrote the column back. Nothing in
> Accessibility marks a zoom, so a double-click on the window's title bar, seen by the input tap, is
> the evidence: that window's first resize within 1 second of the click toggles full width.

> **Reported 2026-09-24.** "When I put a window to full-size using ctrl-F it takes full-size shape, but
> it stops responding to ctrl-R size cycling... In reality full-size is just one of the predefined
> sizes with a special case that it remembers previous configuration... ctrl-R should cycle through
> 1/3, 1/2, 2/3, and 1." The cycle was writing a width that the layout pass then ignored, because a
> column holding a full-width window is 1.0 whatever its stored width says. The default preset list
> gained 1.0, and one rule now decides whether a step means the maximise mode or a ratio. Kai chose
> niri's direction after considering a downward cycle: ascending, with full width wrapping to 1/3.

> **Found 2026-09-24, not reported.** The same blind spot in the scroll gesture, which computed its own
> column widths: it stepped the strip by `max_column_width_ratio` of the viewport for a column
> occupying all of it, so one swipe left the next column part-way on screen.

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

- Only a window arriving on a fullscreen space goes fullscreen. A window leaving one MUST NOT be taken
  off the strip for it, because macOS can report the departure after the window is already back on
  its user space. Read as an entry, that report pulled the window off again for good, and no
  switcher offered it.

> **Reported 2026-09-29.** "zoom shows as a single window on the app switcher, can't switch between
> the main window and the call window for example. it only shows the main window in all the
> switchers". The call window had been fullscreen and back.

## Where it lives

`src/layout/domain/scrolling.rs` is the layout, `src/layout/domain/constraints.rs` the widths,
`src/layout/domain/strip.rs` the geometry, `src/workspaces/engine/commands/` the commands.
A title-bar zoom is told from a resize by `src/windows/domain/zoom.rs`, and the full-width toggle both
routes share is in `src/workspaces/engine/commands/floating.rs`.
Measurements are in `src/layout/docs/strip.md`.
