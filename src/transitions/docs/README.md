# `transitions` — when things happen in a transition

A transition is one change drawn on the overlay: the strip panning or a window moving along it, a
layout pass, a workspace switch, the edge bounce. This feature decides when each step of one happens:
frame zero, the overlay up, the real frames written, pictures swapped in flight, a press merged into
the flight running, the lift and the quiet after it. How things move is `animation`'s, and how the
pictures are obtained is `pictures`'. The plan this feature is part of is
`docs/pictures-motion-transitions.md`.

## What it owns

| | |
|---|---|
| `domain/lifecycle.rs` | `Coordinator`: the lifecycle every kind runs through, as a pure state machine. `on(event, now)` returns the commands the shell carries out; generations tag every picture it asks for |

## The shape that matters

**One lifecycle for every kind.** A kind says what it stages, which pictures frame zero needs, whether
it writes frames and what the lift must match; the coordinator sees a `StageSummary` and decides only
when.

**Every event carries its instant.** Nothing in `domain/` reads a clock, so a lifecycle is a test of
events in and commands out.

## Reading order

`domain/lifecycle.rs`: `Kind`, then `StageSummary`, then `Coordinator::on`.

## Detail

- `docs/pictures-motion-transitions.md` — the three features, the lifecycle's states, and the steps
  still to come

## Known debt

**The coordinator is not wired.** Every transition still runs through the flight engine in
`animation`, which keeps its lifecycle in its own fields. Step 3 of the plan moves the engine and its
decisions here; step 4 replaces its lifecycle with the coordinator.
