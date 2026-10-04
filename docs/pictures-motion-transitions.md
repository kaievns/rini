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
| 4 | The prototype's sequencing: frame zero, end-state watches, generations, holes, constant hairline, focused looks, live background; region pictures, batched SkyLight with per-window knowledge; the mid-flight refresh and the harvest deleted | in progress: `pictures`' side done (region pictures, knowledge, the batched capture, frame-zero and final asks, the `Landing`, `Change` and `Live` watches, hints, focused looks), not yet wired to the coordinator; the renderer's holes, tile snap, corner clip and region crop grid, which nothing calls yet |
| 5 | `animation` plays a `Score`; choreographies and accents produce one | in progress: the pure `Score` and its tracks; the bounce drawn from its track and re-kicked by every push; accents kept through a retarget |
| 6 | Typed coordinate spaces in `rini-geometry` for the three features' interfaces | |
| 7 | The coordinator on its own thread; the main thread renders only | |
| 8 | The transitions journal | |

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

- `ask` a set of windows at a priority (frame zero, focus, end state, background, thumbnail, warm),
  with a deadline;
- `watch` windows: until each lands where it ends (`Landing`), until a window's look changes from a
  print (`Change`), or kept fresh at a rate (`Live`);
- `hint` that a window was placed or focused; `hold` off-screen capture while a transition runs;
- read the cache: `usable`, `focused`, `desktop`, `bar`, `knowledge` (radius, translucency);
- `show(layer, picture)` draws any picture into any layer, regions through `contentsRect`.

Deliveries carry the caller's tag (a transition puts its generation in it) and only pictures the cache
took.

**Inside:** a pure schedule (asks, watches and holds in, the next call out), region arithmetic, the
per-window knowledge, the cache policy; and on the platform side the SkyLight and ScreenCaptureKit
backends, first-sight measurement, and the service thread. Rules the schedule keeps, each a test:
frame zero first; a focus capture never queued behind a background or thumbnail; a landing read from
the window server's own frame; windows that overlap a translucent one never share a composite;
ScreenCaptureKit only while no transition holds it off, except one capture of a clipped focus end.

## `animation`

A `Score` is everything that moves in one transition, as data: tracks of keyframes on containers and
tiles, on three channels (the movement, the bounce, the nudge), each segment linear or a cubic
Bezier, and instant cuts (place, reparent, depth, picture). It evaluates at any instant to what Core
Animation plays: a spring leg as the polyline of its samples, the bounce's way home on the
`(0.42, 0, 0.58, 1)` Bezier.

A `Choreography` composes a score from a stage and retargets a playing one from wherever it is drawn.
An `Accent` adds an additive track (the bounce re-kicks from where it is drawn). The renderer plays a
score, swaps pictures, snaps a tile onto its window's frame, cuts holes over windows that end on
screen, draws the constant hairline, and lifts. A new look, such as the zoom-out spike, is another
choreography; capture, swaps and handover never see what a score contains.

## `transitions`

A `Transition` is one kind of change: the strip panning or a move along it, a layout pass (resizes,
swaps, a window opening), a workspace switch, the edge bounce. Each says what it stages, which
pictures frame zero needs, when real frames are written, what the lift must match, and how it absorbs
a press arriving in flight.

The coordinator is one lifecycle for every kind, as a pure state machine (`on(event, now) -> commands`):

```
Idle ─request─▶ Preparing ─frame-zero pictures, or 80ms─▶ Playing ─clock done, end state in─▶ Landing ─lift─▶ Settling ─quiet─▶ Idle
```

It owns generations: every tag carries one, a press merging in flight starts the next, and a picture
is swapped in only if its generation is current and the frame it was taken at is within 20pt of its
tile's end. A focus report arriving while Preparing is kept for Playing.

## Decisions taken while Kai was away

| Decision | Why | Alternative |
|---|---|---|
| Make-key stays after the raise's Accessibility calls (the prototype's `mkfirst` is not carried) | it measured no gain, and making the window key before the cancellation checks breaks `specs/focus.md` | carry it behind the raise mutex |
| A focus end clipped by the display edge takes one ScreenCaptureKit capture through the hold | keeps `specs/animation.md`'s rule that both ends of a focus change are recaptured | let a clipped end change at rest |
| Baked pictures are clipped to the window's measured corner radius | a crop of a desktop composite fills the rounded corners with wallpaper | capture the corners' alpha separately |
| The bar's menu-extras thread stays in `bar` for now | it has its own pacing and pauses for flights already | move it behind the pictures thread |
| A transition's hold has three states: up at compose, settling at the lift, released at the quiet after it | the bar is pictured 250ms after the lift, as it always was, while the warms the flight held wait for the quiet, which keeps a burst of presses off a busy compositor | release at the lift, and picture the bar 250ms after the quiet instead |
| A transition on a display before its desktop render lands is dressed with a desktop composited on the main thread, and the first with a bar composited there too | the service holds no picture of a display the reactor names just before the flight on it, and renders one only once released; drawn without one, that transition shows black or the other display's wallpaper | let those transitions fly without them until step 4's coordinator waits for frame-zero pictures |
| A warm or background ask settles 50ms before it is listed | the reactor sends a pass's warm just after the flight it starts, so it reaches the service before that flight's hold | have the reactor route those warms through the engine, which holds first |
| Window captures stay at 2x on every display; the display's own scale is for what is read off its framebuffer | ScreenCaptureKit charges per call, a 1x capture flies soft on a Retina display, and a scale change no longer throws away window captures in flight | capture each window at the scale of the display the overlay is on |
| The hairline harvest moved into `pictures` in step 1 with the captures, though step 4 deletes it | the framed capture harvests the ring and a `Picture` carries it, so left in `animation` it would have had `pictures` name `animation` | leave it in `animation` and accept a two-way edge until step 4 |
