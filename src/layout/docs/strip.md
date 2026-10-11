# The scrolling strip

Rules `src/layout/domain/scrolling.rs` and the workspaces engine's focus paths
enforce, with the failure each one answers. niri is the reference where behaviour
is borrowed.

## Column width

- **A lone column keeps its width.** It used to be expanded to the full
  viewport, which made a window's size depend on how many *other* windows the
  workspace held: a full-size window moved from workspace 1 to 2 to 3 went
  half-size on 2 and full on 3 with nothing about the window changing. niri
  does the same; full width is asked for, by `Mod+F`, and that mode is
  remembered per display.
- **The widest preset is a width, not the mode.** `Mod+R` reaching 1.0 makes
  the whole column, its stack kept, the viewport wide; it pulls no window out
  and records nothing to return to, which only the full-width mode does, entered
  by `Mod+F` or its title-bar zoom, a lone window's drag across the viewport or
  a restart restoring one. Entering the mode there split a stacked window out
  and broke the column (Kai, 2026-10-10). It cannot be a bounded ratio like the
  others — `max-column-width` is below 1.0, 0.9 by default, so a preset of 1.0
  asked for as one comes back clamped to it — so `preset_width::next_preset`
  answers `Full` or `Ratio`, and `Full` is given `width_unbounded`, the
  representation a drag across the viewport already gives a stacked column. The
  app resizing its window under such a column, by however much, leaves it the
  viewport wide, as under the mode: `on_window_resized` reads any report under
  a column `constraints::holds_viewport` names as answering rini. Nor does a
  maximum narrow it: `constraints::column_width` skips the windows' maximum for
  a column held to the viewport, so the window sits at its maximum inside a
  viewport-wide column and the gap shows the app's limit (Kai, 2026-10-11, in
  `specs/strip.md`). Narrowed to it, the column's own slot was the window's,
  so the exemption missed it and any report short of that slot was taken as
  the column's width. Read back, a window
  settling 20pt short sat at 0.988, over the 1% `next_preset` reads as the same
  width, so the next `Mod+R` found 1.0 above it again and stood still. A maximized window still takes its
  turn: `ratio_of` reads it as 1.0, where the stored `width_offset` the cycle
  used to read made `Mod+R` look dead on it.
- **Being given a width is what unmaximizes a window.** `Mod+R` and the resize
  keys both do it, and the window stays where it is rather than going back to
  the stack `Mod+F` pulled it out of: its `full_width_origins` entry is
  FORGOTTEN, not kept. A kept one turned the next `Mod+F` pair into a teleport —
  maximizing an already lone window records no new origin, so the press that
  unmaximized it read the stale one and dropped the window into a stack untouched
  since.
- **A key never moves a width against its direction.** A grow at or above
  `max-column-width` and a shrink at or below `min-column-width` do nothing,
  because the bound would pull the width back across itself. Growing a
  maximized column used to unmaximize it onto `max-column-width`, *narrower*
  than what it had, so "grow" visibly shrank the window; a column dragged past
  the bound would have done the same. A shrink from above starts from the
  width the column has, 1.0 for a maximized one rather than the width it had
  before.
- **One place applies the width bounds.** `constraints::column_ratio`, read by
  the layout pass, by `scroll-strip`'s step, by the default width, by the
  resize keys and by a resize report, each with a `ColumnKind`: full width,
  unbounded, or bounded. The layout pass and `scroll-strip` pass the column's own. The
  default width and the keys always pass `Bounded`, which is how a key bounds a
  dragged width again. A report passes `Unbounded` for a side drag or a column
  already unbounded, and `Bounded` otherwise.
  `scroll-strip` used to compute its own and did not know about maximized columns,
  so it stepped the strip by two thirds of a screen for a column occupying all
  of it and one step left the next column part-way on.
- **There is one maximize mode, and it stays in the strip.** A second mode,
  `toggle_fullscreen`, used to assign the usable frame with its own absolute
  origin. That differed from this one by nothing but the outer gap, and it
  reserved no space in the strip and did not scroll: the window sat still while
  its neighbours slid under it, and the column it had left behind stayed the
  width it was. It is deleted rather than fixed, because macOS's own fullscreen
  already covers "cover everything". A window that fills the tiling area is a
  full-width COLUMN, which is what niri means by `maximize-column`.
- **A title-bar zoom is that maximize, not a resize.** Double-clicking a title
  bar has macOS zoom the window, and Accessibility reports it as ordinary
  resizes. rini took the last of them either as a column width, clamped to
  `max-column-width`, or, with its own last write still pending, as that
  write landing a few points off, and either way the next click's layout pass
  wrote the column back. Nothing in the report marks a zoom, so a double-click
  (`kCGMouseEventClickState` 2 on a left press) on the window's title bar is the
  evidence. That window's first resize within a second of the click toggles
  full width through the same path as `Mod+F`, whatever size the app chose: a
  column becomes full width, and a full-width window goes back to the width and
  stack it had. Every later frame the app reports in that second is the rest of
  the zoom: none becomes a width, the window is not left out of layout passes,
  and a pass puts rini's frame back over it. The rule is
  `src/windows/domain/zoom.rs`; the reading is `title_bar_zoom` in
  `src/app/reactor/observations.rs`.

  The title bar is the top 60pt of the frame rini last put the window at.
  AppKit's title bar is 28pt and a unified toolbar about 52pt, and the tab
  strips Terminal and Chrome add are inferred to sit within that; lower is
  content, where a double-click selects a word or opens a file. None of those
  heights is measured here. A point on two windows' bands counts for neither.
  The frame is where rini last committed the window, so during a strip
  animation it is where the window will land rather than where it is on screen,
  and a double-click then can miss (inferred, not seen).

  The press has to reach the reactor before the zoom's first report, so it does
  not go through the main thread, where a busy moment would let the report
  overtake it; why the direct path cannot lose that race is in
  `src/input/docs/README.md`. Any later press spends the double-click, so a drag
  started after it is a drag, and so does any rini layout command: what a window
  does after `Mod+F` or `Mod+R` answers rini. A third or fourth click of the
  same run is a later press too, so if AppKit zooms again on the fourth, that
  zoom goes unanswered (not measured). The second runs from the press's own
  timestamp. Not measured: how long an app's zoom animates, which the second has
  to cover.

  Not a zoom: a frame equal to the whole display, which is native fullscreen,
  since a zoom stops below the menu bar; a resize that moves only the top edge
  up, which is macOS stretching the top edge a double-click landed on; rini's own
  write coming back; a window that is not a column. On a display with no menu
  bar showing and no Dock, a zoom to the screen would equal the whole display
  and be taken for fullscreen (inferred, not seen). An app putting back a frame
  rini has already overruled once is left to the ordinary path: overruling it
  again starts a fight lasting the whole second, one write per report, with an
  app that snaps its size to a grid after every write.
- **macOS's own fullscreen takes the window off the strip entirely.** It is on a
  space of its own and rini does not manage it while it is there: it leaves the
  layout tree (`WindowRemovedPreserveFloating`, so the workspace assignment and
  floating state survive to bring it back), and every space-resolution rule
  answers `None` for it rather than the space it used to be assigned to.

  Both halves used to be conditional and both let a fullscreen window keep a
  column reserved for it. Leaving the strip required rini's own assignment to
  AGREE with the last user space it had seen (`fullscreen_departure`), so a window
  whose bookkeeping disagreed — or one no user space had been observed for — stayed
  in the strip while macOS had it. And three of the five rules in
  `space_resolution` still answered with its old assignment, so the rest of the
  reactor thought it was somewhere it was not. A window macOS has taken is not a
  claim rini's bookkeeping gets to veto.
- **A maximized window never outgrows the space reserved for it.** The column
  reserves the whole viewport and the frame is held to what the window accepts
  (`constraints::clamp_to_constraints`), so a window with a maximum width sits at
  it inside the column. The frame used to be handed the whole tiling width while
  the reservation was clamped, and the next column was laid out on top of it. A
  minimum larger than the slot does not grow the window past it — overlap is
  worse than a window smaller than it asked for.
- **Maximizing a stacked window pulls it out of the stack.** It fills the tiling
  area, which would otherwise cover the siblings sharing its column while the
  tree still claimed they were abreast. It becomes its own column in the
  stack's place, the stack one column right (`split_out` toward `Left`), and a
  second press puts it back beside the neighbour it left (`full_width_origins`). Landing
  right of the stack, as it did, panned the strip one column for a stack flush
  left, 0 to 862.5 on Kai's halves, with the siblings leaving left and the next
  column right, and two for a stack on the right, 0 to 1725. In the stack's place
  a stack flush left does not scroll, everything after it moves right, and a
  right stack scrolls one column. Every route in maximizes the same way — the
  key, an app's title-bar zoom, a lone window's drag across the viewport, and
  restoring a remembered full width at startup. A neighbour is remembered rather than a row index, because
  while the window is maximized its old column can move along the strip or
  change size. If every window it could return to has closed, it stays the
  column it became rather than being put somewhere the user never had it.
  The move rides the ordinary layout pass, so it animates like a join or unjoin.
- **A maximized window is alone in its column.** A window that comes to share a
  full-width column ends that column's full width where it is, as a width given
  does (`keep_full_width_alone`, after a fold-in, a `toggle-stack` join, a full
  width's own toggle back and a swap). Folding a maximized window in is the same
  rule from the other end. Nothing stopped either before: on [A1,A2][B1] with A2
  maximized, the left fold key put A2 back into A1's column still maximized, A2
  at (3,3,1722,1074) over A1 at (3,3,1722,536), and folding B1 into a maximized
  A1 drew B1 1722 wide over A1's lower half.
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

  Gapless configs, single columns, dragged widths and degenerate widths are left alone.
- **A full-width column keeps its strip x.** Assigning the whole tiling rect,
  origin included, lifted the window out of the strip's coordinate space: it
  stopped scrolling, stayed pinned at the viewport's left edge, and other
  columns slid over it (Slack made full-size, then focus moved away).
- **A width given by key keeps the column's place.** Every column start after
  it moves, so with no rescroll a window at the right edge grew off screen and
  the key looked dead until a focus nudge forced a reveal. The neutral reveal
  that cured that kept the left edge on a shrink: a column at the right edge
  grew leftward and shrank from the right (Kai, 2026-10-11). Under `"never"`,
  `cycle_preset_column_width`, `resize_selection_by` and `toggle_full_width`
  keep a `Place` for the selection (`LayoutState::kept_place`): `Centred` under a centre override, or
  else the column's share of the viewport's free space on its left, which each
  pass records for its selection (`last_selected_share`, `strip::place_share`,
  revealed first, a viewport-wide column at a half, which the clamps hold to its
  end of the strip as the first or last column). It is read
  only when that pass had the window selected and no scroll request has come
  since, which is what makes it the place focus left the column at. The next
  pass scrolls the strip to stand the column at `strip::place_x` for its new
  width (`strip::place_offset`, by `Pin::Place`, applied where a kept right edge
  is), then clamps. The share counts the empty desktop past the end of a strip
  narrower than the viewport as free space, so `place_offset` holds the strip's
  end at the right edge: without that, the right one of two thirds took share
  0.5 and its half pushed the left third 141pt off screen with 431pt empty on
  the right. The kept share, not the
  pass's, steps the next width, so it survives a viewport-wide step and the
  clamp of a strip narrower than the viewport, and the old width lands at the
  old x. `unpin` drops it with the other pins; the raise's refocus after each
  key keeps it (`holds_scroll_for`), and without that a step from the whole
  viewport would take the pass's end-of-strip share. With no place, or under
  `"always"`, the width change reveals or aligns as before.
- **The way back from full width lands the column by the same place.** Both
  ways the toggle ran the one neutral reveal from wherever the strip was, so a
  column came back flush left whatever x it had left, and a centred column lost
  its centring to the reveal: Kai's thirds on 2026-10-09 had A at x=578, back
  at x=3 with 575pt empty on the right. A view of its own, the column's start
  against the scroll offset (`View`, pinned by `Pin::Start`), cured that for
  the toggle alone: a `Mod+R` or resize key on a full-width window left full
  width and ran the neutral reveal with no record of where the column had
  stood, and a column scrolled off screen came back off screen. Kai asked for
  the width keys and the toggle to remember the way back by one mechanism
  (2026-10-11). Under `"never"`, entering full width
  is a width change to the whole viewport about the column's `Place`, taken
  through `place_of` as a width key takes it, only when the last pass had the
  window selected, because a title-bar zoom can name a window that was not. For
  a window pulled out of a stack that is the stack's column, and the full-width
  column takes the stack's place, so the place carries over. The way back puts
  the column at `strip::place_x` for the width it returns to, `w0` when nothing
  else changed, so it lands at `x0`; a `Mod+R` or resize key in between ends
  full width in place and steps about the same place. The way back reads only a
  kept place (`kept_place_of`) and takes no fresh one, so with none, after any
  scroll (`LayoutState::unpin`), the startup restore, a drag across the
  viewport or a zoom of a window the last pass did not select, it reveals. The
  place is taken revealed, so a column scrolled off screen comes back on screen
  where the reveal shows it. Refocusing the window keeps the place
  (`holds_scroll_for`), as the app's own resize while full width does by
  returning before any request.

## Navigation

- **Floating windows are not strip members.** Left/right from a floating window
  goes to the strip and resumes at the strip's own selection; left/right at the
  strip's end stops there. Both branches used to walk the floating set: with
  Zoom and System Settings floating, Mod+J/L cycled those two instead of the
  columns, the escape landed on the *first* column, and walking off the last
  column stepped onto Settings and bounced back. Floating windows are still in
  the workspace and reachable with cmd-tab and `switch-focus-between-floating-and-tiling`. A
  floating branch that advanced with `(idx + 1) % len` was a closed cycle that
  never reached the fallback.
- **Resume at the strip's selection, not `first()`.** Taking the first column
  unconditionally is why leaving a floating window jumped to the leftmost
  column.
- **Only left/right report an edge hit** (`boundary::strip_edge`). Up/down is not
  a strip axis; a column's top is not an edge the view can bounce against, and
  bouncing the strip vertically for one would claim the workspace stack stopped.
- **Horizontal movement stops at the display's strip end**, never continuing onto
  the neighbour; vertical still crosses. This was the `isolate_displays` setting,
  defaulting to the hop, and is now the only behaviour: `strip_edge` answers both
  "is this an end" and "may this cross", because they are one question. Only focus
  consulted the setting, so a window pushed past the last column teleported to the
  other monitor while focus in the same direction stopped.
- **Moving reports an edge the way navigating does.** `MoveNode` with nowhere to
  go, and `MoveWindowToWorkspace` with no workspace that way, both set
  `edge_hit` so the view bounces. Neither did, and a move that changed nothing
  and said nothing was indistinguishable from a stuck key.
- **A move that swaps two columns says so.** `MoveNode` left or right that
  swaps the selection's column with its neighbour sets `moved`, the window and
  the way it went, so the flight can draw the two changing places; a move out
  of a stacked column makes a new column instead and sets nothing. See "The
  move flight, staged" in `src/transitions/docs/coordination.md`.

## Joining

`toggle-stack` moves the *selected* window into the *previous* column (niri's
consume-into-column), falling back to the next column only in the first
column. It used to pull the next column's windows into the current one, which
stacked a window the user had not chosen and left the selection untouched.

**`toggle-fold-left` and `toggle-fold-right` are the symmetric ones, and are what a
fold key should be bound to.** `toggle-stack` is not a round trip: folding in moves one
window, folding out explodes the whole column, so a second press on a column of three
does not give you a column of three. A fold moves only the selected window in both
directions, and each action takes a side, so one binding per side gives the same control
each way (`Mod+Comma` left, `Mod+Period` right in `rini.default.kdl`).

Pressing one key twice returns the strip to its shape, which takes two different
mechanisms:

- **Folding OUT lands on the side AWAY from the key's own.** A window folded into
  the column on its left came FROM that column's right, so that is where the left
  key puts it back. The other key unfolds too, to its own side — each key is the
  inverse of itself, not of the other one.
- **Folding IN prefers the row it was folded out of** (`fold_origins`) over
  appending to the end, so the rows keep their order as well as the columns. Only
  from the neighbour on the key's side, the column the fold-out left: anywhere
  else the window joins that neighbour at the bottom, as a window that was never
  folded does. Fold and full width shared one record before, so a window folded
  out of [A1,A2] and maximized went back into A1 on the second `Mod+F`, and one
  folded out of [A1,A2][B1][C1] and moved two columns right jumped back past C1
  and B1 into A1 on the left key. Each now keeps its own, and a fold-in spends
  only the fold's. A fold with no column on its side changes nothing, the record
  and the view kept included.

**The same key twice puts the view back too.** From a right stack `Mod+Comma`
lands the fold-out right of the stack and the reveal scrolls one column, 0 to
862.5 on Kai's halves. The fold back revealed from there, so A1 stayed at
x=-860 with half the viewport empty. `Mod+Period` lands it in the stack's
place and scrolls nothing, and the same rule holds for it. Under `"never"` the
fold-out keeps how the last pass showed the stack (`View` in a `KeptView` naming
the key's side, read before the fold's own reveal): centred, or its start
against the scroll offset from `last_selected_rel_px`, which the fold back pins
(`Pin::Start`) where the pass applies a kept right edge (see "Mouse resize"). A
start rather than a scroll offset, so the column keeps its x through a width
change elsewhere in the strip that scrolls nothing. Only the fold-in of that
window by the same key spends it. The fold keeps a view rather than full
width's `Place`: the fold-out's own reveal would drop a place, and a column
folded out and back off screen is put back off screen where it stood.
Refocusing the window keeps the view, as for full width; once a pass has spent
the pin, refocusing reveals as for any column, because the spent pin still named
the window and the click that should show it did nothing. The other key, or any
scroll in between, drops the view, and the fold back reveals as before. The other key folding into a column that is
not the row's drops it by that fold's own scroll; into the row, which a drag swap
can bring beside the window without scrolling, by the key the view names.

There is no falling back to the other side at the ends of the strip. With both
keys bound, that would make them agree in the first and last columns, which is
the surprise this replaced.

**One window leaves a stack one way.** `split_out` is that way. Unfolding,
expelling, unstacking and unjoining were four copies of the same removal, each
with its own idea of where the new column goes and whether the height weight
travelled with it — 164 lines that are now 35 plus its callers. The column left
behind re-equalises there, in one place, rather than in each caller that
remembered to.

**Two commands cover folding, not five.** `join_window`,
`consume_or_expel_window` and `unjoin_windows` are deleted. They were rift's
tree-era vocabulary for moving a window between containers, and against a single
scrolling layout each was a partial view of the fold: join was its fold-in
half, unjoin its fold-out half, and consume-or-expel almost the whole thing but
with the fold-out landing on the side asked for instead of the side the window
came from, so it never round-tripped. What is left is the fold per side,
`toggle-fold-left` and `toggle-fold-right`, and `toggle-stack`, which keeps its own
meaning: it explodes a whole column rather than moving one window. That history is why
rini does not take niri's `consume-or-expel-window-left`: it is the one that never
round-tripped.

**Folded windows divide the height evenly unless someone has resized them.**
`height_weights` means two different things and they are read differently: equal
ratios in a column nobody has touched, and desired pixel heights after a
deliberate vertical resize (`Column::height_overridden` says which). Reading the
ratios as pixel heights collapsed a folded window to its title bar. The solver
subtracts each window's reported minimum to turn pixel weights into "growth
above the minimum", and against a weight of 1.0 that subtraction floors at 0.001
for any window macOS reported a minimum height for, and leaves 1.0 for any it did
not — so a folded pair split 700/100 instead of 400/400. It only misbehaved when
the two windows disagreed about having a minimum, which is why it came and went.

## Column width and what the window will accept

A column reserves `max(configured width, its windows' minimum width)`, so a
window whose minimum is wider than the configured column gets the width it needs
and its neighbour starts after it.

The limits arrive late. A minimum comes from the WINDOW SERVER, and Accessibility
can report a new window before the server has it, so the first layout runs with
no limits at all and the window is placed at the default width — where macOS
refuses to shrink it and it is drawn clipped. Any later pass fixed it, which is
why left/right navigation appeared to. Learning a limit now asks for that pass
itself: `on_windows_on_screen_updated` reports `changed` when a window's
constraints differ from what was recorded, and `constrains_layout` keeps the
common case of a window with no limits from costing a pass.

## A border takes its width out of the window

niri's border is drawn inside a tiled window's place in the strip, its slot, the way niri draws it
inside the tile: every column width, gap, limit and scroll offset is a slot's, and the window is
the slot inset by the border on every side (`tile::window_in`). The application hands the layout
each tiled window's width as a plain number before every pass, one for every window but those a
window rule gives another (`tile::Borders`, `LayoutEngine::set_borders`), so `layout` names no
decoration. Each window of a stack is inset by its own, inside slots laid out as before.

- **Frames leave once, as windows.** `calculate_layout` lays every slot out as before, then insets
  each and holds the window to its own limits inside it (`clamp_to_constraints`), so a window held
  to its maximum sits at its slot's top-left, inside the border. Every caller gets window frames,
  and none converts.
- **Limits are the slot's.** `column_limits` and each stacked row count the border on both sides of
  each limit a window reports (`tile::slot_limits`); counted without it, a window's minimum would
  leave the slot 2·border short, and the window would be clamped below its own minimum.
- **Reports come back once, as slots.** `on_window_resized` reads the report and the drag's start
  as slots on entry (`tile::slot_of`), so the dragged side, the dragged width, the ratio
  (`constraints::ratio_of_column_width`, see "Mouse resize"), the heights
  a stacked edge trades and the right edge a left-edge drag keeps are all a slot's, and a dragged
  column keeps its slot width whole: the window lands where it was let go. The reactor's
  `DragSession::start_frame` stays the window's frame.
- **What compares windows with windows needs nothing**: an echo of rini's own write
  (`same_as(target)` in the reactor), the title-bar zoom, which ends at the inset full-width frame
  rini writes, `take_strip_layout` and `strip_move::start_where_standing`, the persisted
  fingerprints, and drag-swap overlap.
- A floating window keeps its frame, and the border is drawn outside it.

The width is the band's as it is drawn on the window's display, in whole device pixels, so the band
fills the slot at any width the config writes ("The focus ring and the border, as drawn" in
`src/decorations/docs/README.md`).

## Mouse resize

macOS reports a mouse resize as frames and nothing else. The reactor keeps the frame the window had
before the drag's first report (`DragSession::start_frame`) and hands it to the layout with every
report of the drag, as an `edge_drag::Drag`; an app resizing itself sends `None`. The layout reads
both as the window's slot, the border around it ("A border takes its width out of the window").
`edge_drag::dragged_side` reads the edge off the two frames: the left one when `minX` moved and
`maxX` stayed within 0.5pt. `niri:` paths are at
https://github.com/YaLTeR/niri/blob/ed22699d99462f61ab171472d3ea67e844ea580d/.

- **A left-edge drag keeps the right edge.** The layout used to read only the width and place the
  column by its start, so the growth always landed on the right. `on_window_resized` now pins the
  right edge at the start frame's `maxX`, and the next `calculate_layout` sets the scroll offset that
  puts it there (`strip::offset_keeping_right_edge`), before the reveal and the offset clamp run.
  The pin is applied there rather than when the report arrives because only the layout pass has the
  column's real width, after the bounds and the windows' own limits.
- **The reveal after a kept right edge checks only the left edge** (`Reveal::KeepingRightEdge`).
  A bounded column's x and width are rounded each on its own, so a column flush with the viewport's
  right edge can stand a point past it, and the pin keeps that point. Checked as any reveal, the
  right edge was pulled back by it and the released window slid 1pt left. The point stands until the
  next neutral reveal of that column, an app's own resize of it that changes its width for one,
  which pulls the strip 1pt left then; an app answering rini's write is no reveal ("An app answering
  rini's write changes nothing").
- **The pin is spent by one pass and belongs to one window.** Every scroll clears it, a window
  closing included (`LayoutState::unpin`), and the pass applies it only while the window it names is
  still selected, so a selection moved without a scroll never drags the next column to the old right
  edge. Under `center-focused-column "always"` no pin is set, because the
  centring places the column.
- **The release focuses the resized window, and that keeps the pin.** The mouse-up sends a focus
  event ahead of its passes. After a resize it names the session's window, not the hit test's: the
  cursor lets go on the edge, where the column behind can answer. `select_window` on the window
  already selected issues no scroll request while a pin no pass has spent names it, as it keeps a
  centre override, the view a fold back returns to and the place a width key or the full-width
  toggle keeps; the request would have cleared the pin
  before the only pass that reads it.
- **A centred column takes twice the travel.** Centring moves both edges by half of any change, so
  the column takes the start width plus twice the dragged edge's travel (`edge_drag::dragged_width`)
  and the dragged edge ends under the cursor. niri doubles the delta the same way while it centres
  (`niri:src/layout/scrolling.rs:3621-3632`), but it centres every column under `"always"`
  (`is_centering_focused_column`, `niri:src/layout/scrolling.rs:575-578`). rini's first and last sit
  flush, so `strip::centres`, the rule `anchor_x` places by, leaves them the dragged width. The
  report makes its scroll request first and asks `centres` with the centre override that request
  left, the one the pass then centres by: under `"never"` the request clears it, so a centred window
  there is dragged as any other.
- **A stacked edge trades with the window across it.** `edge_drag::dragged_end` reads the top or
  bottom edge as `dragged_side` reads the left or right, and `edge_drag::trade_height` gives the
  change to that one row. The trade stops where that row reaches 1pt, so the column's total holds and
  no other row moves. It starts from the model's heights, not the start frame's: every report of a
  drag carries the same start frame, so a change measured from it would be taken again from a
  neighbour the last report already shrank. A report back where the drag started moves neither edge
  against that frame, so the drag session keeps the edge its reports moved last
  (`DragSession::dragged_end`, carried as `Drag::end`) and the layout trades back across it; without
  that the column kept the heights of the report before. The session ends at the release, so the
  next drag starts with no edge, even from the same frame. The model's heights are the
  weights in points over the column's height, or equal shares when `height_overridden` is unset,
  because the layout reads such a column as equal shares whatever its weights. A drag that never
  moves the top or bottom edge leaves the heights and `height_overridden` alone. It used to mark the
  column as deliberately split, so a window folded back into it kept the 1.0 weight its own column
  held and ended 1pt high. An app's own height, with no drag, still comes out of the other rows in
  proportion, unless it answers the row's height ("An app answering rini's write changes nothing").
- **Not taken from niri: the whole column sharing a stacked edge.** niri refuses only the topmost
  window's top edge (`niri:src/layout/scrolling.rs:3635-3650`). Every other top or bottom edge goes
  through `set_window_height` (`niri:src/layout/scrolling.rs:5045-5124`), which fixes the dragged
  tile's height and turns every other tile automatic, so they share what is left. niri's own FIXME
  asks for smarter height distribution there (`niri:src/layout/scrolling.rs:3644-3645`). rini trades
  with the one window across the edge, and the bottom-most window's bottom changes nothing too.
- **The first column ends grown to the right.** Its left edge cannot stay off the viewport, so the
  reveal after the kept right edge pulls it back. niri ends there too: it does not reveal during a
  resize (`niri:src/layout/scrolling.rs:1417-1419`), and at the end `interactive_resize_end`
  (`niri:src/layout/scrolling.rs:3655-3671`) aligns a column that is not fully visible to the nearer
  side (`compute_new_view_offset`, `niri:src/layout/scrolling.rs:5588-5620`). niri reads a left edge
  the same way, flipping the sign of the horizontal delta (`interactive_resize_update`,
  `niri:src/layout/scrolling.rs:3594-3653`).
- **Not taken from niri: space before the first column.** niri keeps the right edge on a left-edge
  shrink of a strip at its start, which leaves empty space before the first column until the view
  next moves. rini's offset clamp never shows that space, so the shrink keeps the left edge instead,
  and a strip scrolled by less than the shrink keeps the right edge only until the offset reaches 0.
- **A drag passes the width bounds.** `min-column-width` and `max-column-width` keep the resize keys
  from crawling to 100%, and they snapped a drag back: presses 15 and 16 reached 1413 and 1443pt and
  landed at 1148. A drag that moves the left or right edge marks its column `width_unbounded`, and
  `column_ratio` holds a `ColumnKind::Unbounded` column only to the viewport and a twentieth of it;
  the windows' own limits still apply in `column_width`. niri has neither key: its interactive
  resize is held only by the window and the working area. A centred column's width is doubled, then
  held the same way. A stacked edge's drag leaves the mark as it was. The app's own resize keeps it,
  because an app reports its own size after rini's write, as a terminal snapping to its cell grid
  does, and a clamp there would undo the drag. The mark is saved with the column, under the file
  key `width_dragged`, which older rinis read too, and carried with the width
  (`ColumnWidth::Unbounded`, which the file holds as an `Offset` with the mark beside it, as
  `src/workspaces/docs/launch-memory.md` says); a key or a preset below 1.0 clears it, and the
  preset 1.0 sets it.
- **An app answering rini's write changes nothing.** The reactor drops a report within 0.1pt of
  rini's write before the layout sees it (`same_as`). A report off on one axis, a terminal snapping
  its height to its cell grid for one, still carried the other, and both were taken: the width was
  read as `width / viewport`, and `column_width` took the gap share out of that again, so Kai's
  860pt half came back as 858.5, drawn at 859, a point narrower each time; the row's height became
  pixel weights with `height_overridden` set; and the report's scroll request dropped the centre
  override, the view a fold back returns to and the place a width key or the full-width toggle
  keeps. Now, with no drag, an axis within 1pt of the slot
  the last pass gave the window changes nothing (a lone column's height is no row's), and a report
  that changes neither returns before its scroll request. The pass records that slot for its
  selection as it wrote it, after the windows' own limits (`last_selected_width_px`,
  `last_selected_height_px`, measured for `last_selected_rel_of`); a slot worked out again from the
  column's ratio missed those limits, so a window held to a 900pt minimum in an 860pt half answered
  900 and was taken as a width, which dropped a centring and a fold's way back each time. A report
  from a window the last pass did not select is taken. A width taken is read through
  `constraints::ratio_of_column_width`, the inverse of `column_width`, so it is laid out as
  reported: 982pt lands at 982, not 981. A drag is the user's, so it takes even a 1pt change. Not built: research suggested also ignoring
  a report smaller than the slot by less than the window's own size step. rini reads no size step,
  and it would undo a terminal settling 3pt under a dragged width narrower than the viewport, which
  is taken today.
- **Full width and a drag.** A drag on a full-width column's left or right edge ends full width at
  the dragged width, through `leave_full_width_in_place`; anything else under full width, the app
  settling or a top-edge drag, leaves it. A drag across the whole viewport makes a window alone in
  its column full width, with the drag's start width to go back to, so one width has one record. A
  stacked window stays a column at 1.0, because the mode would pull it out of its stack.
- **A dragged width is whole.** `column_width` takes no gap share from a `ColumnKind::Unbounded`
  column. It took one, so a drag to a width two of which fit abreast landed 1.5pt narrower, rounded
  to 1-2pt, and with the right edge kept the left edge moved by that much at the release. In Kai's
  38 Ghostty releases journalled on 2026-10-07, 18 of the 19 drags to a width with a share that
  were pictured twice ended 1-3pt from where they were let go, and 4 of the 5 without one did not.

Measured 2026-10-06, before this rule: 20 presses on Kai's display (tiling area x=3, w 1722, `gaps 3`)
with Ghostty, polled every 1ms from the window server alongside rini's Accessibility reports.

| presses | what | result |
|---|---|---|
| 1-7, 17 | right edge | the pressed window took the resize; no neighbour resized |
| 9-13 | left edge, inside both bounds | x back at 600 at release, width kept within 2pt: 9, 10 and 13 grew to the right, 11 and 12 shrank from it |
| 8 | left edge, [600, w 595] to [708, w 487] | landed at [600, w 572], `min-column-width` less the 2pt gap share |
| 14 | left edge, [600, w 882] to [334, w 1148] | landed at [577, w 1148]: placed by its start at 600, then the reveal pulled it flush with the screen edge at 1725 |
| 15, 16 | left edge from [577, w 1148], to w 1413 and 1443 | both back at [577, w 1148], `max-column-width` (0.66667 x 1722) |
| 18-20 | title-bar moves | the swap traded the two windows' widths |

In all 17 edge presses no neighbour resized across the 3pt gap, so macOS did not hand the press to
the window beside it. On the left edge, Accessibility reported 56-98% of the frames the poll saw, p50
8.9-13.5ms ahead of the window server.

## Parking

Off-strip windows park at 1pt corners; live parks were measured at y=1085
showing a 32pt band along the bottom. Both must read as off screen, or a parked
window animated back in travels from the bottom corner instead of entering from
the strip's edge (`src/workspaces/domain/hidden_window_placement.rs`).
