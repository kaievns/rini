# The scrolling strip

Rules `src/layout/domain/scrolling.rs` and the workspaces engine's focus paths
enforce, with the failure each one answers. niri is the reference where behaviour
is borrowed.

## Column width

- **A lone column keeps its width.** It used to be expanded to the full
  viewport, which made a window's size depend on how many *other* windows the
  workspace held: a full-size window moved from workspace 1 to 2 to 3 went
  half-size on 2 and full on 3 with nothing about the window changing. niri
  does the same; full width is `maximize-column`'s job, here
  `toggle_fullscreen_within_gaps`, and that mode is remembered per display.
- **There is one maximize mode, and it stays in the strip.** A second mode,
  `toggle_fullscreen`, used to assign the usable frame with its own absolute
  origin. That differed from this one by nothing but the outer gap, and it
  reserved no space in the strip and did not scroll: the window sat still while
  its neighbours slid under it, and the column it had left behind stayed the
  width it was. It is deleted rather than fixed, because macOS's own fullscreen
  already covers "cover everything". A window that fills the tiling area is a
  full-width COLUMN, which is what niri means by `maximize-column`.
- **A maximized window never outgrows the space reserved for it.** The column's
  reserved width is clamped to what its windows accept; the frame was not, so a
  window with a maximum width was handed the whole tiling width and the next
  column was laid out on top of it. Both sides read
  `constraints::clamp_to_constraints` now. A minimum larger than the slot does
  not grow the window past it — overlap is worse than a window smaller than it
  asked for.
- **Maximizing a stacked window pulls it out of the stack.** It fills the tiling
  area, which would otherwise cover the siblings sharing its column while the
  tree still claimed they were abreast. It becomes its own column immediately to
  the right, and a second press puts it back beside the neighbour it left
  (`StackOrigin`). A neighbour is remembered rather than a row index, because
  while the window is maximized its old column can move along the strip or
  change size. If every window it could return to has closed, it stays the
  column it became rather than being put somewhere the user never had it.
  The move rides the ordinary layout pass, so it animates like a join or unjoin.
- **Inner gaps are absorbed into the column width so N columns of ratio 1/N
  fit.** Without it two 0.5 columns need `2*(0.5*W) + gap`, one gap more than
  the viewport; the second column is never fully visible, reveal-on-demand
  nudges the strip on every focus change, and the pair shifts by the gap width
  (measured on a 1720pt viewport with a 4pt gap: x alternated between
  `[0, 864]` and `[4, 868]`). N columns have N-1 gaps, so each gives up
  `(N-1)/N` of a gap, with N inferred from the ratio requested:

  | ratio   | N | column | total            |
  |---------|---|--------|------------------|
  | 0.5     | 2 | 858.00 | 2 + 1 gap = 1720 |
  | 0.33333 | 3 | 570.66 | 3 + 2 gaps = 1719.98 |
  | 0.25    | 4 | 427.00 | 4 + 3 gaps = 1720 |

  Gapless configs, single columns and degenerate widths are left alone.
- **A full-width column keeps its strip x.** Assigning the whole tiling rect,
  origin included, lifted the window out of the strip's coordinate space: it
  stopped scrolling, stayed pinned at the viewport's left edge, and other
  columns slid over it (Slack made full-size, then focus moved away).
- **Changing a width rescrolls to keep the column visible.** Every column start
  after it moves, so a window at the right edge grew off screen and the key
  looked dead until a focus nudge forced a reveal.

## Navigation

- **Floating windows are not strip members.** Left/right from a floating window
  goes to the strip and resumes at the strip's own selection; left/right at the
  strip's end stops there. Both branches used to walk the floating set: with
  Zoom and System Settings floating, ctrl-J/L cycled those two instead of the
  columns, the escape landed on the *first* column, and walking off the last
  column stepped onto Settings and bounced back. Floating windows are still in
  the workspace and reachable with cmd-tab and `toggle_focus_floating`. A
  floating branch that advanced with `(idx + 1) % len` was a closed cycle that
  never reached the fallback.
- **Resume at the strip's selection, not `first()`.** Taking the first column
  unconditionally is why leaving a floating window jumped to the leftmost
  column.
- **Only left/right report an edge hit.** Up/down is not a strip axis; a
  stack's top is not an edge the view can bounce against.
- **With `isolate_displays`, horizontal focus stops at the display's strip
  end** instead of continuing onto the neighbour; vertical still crosses.

## Joining

`toggle_stack` moves the *selected* window into the *previous* column (niri's
consume-into-column), falling back to the next column only in the first
column. It used to pull the next column's windows into the current one, which
stacked a window the user had not chosen and left the selection untouched.

## Parking

Off-strip windows park at 1pt corners; live parks were measured at y=1085
showing a 32pt band along the bottom. Both must read as off screen, or a parked
window animated back in travels from the bottom corner instead of entering from
the strip's edge (`src/workspaces/domain/hidden_window_placement.rs`).
