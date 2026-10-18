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
- A `toggle-overview` bind with no modifier MUST be refused with the reason: nothing could close
  it.
- Releasing the modifier MUST close it, and the release MUST be passed through, or every
  application believes the modifier is held.
- Escape MUST close it as the release does and change nothing more: every press inside already ran
  on the real layout, so there is nothing to cancel.
- The trigger pressed again while it is open, its autorepeat included, MUST be swallowed and change
  nothing; passed, a held key would close and reopen it.
- It MUST close when any key or mouse event shows the modifier up, and when the event tap is
  replaced: a session that outlives its modifier swallows keys with nothing left to release it. A
  key that closes it MUST still go on as it would with the overview shut, but the trigger's key-up,
  swallowed as its press was. It MUST NOT close on a deadline while the modifier is held, however
  long: Kai asked for it "active while im holding", and a deadline would close it under his hand,
  his next press then flying on the real layout. The flags are a level, so the first event after
  a lost release closes it.
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
- Each row's current window, the one a switch to that workspace focuses, MUST sit on one vertical
  axis at the tiling area's centre: Kai asked them aligned by that window, and the centre keeps the
  selection still while a row pans under it. A row whose current window floats, or that has none,
  MUST keep its strip where it shows, since no tiled window is the one to align.
- Every row MUST fit the area below the bar at a depth between 0.2 and 0.5, 24pt between rows and
  at the edges; with more rows than fit at 0.2, the stack MUST scroll the least that keeps the
  selected row in view. First values for Kai to tune.
- Columns past either edge MUST collapse into a stack at that edge: the nearest one on top, its
  outer edge 16pt in, each card behind 8pt further out and 12pt shorter at top and bottom, three
  cards at most, as the switcher stacks an application's windows, on their side. A column the top
  card overlaps MUST join the stack as its top card. The current column MUST NOT stack, and the top
  card MUST NOT cover it: it MUST stay the row's gap clear of the current column, partly past the
  edge where its 16pt inset would bring it nearer. A card not drawn MUST NOT hold a picture.
- An empty row MUST be drawn as its tiling area's outline in the focus ring's inactive paint, and in
  its active paint while selected.
- Floating windows MUST be drawn on their row at their place on the display, over its tiles, never
  stacked, and MUST NOT move while the row pans.
- Thumbs MUST be the windows' pictures; one with none MUST be the stand-in until one lands, and
  MUST NOT hold the overview back. Pictures MUST keep being taken while it is open: every workspace
  of the display is asked to be pictured as it opens, nothing holds the pictures service once it
  has zoomed out, and each picture taken goes onto its thumb at once.
- Only the selected window MUST wear the active ring; every other window MUST be dressed as on a
  workspace not shown, as Kai reported of the strips beside on 2026-10-17.
- Every thumb's ring and border MUST be drawn as wide on screen as at rest: drawn at its width
  under the zoom, a 1pt ring is 0.22pt on screen at depth 0.22, and the ring Kai asked to see move
  does not read. Not yet met: while it zooms out and back in they are drawn at their own width,
  which the zoom thins; from the end of the zoom out to the start of the zoom in they are as wide
  as at rest.

> **Found 2026-10-18, not reported.** Drawing the overview showed Kai's 1pt focus ring under a
> fifth of a point at the depth four workspaces take on the laptop.

## Keys inside it

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

- A key with no bind MUST pass through untouched: swallowing everything is how a live session
  becomes a dead keyboard. The cost: Alt and a letter reach the hidden application.
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

- It MUST zoom out from the screen as it is, on the first half of the zoom look's arc, under every
  look: frame zero draws every window where it is on screen. Every open MUST draw afresh, after one
  the renderer had no overlay for as well: thumbs kept from that one would carry its pictures and
  its widened bands, a 1pt ring drawn near 4.5pt wide.

> **Found 2026-10-18, not reported.** Review of the lifecycle showed an overview the renderer could
> not draw dropped its phase without letting go of its thumbs, which only its lift did.
- On close it MUST zoom back in, on the arc's second half, onto the real layout, the selected window
  focused on its workspace, and hand over to the real windows as a flight does: lifted only once
  the windows it lands on are where it draws them, or its patience has run out.
- A release while it zooms out MUST turn it back from where it is drawn, without a jump, each
  thing it draws as fast as it was going. Opened again while it zooms back in, it MUST zoom out
  again from where it is drawn: a quick let-go and press again would otherwise lift it and open it
  afresh. The overlay MUST be whole again as it does: the holes its close punched over the windows
  it lands on would show each at full size over the thumbs until the next lift.

> **Found 2026-10-18, not reported.** Review of the drawing showed the zoom out again kept the holes
> the close had punched, the real windows and their bands showing through them.

- A press along a row MUST pan the whole row Slide's way under every look, the newly selected
  window arriving on the axis, columns going into and coming out of the edge stacks as they cross
  the edges. A move along the row MUST keep the moved window on the axis, the columns it passes
  crossing beneath it. A press while one plays MUST bend it from where everything is drawn, at the
  speed it is going, as a press merged into a flight does.
- A press up or down the stack MUST move the ring to that row's current window, the stack panning
  Slide's way when it scrolls.
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
`src/input/domain/switch_session.rs` is the tap's side, the hold that opens and closes it, and
`src/app/reactor/overview.rs` the reactor's: the scene's input read off the layout and handed to the
flight engine, the flights held and their frames placed, the warps held, and the close. How it moves
is `src/animation/domain/choreography/overview.rs`, an empty workspace's outline
`src/animation/platform/overlay/outline.rs`, its lifecycle on the overlay
`src/transitions/domain/lifecycle/overview.rs`, and the engine's side of it
`src/transitions/platform/engine/overview.rs`. `src/overview/docs/README.md` explains the feature.
