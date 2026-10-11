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
- **A border takes its width out of the window, not out of the strip.** With `layout › border` on,
  every width, gap and limit above is a window's place in the strip, the window and its border: the
  window is laid out inside it by the border's width on every side, and held there to its own limits,
  which its place counts the border in. So the strip is laid out the same with borders on or off, and
  a border covers nothing beside its window (`decorations.md`). Every frame macOS reports back, a
  drag's or an app's own, MUST be read as the window's place, the border around it, so the column
  keeps the width it reserves.

> **Found 2026-09-30, while acting on a report.** "can you check the gap between windos is 50/50
> configurations? i think it's smaller than other gaps". It was: 2pt between windows against 4pt at
> the edges. Raising it to 4 in the config left a full-width column two columns along the strip at
> left 6 / right 2, because the scroll position is kept in points and each column's start includes
> the gaps before it.
- `Mod+R` steps through the configured preset widths in order and wraps: a third, a half, two thirds,
  the whole viewport, then a third again. A column at a width that is no preset — what the incremental resize keys
  leave behind — MUST join the cycle at the first preset above it rather than at the start.
- **A width given by key MUST grow and shrink the column about where it stood**, `Mod+R` and the
  resize keys alike, under `center-focused-column "never"`. Where it stood is its share of the
  viewport's free space on its left: a column flush with the right edge grows and shrinks from its
  left edge, one flush with the left edge from its right edge, the middle of three thirds equally on
  both sides, and any other in proportion to the free space on each side. A stacked column moves as
  one, its rows' heights unchanged. Stepping back to the width it started at MUST put it back exactly
  where it stood, and the whole viewport fills the screen. The columns beside it are pushed out or
  pulled in on their own sides; where the strip is narrower than the viewport the column stays where
  the strip's start leaves it. A column part off screen counts from where the reveal shows it; one
  that fills the viewport takes its end of the strip, flush left as the first or only column, flush
  right as the last, centred between. A centred column MUST stay centred. Where it stood is taken
  when a width key or `Mod+F` first finds the column focused, and MUST be forgotten when focus moves
  to another column or anything else scrolls the strip: a strip scroll or snap, a window opening or
  closing, a move, centring, a fold, a mouse drag, or the app sizing its window to a width other than
  the one rini wrote. `Mod+F` keeps it both ways (see "Full width and height"). So nothing sticks between visits: the same column reached from the
  other side grows from the other side. Under `"always"` the centring places the column at every
  width.

> **Reported 2026-10-11.** "all looks good to me. one small bug. lets sai i have 2/3 + 1/3 windows on
> the screen the big one on the left and small one on the right. when i resize the small one one the
> right as 33 -> 50 -> 66 -> 100 the window on the left gets pushed out as you i'd expect, but when it
> start circling further back 100 -> 66 -> 50 ... it doesn't shrink from the left side it shrinks
> from the right instead pulling in the window on the right, which is confusing it expanded to the
> left but shrinking from the right. if the window was on the left side the current behaviour is
> correct it grows to ther night and shrinks from the right. but if it was originally on the right
> side it is correctly grows to the left but then shrinks from the right and it needs fixing. a bonus
> point. if a window is originaly in the middle like say 3 1/3 windows it would be great it it could
> grow and shrink from to the middle. extra bonus is to handle all sorts of awkward positions of a
> window in the middle, like say there are more than 3 columns or columens are unequal width, it
> would be neat if the growth would be propotional on the sides and shrink back to the column's
> original position. obviously the original position should be capture at focus time and then
> forgotten as i move to other windows. the same small window could land either on the right or the
> left side depending on which way i'm navigationg so the o/g position should not get stuck. also
> when i say window in this case i think i mean column in general, betcause it's exactly the same
> issue with multiple windows stacked up in the same column. either way needs fixing". A width
> change scrolled the strip only as far as it had to to show the column, so a column at the right
> edge was scrolled back in as it grew and kept its left edge as it shrank. Kai's two thirds and a
> third is 1722.99pt, a point past the viewport, and the focus reveal leaves the third flush right.
- The configured widths (`layout › preset-column-widths`) MAY be any proportions in `(0, 1]`; anything
  else MUST be refused when the config is read, naming the line. A proportion of 1.0 is the whole
  viewport, gap included: a width `max-column-width` does not hold, and not the maximise mode.

## Full width and height

- `Mod+F` toggles a window to the full usable space and back. The window STAYS on the strip with
  everything else, and there is no macOS fullscreen state: full width is rini's one maximise mode,
  and the only state that keeps a way back.
- A full-width column occupies the whole viewport, gap included. It MUST NOT leave a band of
  background down its side.
- When the window is in a column stack, `Mod+F` MUST pull it out of the stack and maximise both
  width and height. The full-width column MUST take the stack's place, the stack one column right,
  so a stack at the left of the screen does not scroll the strip. Pressing again MUST put it back
  where it was, if that place still exists, and the move MUST be animated both ways.

> **Reported 2026-10-09.** "going full-size and back moves surrounding windows out incosistently".
> Part of it was where the full-width column went: right of the stack, so the siblings it left and
> the column after it moved off in opposite directions, and a stack at the left of the screen
> scrolled the strip. Measured in "Maximizing a stacked window pulls it out of the stack"
> (`src/layout/docs/strip.md`).
- **Pressing again MUST put the strip back too**, for a stacked window or a lone one, by the same
  place a width given by key keeps ("Columns and widths"): going full width is a width change to the
  whole viewport about where the column stood, and pressing again MUST land the column where it
  stood, at the width it returns to, and a centred column centred again. `Mod+R` or a resize key on
  a full-width window MUST step from the whole viewport about that same place, so stepping back to
  the width it had puts it where it stood. A column part or wholly off screen when maximised comes
  back where the reveal shows it. Anything else that scrolls the strip in between wins: focus moving
  to another column, a strip scroll or snap, a window opening or closing, a move, or centring. The
  strip then scrolls only as far as it must to show the column. So does it for a window that reached
  full width another way (a restart, a drag across the screen) or was zoomed from its title bar
  without having focus.

> **Reported 2026-10-11.** "i think alt-f is the same mechanics actually and should behave the same
> way, it just have extra animations and height conversion, but as far as remembering where to go
> back both should be identical and work as i just described; ideally run on the same code". The way
> back kept a record of its own, the column's start against the scroll offset, beside the place the
> width keys keep, and the two disagreed: `Mod+R` on a full-width window stepped from where a
> viewport-wide column stands at its end of the strip, not from where the column had stood, and a
> column scrolled off screen came back off screen. Both now run on the one place.
- **A full-width window MUST be alone in its column.** A fold key on it MUST end full width where it
  is and fold it as it folds any lone window; with no column on that side it MUST change nothing. A
  window that comes to share a full-width column, by a fold, `toggle-stack` or a drag swap, MUST end
  that column's full width where it is. Either way the window is drawn the column's width, never the
  viewport's over the windows below or above it.
- A full-width window MUST come back full-width after a restart.
- **`Mod+R` only ever changes the column's width, and MUST NOT enter full width.** Its 100% step MUST
  make the whole column, every window in its stack, the whole viewport wide, gap included and with no
  band, keeping the stack, its heights and its place on the strip — even though 100% is above
  `max-column-width`, which deliberately stops the incremental resize keys, and not a mouse drag,
  from crawling to 100%. No window is pulled out of its stack and nothing is kept to return to: a
  later `Mod+F` maximises afresh, and its toggle back returns to the 100% column. The app resizing
  its window, however far short of the viewport, MUST leave the column 100%, as it leaves a
  full-width one, and the next `Mod+R` MUST wrap to the narrowest preset. A 100% column MUST come
  back a 100% width after a restart, not full width.
- **Only `Mod+F`, the title-bar zoom, a lone window's drag across the viewport and a restart
  restoring one enter full width**, the mode that remembers the width, height and place to go back
  to. `Mod+R` MUST size a full-width window like any other, from 100%.
- **Being given a width is what ends full width.** A window sized by `Mod+R` or by a resize key MUST
  stay where it is and MUST NOT be returned to the stack `Mod+F` pulled it out of. It becomes an
  ordinary column with nowhere to return to, and the next `Mod+F` MAY maximise it afresh.
- **A resize key MUST NOT move a width against its own direction.** A grow at or above
  `max-column-width`, and a shrink at or below `min-column-width`, MUST do nothing: the bound would
  pull the width back across itself. So growing a full-width column does nothing, where a "grow"
  landing on `max-column-width` would make the window smaller. A shrink from above the bound, from
  full width, a dragged width or `Mod+R`'s 100%, MUST start from the width the column HAS, not the
  width it had before being maximised, and from full width MUST end it.
- **A window the user zooms from its title bar MUST become the full-width column**, the same one
  `Mod+F` makes, whatever size the app zooms it to: the screen, or its content as Safari and Finder
  do. It MUST end exactly at rini's full-width frame, inside the viewport by the border's width with
  a border on, and stay there when it is clicked again. Zooming
  it again from its title bar MUST restore the width and the stack it had, whatever size the app
  reports on the way back. The two are one toggle, so `Mod+F` MUST undo a zoom and a zoom MUST undo
  `Mod+F`.
- One double-click MUST toggle once. The frames the app reports after its zoom MUST NOT become the
  column's width, and MUST NOT keep rini's frame off the window.
- Only a double-click on that window's own title bar makes its resize a zoom, and only for 1 second
  after the click. A double-click in a window's content or on another window, a drag that starts after
  the double-click, native fullscreen, and a floating window MUST each be handled exactly as with no
  double-click at all. `Mod+F` or `Mod+R` pressed after a double-click MUST keep the width it gave.

> **Reported 2026-10-10.** "when i alt-r a stacked colum to change it's width, when it reaches 100 the
> currently active window goes full size mode and breaks the colum. the alt-r supposed to change width
> only it should not go into full-size mode, it's the same for full height windows, it should
> eventually take 100% of the screen width but not really enter the full-size mode. it's basically
> about moving the column windth where alt-f toggles full size which takes full height/width but it
> also remembers the window's previous configuration (width, height, column pracement, etc) and
> reverts." The cycle's 100% step entered the maximise mode, which pulled the window out of its
> stack. It is now a width held only to the viewport, the way a drag across it already sized a
> stacked column, and only the full-width mode, entered by `Mod+F` or its title-bar zoom, a lone
> window's drag across the viewport or a restart, remembers where to go back to.

> **Reported 2026-10-09.** "restoring windows back from full-size doesn't respect previous position
> of a column, for example two columns on the screen, im expending a window from a column on the
> left, but when i move back from a full size mode the column shift on the left", and "some of the
> issues seem intermittent". Both ways, the toggle only scrolled as far as it had to to show the
> column, from wherever full width had left the strip, so a column came back flush left whatever x
> it had left, and a centred column lost its centring. It looked right only for a column that had
> been flush left already, which is why it came and went.

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
> gained 1.0, and a step off a full-width window now sizes it like any column. Kai chose niri's
> direction after considering a downward cycle: ascending, with 100% wrapping to 1/3.

> **Found 2026-09-24, not reported.** The same blind spot in `scroll-strip`, which computed its own
> column widths: it stepped the strip by `max-column-width` of the viewport for a column
> occupying all of it, so one step left the next column part-way on screen.

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
  The window ends where the cursor let go, to the point, with a border on as without: the drag is
  read as the window's place, and the place keeps its dragged width whole.
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
  workspace or display, and across a restart, until a resize key or a `Mod+R` step below 100% gives
  the column a width the bounds hold again. `Mod+R`'s 100% is held only to the viewport too. A drag
  that moves neither the left nor the right edge leaves the column as bounded as it was.
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
- A window that resizes itself MUST keep its column where it was: the app's new width is taken, laid
  out at the width the app reported, and its origin is not. Its new height is taken from the other
  windows in its column, in their proportions. A column rini holds to the viewport, full width or
  the whole viewport wide by `Mod+R`'s 100% or a drag across it, MUST stay the viewport wide and
  take no width from its app, however far short of the viewport the app resizes it. A window in it
  whose app can go no wider, a maximum the window server reports included, MUST sit at that width
  inside the column, the gap beside it showing the limit, and the next column MUST NOT move in. A
  report within 1pt of the place rini gave the window on an axis is the app answering rini's own
  write: it MUST change nothing on that axis, and a report that changes neither axis MUST NOT scroll
  the strip, so a centred column stays centred and a fold or full width still comes back to the view
  it left.

> **Reported 2026-10-11**, asked whether a column rini holds to the viewport should take a width its
> app sets itself: "if an app can't go wider ignore it will make it visible, otherwise it'll feel
> like resizing bug". Before it, a maximum the window server reports narrowed such a column to it,
> 1722 to 1408 for an app capped at 1400 under a 4pt border, the next column moving in beside it,
> and an app under that narrowed column resizing itself was taken as the column's width. The
> resize then reads as not taking.

> **Found 2026-10-09, not reported.** An app answering rini's write 1pt off on one axis had the other
> axis read again with it. A half column's 860pt was read back as 860/1722 of the viewport and laid
> out at 858.5, drawn at 859, a point narrower with every such answer. A stack's even split became
> fixed heights, and the answer's scroll dropped a centred column's centring and the view a fold
> back returns to. Ghostty snapping to its cell grid is the likely sender; not measured.

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

> **Reported 2026-10-07**, on a Ghostty window: "wher i resize a ghosty window by the left size it
> still does a small 1-2px wobble during the animation when i release". A dragged column gave up its
> share of the gap, 1.5pt at a width two of which fit abreast, so the window landed 1-3pt from where
> it was let go and its tile slid there. Of the 38 Ghostty releases journalled, 18 of the 19 drags to
> a width with a share that were pictured twice moved 1-3pt, and 4 of the 5 without one did not. A
> column flush with the right edge also stood a rounded point past it, and the release's reveal
> pulled that point back.

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
- **A fold out and back in with the same key MUST put the strip back too**: the column at the screen
  x it left, and a centred column centred again. Anything else that scrolls the strip in between
  wins, as for full width, and so does folding back with the other key. The strip then scrolls only
  as far as it must to show the column. Once back, focusing the window again MUST show it, even
  where the strip put it back off screen.
- **A fold-in MUST return a window to the row it was folded out of only from the column it left**,
  the neighbour on the key's side. Folded into any other column, it MUST join it at the bottom, as a
  window that was never folded out does. A fold that has no column on its side MUST change nothing,
  so the fold back by the key that folded the window out still finds its row and its strip.
- **A full-width toggle MUST NOT send a folded-out window back into the stack it was folded out
  of.** Full width puts back only what full width took.
- A folded window MUST NOT be squashed to its title bar. A column's stored heights are either
  deliberate pixel heights from a vertical resize or the equal ratios a freshly folded column starts
  at, and the two MUST NOT be conflated.

> **Found 2026-10-09, not reported.** Folding and full width shared one record of where a window
> came from, and neither checked that a full-width window was alone. A window folded out of
> [A1,A2] and maximized went back into A1 on the second `Mod+F`. One folded out of [A1,A2][B1][C1]
> and moved two columns right jumped past C1 and B1 back into A1 on `Mod+Comma`. On [A1,A2][B1]
> with A2 maximized, `Mod+Comma` put A2 back into A1's column still full width, drawn over A1.

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
