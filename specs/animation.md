# What moves

Animation is cosmetic and MUST NOT be load-bearing. A build with animation off behaves identically;
only the transitions are missing.

## What animates

- A window that changes position or size in a layout pass.
- A window opening, from the frame macOS first showed it at to its slot.
- The strip scrolling, and a workspace switch.

## What does not

- A window that has not moved MUST NOT be re-placed. The round trip invites another layout pass.
- A window whose whole movement is off screen MUST NOT be drawn. A layer and a capture for something
  nobody can see is pure cost.
- A column parked off the strip MUST NOT be raised. Nothing of it is visible, each raise is an
  Accessibility round-trip, and a raise issued after the on-screen windows puts a 1pt sliver in front
  of them.

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

> **Reported 2026-09-24.** "The little bounce animation when navigation reaches an end of strip or
> workspaces stack is neat but a bit too small, I need a little more swing. Also when I'm moving a window
> in a strip or between workspaces it needs similar animations, because otherwise there is no visual cue
> and it feels like a bug/stuck." The overshoot was 36pt; moving reported no edge at all, so it never
> reached the bounce.

## Arriving mid-flight

The layout does not wait for an animation to land, so a second pass can arrive with a different
destination for a window already moving.

- A pass repeating a destination MUST be treated as redundant, not as a new flight. Otherwise a held
  key restarts the animation on every repeat and it never lands.
- A pass with a new destination for a moving window MUST bend that window toward it rather than
  restarting.
- A window the flight has not seen MUST be able to join it.

## A window that has just opened

- It travels from the frame the window server reports to its slot, if there is a usable picture of it
  and the capture budget allows.
- Otherwise the flight holds a place for it and waits for its first picture. Each reason for holding
  MUST be recorded distinctly: "the window appeared without animating" has four causes and the log is
  the only way to tell them apart afterwards.

## Correctness the user can see

- The frame written to a window is the layout's answer, never the animation's. A window parked off the
  strip is written its real park position; the tile is merely drawn heading off the edge.
- A window MUST NOT be left at an animation's intermediate frame if a pass is interrupted.
- The overlay MUST stop drawing a tile whose window is gone, and MUST drop a container once the last
  tile leaves it. An empty container is a layer the compositor keeps compositing.

## Where it lives

`src/animation/domain/motion/` is the planning, `src/animation/domain/admission.rs` the mid-flight
rules, `src/animation/platform/engine.rs` and `overlay.rs` the Core Animation side. Measurements are in
`src/animation/docs/animation-smoothness.md` and `capture-overlay-research.md`.

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
- A picture MUST NOT be built from a capture of the window's rect taken while the window was somewhere
  else. The rect then holds whatever the window was covering, and merging that in replaces the window
  with another one — found on the first live run, and far worse than grey.

> **Reported 2026-09-28.** "Screenshots for apps that use blur... currently they render with some grey
> colour and cause flicker when the real window switches in... there are a lot of windows that use blur
> here and there now, so it's quite prevalent and needs a better fix." Every per-window capture route
> had already been measured returning the same flat grey. The window captured together with what is
> below it carries the real blur and leaves out anything above it, measured the same day.

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
