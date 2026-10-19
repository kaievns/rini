# The overview

Every workspace of one display at once, zoomed out, while its key is held.

> **Reported 2026-10-18.** "okay, now a new feature sort of like expose but better, basically a view
> of all workspaces (bind to alt-/ and active while im holding the alt-) this view pops in with zoom
> out (similar to the zoom navigation animation) and presents an overview of all windows thumbs,
> workspaces stacked up vertically and aligned vertically by the current window (the one that would
> pop up if i try alt-p/; between workspaces) if there are too many windows one side or another they
> should stack up at the end of the screen, sort of how they stack up in the cmd-tab app switcher.
> when i'm in that view the normal alt-u/i/o/p/j/k/l/; buttons and their alt-shift variants should
> work while im still holding the alt button. the navigation switches the way normal navigation
> switches up/down left/right, the orange focus ring should move and the left/right aimation should
> animate like normal slides (not zoom) navigation animation with the whole strip moving left right
> (overflows stack up/out as needed)"

## Opening and closing

- `toggle-overview` MUST open the overview, and it MUST stay open while the bind's modifiers are
  held: Kai asked for it "active while im holding". niri's toggles; a niri config's bind still
  opens it. Every `toggle-overview` bind MUST open it, held by that bind's modifiers: a bind the
  config accepts MUST NOT do nothing.
- Cmd+Caps, which Kai's keyboard layer sends as `Super+F19`, MUST open it held by Cmd as
  `Mod+Slash` does held by Alt: Kai asked for it in place of the switcher, and for the combination
  to stay his to move. Both bound at once, each MUST open it, held by its own modifiers. The keys
  inside it work only as the keyboard sends them under that hold ("Keys inside it", below).

> **Reported 2026-10-19.** "additionally i want to change the activation logic, and replace cmd-caps
> from the app switcher to the overview (keep the app switcher around unbinded with an option to
> bind it somwhere for those who wants it) cmd-caps activates the overview and it's active while i
> keep holding cmd, while im in the overview i can use normal u/i/o/p/j/k/l/; bindinings without
> meta- to navigate around and ,/. for folding unfolding, adding shift starts moving windows around
> as in non-overview mode. this will need to be configureable, if say i want to swap the cmobo back
> to meta-/ tomorrow it should keep working the same way, wile i'm holding the mod button it lets me
> navigate and move windows around in the button"

- A `toggle-overview` bind with no modifier MUST be refused with the reason: nothing could close
  it.
- Releasing the modifier MUST close it, and the release MUST be passed through, or every
  application believes the modifier is held.
- Escape MUST close it as the release does and change nothing more: every press inside already ran
  on the real layout, so there is nothing to cancel.
- The trigger pressed again while it is open, its autorepeat included, MUST be swallowed and change
  nothing; passed, a held key would close and reopen it.
- It MUST close when any key event, click or drag shows the modifier up, a mouse move too while the
  tap is asked for moves, as with focus-follows-mouse on, and when the event tap is replaced: a
  session that outlives its modifier swallows keys with nothing left to release it. A key that
  closes it MUST still go on as it would with the overview shut, but the trigger's key-up, swallowed
  as its press was. It MUST NOT close on a deadline while the modifier is held, however long: Kai
  asked for it "active while im holding", and a deadline would close it under his hand, his next
  press then flying on the real layout. The flags are a level, so the first event after a lost
  release closes it.
- It MUST open on the focused display only: one overlay flies on one display at a time. A press
  taking focus off that display MUST close it. The other displays MUST keep working while it is
  open, their windows placed as cuts, since the one overlay is the overview's.
- The open and the close MUST judge the focused display the same way, from the window the layout
  has focused: a key window the layout does not track, on another display, would open it there
  and close it in the same batch, so Alt+/ does nothing.

> **Found 2026-10-18, not reported.** Review of the reactor's wiring showed the open read the key
> window first and the close the layout's focus; not reproduced live, since it takes two displays
> and a key window the layout does not track.

- It MUST NOT open on a space rini does not manage, a native full-screen one included: there is no
  strip there to lay out. Refused or closed from rini's side, its key MUST stop being held for it,
  or the trigger stays swallowed with no overview open. That end MUST end only the session it was
  for: reaching the tap after the overview opened again, it would leave the tap holding nothing
  while rini still shows the new one, every flight held, with nothing but another Alt+/ to close
  it.

> **Found 2026-10-18, not reported.** Review of the reactor's wiring showed the end could cross the
> next open on its way to the tap and close the new session there.

## What it shows

- Every workspace of the display MUST be a row, in the stack's order, top to bottom, an empty one
  included: up and down step through every workspace, and the selection needs somewhere to land.
- Each row's current window MUST sit on one vertical axis at the tiling area's centre: Kai asked
  them aligned by that window, and the centre keeps the selection still while a row pans under it.
  A row's current window MUST be on its strip: the window a switch to that workspace focuses, or,
  where that one floats, the strip's own selection, which a focus press off the floating window
  returns to; on the shown workspace, the focused window while it is on the strip. A row with no
  current window MUST keep its strip where it shows.
- Every row MUST fit the area below the bar at a depth between 0.2 and 0.5, 12pt between rows and
  24pt at the edges; with more rows than fit at 0.2, the stack MUST scroll the least that keeps the
  selected row in view.

> **Reported 2026-10-19.** "reduce the gaps butween the workspaces a bit". The rows were 24pt apart
> on screen, as far as the stack is from the edges; at 12pt four workspaces on the laptop draw at
> depth 0.231 where they drew at 0.222.
- Columns past either edge MUST collapse into a stack at that edge: the nearest one on top, its
  outer edge 16pt in, each card behind 8pt further out and 12pt shorter at top and bottom, three
  cards at most, as the switcher stacks an application's windows, on their side. The current column
  MUST NOT stack. The thumb beside a stack MUST stay in its row and be drawn over the stack's top
  card, the current column among them, and the top card MUST move in from the edge where it would
  otherwise reach less than 24pt under that thumb: no gap may open between a row and its stacks.
  The top card MUST reach no further in than that thumb's inner edge, out past the display's edge
  if it must, and so move with that thumb as the row pans: a wide card beside a narrow thumb would
  otherwise show in the gap inside it, which is there to tell the windows apart.
  A card not drawn MUST NOT hold a picture.

> **Reported 2026-10-19.** "there are gaps between stacks on the sides and next thumbs, leth the
> strip thumbs overlap the stacks instead so there were no gaps". A column the top card overlapped
> joined the stack as its top card, leaving a gap as wide as that column between the row and the
> stack.

> **Found 2026-10-19, not reported.** Review of the stacks showed a full-width column stacked
> beside a half-width thumb at depth 0.3 on 1728pt reach 166pt past that thumb's inner edge, into
> the card's gap inside it.

- Two tiled thumbs side by side MUST be at least the card's gap apart, 14pt on screen, and two
  stacked in a column as far, each its window's place narrowed and shortened by that gap about its
  centre: at the overview's depth the strip's own gap is under a point, and neighbouring windows
  read as one. Kai asked for the horizontal gap; a stacked column's windows read as one slab too
  with less, 2.7pt for a window 518pt wide and 100pt tall with its proportions kept.

> **Reported 2026-10-19.** "add horisontal spacing between windows on the strip, otherwise they're
> luped together and hard to see which one which, for example the rainbow terminals on ws3 is a
> mess". Kai's 3pt gap is 0.9pt on screen at depth 0.3.

- The desktop behind the thumbs MUST be darkened and blurred while the overview is held, so it
  reads as a mode and its wallpaper is quieter under them: the card's backdrop, `--n0`, the colour
  the switcher darkens what is behind its popup with, at 0.60, and a Gaussian blur of 6pt, light
  enough to keep the shape of what is behind as the switcher's clear glass does. Both are first
  values for Kai to tune. The switcher's 0.40 darkens a glass that already quiets what is behind
  it; the overview's lies on the desktop itself. Each MUST rise with the zoom out from none at frame
  zero and fall with the zoom in to none at the lift, so neither hand-over to the real desktop
  jumps, and a flight MUST never be drawn darkened or blurred. The bar MUST NOT be darkened or
  blurred: it is drawn over every thumb, as at rest. The blur MUST NOT drop a frame of the zoom or
  hold its first frame back, so it is Core Animation's own Gaussian filter played on the desktop
  with the zoom: a `CIFilter` adds three times as much to a frame, and a picture blurred as it
  opens costs about 30ms before frame zero (`src/overview/docs/README.md`, "The desktop darkens and
  blurs on the score too").

> **Reported 2026-10-19.** "tint/darken the background image a bit to distinct that we're in the
> overview mode and so it would be less visually noisy".

> **Reported 2026-10-19.** "much better. make the background dimmer (blur maybe a little?) and add
> grey outlines to the inactive windows same way as the app switcher does". The desktop was
> darkened at the switcher's 0.40 and not blurred, and a thumb beside the selected one wore no ring
> and no edge but its window's own 1pt hairline, a third of a point on screen at depth 0.3.

- Every thumb MUST cast a shadow while the overview is held, so it floats over what is behind it,
  and the selected one a stronger one to accent it: the lift a carried window takes in a flight,
  all of it for the selected thumb and 0.4 of it for every other, as wide and falling as far on
  screen held as in a flight, its blur and its fall divided by the zoom. Frame zero and the landing
  MUST cast none, since the screen at rest has none: the shadows rise with the zoom out, fall with
  the zoom in, and move with the selection. Cast at its held size from the open, a shadow is drawn
  wider than that while the zoom is nearer the screen, where it is faint.

> **Reported 2026-10-19.** "add a bit of depth to the thumbs, like shadows or something so they'd
> appear a bit floating, more so for the currently selected window to accent it". Under the zoom a
> window's own shadow is a fifth of its size and reads as none.

- Its look MUST be the switcher's, from one record both draw from: the stacks' step, inset and three
  cards drawn, the gap between cards, the selected card's ring width, the colour that darkens what
  is behind, the card's faint edge, the card's 11pt corner and the `--ember-soft` wash under the
  selected card. Kai asked for one visual style across the two, reusing components where they apply;
  two copies of a value drift. Held, every thumb MUST round to the card's corner on screen at any
  depth (at its window's corner a thumb reads 3pt on screen for a 10pt corner at depth 0.3), or to
  the corner its picture keeps on screen where that is wider, since no clip rounds a picture tighter
  than its own corner (a 26pt window at depth 0.5 keeps 13pt), its ring and border round that
  corner, and the selected thumb MUST wear the card's selection as the switcher's selected card
  does: the wash under it 4pt out at a corner 3pt past the thumb's, 14pt on the card's, and its ring
  laid with its outer edge on the wash's. Through the zoom out and back in each MUST ease on screen
  from the window's own corner, ring and no wash to the card's with the zoom, the wash never
  reaching past the ring, so frame zero and the landing are the screen and neither hand-over to the
  real windows jumps.

> **Reported 2026-10-19.** "overall visual style must mutch the main app switcher, both should be
> consistent with each other and reuse components where applicable". The stacks' values were the
> overview's own copies of the switcher's.

- Held, every thumb but the selected one MUST wear the card's faint edge over its picture, as the
  switcher edges its cards: `--n11` at 0.16, 1pt wide on screen at any depth and through the zoom,
  round the thumb's corner, its cards in an edge stack included, so a dark window reads as a window
  against the darkened desktop. Kai asked for grey outlines on the inactive windows "same way as
  the app switcher does" (Reported 2026-10-19, above). The edge MUST rise with the zoom out from
  none at frame zero and fall with the zoom in to none at the landing, since the screen has none.
  The selected thumb MUST NOT wear it: it wears its ring and wash. An empty workspace's outline
  MUST keep the focus ring's paint, which is not the card's edge.

- A row with nothing on its strip, a workspace holding only floating windows included, MUST be drawn
  as its tiling area's outline in the focus ring's inactive paint, and in its active paint while
  selected.
- Only the windows tiled on a workspace's strip MUST be thumbs, on every row, the shown one's and
  the hidden ones' alike. A floating window MUST NOT be a thumb or in an edge stack, and neither
  MUST a window rini does not tile: a panel, a window rini leaves unmanaged, a window of an
  application it does not manage. Kai asked for the overview to hold the strip's windows only: a
  floating window over a row covers the thumbs under it and is on no axis a press moves along. The
  shown workspace's floating windows are drawn only as the screen has them, fading out as it zooms
  out and back in as it zooms in ("How it moves"), and not at all while it is held.

> **Reported 2026-10-19.** "oh and i forgot there is a zoom window on the overview and it shouldn't
> be there. the overview is only for the on-strip windows". Zoom is the video-call application. The
> overview drew every floating window on its row at its place on the display, over the tiles, never
> stacked and still while the row panned, as the 2026-10-18 report's "presents an overview of all
> windows thumbs" had been read.
- Thumbs MUST be the windows' pictures; one with none MUST be the stand-in until one lands, and
  MUST NOT hold the overview back. Pictures MUST keep being taken while it is open: every workspace
  of the display is asked to be pictured as it opens, nothing holds the pictures service once it
  has zoomed out, and each picture taken goes onto its thumb at once.
- Only the selected window MUST wear the active ring, and none as it closes on a floating window's
  focus ("How it moves"); every other window MUST be dressed as on a workspace not shown, as Kai
  reported of the strips beside on 2026-10-17.
- Every thumb's ring and border MUST be drawn as wide on screen as at rest, through the zoom as
  well as held, and an empty workspace's outline its ring: drawn at its width under the zoom, a 1pt
  ring is 0.22pt on screen at depth 0.22, and the ring Kai asked to see move does not read. The
  selected thumb's ring MUST be at least the card's ring, 2pt on screen, the width the switcher
  rings its selection with, in the ring's own paint, eased from its own width with the zoom.

> **Found 2026-10-18, not reported.** Drawing the overview showed Kai's 1pt focus ring under a
> fifth of a point at the depth four workspaces take on the laptop.

## Keys inside it

- A key pressed inside it MUST run the bind it has with `Mod` in place of the modifiers holding it
  open, every other modifier held kept: with the hold on Cmd, J alone runs `Mod+J`, Shift+J
  `Mod+Shift+J` and Comma `Mod+Comma`. With the hold on `Mod` itself the key MUST be looked up as
  pressed, sides and all, as outside: Alt+J runs `Mod+J`, and a bind on the right Alt alone fires
  on the right Alt key. Kai asked for the keys "without meta-" while Cmd holds it, and for a hold
  moved back to `Mod+Slash` to work the same (Reported 2026-10-19, above). A bind on the hold's
  modifiers and the key MUST NOT run inside: that chord is the hold and the key, so under a Cmd
  hold a `Super+J` bind is out of reach until it closes. Under a Cmd hold, a chord macOS answers in
  the window server, Cmd+Tab's application switcher, still reaches macOS, whatever its `Mod` bind
  does: an event tap cannot take it back.
- A key MUST be looked up as macOS delivers it, its key code and the modifiers its event carries:
  rini cannot see the physical key a keyboard layer sent it for. So the keys work inside only as
  the keyboard sends them under the hold. Kai's layer sends U, I, O, P, J, K, L, Semicolon, Comma
  and Period unchanged with Cmd held from the f home-row key, which raises his `cmd` layer. Held
  from the EDIT thumb, which raises his `edit` layer, the same keys send that layer's symbols: I
  runs `Mod+Minus` and K `Mod+Equal`, resizing the column, J `Mod+Shift+Comma`, exploding it, and
  U, O, P, L, Comma and Period no bind, so they are swallowed. Opened from that thumb, the overview
  needs his layer to send those keys unchanged until the thumb lifts.

> **Found 2026-10-19, not reported.** Review of Kai's keyboard layer against his binds showed the
> EDIT thumb's Cmd+Caps opening the overview with the edit layer still up; read off the layer's
> tables, not tried on the keyboard.
- Every bind MUST do what it does outside, on the real layout, and the overview MUST re-lay out from
  what it did: Kai asked for the same keys to work. What a press asks to fly MUST NOT fly while it
  is open, since the overview is what is seen to move; the real windows MUST go straight to where
  the flight would have put them, and the press MUST be kept for the overview to play. Every press
  of a batch MUST be kept, in order: autorepeat while rini is busy runs several in one, a step and
  then a push against the end, and the push alone would play a bounce where a pan and a bounce
  happened.

> **Found 2026-10-18, not reported.** Review of the reactor's wiring showed each held flight's
> press overwrote the one before it in the batch.
- A flight that was flying as it opened MUST NOT, landing, put back a window a press inside it has
  placed since: the press's frame is the later one.

> **Found 2026-10-18, not reported.** Wiring the overview showed it: a press flies, the overview
> opens within the flight's 0.35s, a press inside places a window again, and the first flight,
> landing, would put that window back where it was taking it.

- Opened with a floating window focused, it MUST move no focus: the selection starts on the shown
  row's current window, the strip's, and nothing changes until a press. A press of a layout action
  inside, every focus, move, fold, resize and workspace action among them, and a move to another
  display that names no window, MUST act on the strip from the selection: while a floating window
  holds focus, focus MUST go to the selected window first, raised as a focus press off a floating
  window raises it, and the press then runs from there. Run from the hidden floating window, a focus
  press would land on the selection the ring is already on, a press seen to do nothing, and a move
  press would move that window unseen. `switch-focus-between-floating-and-tiling` MUST NOT be
  handed off first: from a floating window it lands on the selection itself, and handed off it
  would turn focus back to the floating window, a press seen to do nothing. A press MUST NOT select
  a floating window: the selection is always the row's current window, so a press that focuses one
  leaves the selection on the strip. A row with nothing on its strip selects no window, so there a
  press runs from the floating window as it does outside. Not yet met: `close-window` closes the
  window macOS reports focused, so it closes the floating window, unseen.

> **Found 2026-10-19, not reported.** Taking floating windows out of the overview for Kai's report
> above showed it opened on a floating window's focus: by the layout's own rules, a focus press from
> that window lands on the strip's selection, which the ring was already on, and Kai's
> `Mod+Shift+K` moves that window to the workspace below. Read off the layout's rules and its tests,
> not tried live. Review of that change found Kai's `Mod+Ctrl+A` handed off first turned focus back
> to the floating window, and `Mod+M` sent the floating window to the other display, unseen.
- A key with no bind, looked up as above, MUST be swallowed, its key-up passed as a bound key's is.
  Passed, it reaches the hidden application with the hold still down: Cmd+Q would quit it and Cmd+W
  close its window, unseen. This cannot leave a dead keyboard, since a key whose flags show the hold
  up closes it first and goes on. A key rini has no name for passes, since no bind can name it.

> **Found 2026-10-19, not reported.** Moving the hold to Cmd for Kai's report above showed the old
> rule, that a key with no bind passes through untouched so a live session never becomes a dead
> keyboard, would hand Cmd+Q and Cmd+W to the hidden application. The swallow is safe because any
> event whose flags show the hold up closes the overview before it is read.
- A switcher's trigger MUST be swallowed while it is open, and the overview's while a switch is:
  one session is live at a time.
- Focus MUST NOT follow the mouse and the pointer MUST NOT be warped while it is open; the last warp
  asked for MUST be carried out as it closes. The windows under the pointer are hidden, and a warp
  per press moves the cursor across the overview. Turning focus-follows-mouse off in the event tap
  MUST NOT be how: a new event mask replaces the tap, which closes the overview.
- A click MUST NOT reach a hidden window: the overlay ignores the mouse, so a press of any button
  would land on the real window under the pointer, pressing what Kai cannot see and moving focus.
  Every button's press inside it MUST be swallowed, its drags and its release with it, after the
  close too; a press that began before it opened MUST finish as it began, so no application sees
  half a click. Choosing a thumb by clicking is not supported. A scroll still reaches the window
  under the pointer, since rini taps no trackpad gesture, a two-finger scroll among them
  (`specs/input.md`).

## How it moves

- It MUST zoom out from the screen as it is, under every look: frame zero draws every window of the
  shown workspace where it is on screen, its floating windows included, each in front of the strip
  or behind it as the window server stacks it. Each floating window MUST stay where the screen has
  it, outside the zoom, fading out as the desktop darkens until it is gone while held, and back in
  as the desktop lightens on the zoom in, so it is on screen at the lift as at the show, wearing
  what it wears there and never a thumb's card dress. It MUST NOT be moved, hidden or written to.
  Every open MUST draw afresh, after one the renderer had no overlay for as well: thumbs kept from
  that one would carry its pictures.

> **Found 2026-10-19, not reported.** Review of the change for Kai's 2026-10-19 report above found
> the floating windows drawn nowhere from the show to the lift. The overlay draws the desktop under
> its tiles, so it cut each out: a Zoom call focused as the overview opened vanished at the show and
> came back at the lift, the focused window popping in at the hand-off. Merging that with the
> thumbs' card dress showed the overlay dressing every tile it held, those floating windows too: the
> focused one, ringed active, faded out and in through the card's corner and the selection's wash.

> **Found 2026-10-18, not reported.** Review of the lifecycle showed an overview the renderer could
> not draw dropped its phase without letting go of its thumbs, which only its lift did.
- On close it MUST zoom back in onto the real layout, focus where the last press left it, the
  selected window on its workspace, and hand over to the real windows as a flight does: lifted only
  once the windows it lands on are where it draws them, or its patience has run out. Closed with a
  floating window still focused, no press having taken focus from it, that window MUST keep focus,
  and no thumb MUST wear the active ring as it zooms back in: the ring is that window's, which fades
  back in on it. A thumb ringed until the lift would hand the ring over as the overlay lifts.
- The zoom out, the zoom back in and every press inside it MUST run at the pace a focus press pans
  the strip: on the same curve, over the same length, 0.35s at slowdown 1. Kai asked them to match
  the navigation transitions; on half the zoom look's arc the zoom out started at rest and was 97%
  of the way there only at 0.229s, where a pan is at 0.175s.

> **Reported 2026-10-19.** "speed up the animation to match tha navigation transitions". The zoom
> ran half the zoom look's arc over 0.257s; it now runs as a pan does
> (`src/transitions/docs/coordination.md`, "The overview"). Its whole length grew with it, so the
> zoom back in, and the lift that hands over to the real windows, end 0.093s later than they did: a
> press right after the release waits for that lift and flies up to 0.093s later.
- A release while it zooms out MUST turn it back from where it is drawn, without a jump, each
  thing it draws as fast as it was going. Opened again while it zooms back in, it MUST zoom out
  again from where it is drawn: a quick let-go and press again would otherwise lift it and open it
  afresh. The overlay MUST be whole again as it does: the holes its close punched over the windows
  it lands on would show each at full size over the thumbs until the next lift. Opened again on
  another display, it MUST land and lift first and then open afresh there: zoomed out again, it
  would draw the other display's scene on this one's overlay, and nothing on the other display.

> **Found 2026-10-18, not reported.** Review of the drawing showed the zoom out again kept the holes
> the close had punched, the real windows and their bands showing through them.

> **Found 2026-10-18, not reported.** Review of the engine showed the turn back taken for an open on
> the other display, reachable with Kai's binds: Alt+I or Alt+K past the top or bottom row moves
> focus to the other display, which closes it, and Alt+/ again within the zoom back in.

- A press along a row MUST pan the whole row Slide's way under every look, the newly selected
  window arriving on the axis, columns going into and coming out of the edge stacks as they cross
  the edges. A move along the row MUST keep the moved window on the axis, the columns it passes
  crossing beneath it. A press while one plays MUST bend it from where everything is drawn, at the
  speed it is going, as a press merged into a flight does.
- A press up or down the stack MUST move the ring to that row's current window, the stack panning
  Slide's way when it scrolls. A step up or down within a column flies nothing, inside it or out,
  so there is no press to keep; the ring MUST still move to the window it reaches, the scene sliding
  there, since Kai asked to see the ring move.
- A press at an end of a row or of the stack MUST bounce it, 72pt on screen.
- With animation off or in Low Power Mode it MUST open, change and close as cuts.
- An overview opened while a flight runs MUST wait for that flight to lift, then zoom out from the
  screen it left. Not yet met: taking the flight's tiles from where they are drawn instead.
- A flight asked for while it holds the overlay, from its open to its lift, MUST wait for its lift
  and then fly as asked, and so MUST a move of the overlay to another display: the one overlay is
  the overview's until then.

> **Found 2026-10-18, not reported.** Wiring the drawing showed that a press right after the release
> would fly its flight on the overlay the zoom back in is still drawn on.

## Where it lives

`src/overview/domain/` is the scene: `scene.rs` lays it out, `depth.rs` sets its depth and its
rows' tops, `stacks.rs` its edge stacks, and `changes.rs` finds what changed between two scenes.
The card it shares with the switcher is `src/chrome/domain/card.rs`.
A row's current window on its strip is `on_strip` in `src/workspaces/domain/workspace_focus.rs`.
`src/input/domain/switch_session.rs` is the tap's side, the hold that opens and closes it and the
chord a key inside it is looked up as, `src/input/domain/router.rs` the lookup, and
`src/app/reactor/overview.rs` the reactor's: the scene's input read off the layout and handed to the
flight engine, focus taken off a floating window ahead of a press, the flights held and their frames
placed, the warps held, and the close. How it moves is
`src/animation/domain/choreography/overview.rs`, the thumbs' card dress
`src/animation/domain/choreography/overview/card.rs`, the desktop's tint and blur and the floating
windows' fade `src/animation/platform/overlay/tint.rs`, an empty workspace's outline
`src/animation/platform/overlay/outline.rs`, its lifecycle on the overlay
`src/transitions/domain/lifecycle/overview.rs`, and the engine's side of it
`src/transitions/platform/engine/overview.rs`. `src/overview/docs/README.md` explains the feature.
