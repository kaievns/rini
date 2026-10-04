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
| **The port** | `domain/request.rs` — `AnimationRequest`, one window of a layout pass, `SurfaceWindow`, one window of a strip movement, and `Cause`, the press a request answers; what the reactor builds and the engine carries out |
| **A layout pass, sorted** | `domain/pass.rs` — `plan`: which windows move, which stay and are still drawn, which are warmed |
| **Where a tile starts and ends** | `domain/travel.rs` — the window server's frame against the request's, a park's way in and out by its neighbours' travel, what is worth flying |
| **A move along the strip, staged** | `domain/strip_move.rs` — where the two columns changing places start (`swap_starts`), and the look's staging of the press put on the strip surface (`stage_move`); which column is held and whether the strip steps are the look's |
| **A switch's stack** | `domain/strip_stack.rs` — the stacked workspace strips a vertical switch scrolls through |
| **Arriving mid-flight** | `domain/admission.rs` — a pass merging into a running flight, reserved entrances, how an opened window enters, and how each window's picture where it ends is waited for (`EndPicture`) |
| **A flight's score** | `domain/flight_score.rs` — `FlightScore`: the score the flight's look composes and bends, every edit it hands the renderer folded into it first, the accents waiting for the motion, and the clock and the lift's settle read from it |
| **What a flight is** | `domain/flight.rs` — border companions, the handover report, when a pass is worth an overlay |
| **When** | `domain/timing.rs` — the tick, the coalesce window, the reveal hold, the lift's grace and the quiet after it |
| **The writes** | `domain/frame_writes.rs` — which final frames go out to the apps, and in what order |
| **The lifecycle** | `domain/lifecycle.rs` — `Coordinator`: the lifecycle every kind runs through, as a pure state machine. `on(event, now)` returns the commands the engine carries out; generations tag every picture it asks for |
| **What waits for the screen** | `domain/drawn.rs` — `Drawn`: the batches of render commands handed to the renderer by number, the last it committed, and what waits for one (`Owed`): the real frames for the `Show`'s, the bar's picture and the quiet for the `Lift`'s, by the flight that owes it; a new flight's hold drops the last lift's, an undrawn batch releases what waited for it, and an abandoned flight's own are dropped |
| **The journal** | `domain/journal.rs` — `Journalled`: the coordinator with its journal, the press each request answers, every command it gives, every event it takes but a picture and frame zero, a tick only by the first `at_rest` and `landed` it reads in a generation, every delivery of pictures to it, every drain the renderer commits and each held lift's hide and handover a mark in a ring of 8192, and the burst due once a transition settles; the JSON lines a burst is written as; and which lifts are held for the overlay's own picture (`measures`) and when one hides (`Journalled::heard`) |
| **The engine** | `platform/engine.rs` — `FlightEngine`, the actor on the `transitions` thread, made there from `Parts`: it holds the coordinator with its journal (`Journalled`), stages each request and merges each pass, and carries out what the coordinator says on the renderer, each event's render commands one batch sent through `Draw` and its commit heard as `Event::Rendered`, and on the pictures service, placing the real windows through the reactor once the overlay covering them is drawn; every instant it reads is its clock's, and every track begins on the renderer's media clock; while the handover is measured, it holds each lift's hide for the overlay's own picture until the journal says it hides, and the bar's picture and the quiet with it |
| **The journal's writer** | `platform/journal.rs` — `Writer`: the `journal` thread, which appends each burst to `~/Library/Logs/rini/transitions.jsonl` and starts it afresh past 50MB |
| **Settings** | `settings.rs` — `JournalSettings`: whether the journal is written and the handover measured (`animations › journal`) |

Its interface and `domain/` speak rini-geometry's spaces (`crates/rini-geometry/docs/README.md`):
requests, travel, the frame writes and what the coordinator hears and asks for in `Screen`, a strip
movement's windows and viewport offsets in `Surface`, and an entrance's destination, the pan, and
the coordinator's holes and snaps in `Overlay`. The reactor converts at the door, and CG stays in
`platform/`, at the window server's reads.

## The shape that matters

**One actor, one flight at a time.** Every transition goes through `FlightEngine`. A pass arriving
while a flight runs merges into it instead of starting another, which is how a burst of presses reads
as one movement.

**The reactor asks, and hears two things back.** It sends a layout pass (`Event::Animate`), a strip
movement (`Event::AnimateSurface`), each naming the window focused as it began and the key window as
it went out, a bounce, each of the three carrying its press (`Cause`: when the reactor took the event
it answers, and what that was), the windows whose look each change of the key window turned
(`Event::LooksTurned`), the display the overlay flies on, and whether the journal is written and each
lift's handover measured (`Event::Journal`). A pass, a movement and a bounce each name
the `Look` a flight they begin moves by, and a movement and a bounce the press (`Cue`): the engine
hands both to the look and matches on neither. The pictures service's sink, wired in
`src/main.rs`, tells the engine what it delivered under the engine's tag, which watched windows'
pictures will not come, and a handover check's overlay picture taken and its measure. The engine answers through
what `src/main.rs` hands it in `Parts`: the frames to write (`PlaceFrames`, back to the reactor),
whether a flight is up (`OnFlight`, to the bar), and each batch to draw (`Draw`, to the renderer,
whose answer comes back as `Event::Rendered`). The reactor never learns more of a flight than the
frames.

**The decisions are pure and the actor is not.** What `domain/` decides is tested over plain rects
and instants; the engine reads the window server and the clock it is handed, and holds what each
flight composed between events. Its messages are tested as sent (`protocol` in `platform/engine.rs`):
over a pictures service with nothing behind it, a clock the test advances, and the renderer's
answers given by the test.

**The journal observes, and nothing reads it back.** Every command the coordinator gives and every
event it takes is a mark pushed by `Journalled::drive`, but a picture and frame zero, which their
delivery marks, and a tick, marked only by the first `at_rest` and `landed` it reads in a
generation; so is each request's press, by the request's first drive. Every delivery under the
engine's tag is a mark pushed by `Journalled::delivery`, with when the pictures service sent it;
every commit the renderer reports, by `Journalled::drawn`, with when its batch was sent, played and
flushed, before what it releases is carried out; and a held lift's hide and its handover are pushed
by `Journalled::heard` and `Journalled::handover`; no mark is pushed anywhere else. The engine
branches on nothing the journal holds, and only hands each burst it gives to the writer, once a
transition has settled. The one thing measuring changes is when a lift's overlay comes down, and the
bar's picture and the quiet with it: with `measure-handover` on, a lift with a window ending on
screen is held, up to 60ms, for the overlay's own picture, and `Journalled::heard` says when it
hides: on that picture, a request, the overlay about to move, or the patience. What each mark says
and when it is written: [`journal.md`](journal.md).

**One lifecycle for every kind.** `domain/lifecycle.rs` is the lifecycle as a state machine: a kind
says what it stages, which pictures frame zero needs, whether it writes frames and what the lift must
match; the coordinator sees a `StageSummary` and decides only when. Every event carries its instant, so
a lifecycle is a test of events in and commands out. The engine tells it each request before
composing it, so frame zero's pictures are asked for first, and each pass after; its frame clock ticks
from the begin to the lift.

## Reading order

`domain/request.rs` → `domain/pass.rs` → `domain/travel.rs` → `domain/admission.rs` →
`domain/flight.rs` → `domain/flight_score.rs` → `domain/timing.rs` → `domain/lifecycle.rs` (`Kind`,
then `StageSummary`, then `Coordinator::on`) → `domain/journal.rs` (`Journalled::drive`) →
`domain/drawn.rs` → `platform/engine.rs`, the actor and the largest file → `platform/journal.rs`.

## Detail

- `docs/pictures-motion-transitions.md` — the three features, the lifecycle's states, and the steps
  still to come
- [`coordination.md`](coordination.md) — how a flight runs: the lifecycle, layout changes and strip
  movements, routing, passes arriving mid-flight and pictures landing in one, focus changing during
  one, the capture work in flight, entrances and holds, the handover and the lift
- [`journal.md`](journal.md) — the journal: what each mark says, the press, the line format, the
  clock, the ring, when it is written, and how `tools/focuslab/journal.py` reads it
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

**A panel's keys are seen only as far as rini hears of them.** The window focus leaves is taken to
look focused at frame zero when it was the key window as the request went out, and the key window is
the window server's report while the application in front owns it. A panel of another application
that takes the keys is seen only if the report names it, and the report names the key-focus process's
frontmost document window, or if its application comes to the front: neither is measured for
Spotlight or Raycast. Unseen, the window under it is still taken as key and its frame-zero picture
kept as its focused look. Seen, its tile is still drawn focused until focus hands over, ring and shadow
over an unfocused picture.

**The kinds' builders live in the reactor.** `start_strip_pan`, `start_strip_switch`,
`start_edge_bounce` and `animate_layout` (`src/app/reactor/`) stage each kind with this feature's
`domain/` and send it to the engine; they move here when the coordinator composes flights.
