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
| **Arriving mid-flight** | `domain/admission.rs` — a pass merging into a running flight, reserved entrances, how an opened window enters, how each window's picture where it ends is waited for (`EndPicture`), a move's nudge waiting for its flight to move, and the bounce drawn, which a push against an end starts the next from |
| **What a flight is** | `domain/flight.rs` — border companions, the handover report, when a pass is worth an overlay |
| **When** | `domain/timing.rs` — the tick, the coalesce window, the reveal hold, the lift's grace and the quiet after it |
| **The writes** | `domain/frame_writes.rs` — which final frames go out to the apps, and in what order |
| **The lifecycle** | `domain/lifecycle.rs` — `Coordinator`: the lifecycle every kind runs through, as a pure state machine. `on(event, now)` returns the commands the engine carries out; generations tag every picture it asks for |
| **The engine** | `platform/engine.rs` — `FlightEngine`, the actor on the main thread: it holds the coordinator, stages each request and merges each pass, and carries out what the coordinator says on the renderer and the pictures service, placing the real windows through the reactor |

## The shape that matters

**One actor, one flight at a time.** Every transition goes through `FlightEngine`. A pass arriving
while a flight runs merges into it instead of starting another, which is how a burst of presses reads
as one movement.

**The reactor asks, and hears two things back.** It sends a layout pass (`Event::Animate`), a strip
movement (`Event::AnimateSurface`), each naming the window focused as it began, a bounce, and the
display the overlay flies on; the pictures service's sink, wired in `src/main.rs`, tells the engine
what it delivered under the engine's tag. The engine answers through
the callbacks `src/main.rs` installs: the frames to write (`PlaceFrames`, back to the reactor) and
whether a flight is up (`OnFlight`, to the bar). The reactor never learns more of a flight than that.

**The decisions are pure and the actor is not.** What `domain/` decides is tested over plain rects
and instants; the engine reads the window server and the clock, and holds what each flight composed
between events.

**One lifecycle for every kind.** `domain/lifecycle.rs` is the lifecycle as a state machine: a kind
says what it stages, which pictures frame zero needs, whether it writes frames and what the lift must
match; the coordinator sees a `StageSummary` and decides only when. Every event carries its instant, so
a lifecycle is a test of events in and commands out. The engine tells it each request before
composing it, so frame zero's pictures are asked for first, and each pass after; its frame clock ticks
from the begin to the lift.

## Reading order

`domain/request.rs` → `domain/pass.rs` → `domain/travel.rs` → `domain/admission.rs` →
`domain/flight.rs` → `domain/timing.rs` → `domain/lifecycle.rs` (`Kind`, then `StageSummary`, then
`Coordinator::on`) → `platform/engine.rs`, the actor and the largest file.

## Detail

- `docs/pictures-motion-transitions.md` — the three features, the lifecycle's states, and the steps
  still to come
- [`coordination.md`](coordination.md) — how a flight runs: the lifecycle, layout changes and strip
  movements, routing, passes arriving mid-flight and pictures landing in one, focus changing during
  one, the capture work in flight, entrances and holds, the handover and the lift
- [`src/animation/docs/animation-smoothness.md`](../../animation/docs/animation-smoothness.md) — how
  a flight moves once it runs

## Known debt

**Some pure rules are still the engine's.** How a later pass merges into a flight's tiles, how a
reserved entrance joins, what frame zero's pictures and a focus handover change, which tiles the
coordinator is told of, how tiles band, a border's tile, which baked picture is clipped to its corners
and which focused look a parked target flies are methods on `RunningAnimation` and functions over
`OverlayTile` and `Picture`, and both carry a platform `Picture`. They and their tests
(`platform/engine.rs`, grouped by unit) move to `domain/` once a tile holds a picture handle instead.
`summary`, which assembles the coordinator's `StageSummary` from them and from what the pictures
service knows, is the actor's glue rather than a rule.

**The kinds' builders live in the reactor.** `start_strip_pan`, `start_strip_switch`,
`start_edge_bounce` and `animate_layout` (`src/app/reactor/`) stage each kind with this feature's
`domain/` and send it to the engine; they move here when the coordinator composes flights.
