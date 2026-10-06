# Pictures, motion, transitions

How rini draws a window moving: four features, each with one job. Agreed with Kai on 2026-10-03,
`decorations` on 2026-10-06; the measurements it rests on are in
`src/animation/docs/capture-overlay-research.md` ("Where a moving focus change spends its time",
"The handover, measured") and `src/pictures/docs/capture-research.md` ("What a capture costs in a
press").

## Four concerns, four owners

| Feature | Owns | Never touches |
|---|---|---|
| **`decorations`** | what a window wears: the hairline's and the shadow's shape, their paint by focus and their depth, the Core Animation layers each tile wears them on, and the corner radius a tile is drawn with until its window's own is measured | captures, how a tile moves, when a transition does what |
| **`pictures`** | every capture of a window, the desktop or the bar; the cache; what is known about each window (translucency, material, corner radius, its focused look); the capture schedule and its thread | layouts, the overlay's layer tree (it only sets a layer's contents), when a transition does what |
| **`animation`** | how things move: the `Score`, choreographies that compose one, accents (the edge bounce, a move's nudge), and the renderer that plays a score on Core Animation | captures, window frames, focus |
| **`transitions`** | when things happen: frame zero, the overlay up, frame writes, picture swaps in flight, merged presses, the handover and the lift | how pixels are obtained, what a curve looks like |

`decorations` is upstream of all three and names none of them. `pictures` is upstream of both
others: the switcher, the bar and a future exposé are its clients too. `animation` knows ids,
geometry, `windows`' stacking group and what each tile wears (`decorations`) only. `transitions`
uses both, and the reactor drives it.

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

The coordinator runs on its own thread, `transitions`. The main thread runs what draws, the renderer,
the switcher and the bar, and eight actors not yet moved off it (the `src/main.rs` row of "What is
still not where the map says" in `docs/architecture.md`). Merges never read Core Animation back: a
score evaluates to exactly what the render server draws, so the coordinator retargets from the score.

## `pictures`

**Interface.** A handle any thread can hold:

- `ask` windows anywhere at a priority (a clipped focus end, background, warm; the switcher's
  pictures ride warm);
- `ask_on_screen` windows on screen under a tag, at frame zero or for the last capture before the
  lift (`Moment::FrameZero`, `Moment::Final`);
- `refresh` windows whose look has just turned, wherever they are, once no transition holds it;
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
while the handover is measured, the screen a lift's check takes as the transition settles; a refresh
only while none does, through SkyLight where its window is wholly on the display.

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
pictures, cuts holes over windows that end on screen, draws each tile's decorations, and lifts. A new
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
| A focus target clipped by the display edge takes one ScreenCaptureKit capture through the hold; the window losing focus, ending clipped, takes none, turns at rest after the lift and is pictured again at the quiet, one with a material keeping its blurred picture until a flight next pictures it on screen (Known debt in `src/pictures/docs/README.md`) | keeps `specs/animation.md`'s rule that the window gaining focus is recaptured wherever it ends, with one ScreenCaptureKit capture a flight: four out made frame zero three times as slow | let a clipped end change at rest; or a second capture through the hold for the window losing focus |
| Baked pictures and the stand-in are clipped by the overlay to the window's measured corner radius | a crop of a desktop composite fills the rounded corners with wallpaper, and the stand-in, one flat pixel, has no corners of its own | capture the corners' alpha separately; draw the stand-in's corners into its image |
| The bar's menu-extras thread stays in `bar` for now | it has its own pacing and pauses for flights already | move it behind the pictures thread |
| A transition's hold has three states: up at its begin, before anything is composed, settling at the lift, released at the quiet after it | the bar is pictured 250ms after the lift, as it always was, while the warms the flight held wait for the quiet, which keeps a burst of presses off a busy compositor | release at the lift, and picture the bar 250ms after the quiet instead |
| A transition on a display before its desktop render lands is dressed with a desktop composited on the `transitions` thread, and the first with a bar composited there too | the service holds no picture of a display the reactor names just before the flight on it, and renders one only once released; drawn without one, that transition shows black | let those transitions fly without them until the coordinator waits for frame-zero pictures |
| A warm or background ask settles 50ms before it is listed | the reactor sends a pass's warm straight to the service just after the request for its flight, so it usually arrives before that flight's hold, which the engine sends as it takes the request; one listed before a later hold is listed again at the release | have the reactor route those warms through the engine, which holds first |
| Window captures stay at 2x on every display; the display's own scale is for what is read off its framebuffer | ScreenCaptureKit charges per call, a 1x capture flies soft on a Retina display, and a scale change no longer throws away window captures in flight | capture each window at the scale of the display the overlay is on |
| A tile's hairline is a layer beside its picture, with the outline as its child | a corner clip on a baked picture clips everything inside the picture, and the outline lies outside the window | draw the outline on the shadow caster, whose mask leaves only what is outside the window's silhouette |
| `set_tile_focused` switches the shadow's depth with the hairline's white | a tile drawn with one focus and a shadow of the other matches neither end of the handover | switch the hairline alone |
| The turned picture of the window gaining focus waits for the window losing it at most `CHANGE_EVERY` (15ms), checked on the flight's ticks, and each tile's hairline and shadow turn with its own picture | macOS turns the old window first, and Kai asked for the hairline and shadow per window (2026-10-05); the watch reports the old window `Lost` once it is done with no change, out of tries, refused or clipped, and the target waits no longer then, so one interval bounds the wait only while the old window is still watched | swap each picture as it comes, ordered only within one round |
| While one end of a focus watch is not where it ends yet, the other is looked at every 15ms, not polled with it every 1ms | an end that is there would be captured every millisecond, a SkyLight call each, and spend its tries in 6ms | each end on its own clock, which splits their rounds |
| A tile's corner radius is read from `pictures` when the flight composes it | the tile is described once, like its focus; a window first measured by the frame-zero capture flies that flight at 10pt | an overlay setter the coordinator calls when a window's knowledge lands |
| The radius a window is drawn with until its own is measured, `CORNER_RADIUS`, and the hairline's and the shadow's geometry are `decorations`' (`domain::shape`) | the overlay draws the hairline, the shadow and a clipped picture's corner with them, and `decorations` names no feature | one constant in each feature |
| A change of the key window reaches the engine as the two looks it turned; at rest both are refreshed at once, in flight what the flight does not picture is refreshed at its quiet | the engine alone knows whether a flight will picture them, and a hold cancels a refresh and the release asks again for what the flight did not picture, so a click that also starts a flight starts no capture under it | the reactor asks both ends at focus priority whenever no flight is reported up: a report just ahead of the request it causes starts both captures under that flight's frame zero, and one the quiet would have covered is asked twice |
| A refresh is split by the pictures service, by where the window server has each window as it is looked at: SkyLight wholly on the display, ScreenCaptureKit anywhere else | the switcher and an exposé want the same rule, and the frame a capture is taken at is the server's then, not the one the reactor sent | `Command::Refresh` split by the coordinator over the frames it carries |
| The end state is an on-screen ask at `Moment::Final`, not a priority of `ask` | on-screen captures are tagged, batched SkyLight calls with delivery rules of their own: only what is news, under the asker's generation | an end-state priority on `ask` |
| The transitions journal is a module of `transitions` (`domain::journal`, `platform::journal`), not a crate | every mark is in transitions' terms (`Generation`, `Kind`, `Command`); a crate would hold a ring and a writer for one client, and `crates/` holds libraries | `crates/rini-journal` once a second feature journals, the marks staying in each feature |
| The coordinator and the journal are one pure unit, `Journalled`, whose `drive` returns the commands and the burst due | what is marked and when a burst is written are tested as a pure unit, without a `FlightEngine` and the channels, clocks and pictures service it is made from | mark calls in the engine's `drive`, tested only through the engine's harness (`protocol` in `src/transitions/platform/engine.rs`) |
| The journal is always kept, in a ring of 8192 marks | a push is a copy into a ring allocated once, on the thread that holds the event, a few nanoseconds; with the journal off the ring still holds the recent flights, which turning it on writes (the cost and how many flights: `src/transitions/docs/journal.md`, "The ring") | nothing kept while off: one branch fewer, but the glitch that prompted turning it on is lost |
| The journal is written when asked: `animations { journal; }` turns it on, and `journal false` or no node leaves it off | Kai, 2026-10-05: "journal off by default, on on my laptop for dev tracing." A bare flag names the non-default, as `docs/config.md`'s rule asks, and the file is read live | written by default with `animations { journal { off; } }` turning it off, from Kai's "a record overything as i move around" (2026-10-03); `RINI_JOURNAL=1`, which needs the launch agent's environment and a restart |
| A burst is written at the quiet after its lift (`ReportSettled`), and the file starts afresh past 50MB | Kai: "make sure you don't flush until after the transition settles so the disk io didn't add to the durations". A ring cannot grow, so the prototype's 200k-mark cap goes | the prototype's write once 400ms pass with no mark: a second timer for what the quiet already says |
| JSON lines at `~/Library/Logs/rini/transitions.jsonl`, `t` in uptime nanoseconds | focuslab's probes read `DispatchTime.uptimeNanoseconds`, the same clock, so a probe's trial and rini's marks join with no wall clock. A new name, so no reader of the prototype's text log misreads it | wall-clock milliseconds, which drift against the probe and are ambiguous across a sleep |
| `Event::Picture` and `Event::FrameZero` have no mark; `Event::Lost` is the `lost` mark | the first two restate a delivery of the pictures service, which the journal marks whole where the engine hears it. The engine turns every pictures `Lost` under its tag into exactly one `Event::Lost` | mark both: two records of one fact |
| The pictures service stamps each delivery with when it sent it (`Delivered.sent`) | a frame-zero picture's `taken` is stamped before its SkyLight call, and the engine hears the delivery only once it is free, so without the round's end the SkyLight round and the wait for the engine are one gap, and that split is what the time from a press to the overlay up needs | infer the engine's wait from the compose's end: ambiguous exactly when the capture and the compose overlap |
| A delivery is one mark (`delivery`: `of`, `why`, `sent`, a count), and each of its pictures one more after it (`delivered`: window, frame, `taken`) | `of`, `why` and `sent` are facts of the delivery, and window, frame and `taken` of each picture; an empty frame-zero delivery is a `delivery` alone, and the largest mark stays the size `src/transitions/docs/journal.md` ("The ring") records | one mark per picture with the delivery's fields: each repeated per picture, an optional window and frame for the empty one, and a `Stamped` of 80 bytes, at the bound (measured 2026-10-04) |
| A request carries its press (`Cause`), which the engine hands to `Journalled::pressed` before handling the request, calling `answered` after | the request's first drive pushes the press once, ahead of its `begin` or `merge` and in the generation it left, and a request declined before any drive has none; `drive` keeps one signature and the rule is tested without a `FlightEngine` | `drive(event, cause, now)`: every staging function between the inbox and the first drive carries the cause |
| The press is when the reactor takes the event, named by its variant or, for a command, the command's IPC name | the research measured from there ("ms after rini takes the press"); a nested event puts the outer one back, so the passes after it carry the outer event | the key-down's own `CGEvent` timestamp, which the tap would carry through the reactor's queue |
| `tools/focuslab/journal.py probe` joins a probe's trial to the first press rini took within 250ms after the probe posted it, and `overlay.swift` writes one JSON line a press for it | the probe and the journal read one clock, so the join needs no wall clock, and only the window list says when the overlay is seen up after its `show` | leave the time from the show to the overlay seen up unmeasured |
| What the renderer commits is a journal mark, `drawn`, and overlay up counts to it | the launch agent runs rini at `RUST_LOG=error,warn,info` (`src/app/launch_agent.rs`), so a `debug!` line is never written by the deployed service; and once the renderer answers by message, `show` is the coordinator's decision, a hop and a transaction before the overlay is up | a `debug!` line for each committed batch, with the launch agent's `RUST_LOG` raised, a plist change and a restart, and overlay up still read off `show` |
| CPU and GPU use are not journalled | Kai allowed them if they cost nothing; what the IOKit read costs is not measured | sample them at the overlay up and the lift, as the prototype did |
| The handover check is opt-in (`animations { journal { measure-handover; } }`), with the research's sampling | it holds every lift with a window ending on screen for the overlay's own picture, one SkyLight call (what one costs: "A SkyLight capture costs what the time since the previous one says" in `src/pictures/docs/capture-research.md`), and its screen capture goes through ScreenCaptureKit's delivery queue; the same sampling keeps its numbers comparable with "The handover, measured" | always on: it perturbs the thing it measures |
| While measuring, a lift with a window ending on screen holds its hide in `Journalled`, outside the `Coordinator`, for the overlay's picture, any request, the overlay about to move to another display, or 60ms, and its `hide` is marked; only the hide, the tiles' release, and the bar's picture and the quiet that wait for the hide to be drawn move | it is instrumentation that never runs unmeasured; the bar is read off the screen, so `Settle` waits for the hide to be drawn and the quiet counts from it, a lift that hid on a request or the patience is not measured, and each lift's own mark says whether it was held, where a burst's header could only say what the setting was as it was written | a lifecycle `Command::Lift` that hides on `Event::OverlayTaken`: a lifecycle state and its tests for a measuring-only path; the setting in each burst's header, which a burst written as the journal turns on gives its lifts taken while it was off |
| A transition beginning drops the handover check under way, and the overlay's picture with it; neither picture is taken once a transition is asked, and the two are drawn and compared off the pictures thread | the screen 45ms after the lift would show the next flight's overlay, and a press asks its frame zero of the pictures thread, so it waits at most for the one capture under way as it lands | the engine drops the measure of any lift a request followed, which also drops one whose screen was taken before the press |
