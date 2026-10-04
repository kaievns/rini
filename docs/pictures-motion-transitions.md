# Pictures, motion, transitions

How rini draws a window moving: three features, each with one job. Agreed with Kai on 2026-10-03;
the measurements it rests on are in `src/animation/docs/capture-overlay-research.md`
("Where a moving focus change spends its time", "The handover, measured") and
`src/pictures/docs/capture-research.md` ("What a capture costs in a press").

## Progress

This section goes when the last row is done.

| # | Step | State |
|---|---|---|
| 1 | `pictures`: capture, cache and layer helpers out of `animation`; switcher and bar its clients | done |
| 2 | The pictures service: the capture schedule and its thread, the cache and the ScreenCaptureKit captures moved in; the engine, the reactor and the switcher its clients | done |
| 3 | `transitions`: the coordinator's decisions out of `animation`; the lifecycle as a pure state machine | done |
| 4 | The prototype's sequencing: frame zero, end-state watches, generations, holes, constant hairline, focused looks, live background; region pictures, batched SkyLight with per-window knowledge; the mid-flight refresh and the harvest deleted | done |
| 5 | `animation` plays a `Score`; choreographies and accents produce one | done |
| 6 | Typed coordinate spaces in `rini-geometry` for the three features' interfaces | done |
| 7 | The coordinator on its own thread; the main thread renders only | |
| 8 | The transitions journal | done |

## Three concerns, three owners

| Feature | Owns | Never touches |
|---|---|---|
| **`pictures`** | every capture of a window, the desktop or the bar; the cache; what is known about each window (translucency, material, corner radius, its focused look); the capture schedule and its thread | layouts, the overlay's layer tree (it only sets a layer's contents), when a transition does what |
| **`animation`** | how things move: the `Score`, choreographies that compose one, accents (the edge bounce, a move's nudge), and the renderer that plays a score on Core Animation | captures, window frames, focus |
| **`transitions`** | when things happen: frame zero, the overlay up, frame writes, picture swaps in flight, merged presses, the handover and the lift | how pixels are obtained, what a curve looks like |

`pictures` is upstream of both: the switcher, the bar and a future exposé are its clients too.
`animation` knows ids, geometry and `windows`' stacking group only. `transitions` uses both, and
the reactor drives it.

They are features, not crates: `docs/architecture.md` ("Why this shape and not one crate per
feature") records why, and `tests/architecture.rs` holds each one's `domain/` free of macOS.

## Who talks to whom

```
             reactor ──Request──▶ transitions coordinator ──Write / Raise──▶ reactor
   app actors ──Placed──▶            │  ▲                 ──Stage / Play / Swap / Lift──▶ animation renderer (main thread)
   window server ──Focused──▶        │  │ pictures(tag)
                              ask /  ▼  │
                              watch  pictures service (own thread: one SkyLight call at a time)
                                        ▲          ▲
                              switcher ─┘          └─ bar, exposé
```

The main thread runs the renderer and nothing else. Merges never read Core Animation back: a score
evaluates to exactly what the render server draws, so the coordinator retargets from the score.

## `pictures`

**Interface.** A handle any thread can hold:

- `ask` windows anywhere at a priority (a clipped focus end, background, warm; the switcher's
  pictures ride warm);
- `ask_on_screen` windows on screen under a tag, at frame zero or for the last capture before the
  lift (`Moment::FrameZero`, `Moment::Final`);
- `watch` windows: until each lands where it ends or is lost (`Landing`, its deadline the watch's
  `until`), until a window's look changes from a print (`Change`), or kept fresh at a rate (`Live`);
  `retarget` or `stop` a watch;
- `hint` that a window was placed or focused; `hold` off-screen capture while a transition runs;
- `check_handover` a lift: the overlay alone, then the screen 45ms later, compared over every window
  ending wholly on screen;
- read the cache: `usable`, `focused`, `knowledge` (radius, translucency, material), and `dressing`
  (the desktop and the bar);
- `set_layer_contents(layer, picture)` draws any picture into any layer, regions through
  `contentsRect`.

Deliveries carry the caller's tag (a transition puts its generation in it), when the service sent
them, and only pictures the cache took.

**Inside:** a pure schedule (asks, watches and holds in, the next call out), region arithmetic, the
per-window knowledge, the cache policy; and on the platform side the SkyLight and ScreenCaptureKit
backends, first-sight measurement, and the service thread. Rules the schedule keeps, each a test:
frame zero first; a focus capture never queued behind a background or warm ask; a landing read from
the window server's own frame; windows that overlap a translucent one never share a composite;
ScreenCaptureKit only while no transition holds it off, except one capture of a clipped focus end and,
while the handover is measured, the screen a lift's check takes as the transition settles.

## `animation`

A `Score` is everything that moves in one transition, as data: tracks of keyframes on containers and
tiles, on three channels (the movement, the bounce, the nudge), each segment linear or a cubic
Bezier, and instant cuts (place, reparent, depth, drop). It is the fold of
its edits, and evaluates at any instant to what Core Animation plays: a spring leg as the polyline of
its samples, the bounce's way home on the `(0.42, 0, 0.58, 1)` Bezier. A picture swap is not a score
edit: it changes what a tile shows, never where it is.

A `Look` is how a flight moves, chosen as the flight begins; `Look::ALL` holds every look, and each
passes the same tests. Its `Choreography` makes the flight's plan, composes a score from a stage,
folds a press before the motion and retargets a playing score from wherever it is drawn. Before a
strip movement is asked for, the look stages its press (`Look::stage`): the reactor names the press as
a `Cue`, `transitions` finds where a moved pair starts and passes look and cue through without
matching on either, and the look says which windows cross, which are held and which accent rides the
movement. An `Accent` adds an additive track (the bounce re-kicks from where it is drawn). The
renderer plays a score's edits, the snap of a tile onto its window's frame among them, swaps
pictures, cuts holes over windows that end on screen, draws the constant hairline, and lifts. A new
look, such as the zoom-out spike, is another `Look`, its choreography and its staging, in `animation`
and the reactor's choice of it; capture, swaps and handover never see what a score contains.

## `transitions`

A `Transition` is one kind of change: the strip panning or a move along it, a layout pass (resizes,
swaps, a window opening), a workspace switch, the edge bounce. Each says what it stages, which
pictures frame zero needs, when real frames are written, what the lift must match, and how it absorbs
a press arriving in flight.

The coordinator is one lifecycle for every kind, as a pure state machine (`on(event, now) -> commands`):

```
Idle ─request─▶ Preparing ─frame-zero pictures, or 80ms─▶ (Coalescing, Holding) ─▶ Playing ─clock done─▶ Landing ─tiles and windows where they end, or 350ms─▶ lift ─▶ Settling ─quiet─▶ Idle
```

It owns generations: every tag carries one, a press merging in flight starts the next, and a picture
is swapped in only if it was taken for the end its tile has now, under a generation no older than the
one since which the tile has ended there, and the frame it was taken at is within 20pt of that end. A focus report arriving while Preparing still arms the focus target's watch: the pictures
service remembers it for a second.

## Decisions taken while Kai was away

| Decision | Why | Alternative |
|---|---|---|
| Make-key stays after the raise's Accessibility calls (the prototype's `mkfirst` is not carried) | it measured no gain, and making the window key before the cancellation checks breaks `specs/focus.md` | carry it behind the raise mutex |
| A focus target clipped by the display edge takes one ScreenCaptureKit capture through the hold; the window losing focus, ending clipped, takes none, turns at rest after the lift and is pictured again at the quiet | keeps `specs/animation.md`'s rule that the window gaining focus is recaptured wherever it ends, with one ScreenCaptureKit capture a flight: four out made frame zero three times as slow | let a clipped end change at rest; or a second capture through the hold for the window losing focus |
| Baked pictures are clipped to the window's measured corner radius | a crop of a desktop composite fills the rounded corners with wallpaper | capture the corners' alpha separately |
| The bar's menu-extras thread stays in `bar` for now | it has its own pacing and pauses for flights already | move it behind the pictures thread |
| A transition's hold has three states: up at its begin, before anything is composed, settling at the lift, released at the quiet after it | the bar is pictured 250ms after the lift, as it always was, while the warms the flight held wait for the quiet, which keeps a burst of presses off a busy compositor | release at the lift, and picture the bar 250ms after the quiet instead |
| A transition on a display before its desktop render lands is dressed with a desktop composited on the main thread, and the first with a bar composited there too | the service holds no picture of a display the reactor names just before the flight on it, and renders one only once released; drawn without one, that transition shows black or the other display's wallpaper | let those transitions fly without them until step 4's coordinator waits for frame-zero pictures |
| A warm or background ask settles 50ms before it is listed | the reactor sends a pass's warm straight to the service just after the request for its flight, so it usually arrives before that flight's hold, which the engine sends as it takes the request; one listed before a later hold is listed again at the release | have the reactor route those warms through the engine, which holds first |
| Window captures stay at 2x on every display; the display's own scale is for what is read off its framebuffer | ScreenCaptureKit charges per call, a 1x capture flies soft on a Retina display, and a scale change no longer throws away window captures in flight | capture each window at the scale of the display the overlay is on |
| A tile's ring is a layer beside its picture, with the outline as its child | a corner clip on a baked picture clips everything inside the picture, and the outline lies outside the window | draw the outline on the shadow caster, whose mask leaves only what is outside a 10pt silhouette |
| `set_tile_focused` switches the shadow's depth with the ring's white | a tile drawn with one focus and a shadow of the other matches neither end of the handover | switch the ring alone |
| A tile's corner radius is read from `pictures` when the flight composes it | the tile is described once, like its focus; a window first measured by the frame-zero capture flies that flight at 10pt | an overlay setter the coordinator calls when a window's knowledge lands |
| The radius a window is drawn with until its own is measured, `CORNER_RADIUS`, is `pictures`' (`domain::knowledge`); the hairline's geometry is `animation`'s (`domain::motion::hairline`) | the stand-in draws its corners with that radius, and `pictures` may not name `animation` | one constant in each feature |
| A change of the key window reaches the engine as the two looks it turned; at rest both are asked for at once, in flight what the flight does not picture is asked for at its quiet, at background priority either way | the engine alone knows whether a flight will picture them, and background priority waits through a hold, so a click that also starts a flight starts no ScreenCaptureKit capture under it | the reactor asks both ends at focus priority whenever no flight is reported up: a report just ahead of the request it causes starts both captures under that flight's frame zero, and one the quiet would have covered is asked twice |
| The end state is an on-screen ask at `Moment::Final`, not a priority of `ask` | on-screen captures are tagged, batched SkyLight calls with delivery rules of their own: only what is news, under the asker's generation | an end-state priority on `ask` |
| The transitions journal is a module of `transitions` (`domain::journal`, `platform::journal`), not a crate | every mark is in transitions' terms (`Generation`, `Kind`, `Command`); a crate would hold a ring and a writer for one client, and `crates/` holds libraries | `crates/rini-journal` once a second feature journals, the marks staying in each feature |
| The coordinator and the journal are one pure unit, `Journalled`, whose `drive` returns the commands and the burst due | what is marked and when a burst is written are tested without a `FlightEngine`, which needs a `MainThreadMarker` until step 7 | mark calls in the engine's `drive`, tested only through step 7's harness |
| The journal is always kept, in a ring of 8192 marks | a push is a copy into a ring allocated once, on the thread that holds the event: 2.1 to 3.8ns. With the journal off the ring still holds the last 80 to 200 flights (inferred), which turning it back on writes | nothing kept while off: one branch fewer, but the glitch that prompted turning it on is lost |
| The journal is written by default; `animations { journal { off; } }` turns it off | Kai asked for "a record overything as i move around" (2026-10-03), so the record is the default. `off` is niri's form for turning an animation off, so `docs/config.md`'s rule that a flag names the non-default holds, and the file is read live | `journal false`, a default-on flag written as `x false`; `RINI_JOURNAL=0`, which needs the launch agent's environment and a restart |
| A burst is written at the quiet after its lift (`ReportSettled`), and the file starts afresh past 50MB | Kai: "make sure you don't flush until after the transition settles so the disk io didn't add to the durations". A ring cannot grow, so the prototype's 200k-mark cap goes | the prototype's write once 400ms pass with no mark: a second timer for what the quiet already says |
| JSON lines at `~/Library/Logs/rini/transitions.jsonl`, `t` in uptime nanoseconds | focuslab's probes read `DispatchTime.uptimeNanoseconds`, the same clock, so a probe's trial and rini's marks join with no wall clock. A new name, so no reader of the prototype's text log misreads it | wall-clock milliseconds, which drift against the probe and are ambiguous across a sleep |
| `Event::Picture` and `Event::FrameZero` have no mark; `Event::Lost` is the `lost` mark | the first two restate a delivery of the pictures service, which the journal marks whole where the engine hears it. The engine turns every pictures `Lost` under its tag into exactly one `Event::Lost` | mark both: two records of one fact |
| The pictures service stamps each delivery with when it sent it (`Delivered.sent`) | a frame-zero picture's `taken` is stamped before its SkyLight call, and the engine hears the delivery only once it is free, so without the round's end the SkyLight round and the wait for the engine are one gap, and that split is what the time from a press to the overlay up needs | infer the engine's wait from the compose's end: ambiguous exactly when the capture and the compose overlap |
| A delivery is one mark (`delivery`: `of`, `why`, `sent`, a count), and each of its pictures one more after it (`delivered`: window, frame, `taken`) | `of`, `why` and `sent` are facts of the delivery, and window, frame and `taken` of each picture; an empty frame-zero delivery is a `delivery` alone, and the largest mark stays 48 bytes, a `Stamped` 72 | one mark per picture with the delivery's fields: each repeated per picture, an optional window and frame for the empty one, and a `Stamped` of 80 bytes, at the bound (measured 2026-10-04) |
| A request carries its press (`Cause`), which the engine hands to `Journalled::pressed` before handling the request, calling `answered` after | the request's first drive pushes the press once, ahead of its `begin` or `merge` and in the generation it left, and a request declined before any drive has none; `drive` keeps one signature and the rule is tested without a `FlightEngine` | `drive(event, cause, now)`: every staging function between the inbox and the first drive carries the cause |
| The press is when the reactor takes the event, named by its variant or, for a command, the command's IPC name | the research measured from there ("ms after rini takes the press"); a nested event puts the outer one back, so the passes after it carry the outer event | the key-down's own `CGEvent` timestamp, which the tap would carry through the reactor's queue |
| `tools/focuslab/journal.py probe` joins a probe's trial to the first press rini took within 250ms after the probe posted it, and `overlay.swift` writes one JSON line a press for it | the probe and the journal read one clock, so the join needs no wall clock, and only the window list says when the overlay is seen up after its `show` | leave the time from the show to the overlay seen up unmeasured |
| CPU and GPU use are not journalled | Kai allowed them if they cost nothing; what the IOKit read costs is not measured | sample them at the overlay up and the lift, as the prototype did |
| The handover check is opt-in (`animations { journal { measure-handover; } }`), with the research's sampling | it holds every lift for the overlay's own picture, one SkyLight call of 16-43ms, and its screen capture goes through ScreenCaptureKit's delivery queue; the same sampling keeps its numbers comparable with "The handover, measured" | always on: it perturbs the thing it measures |
| While measuring, a lift with a window ending on screen holds its hide in `Journalled`, outside the `Coordinator`, for the overlay's picture, any request, or 60ms, and its `hide` is marked; only the hide and the tiles' release move | it is instrumentation that never runs unmeasured; the quiet and `Settle` go out at the lift as ever, a lift that hid on a request or the patience is not measured, and each lift's own mark says whether it was held, where a burst's header could only say what the setting was as it was written | a lifecycle `Command::Lift` that hides on `Event::OverlayTaken`: a lifecycle state and its tests for a measuring-only path; the setting in each burst's header, which a burst written as the journal turns on gives its lifts taken while it was off |
| A transition beginning drops the handover check under way, and the overlay's picture with it; neither picture is taken once a transition is asked, and the two are drawn and compared off the pictures thread | the screen 45ms after the lift would show the next flight's overlay, and a press asks its frame zero of the pictures thread, so it waits at most for the one capture under way as it lands | the engine drops the measure of any lift a request followed, which also drops one whose screen was taken before the press |
