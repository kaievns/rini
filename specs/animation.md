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
