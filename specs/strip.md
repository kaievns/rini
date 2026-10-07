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
- The configured widths (`layout › preset-column-widths`) MAY be any proportions in `(0, 1]`; anything
  else MUST be refused when the config is read, naming the line. A proportion of 1.0 means full width,
  which is the maximise mode rather than a ratio.

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
  though full width is above `max-column-width`, which deliberately stops the incremental resize
  keys, and not a mouse drag, from crawling to 100%.
- **Being given a width is what ends full width.** A window sized by `ctrl-R` or by a resize key MUST
  stay where it is and MUST NOT be returned to the stack `ctrl-F` pulled it out of. It becomes an
  ordinary column with nowhere to return to, and the next `ctrl-F` MAY maximise it afresh.
- **A resize key MUST NOT move a width against its own direction.** A grow at or above
  `max-column-width`, and a shrink at or below `min-column-width`, MUST do nothing: the bound would
  pull the width back across itself. So growing a full-width column does nothing, where a "grow"
  landing on `max-column-width` would make the window smaller. A shrink from above the bound, out of
  full width or a dragged width, MUST end full width and start from the width the column HAS, not
  the width it had before being maximised.
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
> adopted as a column width, clamped to `max-column-width`, or swallowed as a late echo of
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
> column widths: it stepped the strip by `max-column-width` of the viewport for a column
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

## Resizing with the mouse

- **A drag on a window's left edge MUST move that edge and keep the right one.** The column takes the
  dragged width, its right edge stays where it was, and the columns before it scroll by the change.
  The window ends where the cursor let go, less the column's share of the gap.
- A column grown past the viewport's left edge MUST end with its left edge at the viewport's left
  edge and the rest of its growth on the right. The first column therefore ends grown to the right,
  as niri ends a resize of its first column.
- A shrink from the left with the strip at its start MUST keep the left edge and shrink from the
  right. Keeping the right edge would need space before the first column, which the strip never
  shows. A strip scrolled by less than the shrink keeps the right edge until it reaches its start.
- Under `center-focused-column "always"` the centring places the column, not the kept edge. A
  centred column MUST take twice the dragged edge's travel, so it stays centred and the dragged edge
  ends where the cursor let go. The first and last columns sit flush, so they MUST take the dragged
  width and grow away from their flush edge.
- **A drag MUST NOT be held to `min-column-width` or `max-column-width`.** Those bound the resize
  keys and the default width. A dragged column takes the dragged width, held only by its windows'
  own limits and the viewport, and keeps its right edge; a centred column's doubled width likewise.
  The width MUST stay unbounded while the app settles its own size, when the window moves to another
  workspace or display, and across a restart, until a resize key or `ctrl-R` gives the column a
  width the bounds hold again. A drag that moves neither the left nor the right edge leaves the
  column as bounded as it was.
- The kept edge belongs to the window dragged. When focus moves to another column, or the dragged
  window closes, before the strip is laid out again, the column focused next MUST be placed as any
  focus change places it.
- Letting go of a left-edge drag MUST NOT move the resized window on screen: it is drawn from where
  the cursor let go, and only the columns the change scrolls move. A column after it MUST NOT be
  seen at all when it was off screen beyond the right edge, since the kept right edge keeps its
  place.
- Letting go of a resize MUST leave the resized window focused, whatever window the cursor is over:
  the cursor lets go on the window's edge, where the window behind it can answer. Focusing the
  window already focused MUST keep the edge its drag keeps.
- A drag on a full-width column's left or right edge MUST end full width at the dragged width. A
  drag that moves neither, or the app settling its own size under it, MUST leave it full width.
- A drag across the whole viewport MUST make a window alone in its column the full-width column, with
  the width the drag started from to go back to. A stacked window MUST stay in its stack, as a column
  the whole viewport wide, because full width would pull it out.
- A drag MUST resize only the focused window. Any frame reported under a held button counts as the
  drag, including rini's own write to a neighbour coming back, and resizing that neighbour would
  fight the user.
- **A drag on a stacked window's top or bottom edge MUST trade height only with the window across
  that edge.** Every other window in the column keeps its height. An edge with no window across it,
  the top window's top or the bottom window's bottom, MUST change nothing.
- A drag that never moves the top or bottom edge MUST leave the column's heights as they were. A
  column nobody resized vertically then still divides evenly when a window joins or leaves it.
- A stacked edge dragged back to where it started MUST give the window across it its height back.
- A window that resizes itself MUST keep its column where it was: the app's new width is taken and
  its origin is not. Its new height is taken from the other windows in its column, in their
  proportions.

> **Reported 2026-10-06.** "the left edge is finicky. i can reduce the size ok. and i can extend it
> but it extends to the right not to the left. and when the right edge reaches the screen edge it
> will stop resizing". rini placed every column by its start along the strip and took the dragged
> window's new width without its origin, so every left-edge drag kept the left edge and moved the
> right one. A grow then stopped at `max-column-width`, two thirds, 1148pt on that display, and the
> reveal had put that column flush with the screen's right edge: that is the stop Kai saw, and a drag
> no longer meets either bound. macOS
> played no part: in all 17 edge presses recorded, the pressed window took the resize and no
> neighbour resized across the 3pt gap.

> **Reported 2026-10-06**, after the left edge first grew to the left: "it sort of works, but it
> makes a weird back and fourth wobble on release". The release scrolls the strip by the left edge's
> travel as one pan, which draws the finished layout and starts the view back by the scroll. The
> resized window already stood where the scroll puts it, so its tile started the drag's travel to
> the right of where it was let go and slid back: 189pt and 275pt in two of the 15 releases
> journalled, read off the frames either side. Its picture at the press was refused, asked for
> where its tile started, so the tile flew the cache's.

> **Reported 2026-10-07.** "if the window's right edge is at the screens right edge and i _shrink_
> the window by the left edge, on release during the strip animation the off-screen window on the
> right jumps under the resized window by what looks like ~100px and then animates away. in this
> case i shouldn't see it at all since it's off the screen beyond the right screen edge to begin
> with." The release's pan starts every window it does not find on screen at its new place shifted
> back by the scroll, which is where the scroll carried it from only when the strip changed nothing
> else. The shrink pulled the columns after the dragged one left on the strip as the scroll moved
> them right, so on screen they kept their place, and the one parked beyond the right edge was drawn
> the shrink onto the screen and slid off. It now starts where the strip had it at the last pass.

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
Which edge a mouse drag moves, and what it asks of the column, is `src/layout/domain/edge_drag.rs`.
A title-bar zoom is told from a resize by `src/windows/domain/zoom.rs`, and the full-width toggle both
routes share is in `src/workspaces/engine/commands/floating.rs`.
Measurements are in `src/layout/docs/strip.md`.
