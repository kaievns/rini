# `transitions` — when things happen in a transition

A transition is one change drawn on the overlay: the strip panning or a window moving along it, a
layout pass, a workspace switch, the edge bounce. This feature decides when each step of one happens:
frame zero, the overlay up, the real frames written, pictures swapped in flight, a press merged into
the flight running, the lift and the quiet after it. How things move and the renderer that plays them
are `animation`'s; how the pictures are obtained is `pictures`'. The reactor drives it. The plan this
feature is part of is `docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| **The port** | `domain/request.rs` — `AnimationRequest`, one window of a layout pass, and `SurfaceWindow`, one window of a strip movement; what the reactor builds and the engine carries out |
| **A layout pass, sorted** | `domain/pass.rs` — `plan`: which windows move, which stay and are still drawn, which are warmed |
| **Where a tile starts and ends** | `domain/travel.rs` — the window server's frame against the request's, a park's way in and out by its neighbours' travel, what is worth flying |
| **A move along the strip, staged** | `domain/strip_move.rs` — where the two columns changing places start, whether the move steps the whole strip, and the column such a move holds on it |
| **A switch's stack** | `domain/strip_stack.rs` — the stacked workspace strips a vertical switch scrolls through |
| **Arriving mid-flight** | `domain/admission.rs` — a pass merging into a running flight, reserved entrances, the frames a coalescing merge sends again, how an opened window enters, what frame zero sends and chases, a move's nudge waiting for its flight to move, and the bounce a push against an end starts |
| **What a flight is** | `domain/flight.rs` — when a picture landing in flight is swapped in, the focus refresh, border companions, the handover report, when a pass is worth an overlay, the flight report |
| **When** | `domain/timing.rs` — the tick, the coalesce window, the apply points, the reveal hold, the lift and the quiet after it |
| **The writes** | `domain/frame_writes.rs` — which final frames go out to the apps, and in what order |
| **The lifecycle** | `domain/lifecycle.rs` — `Coordinator`: the lifecycle every kind runs through, as a pure state machine. `on(event, now)` returns the commands the shell carries out; generations tag every picture it asks for. Not yet wired |
| **The engine** | `platform/engine.rs` — `FlightEngine`, the actor on the main thread: it composes each flight, keeps it running, places the real windows through the reactor and lifts the overlay; the renderer and the pictures service do the drawing and the capturing |

## The shape that matters

**One actor, one flight at a time.** Every transition goes through `FlightEngine`. A pass arriving
while a flight runs merges into it instead of starting another, which is how a burst of presses reads
as one movement.

**The reactor asks, and hears two things back.** It sends a layout pass (`Event::Animate`), a strip
movement (`Event::AnimateSurface`), a bounce, and the display the overlay flies on; the pictures
service's sink, wired in `src/main.rs`, tells the engine a picture landed. The engine answers through
the callbacks `src/main.rs` installs: the frames to write (`PlaceFrames`, back to the reactor) and
whether a flight is up (`OnFlight`, to the bar). The reactor never learns more of a flight than that.

**The decisions are pure and the actor is not.** What `domain/` decides is tested over plain rects
and instants; the engine reads the window server and the clock, and holds the state between events.

**One lifecycle for every kind, still to come.** `domain/lifecycle.rs` is the lifecycle as a state
machine: a kind says what it stages, which pictures frame zero needs, whether it writes frames and what
the lift must match; the coordinator sees a `StageSummary` and decides only when. Every event carries
its instant, so a lifecycle is a test of events in and commands out. Nothing drives it yet.

## Reading order

`domain/request.rs` → `domain/pass.rs` → `domain/travel.rs` → `domain/admission.rs` →
`domain/flight.rs` → `domain/timing.rs` → `platform/engine.rs`, the actor and the largest file.
`domain/lifecycle.rs` is what replaces the engine's lifecycle: `Kind`, then `StageSummary`, then
`Coordinator::on`.

## Detail

- `docs/pictures-motion-transitions.md` — the three features, the lifecycle's states, and the steps
  still to come
- [`coordination.md`](coordination.md) — how a flight runs: layout changes and strip movements,
  routing, passes arriving mid-flight and pictures landing in one, the capture work in flight, the
  apply point, entrances and holds, the handover and the lift
- [`src/animation/docs/animation-smoothness.md`](../../animation/docs/animation-smoothness.md) — how
  a flight moves once it runs

## Known debt

**The engine keeps its lifecycle in its own fields.** `RunningAnimation` and `FlightEngine` hold the
phase as `started`, `awaiting` and a quiet timer, and no flight carries a generation, so an event an
old timer posted acts on whichever flight is current. Step 4 replaces them with the `Coordinator`.

**Some pure rules are still the engine's.** The flight's clock, when its frames are due, how a later
pass extends its hold or merges into its tiles, what a held flight claims, how a late entrance joins,
what a landing picture may change, how tiles band, a border's tile and which stand-ins are chased are
methods on `RunningAnimation` and functions over `OverlayTile`, and both carry a platform `Picture`.
They and their tests (`platform/engine.rs`, grouped by unit) move to `domain/` once a tile holds a
picture handle instead.

**The kinds' builders live in the reactor.** `start_strip_pan`, `start_strip_switch`,
`start_edge_bounce` and `animate_layout` (`src/app/reactor/`) stage each kind with this feature's
`domain/` and send it to the engine; they move here when the coordinator composes flights.
