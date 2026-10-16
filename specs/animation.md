# What moves

Animation is cosmetic and MUST NOT be load-bearing. A build with animation off behaves identically;
only the transitions are missing.

## What animates

- A window that changes position or size in a layout pass. **A column resized where it stands MUST
  animate even when nothing else on screen moves**: a lone column, or the last one at the strip's left
  edge with the columns before it parked, sized by `switch-preset-column-width`, a resize key or the
  full-width toggle.
- A window opening, from the frame macOS first showed it at to its slot.
- The strip scrolling, and a workspace switch.
- A window moved along the strip, changing places with the column it passes.

## What does not

- A window that has not moved MUST NOT be re-placed. The round trip invites another layout pass.
- A window written back to the slot it already had MUST NOT on its own make a pass fly. An app that
  answered rini's write by snapping a few points off its slot, to its own grid, is given the slot again
  by the next pass, and that resizes nothing the layout changed.
- A window whose whole movement is off screen MUST NOT be drawn. A layer and a capture for something
  nobody can see is pure cost.
- A column parked off the strip MUST NOT be raised. Nothing of it is visible, each raise is an
  Accessibility round-trip, and a raise issued after the on-screen windows puts a 1pt sliver in front
  of them.

> **Reported 2026-10-09.** "alt-R is broken now, doesn't animate anything". The focused Visual Studio
> Code window stood full width at the strip's left edge, the columns before it parked off screen and
> nothing beside it on screen to move. Each `alt-R` changed its width at once and flew nothing: a pass
> was judged worth flying only by how far a window's origin travelled, and this one's stood still.

## Pushing past an end

- A command that runs into an end MUST bounce the view: the ends of a strip, and the ends of the
  workspace stack when wrapping is off. A stop with no cue is indistinguishable from a dropped keypress.
- **Moving a window MUST bounce exactly as navigating does**, at both kinds of end.
- The bounce MUST be large enough to notice at a glance and small enough that nothing appears to change
  places. 72pt of a 1720pt viewport.
- Real windows MUST NOT move. The bounce is the drawn surface giving, and the window frames are
  untouched throughout.
- A request that simply cannot be honoured — a workspace named by an index that does not exist — is NOT
  an end and MUST NOT bounce. Bouncing would claim the stack has an edge in a direction nobody named.
- **Every push against an end MUST show.** One arriving while the bounce plays MUST push the view out
  again from wherever it is drawn: it MUST NOT be dropped, and MUST NOT start the bounce over from
  rest. A burst at the wall holds the view there, and the view is home within one bounce of the last
  press, never later. A push arriving before the motion begins, up to 300ms behind a window opening,
  450ms for one ending part off the display, MUST wait for it, and the pushes of that wait show once,
  however many arrive: a CSS animation restarted twice before its first frame shows once.

> **Reported 2026-10-03.** "the only thing that still mildly bugs me is the end of strip/stack
> animation, it throttles rapid key presses and it feels like a bug. if you can make it react to
> every single press like a pure CSS based bouncer would, that'd be fantastic." A push while a
> bounce played was dropped, so the second and third press of a burst showed nothing.

> **Reported 2026-09-29.** "espcecially visible when you hit the end of the strip and it just bounces
> off the wall several times". Each press at the wall restarted the bounce from rest, and presses the
> reactor handled late went on bouncing after the pressing stopped. Both stay excluded above.

> **Reported 2026-09-24.** "The little bounce animation when navigation reaches an end of strip or
> workspaces stack is neat but a bit too small, I need a little more swing. Also when I'm moving a window
> in a strip or between workspaces it needs similar animations, because otherwise there is no visual cue
> and it feels like a bug/stuck." The overshoot was 36pt; moving reported no edge at all, so it never
> reached the bounce.

## Moving a window along the strip

- A window moved along the strip MUST be seen to change places with the column it passes, whether or
  not the strip scrolls. Both start from the places they held. A moved column narrower than the
  viewport and the column it passes cross, and the rest of the strip moves only as the view does.
  Drawing the finished layout and scrolling the view to it shows the swap only where nothing
  scrolls.
- Every window MUST start from where it was on screen. The view moves wherever the strip moves on
  screen, not only when it scrolls: a centred column that loses its centring as it moves shifts the
  whole strip with the scroll offset unchanged.
- Only a moved column that fills the viewport MUST step: one as wide as the tiling area, the
  display's usable width less both outer gaps, within a point, whether the full-width toggle or a
  width preset made it so. The view follows such a column, so nothing else on screen shows it moved.
  A narrower column, half or two-thirds of the display, is seen moving and MUST NOT step.
- The step MUST go away from where the window goes, a third of the display and back: a move right
  steps left and opens the right side, where the column it passes was.
- The whole strip MUST step: every tiled window MUST step; floating windows MUST stay where they
  are. Not yet met: a window that opens or resizes mid-step leaves the step where it is drawn and
  does not carry the rest of it.
- The column passed MUST be seen going beneath the moved window from the side the step opens, and
  that side MUST NOT show the desktop, except past the strip's end: the column passed fills it, then
  the strip beyond it. The column passed is drawn in its place on the strip, riding the strip
  beneath the moved window, which covers it at the end.
- A move arriving while a flight still gathers its passes MUST step once that flight moves.
- A flight MUST run longer to carry the step only when it steps.
- The step MUST leave and come back at rest. On the bounce's shape a third of a 1720pt display,
  573pt, leaves at 29,235pt/s over the default 0.35s: the kind of jolt reported under "Arriving
  mid-flight".
- The moved window MUST be drawn over the column it passes.
- A burst of moves MUST read as one movement with one step: a move arriving mid-flight carries the
  flight on, and a press while the step plays MUST NOT start another.
- The step and the bounce at an end MUST NOT hold each other off.
- The step is drawn only. Real windows go to the layout's frames, the column passed included.

> **Reported 2026-09-30.** "what i have two 50/50 windows at the beginning of a strip, when i move one
> of them in place of the other there is a nice transition animation where two windows are moving to
> take each other's places. everywhere else though the window just jumps to new place and then there
> is a focus navigation animation on top of it, the transition is instant and confusing, it is
> especially problematic for full sized windows where the movement is generally invisible and it
> doesn't feel like anything is actually happening, which feels broken." And: "maby moving full-size
> windows 1/3 to either side temporarily to show the swap animation and then bounce it back in focus
> would be an option too". Every move that scrolled the strip was drawn as the finished layout panning
> into view, so the two windows had changed places by the first frame. The 50/50 pair at the start of
> a strip was the one case that did not scroll.

> **Reported 2026-09-30.** "two fixes for the moving animation. 1. only full-size windows should
> cause the wobble (ecause you can't see otherwise it's movement) currently smaller windows do that
> too. 2. the wobble should go the opposite direction from current implementation". Any move whose
> own travel on screen was under a quarter of the display stepped, so half-width moves the view
> followed stepped too, and the moved window stepped alone toward where it went. The column it passed
> crossed the display in about 60ms, so the third the step opened showed the desktop.

## The vertical workspace switch

- The workspaces MUST scroll as one column, each sitting directly on the one above it. There MUST NOT
  be a band of empty desktop the size of the bar between one workspace and the next: the only space
  between two workspaces' windows is their own outer margins, the same gap two tiled windows have.
- The animation MUST NOT extend under the bar. Its surface starts at the bar's lower edge, and a
  window scrolling up off the top passes behind the bar rather than into a strip above it.

> **Reported 2026-09-29.** "remove the menu bar sized gap between workspaces during the animation so
> they'd sit directly on top of each other during the workspaces vertical switch animation" and "make
> the animation overlay a bit smaller at the top to start right under the new menu bar". The strips
> were stacked one FULL display height apart while each workspace's windows started below the bar
> inside its row, so a bar-sized strip of desktop showed between workspaces as they scrolled. The
> overlay covered the whole display, including the band the bar sits in.

- The first switch after rini starts MUST slide like every other, whichever workspace a restored
  layout opens on. A switch slides from the workspace the display last showed, which is known from
  the first time rini lays the display out, so it MUST NOT wait for an earlier switch to learn it.

> **Found 2026-10-05, not reported.** The transitions journal showed the first `prev_workspace` press
> after a restart with no press mark and no flight, and every later switch sliding. The workspace a
> switch slides from was recorded only by a switch, so the first one on each display had none and
> cut to the destination.

## Arriving mid-flight

The layout does not wait for an animation to land, so a second pass can arrive with a different
destination for a window already moving.

- A pass repeating a destination MUST be treated as redundant, not as a new flight. Otherwise a held
  key restarts the animation on every repeat and it never lands.
- A pass with a new destination for a moving window MUST bend that window toward it rather than
  restarting: it continues from where it is drawn, at the speed it is moving. Its speed MUST NOT jump,
  so a burst of presses reads as one movement that settles after the last. That holds for a window a
  pass takes out of the piece it rode, as a second move of a narrower column does with the column it
  passes.

> **Reported 2026-09-29.** "as a general rule animation should not get interrupted mid-filight and
> restarted a new, the target should fluidly move to the next window and feel like one smooth extended
> flight". Each press restarted the motion curve from wherever the strip was drawn. The curve leaves
> at 6.25x its average speed, so a strip that had nearly stopped was thrown forward again: about
> 890pt/s to 15,375pt/s in one frame at a typical interval between presses.
- A window the flight has not seen MUST be able to join it.
- A merged press MUST NOT show a stale, transparent, stretched or zoomed tile. A picture taken for an
  end its tile no longer has, or of a window more than 20pt from where its tile ends, never reaches a
  tile: drawn, it is a stale tile, or one stretched or zoomed to the other size.
- A merged press MUST NOT lose a picture taken for an end its tile still has. Pictures are taken on
  their own thread and arrive while a press is merged, and none is taken twice.
- A merged press MUST NOT replace a tile's picture with the cache's: the tile keeps frame zero's, a
  focused face, or a picture the flight took.
- A press merged before the flight moves MUST compose with it as one movement from frame zero: the
  strip, and on a switch every row and floating window, travel together as one piece. Composed window
  by window from where each tile starts to where the last press sends it, the pieces slid over one
  another.
- A window a merged press brings, before the flight moves or while it does, MUST start beside the
  windows it sits beside in the layout, wherever they are drawn, and travel with them. Before the
  motion a column the second press brought started under the column the first had brought; once
  moving, one placed where the last press sends the strip started short of the strip by the travel
  the strip had left.

> **Reported 2026-10-03.** "rapid press navigation messes things up, i periodically see missing tiles
> (or they're just transparrent), occasionally i see tiles that are zoomed in." Each picture is now
> pinned to the end its tile had when it was taken and to the frame its tile ends at; which picture
> each of those tiles drew was not measured. A press merged before the flight moved also split the
> strip into pieces that slid over one another, one covering another; it now composes as one
> movement. Whether either is what was seen is not measured.

## A window that has just opened

- It travels from the frame the window server reports to its slot, if there is a usable picture of it
  and the capture budget allows.
- Otherwise the flight holds a place for it and waits for its first picture. Each reason for holding
  MUST be recorded distinctly: "the window appeared without animating" has four causes and the log is
  the only way to tell them apart afterwards.
- One that travels MUST be pictured at its slot only once it has repainted there. One that has not
  by the last capture before the lift is not pictured then: it flies its first picture, with its
  ground where that ends, to the lift, and the lift shows the window.

## The pictures a flight starts with

- A flight MUST start from pictures taken at the press: every window wholly on screen where its tile
  starts, and the floating windows seen behind them, a pass merged before the overlay is up included.
  The overlay MUST wait for them no longer than 80ms, and then flies what the cache holds until they
  come: each then replaces the cache's on its tile, and the flight MUST NOT start again.
- A real window MUST NOT move before the overlay covering it is on screen, whatever the flight: a
  write that reaches the app first shows the window jumping to its end while its tile still starts.
  Every frame MUST be written as soon as the overlay is on screen, so the window has the whole flight
  to land in.
- A window the overlay covers MUST keep painting: a point of the overlay is left clear over each
  window that ends wholly on screen, and over each grow ending cut by the display's right edge, so
  the window server counts it visible. A grow ending cut by its left edge has no such point: the
  point sits just in from a window's left edge, off the display for that one. Covered wholly, a
  terminal never reached its focused face within 450ms in 24 tries; with the point clear it did in
  all 24. So MUST a decoration rini draws around one at rest (`decorations.md`): a point is left
  clear over its band too.
- A floating window seen through a translucent one MUST show its live content while the flight runs,
  kept fresh behind the strip once the window gaining focus has its face.

> **Reported 2026-10-03.** "the background window images are served stale constantly, it's not a
> minor issue, because i can see changing content in the background windows during animation with
> semi-transparrent windows. this won't do." The windows behind the strip flew the cache's pictures,
> which no capture landing mid-flight replaced; the cache's pictures a flight drew were 1.6s old at
> the median, measured 2026-10-02.

## Resizing

- A resizing window's content MUST stand still against the edge of each axis that moves less over
  the movement, and MUST be clipped at the edge that moves: a window whose bottom edge moves, as a
  top row growing down or shrinking up, keeps its top rows where they are and reveals or swallows
  rows at its bottom; one whose top edge moves, as a bottom row growing up or shrinking down, keeps
  its bottom rows where they are, its title bar riding the top edge; a window widening leftwards
  keeps its right side where it is. Where both edges move, the one moving less holds the content,
  the top or the left when they move alike. The window's edges and rounded corners MUST ride the
  edges they belong to, whole.
- A resizing window's content MUST NOT be stretched or squashed. Where its picture does not cover
  its frame, the picture MUST be drawn whole at its own size and the rest MUST be the window's own
  ground, its background colour with its alpha, never the desktop behind it and never the picture's
  edge pixels drawn on. The ground MUST NOT be drawn under the picture, only where the picture does
  not reach: a translucent window, as Ghostty at 0.95, would show it through and darken. Only a
  picture the size of its frame at both ends of a movement, and the dark stand-in of a window with no
  picture, MAY be scaled.
- Reading a window's ground MUST NOT convert its whole picture: it MUST be read in the picture's own
  colour space and pixel format, and filled in the colour space it was read in, so the fill beside
  the picture takes no tint. Converted whole, a synthetic 1148x1079pt picture in Display P3 took
  11.5ms to read, which the overlay's main thread pays as it fills a crop-drawn tile (measured
  2026-10-10, "A picture's ground is read only when a crop-drawn tile asks for it" in
  `src/pictures/docs/README.md`).
- A resize MUST start as a strip movement does: from pictures taken at the press, the overlay up
  with them, and the windows written their new frames as it covers them. A resize that grows a
  window on either axis, one shrinking it on the other included, MUST then hold the motion, the
  overlay standing still, until that window has repainted at its new size, up to 300ms from the
  overlay going up or from the latest press growing it before the motion starts, 450ms for a window
  ending part off the display, and 600ms in all from the overlay going up, and that window MUST fly its repainted picture from the motion's first
  frame, or for a fold or an unfold from its corner (below), so its growth shows its content and
  never a fill cut to content mid-flight. A grow ending
  part off the display MUST be held for the same way, up to 450ms in place of 300ms: SkyLight returns
  only a window's visible part, so its repaint there MUST be watched through ScreenCaptureKit, which
  returns the window whole, and counted on two of its captures in a row, which takes longer. A press
  growing another window MUST NOT cut that hold short. A grow ending wholly off the display, or showing no more
  of itself there than a parked window's sliver, MUST NOT be held for: no watch sees it repaint. A
  window growing on one axis as it shrinks on the other, as a fold or an unfold, has no picture
  covering every frame, and MUST fly in two steps, never showing its ground: first the shrinking
  axis to its end, flying its picture from the press, which covers every frame of that step, then
  the growing axis, flying its repainted picture, which covers every frame of that one. Its picture
  MUST be cut from the one to the other at the corner between the steps, a cut between two
  renderings of the window, and its repainted picture MUST NOT be drawn before the corner: one
  landing before the motion or in the first step waits for it. One not landed by the corner leaves
  the second step its picture from the press, the window's ground in the growth, until it is cut in
  as it lands. The two steps MUST read as one movement, on the flight's one curve and length, its
  edges as fast through the corner as either side of it, and a leg a merge bends from where the
  tile is drawn MUST fly in two steps the same way.
  A resize growing no window MUST move as soon as the layout's follow-up passes are in, and MUST NOT
  wait for a resized window to repaint. The press's sequence stays a navigation's, the overlay first
  and the frames written at once; the only addition is the wait, and only for a window that grows. A
  window that has just opened keeps its own entrance, and a flight MAY wait for the first picture of
  a window it has none of.
- A pass that grows a window on either axis while a flight is already moving, a press arriving
  before the flight of the one before it has ended, MUST hold that window the same way: the flight
  MUST play on as it was, its tiles going on to the ends they had and resting there with the overlay
  up, and the pass MUST merge into it only once that window has repainted where the pass ends it,
  up to 300ms from the pass's arrival, 450ms for a window ending part off the display. Only that
  repaint merges it: a picture of the window taken
  anywhere else, as where the flight ended it before the pass, MUST NOT. The tiles then MUST set off from where each is drawn at that
  moment, the grown window flying its repainted picture, so its growth shows its content. A merged
  pass grows a window when it ends it wider or taller than the picture its tile carries, whatever its
  own sizes: a press shrinking a window whose earlier grow has not repainted yet still grows it past
  the picture on its tile, and a pass moving a window a held pass grows, a strip movement included,
  is judged so too; a move of any other window stays a navigation. A pass arriving while one is
  held replaces what the flight waits for, its own grows from its own arrival, up to 600ms in all
  from the first one's, and the repainted picture where the latest one ends the window MUST go onto
  its tile as they merge. A pass that grows no window MUST merge at once, with any pass held before
  it. Each held pass MUST write its windows as it is held, and their merging MUST NOT write
  a window again, least of all back where an earlier held pass sent it. A held pass whose window misses the 300ms, the 450ms of one ending part off the display, or the
  600ms of a burst, merges with the
  picture its tile has, the window's ground in the growth, and the repainted picture replaces that
  one when it comes.
- Each window a resize grows to end on the display, wholly or cut by its edge, MUST be pictured at
  its new size once it has repainted there. One whose
  repaint missed the hold flies its picture from the press, with the window's ground in its growth,
  and its repainted picture MUST replace that one mid-flight once it comes, laid against the same
  edges as the old one, with the window's ground wherever it does not cover the frame still to come.
  A window a resize only shrinks MUST fly its picture from the press, clipped, to the lift: that
  picture covers every frame of the movement, so there is no ground to cut from, and its repainted
  picture and the last capture before the lift MUST NOT be drawn on it. It is never held for itself,
  and moves when its flight does, with the held grows when the same resize grows another window. One
  starting clipped or off the display has no picture from the press: it MUST fly the cached picture
  of it the same way, to the lift, when that picture fits where it starts, its content as old as that
  picture; only one whose tile starts with no picture covering its start MUST have its repainted
  picture replace the one it flies as it comes. Where passes merge, the latest one resizing a window decides whether it grows or only
  shrinks, against the picture its tile carries, and so whether the motion still waits for it,
  before the motion starts as once it moves. A picture taken at its
  new size MUST NOT be drawn before the window has repainted there, not even to show its focus turn;
  one that has not by the lift is never drawn, and the lift shows the window. Until then the tile
  keeps the face its picture from the press shows, the title bar and controls macOS draws focused or
  not, while its hairline, shadow, ring and border turn as any window's: a window that never settles,
  as one streaming output, and a window a resize only shrinks inside the picture it starts with,
  turn their own face with the lift. A window MUST count as repainted at its new size only once two
  captures in a row show the same rendering, never because a capture differs from the picture it had before.

> **Reported 2026-10-09.** Kai: "there are multiple issues during stacking/unstacking windows into
> columns. both on windows that are making space and the one that is being tucked under or pulled
> out.", "window content being squashed/stretched, where it should be clipped really. or it's
> clipped in an akward place slicing the content. i regularly see clipping on folding/unfolding
> animations going in a wrong direction, like for example a bottom stack window expanding up but the
> clipping unfolding going down", "some of the issues seem intermittent".
> Every resize was clipped one way, the title bar held at the top and the content held against the
> bottom, whichever edge moved. Recorded the same day on Ghostty windows of numbered rows
> (`docs/data/2026-10-09-resize`, runs s1 and s2): a top row folding a window under it showed its
> rows from 29 down under its title bar, the rows above them sliced out, and a top row maximizing
> unrolled its content downward from under the title bar instead of revealing rows at the bottom. A
> bottom row maximizing, the case Kai named, was recorded holding its content against the bottom and
> revealing rows at the top, as this requires, so what Kai saw there was not caught. A bottom row
> restored from maximized had its small picture cut in a tenth of the way through and stretched, its
> glyphs 1.5 to 2 times their size on both axes.
>
> **Reported 2026-10-09**, after the recordings above. Kai: "your rework should utilse the same
> tooling/sequencing that we've built for navigations: skylight frame zero -> overlay -> skylight new
> positions -> swap pics in animations mid-run. the tricky part is that the pictures of the windows
> should be fixed on the opposite sides of the resizing animation; if a window stretches down it's
> fixed on top, expanding up it's fixed at the bottom, etc. the current navigation animations are very
> smooth and performant and we should lean into the same structure. feel free to extract reusable bits
> into modules/submodules as necessary". A grow held the overlay still for up to 300ms for its window
> to repaint before anything moved, 4 of 21 maximizes that day to the limit; a shrink had its new
> picture cut in as soon as the window was there, however it was painted; and a window unfolded out of
> a stack flew a half-painted picture, its old rows at the top of a blank surface, the whole flight.
>
> **Reported 2026-10-09**, after trying the change the report above led to. Kai: "this is a massive
> improvement. thank you. one bug i can see straight away though is a flicker between blank fill
> from a smaller window to actual enlarged window. i'd rather take a lag of waiting for actual new
> window content than see a flicker." Each grow flew its growth as the window's flat ground until
> its repainted picture was cut in mid-flight: that cut was the flicker, and each shrink had its
> repainted picture cut in too. Measured in "A grow waits for its repaint"
> (`src/transitions/docs/coordination.md`).
>
> **Reported 2026-10-10**, after trying the change the report above led to. Kai: "alt-r is fixed i
> can confirm. resizing still intermittently produces black sections flicker when a window size
> increases. easiest way to reproduce is to trigger alt-r rapidly on one of those rainbow terminals
> you made." Only a flight's start waited for a grow's repaint: a press reaching a flight already
> moving merged into it at once, so its grow flew the old picture with the window's ground in the
> growth until the repaint was cut in mid-flight. Measured in "Mid-flight passes"
> (`src/transitions/docs/coordination.md`). Recorded the same evening on the build holding merged
> grows (`docs/data/2026-10-10-rapid-altr`, run altr1, 12 presses): 2 of 5 merges of held passes
> went at once on a picture of the window at the end the flight had for it before the press, taken
> before the press. After the second, lab-A's growth from 860 to 1148 showed its ground, up to
> about 170pt wide, for 129ms (frames 1954-2083ms) until its repaint was cut in. A fold and an
> unfold recorded the same evening (`docs/data/2026-10-10-fold`, run fold1) showed up to 422 flat
> dark blocks, about a quarter of the screen: a window starting part off the display shrank with its
> landing swapped on before the motion, its ground under it; the window folding under it flew its
> repainted picture, 538 tall, over its 1079-tall start, its ground above it until its height caught
> up; and the first grew back to end part off the display with its ground in the growth, never held
> for, for the 359ms from the motion's start to the lift.
>
> **Reported 2026-10-11**, on the unfold recorded after the change the report above led to. Kai:
> "unfold flash take longer wait". Recorded on 2026-10-10 (`docs/data/2026-10-10-fold`, run fold2):
> lab-A grew back to end part off the display, held for its repaint, which came through
> ScreenCaptureKit 364ms after the overlay went up. The hold let go at 300ms, so 3 frames, 4033 to
> 4067ms, flew its growth with its ground, 67, 156 and 266 flat dark blocks, until the repaint was cut
> in 64ms into the motion. The hold for a window ending part off the display became 450ms, chosen
> above that one sample: such an unfold sets off on its repaint, 64ms later on fold2 and at most
> 150ms later than before, with no ground.

## Correctness the user can see

- The frame written to a window is the layout's answer, never the animation's. A window parked off the
  strip is written its real park position; the tile is merely drawn heading off the edge.
- A flight MUST draw every window on its display, the ones its pass leaves where they are included:
  the overlay is opaque and spans the display, so a window it is not handed shows as wallpaper for
  the whole flight. Measured: a floating window re-placed by one point flew alone and every other
  window on screen showed as wallpaper for 350ms.
- A flight's frames MUST be written to the windows ending on the display before the windows ending
  parked off it. Each write is several round trips the app answers one after another, and a strip
  switch interleaves the leaving window's park with the arriving windows' places: written first, a
  park nobody sees delays a window everybody does, which lands late, after the lift.
- Every tile MUST start where its window is on screen at the press. A strip scroll draws the
  finished layout and starts the view back by the scroll, which puts back only the windows the
  scroll carries: a window already standing where the scroll puts it, as the one a left-edge drag
  resized stands at its release, MUST start where it stands ("Resizing with the mouse" in
  `specs/strip.md`). A window parked at the press MUST start where the strip had it, never from its
  park corner, however much of it macOS left on screen, on a scroll and on any other layout change:
  one the change carries comes in along the strip, in the row and at the size it ends at, and one
  whose place on the strip the change kept, as the columns after a left-edge shrink keep theirs,
  MUST stay where it was, off screen.

> **Reported 2026-10-07.** "stil diagonal moves animation. specifically when i switch between the
> two chrome windows on ws 1". A window standing on screen away from where the scroll drew it
> started from where it stood, and macOS leaves more of a parked Chrome window on screen than the
> off-screen test allows, so it flew in from the bottom-right corner, in x and y at once.
- A window a layout change parks MUST leave along its own place on the strip, in the row and at the
  size it is seen at, and end off screen: it moves with its column, and a resize the change gives it,
  as the stack a maximize leaves is evened out over its column, happens off screen. No window MAY
  take its movement from a window the change resizes: a resizing window's corner moves by its own
  resize, not by the strip. A window parked at both ends MUST NOT be seen unless the change carries
  it across the display. Off screen means off the whole display: a parked column's place on the
  strip can lie where a side Dock is, and it MUST NOT be drawn there. The movement MUST NOT wait for
  a window the change parks, however it grows: no watch sees it repaint off screen.

> **Reported 2026-10-09.** "going full-size and back moves surrounding windows out incosistently
> and they ovelap during the animations", "both on windows that are making space", "different
> issues based on whether a window is at the top, middle, or bottom of a stack", "some of the issues
> seem intermittent". A window a maximize, a restore, a fold or an unfold parked or brought back
> moved by the nearest window moving on screen, usually the one being resized. On Kai's 1728x1080
> screen the siblings and the next column slid up 359pt off the screen from the middle row of a
> 3-stack and 539pt from the bottom of a 2-stack, diagonally from a right stack, and came back down
> from above on the restore; which window lent the movement changed with how near each was.
- A window MUST NOT be left at an animation's intermediate frame if a pass is interrupted.
- The overlay MUST stop drawing a tile whose window is gone, and MUST drop a container once the last
  tile leaves it. An empty container is a layer the compositor keeps compositing.
- Every window MUST be asked for a picture the first time rini lays it out, on whatever workspace,
  before anything flies. After a restart nothing moves, and a pass that moves nothing asked for no
  pictures, so the first flights had none to draw. Per window, not per display: apps are found one
  by one at startup, and the first pass on the display knew one window of 21.
- A picture read off the screen after a lift, such as the bar's, MUST NOT show the overlay: the bar's
  is drawn over every flight after it, so one taken with the overlay up draws the overlay where the
  bar should be. The bar this pictures is one another application draws, with rini's bar off
  (`bar.md`, "Showing it").
- A change of display MUST NOT drop the window pictures being captured. Only the desktop render belongs
  to a display; with a second display attached, every flight on the other one threw the window captures
  in flight away.

> **Reported 2026-09-29.** "i also occassionally run into issues where windows are missing during the
> animation, or maybe have holes in their screenshots... i just notice momentary gaps where only the
> window frame is moving during the animation. i've seen plenty of those during your automated testing
> too". Over four restarts no picture landed for the first 11s, and 49% of the flights in the first
> 30s were missing a window; after 2 minutes, 5%. The replays ran seconds after each restart.
- A change to what a flight does at its show, in flight or at its lift MUST NOT slow navigation.
  Before it is called done, the timing battery flies the build before it against the build with it,
  the pans and the workspace switch, and every measure of what is seen MUST pass the battery's
  two-stage rule, or fail it no more than the rule fails one build against itself. The ring and the
  border are the case "Through a flight" in `specs/decorations.md` holds against both off.

> **Reported 2026-10-09**, closing Kai's report on the resize animations: "make sure you don't cause
> regressions in navigation animations again". The resize work flown against the build before it
> on 2026-10-11 passed 81 of 87 measures and failed 6 at the second stage, none by more than 3ms,
> within what the rule fails one build against itself
> (`docs/data/2026-10-11-resize-battery/readings.md`).

## Windows with no picture yet

- A window with no picture MUST NOT leave a hole in the strip. A known window parked off this display —
  every window after a restart, with the picture cache empty — MUST fly with a dark stand-in tile in its
  place, moving exactly as its picture would. That holds for every flight: a layout pass, a strip
  scroll, a switch and a move, where the column a full-width window passes is parked at both ends.
- The stand-in MUST keep the window's rounded corners, and MUST be replaced by the window's real picture
  as soon as one lands, on the same tile, so nothing else in the flight is disturbed. A flight that does
  not wait for pictures, a strip movement, MUST ask for the picture of a window it brings onto this
  display once the window is placed there.
- The stand-in MUST NOT be cached. Cached, the switcher would read it as the window's picture and the
  next flight would take it for one.
- A genuinely new window, on this display with no picture, keeps its spawn capture and entrance.

> **Reported 2026-09-28.** "When the initial image is missing (after a restart or whatnot) the
> animations render a hole in the strip. I think having a default dark grey fallback image would be
> better." A parked window with no picture took the reservation meant for a newly opened window, which
> draws nothing in its slot until a picture lands.

## The pictures held

- A window rini manages MUST be pictured under its own identity, and every flight and the switcher
  MUST draw it by that identity.
- A window parked off the display MUST NOT be pictured through the sliver it shows at the corner. It
  keeps its full size there, so the picture costs as much as one of a window on screen, and it is
  pictured by its own identity like every managed window.
- A picture delivered to a flight MUST be held for it only while one of its tiles can still take it.
  One a tile turned down or one delivered to a flight before MUST NOT be held, nor any once the
  overlay has lifted. The pictures kept for the next flight are the cache's.
- A lifted overlay MUST NOT hold a picture of the desktop. The desktop the next flight is dressed
  with is the one the pictures service holds, so a second, older one behind the lifted overlay is
  memory nothing draws. A flight dressed before its display's desktop has been rendered MAY draw one
  composited without its wallpaper, since there is no earlier desktop left to prefer.

> **Reported 2026-10-06.** "fix that runaway thread and stale cache cleaning" and "stale copies
> definitely need deleting, no point in keeping them around". Fixed.
>
> Measured with a read-only listing under the same checks, 2026-10-06: the pictures taken whenever
> the display the overlay flies on changed counted 26 windows, 552MB at 2x, because a parked window's
> sliver passed as on screen. One of the 26 was on screen. Each was a second picture of a managed
> window or one of a window rini does not manage. Read from the code: nothing ever let go of a
> picture of a window rini does not manage, so those of closed windows stayed until rini restarted.
> How many the running build held was not counted. From the journal, 2026-10-06: 237 of 580
> lifts left a picture delivered after the lift held by the flight's engine until the next lift.
> Read from the code, each can pin a display-sized capture. Read from the code and measured with
> vmmap, 2026-10-06: the overlay kept the last flight's desktop after the lift, 29.5MB at 3456x2234,
> beside the newer one rendered at the release after it.

- rini MUST offer no debug action: a bind naming `debug`, `serialize`, `show-timing`,
  `debug-warm-snapshots` or `debug-overlay-slide` MUST be refused as an unknown action.

> **Reported 2026-10-15.** Kai's decision on the debug actions: "delete". The debug slide and the
> warm were the only paths that pictured a window rini does not manage, so the requirements on those
> pictures went with them.

## Windows with blurred materials

- A window whose sidebar, toolbar or background is a blurred material MUST fly with its blur, not the
  flat grey every capture of the window on its own returns there. The grey against the live blur at
  landing reads as a flicker, and blurred materials are now on most windows.
- A captured blur MUST NOT pick up anything that was above the window: an overlapping window, the
  switcher's popup, or rini's own overlay.
- A window's rounded corners and any real per-pixel transparency MUST stay transparent in its picture.
  A capture that contains the blur also fills the corners with whatever was behind them.
- Once a window's picture carries its blur, a later capture of the same size that lacks it MUST NOT
  replace it.
- A window MUST be found to have a material wherever the material covers a visible part of it, a
  sidebar as much as a whole background, however close what is behind it comes to the grey on
  average over the whole window. What is found is kept until the window goes, so a window wrongly
  found to have none flies grey for as long as it lives. Not yet met: a material only within the
  outer tenth of the window each way, such as a toolbar along its top, is left out of the test with
  the corners.
- A window MUST be found to have no material only while it is active: macOS draws an inactive
  window's material flat, the same alone as over what is behind it. Seen with none while inactive,
  a window is pictured alone, which is what the screen shows of it then, and its material is
  measured again the first time it is pictured active, 200ms or more after the window server
  reports it focused, while its application holds the keys. Until then a flight that brings it in
  focused flies the flat picture, its landing included when it lands sooner than that.
- A picture MUST NOT be built from a capture of the window's rect taken while the window was somewhere
  else. The rect then holds whatever the window was covering, and merging that in replaces the window
  with another one — found on the first live run, and far worse than grey.

> **Reported 2026-09-28.** "Screenshots for apps that use blur... currently they render with some grey
> colour and cause flicker when the real window switches in... there are a lot of windows that use blur
> here and there now, so it's quite prevalent and needs a better fix." Every per-window capture route
> had already been measured returning the same flat grey. The window captured together with what is
> below it carries the real blur and leaves out anything above it, measured the same day.

> **Reported 2026-10-08.** "you broke the blurred windows capture, they travel with the default gray
> again", after the borders deploy restarted rini, which learns every window's material afresh at
> each start and keeps it while the window lives. Rini's test read outside rini the same day put
> Outlook with its sidebar at 1.99 levels from the window alone, averaged over the whole window,
> under the 2-level threshold: no material, so pictured alone, grey where the sidebar blurs. That
> reading came over an hour after rini learned its windows from 14:09, and nothing reads what rini
> learned, so Outlook as the window and this as the cause are inferred. The borders change touched
> neither the test nor the capture. Part of a window changed by its material now counts, not only
> the whole window's average (`docs/data/2026-10-08-materials`).

> **Reported 2026-10-08.** "outlook and messanger, still grey", with the share rule deployed. Rini
> learned Outlook's material at its start, 16:34, with Outlook on screen and another application in
> front: inactive, Outlook's folder pane reads 0.00 levels baked against alone, and active 1.96 with
> 9% of samples changed, so it was learned to have none, pictured alone, and flew grey. Reproduced
> outside rini, 17:32-17:35, by turning Outlook inactive in place; that rini's own first reading was
> inactive is inferred from the journal, whose first flight after the start had no focus. With its
> pictures carrying no blur, nothing kept the focused face Outlook flew in with: kept as it lost
> focus, it was gone by the next flight to it, from 17:22 on (the journal; why it went is inferred
> from the code, a capture at rest showing a different body). Messages reads 0.00 alone against the
> screen active and inactive, and ScreenCaptureKit's capture of it equals SkyLight's over its
> sidebar, so no route loses anything of it; what flew grey of it is not established. Readings in
> `docs/data/2026-10-08-materials`.

## The window's hairline

- Every tile MUST wear the hairline macOS composites around each window, which no capture carries: a
  1pt line of white on the window's outermost point, at 20% on the focused window and 18% on any
  other, and a 0.5pt stroke of black at 90% in the half point just outside it.
- The hairline, the shadow and the space it is cut around, and a picture clipped to its corners, the
  stand-in's included, MUST round to one corner: the window's own radius as the window server
  reports it, read when the flight composes the tile and, for a window that opens, when its first
  picture lands, so a window never pictured and a parked one round right on their first flight;
  16pt where it reports none; never past half the tile's shorter side; on the window's own
  continuous curve at every size the tile passes through. Each is drawn separately,
  and any two that differ show as a step at the corner. Why the server's radius and why 16pt: "The
  corner, measured" in `src/decorations/docs/README.md`.
- The hairline's line and stroke MUST follow the tile through a resize, on the tile's own timing,
  every piece at the one corner of each size it passes through, and onto its window's real frame
  when the tile snaps there. The outline MUST stay whole on a tile whose picture is clipped to
  its corners.
- When focus moves during a flight, each window's hairline and shadow MUST turn with its own picture: the
  window losing focus dimmer as its picture turns unfocused, the window gaining focus brighter as its
  picture turns focused, each at its own time. Its focus ring and its border do not: they wear
  where the flight ends from its show (`decorations.md`).

The hairline's values are a fit measured 2026-10-02 in dark mode, on 27 window edges across 11
applications: within 0.3 levels rms on focused edges and 0.1 on unfocused ones. Light mode is not
measured. Before it, a tile wore a hairline cut out of a capture of its window, and none until that
capture had run: the 53-level p99 at the focused window's edge in "The handover, measured"
(`src/animation/docs/capture-overlay-research.md`).

> **Reported 2026-10-06.** "refactor the two into a single module to avoid duplicate
> implementations". The hairline rounded to the window's measured radius while the shadow, the space
> it was cut around and the stand-in rounded at 10pt, two corners on one tile. Every decoration a
> window wears now has one home, and every piece one corner.

> **Reported 2026-10-05.** "regarding the ring and shadow. i'd like them to be orchestrated
> independently whenever blur or focus are done switching." Both tiles turned in one cut when the
> picture of the window gaining focus turned, whenever the other window's picture had. Each tile's
> hairline and shadow now turn with its own picture.

## Focus changing during a flight

- When a flight moves focus from one window to another, both MUST land in their new focus rendering:
  the window gaining focus active, the window losing it inactive. The change MUST reach each tile while
  the flight still moves, so nothing changes at rest after the lift.
- From the moment the window server reports the window gaining focus focused, it MUST be pictured
  where it ends until its face changes, and the window losing focus with it, in the same rounds, until
  its own face changes, if it ends wholly on a display and no pass opened or resized it: one a pass
  opened or grew is pictured once it has repainted instead, its own face coming only with that
  picture, and one a pass only shrinks inside the picture it starts with keeps that picture, its
  hairline and its shadow turning once it has repainted, or just before the window gaining focus when
  it loses focus, and its own face with the lift (Resizing). One a pass only shrinks that starts with
  no picture covering its start turns with its repainted picture, as a grow does. A face a
  window already had when it landed counts as changed. Each MUST be drawn with its turned picture as it comes in, its hairline and its
  shadow turning with it: the window losing focus unfocused, the window gaining focus focused. Its
  focus ring and its border wear where the flight ends from the show (`decorations.md`).
- The window losing focus MUST turn first, as macOS draws it. The turned picture of the window gaining
  focus MUST wait for the other's at most 15ms, one capture interval, and not at all once the other's
  turn can no longer be pictured; two that come in together MUST turn old, then new. Waiting for the
  other MUST NOT delay looking at the window gaining focus: it MUST be looked for as often as any
  window landing until it is where it ends, and the windows behind the strip MUST wait only for its
  turn. A window losing focus whose turn has not shown by then, one ending clipped or parked, one a
  pass opened or resized, or one whose watch ran out, MUST be drawn unfocused just before the window
  gaining focus is drawn focused. If neither turn shows by the last capture before the lift, both MUST turn then all
  the same, old then new, while the flight still moves, but for one a pass only shrinks inside the
  picture it starts with, or one a pass opened, grew or shrank that has not repainted where it ends: it is
  not pictured then, only its hairline and its shadow turn, and its own face turns with the lift. Until its turn the window losing focus MUST be
  drawn focused, as it was on screen when the flight began. Ending clipped by the display's edge, it
  keeps its picture to the lift, only its hairline and its shadow turning unfocused, and its face turns at
  rest, unless it has a material: then it keeps its blurred picture until a flight next pictures it on
  screen (below). SkyLight returns only its visible part, and the one ScreenCaptureKit capture below is
  the window gaining focus's.
- A press moving focus back to a window already drawn unfocused, before the window it went to has
  turned, MUST leave it drawn unfocused, hairline and shadow with it, until its focused picture comes in,
  and MUST then turn it focused as any window gaining focus. A press moving focus on before then MUST
  NOT turn it again. One starting off the display flies its focused face instead (below), drawn
  focused again, and a press moving focus on MUST turn it unfocused again: no tile is left drawn
  focused beside the window gaining focus. A turned picture waiting for the old window's MUST be dropped when a press moves
  focus on, and one that no longer fits its tile MUST turn nothing: drawn focused over the picture
  from before, a hairline disagrees with its window.
- A focus report that arrives before the flight watches for it MUST still count, and so MUST one that
  arrives before the request it causes, as a click's or an app activation's does: the window focused
  before it is the one the flight begins drawn focused.
- A window gaining focus that starts off the display, where nothing can picture it, MUST fly the last
  picture taken of it focused, drawn focused from the first frame of the press giving it focus, a press
  merged into a flight included, unless that picture no longer fits it or a later picture shows its
  content changed. Its landing MUST NOT replace that picture before focus hands over: it is often taken
  before the window repaints focused, and would turn it unfocused and back.
- A picture MUST be kept as a window's focused face only if it shows the window focused: its change
  pictured as it gains focus, or frame zero's picture of the window focus leaves if that window was
  the key window as the press went out. A click's report has made its target key before the request it
  causes, and another application's panel taking the keys, Spotlight's or Raycast's, leaves the
  layout's window drawn unfocused: kept, that picture flies the window in as its focused face looking
  unfocused.
- A window gaining focus that ends clipped by the display's edge MUST still be recaptured. SkyLight
  returns only its visible part, so it takes ScreenCaptureKit's one capture.
- Every other ScreenCaptureKit capture, a warm or a background capture included, MUST wait from the
  press to the quiet after the lift, but for a grow's repaint where it ends cut by the display's
  edge (Resizing) and the screen a measured lift's handover check takes once its overlay is down (`journal { measure-handover; }`, below). With four of its captures out, the
  capture the overlay waits for at the press took three times as long.
- A flight that moves focus nowhere MUST NOT recapture through ScreenCaptureKit, and MUST NOT watch for
  a change of focus: no report comes to arm the watch, and one left waiting holds the windows behind
  the strip back for the whole flight.
- Both windows a change of the key window turns, the one that becomes key and the one that stops being,
  MUST be pictured again for the next flight and the switcher, whoever moved focus: a click, rini's own
  move, or another application's panel taking the keys. With nothing flying both MUST be asked for at
  once; in flight, each one no picture of the flight showed after the change MUST be asked for at the
  quiet after the lift, the window losing focus that ends clipped or parked among them. A focus change
  alters a window's face and never its size, and a picture that still fits was never taken again for
  its own sake, so a window kept the face it had until it was resized, and its tile turned at the lift.
- Pictured again at rest, a window wholly on the overlay's display MUST be pictured with its blur and
  with its face turned. Both windows of one change MUST be looked at together, and each MUST be
  pictured once, with the face it settled on. A capture of the window alone has no blur, and a picture
  with its blur is never replaced by one without, so a window with a material kept its old face until a
  flight next pictured it on screen; a capture made as the change is reported shows the face from
  before the app repaints; the picture held from before can differ in its title band for reasons
  other than focus, so the turn is judged only from captures taken after the change; and a face has
  settled once two captures in a row, the later taken 70ms or more after the change, show the same
  title band, past the 68ms (p90) the app takes to repaint. Looked at one window at a time, a click
  took twice the rounds, and every capture went to the cache and to every client as news.
- A capture at rest MUST NOT start from a press to the quiet after the lift, as no ScreenCaptureKit
  capture does: SkyLight serves one call at a time, and one already out delays the one the overlay
  waits for. A press MUST cancel the polling of a window being pictured again at rest, and the quiet
  MUST ask again for each window the flight did not picture at least 70ms after its face turned: the
  app repaints up to 68ms after the change (p90), so a picture taken sooner, frame zero's among them,
  can show the face from before. A call already out when the press comes cannot be recalled, so at
  most one SkyLight call at rest MAY be ahead of a frame zero.
- A window with a material that is not wholly on the overlay's display when it is pictured again at
  rest, clipped, parked or on another display, keeps its blurred picture, and its face turns when a
  flight next pictures it on screen: ScreenCaptureKit's capture of it has no blur, and SkyLight's
  covers only what is on that display ("Known debt" in `src/pictures/docs/README.md`).
- Once the overlay is up, a tile MUST change its picture only for one the flight took: frame zero's
  coming after the overlay went up without it, before any later picture of its window, as its window
  lands, as the window gaining focus
  changes its face, as a floating window behind the strip is refreshed, or just before the lift. A
  capture made for anything else shows the window as it was, and cut in after the flight's own it
  would cut the tile back.
- A picture the cache refuses MUST NOT reach a tile either. The flight would draw what the next one
  will not.
- A picture of a window that was not wholly on one display while it was captured MUST NOT be drawn,
  cached as a picture, or cut onto a tile. The capture comes back full-size with the off-screen part
  transparent, and drawn it is a half or fully missing window.

> **Reported 2026-10-05.** "i noticed that old window blur often animates _after_ the new window focus.
> i understand that blur actually renders _before_ focus in macos itself." Only the window gaining
> focus was watched in flight. The window losing focus kept its frame-zero picture, its focused face,
> until a landing or the last capture before the lift showed it unfocused, so it turned after the
> other. Both are now watched in the same rounds, and the turn of the window gaining focus waits up to
> 15ms for the old window's.

> **Reported 2026-10-05.** "did you want that in case of rapid switch? in case current polling starts
> interferring with the next position? how do you manage that btw? cancel the previous polling and let
> the last capture finish?" A press paused the polling and the release resumed it with the tries it
> had left, judged from a capture taken before the flight moved the window, and the two calls of a
> window never pictured before ran back to back, so two could be ahead of a frame zero. A press now
> cancels the polling, only the call already out finishing, and the quiet asks again.

> **Reported 2026-10-05.** "why are you capturing each old/new window independently? seems like a
> waste, could capture both at the same time as a group call, no? stop polling when both re-rendered."
> Each round looked at one window, so a click's two ends took twice the rounds. Both are now captured
> in one round, each dropping out once its face has settled, and the polling stops when both have.

> **Reported 2026-10-05.** "about focus refresh. are you pushing new images on every scan or only once
> when they change?" Every capture of a refresh went to the cache and to every client, identical or
> not, and a window whose first capture already showed the turn was captured all six times. A window is
> now pictured once, with the face it settled on.

> **Reported 2026-10-03.** "fcache doesn't bother me … windows arrive in focus every single time". The
> prototype flew a focus target parked off the display with its last focused picture, which can be
> older than the window's content; asked about that, Kai preferred the window arriving focused.

> **Reported 2026-10-02.** "mostly bugged by the flicker into the active window". The window gaining
> focus flew its picture from before the change and turned focused only at the lift.

> **Reported 2026-09-28.** "The old window stays in its old active state throughout the entire
> transition and then flickers in in its inactive state once the animation finished. Would be great if
> we could swap the image during the animation itself if it's available." The recapture already
> covered both ends, but at 0.5 through the background service. Its pictures landed at 0.82 to 1.16 of
> the flight, measured the same day, past the 0.6 cutoff it had then, so neither end was ever cut in.

> **Reported 2026-09-28**, against the fix above. "Windows half cut by the display cause all sorts of
> random issues, it displays half or fully missing frame during transitions constantly." The refresh
> judged a window wholly on screen, and the strip pan moved it past the edge before the capture ran.

## Which windows come forward together

- A flight MUST draw the front-to-back order the screen LANDS in. An order that only exists for the
  length of the animation reads as the windows rearranging themselves twice.
- The strip is one set. Focusing any window on it MUST bring the whole strip in front of everything not
  on it, because a scrolling workspace is a single surface and cannot have a hole in it.
- An APPLICATION is the other set. Focusing one window of a multi-window application MUST bring that
  application's windows forward together, and MUST leave every other application where it was. This is
  what macOS does on its own: raising a window activates its application, and that raises its windows as
  a set. So it is also the order the flight lands on.
- "Off the strip" is NOT a set. Two windows being off the strip says nothing about whether they come
  forward together.

- An off-strip window the server already has in front of the strip MUST stay in front of it. macOS
  raises the application gaining focus over it and does not push it behind the strip.
- Inside the lifted band the application gaining focus MUST lead. The server has not raised it yet
  when a flight starts, so its own order can still put another application between two of its windows.

> **Reported 2026-09-25.** "During the animation from here to 1pass it renders zoom window under it too.
> When it lands the stack is zoom in bg -> strip -> 1pass, but during the animation it renders strip ->
> zoom -> 1pass." Then, on the rule: "a multi-window application like zoom, focusing on one the
> application windows brings that entire application windows set up. It doesn't mean bring all off
> strip apps up, only the one being focused." Fixed.
>
> The z rule banded by `StackGroup`, which had two values, `Tiled` and `Floating`, so every off-strip
> window was one group and focusing 1Password promoted zoom with it. It now draws three bands with the
> strip always in the middle (`z_group::stack`).
>
> Fixing the depths alone could not have worked. Tile layers are children of their container, a
> container's `zPosition` alone decides its children's order against every other container, and one
> container held every floating window. So the floating container has a twin in front of the strip that
> rides every translation the floating container does, and each floating tile is moved into whichever
> of the two its band names. A twin rather than one container per application, because only the
> focused application changes band: every other application keeps the server's order among the rest,
> which one container behind the strip already expresses.

## Watching transitions: the journal

- rini MUST record every step of a transition: the press that began it, as rini took it, each thing
  its lifecycle hears and decides, a frame's tick only when it first finds the transition at rest or
  landed, each delivery of pictures to it with when the pictures service sent it and when each
  picture's capture started, each picture a tile takes with which way it was taken, whether it
  carries its window's blur and how old it is, and each commit of what it draws, all with when, so a
  glitch seen while
  moving around can be read back afterwards, and the time from a press to the overlay up, counted to
  the commit that puts the overlay on screen, split between rini's work on the press, the captures,
  the time the transition waited to hear of them, the wait for the thread that draws it, and that
  commit.
- rini MUST record every turn of the thread that draws a transition that runs longer than one frame
  at 120Hz (8ms), with when it began, how long each of the jobs sharing that thread took in it, the
  time outside them, and each window change rini made from 50ms before it began to its end, so a
  draw that waited for that thread can be traced to what held it.
- rini MUST keep the last 8192 steps of its transitions in memory at all times, so a record can be
  written of a glitch seen with the journal off.
- rini MUST NOT write the journal unless it is turned on (`animations { journal; }`). With it on,
  rini MUST write each burst of transitions once it has settled, and MUST NOT start a write while a
  transition is up, so writing adds nothing to the durations it records. The record MUST be at most
  two files: the current one, started afresh once it passes 50MB, and the one before it.
- Writing the journal or not MUST NOT change what is drawn or when; only measuring the handover may
  (below), and `journal false` turns measuring off with the writing.
- With `animations { journal { measure-handover; } }`, rini MUST record for each lift, over every
  window ending wholly on screen, the mean RGB difference between the overlay's own picture just
  before the lift and the screen just after it, taken apart over the window's interior, inset 4pt,
  over the 1pt either side of its outline, and over the band a decoration draws outside the outline
  where the window wears one at rest (`decorations.md`), so a lift can be read against the handover
  measured on 2026-10-02. Measuring MAY hold each lift's hide up to 60ms for the overlay's own picture; the bar's
  picture and the quiet after the lift MUST then count from the hide, so the bar is never pictured
  with the overlay over it. That bar is one another application draws, with rini's bar off
  (`bar.md`, "Showing it"). It MUST NOT start a capture once a press has asked for a
  transition, so that press waits at most for the one capture of the overlay or the screen under way
  as it lands, and it MUST be off unless asked for.
- Each step MUST carry its time on the outside probe's clock (mach absolute time), so a probe's trial
  and rini's steps line up with no wall clock between them.
- CPU and GPU use MAY be recorded only once what reading them costs is measured, and found to slow
  nothing down.

> **Reported 2026-10-03.** "can you add transitions logs? as in log timing for every transition step,
> make sure you don't flush until after the transition settles so the disk io didn't add to the
> durations. i want you to have a record overything as i move around so you could have a postmortem
> picture for debugging, you can log CPU/GPU utilisation in the moment too if it doesn't cost anything
> in terms of slowing down". The `scratch/handover` prototype had kept a text journal from about 135
> call sites, with a lock and an allocation for each mark. This one stamps each press where the
> reactor takes it, marks the lifecycle where it is driven and each delivery of pictures where the
> transition hears it, keeps its marks in a ring allocated once, and writes them once asked.

> **Reported 2026-10-05.** "journal off by default, on on my laptop for dev tracing." The journal had
> been written unless turned off, as the record of everything above asked; it is now off unless
> `animations { journal; }` asks for it, and the ring is kept either way.

## Reading the window server

- A read of windows MUST NOT wait on a Core Animation commit of rini's own windows, on any thread.
  rini's own windows moving, as the focus ring's and the border's do whenever a window they follow
  moves, MUST NOT slow a flight, a press or the main thread. A read that shares the connection those
  windows belong to waits for their last commit while holding the connection's lock, and the main
  thread's commit waits for that lock: both stopped 500ms, until macOS timed the wait out.
- A list of the windows on screen or a picture of them MUST be read so it does not wait either,
  however macOS offers it: the stacking order a flight reads as it starts, the picture of a window
  that has just opened, the desktop's and the bar's windows, the menu extras and the screen a
  handover measure pictures. macOS's own window list and window picture wait on rini's commits and
  take no choice of connection, so rini MUST NOT read through them.

> **Reported 2026-10-08.** Kai: "all the animations are _significantly_ slower with the rings". A
> 3s sample of rini held the main thread 528ms in its Core Animation commit,
> waiting for the lock the reactor held in a window query that waited for that same commit, and the
> flight engine's handover query queued behind them. That a rest window's frame change left the
> commit pending is inferred: outside rini, a window of the reader's process moving every 4ms
> stalled queries on the same connection 23 times in 6 runs, each run's worst 500.0 to 503.6ms,
> its frame changing alone stalled them and its alpha or order alone did not; on a connection of
> the reader's own, none stalled in 6, the worst 29.1ms. macOS's own window list, read the same way,
> stalled 41 times in 4 runs of 10s, the worst 501.7 to 504.3ms.

## Where it lives

`src/animation/domain/motion/` is how things move: the rigid pieces and their merge, the curves, the
depth bands, the edge bounce and the nudge; how a resizing tile is cropped, the ground it shows
where its picture ends and the edges it stands still against are
`src/animation/domain/motion/tile.rs`, a window's ground is read off its picture in
`src/pictures/domain/ground.rs`, and when a resized window has repainted is
`src/pictures/domain/settle.rs`. The looks
(`src/animation/domain/choreography.rs`, Slide the default in
`src/animation/domain/choreography/slide.rs`) compose a flight's score, fold a press before its motion
into it and bend it at a press after, and stage a press on the strip. `src/animation/platform/renderer.rs`
plays the score on Core Animation on the main thread, on the overlay in
`src/animation/platform/overlay.rs`, which lets go of the tiles and the desktop at the lift.
`src/decorations/domain/` is what each tile wears: the hairline's, the shadow's, the focus ring's and
the border's shape, style and depth, which the overlay draws on the layers in
`src/decorations/platform/layers.rs`. A window's corner radius is read in
`src/windows/platform/window_server.rs`, decoded in
`src/windows/domain/info.rs` and resolved, with the fallback and the clamp, in
`src/decorations/domain/shape.rs`. When each step happens is `src/transitions/`:
`src/transitions/domain/flight_score.rs` keeps a flight's score, `src/transitions/domain/strip_move.rs`
finds where a moved pair starts and puts the look's staging of it on the strip,
`src/transitions/domain/lifecycle.rs` decides when every step of a flight happens,
`src/transitions/domain/admission.rs` how a pass arriving mid-flight is admitted and how a window's
picture where it ends is waited for,
`src/transitions/domain/frame_writes.rs` which frames are written and in what order,
`src/transitions/domain/travel.rs` where each tile starts and ends, a parked window's along its place
on the strip, and whether a pass moves or resizes anything far enough to fly,
`src/transitions/domain/running.rs` what a flight composed, how a later pass, an entrance and each
picture change it and how its tiles band in depth, and `src/transitions/platform/engine.rs` is the
actor that carries them out, on a thread of its own,
which touches no Core Animation. The journal is kept with the
lifecycle in `src/transitions/domain/journal.rs` and written by `src/transitions/platform/journal.rs`;
what each line says is `src/transitions/docs/journal.md`, and `tools/focuslab/journal.py` reads it.
The main thread's turns are timed in `crates/rini-runloop/src/turns.rs`, from the observer in
`crates/rini-runloop/src/run_loop.rs` that `src/main.rs` installs.
A measured lift's handover is compared in `src/pictures/domain/handover.rs`.
Every read of windows that waits on Core Animation is made on the read connection in
`src/windows/platform/window_server.rs`; which reads wait, and the measurements, are in
`src/windows/docs/window-server-reads.md`.
The pictures a flight draws are `src/pictures/`: whether one covers and fits its window, the
stand-in, the blur, and the schedule that holds background capture off while a flight runs in
`src/pictures/domain/`; the captures in `src/pictures/platform/` (`src/pictures/platform/skylight.rs`
for the framed capture with its blur, `src/pictures/platform/screencapturekit.rs` for the background
captures and those of a window cut by the display's edge, a focus target's or a grow's repaint), and the service that makes them and keeps the cache, `src/pictures/platform/service.rs`.
Measurements are in `src/transitions/docs/coordination.md` (when a flight does what),
`src/animation/docs/animation-smoothness.md` (how it moves),
`src/animation/docs/capture-overlay-research.md` (the overlay, and "The handover, measured", which a
measured lift is sampled as) and
`src/pictures/docs/capture-research.md` (the captures).
