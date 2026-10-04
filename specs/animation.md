# What moves

Animation is cosmetic and MUST NOT be load-bearing. A build with animation off behaves identically;
only the transitions are missing.

## What animates

- A window that changes position or size in a layout pass.
- A window opening, from the frame macOS first showed it at to its slot.
- The strip scrolling, and a workspace switch.
- A window moved along the strip, changing places with the column it passes.

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
- **Every push against an end MUST show.** One arriving while the bounce plays MUST push the view out
  again from wherever it is drawn: it MUST NOT be dropped, and MUST NOT start the bounce over from
  rest. A burst at the wall holds the view there, and the view is home within one bounce of the last
  press, never later. A push arriving before the motion begins, up to 300ms behind a window opening,
  MUST wait for it, and the pushes of that wait show once, however many arrive: a CSS animation
  restarted twice before its first frame shows once.

> **Reported 2026-10-03.** "the only thing that still mildly bugs me is the end of strip/stack
> animation, it throttles rapid key presses and it feels like a bug. if you can make it react to
> every single press like a pure CSS based bouncer would, that'd be fantastic." A push while a
> bounce played was dropped, so the second and third press of a burst showed nothing.

> **Reported 2026-09-29.** "espcecially visible when you hit the end of the strip and it just bounces
> off the wall several times". Each press at the wall restarted the bounce from rest, and presses the
> reactor handled late went on bouncing after the pressing stopped. Both stay excluded above.

> **Reported 2026-09-24.** "The little bounce animation when navigation reaches an end of strip or
> workspaces stack is neat but a bit too small, I need a little more swing. Also when I'm moving a window
> in a strip or between workspaces it needs similar animations, because otherwise there is no visual cue
> and it feels like a bug/stuck." The overshoot was 36pt; moving reported no edge at all, so it never
> reached the bounce.

## Moving a window along the strip

- A window moved along the strip MUST be seen to change places with the column it passes, whether or
  not the strip scrolls. Both start from the places they held. A moved column narrower than the
  viewport and the column it passes cross, and the rest of the strip moves only as the view does.
  Drawing the finished layout and scrolling the view to it shows the swap only where nothing
  scrolls.
- Every window MUST start from where it was on screen. The view moves wherever the strip moves on
  screen, not only when it scrolls: a centred column that loses its centring as it moves shifts the
  whole strip with the scroll offset unchanged.
- Only a moved column that fills the viewport MUST step: one as wide as the tiling area, the
  display's usable width less both outer gaps, within a point, whether the full-width toggle or a
  width preset made it so. The view follows such a column, so nothing else on screen shows it moved.
  A narrower column, half or two-thirds of the display, is seen moving and MUST NOT step.
- The step MUST go away from where the window goes, a third of the display and back: a move right
  steps left and opens the right side, where the column it passes was.
- The whole strip MUST step: every tiled window and every border tracing one. Floating windows and
  their borders MUST stay where they are. A border arriving mid-step steps with the window it
  traces. Not yet met: a window that opens or resizes mid-step leaves the step where it is drawn
  and does not carry the rest of it.
- The column passed MUST be seen going beneath the moved window from the side the step opens, and
  that side MUST NOT show the desktop, except past the strip's end: the column passed fills it, then
  the strip beyond it. The column passed is drawn in its place on the strip, riding the strip
  beneath the moved window, which covers it at the end.
- A move arriving while a flight still gathers its passes MUST step once that flight moves.
- A flight MUST run longer to carry the step only when it steps.
- The step MUST leave and come back at rest. On the bounce's shape a third of a 1720pt display,
  573pt, leaves at 29,235pt/s over the default 0.35s: the kind of jolt reported under "Arriving
  mid-flight".
- The moved window MUST be drawn over the column it passes.
- A burst of moves MUST read as one movement with one step: a move arriving mid-flight carries the
  flight on, and a press while the step plays MUST NOT start another.
- The step and the bounce at an end MUST NOT hold each other off.
- The step is drawn only. Real windows go to the layout's frames, the column passed included.

> **Reported 2026-09-30.** "what i have two 50/50 windows at the beginning of a strip, when i move one
> of them in place of the other there is a nice transition animation where two windows are moving to
> take each other's places. everywhere else though the window just jumps to new place and then there
> is a focus navigation animation on top of it, the transition is instant and confusing, it is
> especially problematic for full sized windows where the movement is generally invisible and it
> doesn't feel like anything is actually happening, which feels broken." And: "maby moving full-size
> windows 1/3 to either side temporarily to show the swap animation and then bounce it back in focus
> would be an option too". Every move that scrolled the strip was drawn as the finished layout panning
> into view, so the two windows had changed places by the first frame. The 50/50 pair at the start of
> a strip was the one case that did not scroll.

> **Reported 2026-09-30.** "two fixes for the moving animation. 1. only full-size windows should
> cause the wobble (ecause you can't see otherwise it's movement) currently smaller windows do that
> too. 2. the wobble should go the opposite direction from current implementation". Any move whose
> own travel on screen was under a quarter of the display stepped, so half-width moves the view
> followed stepped too, and the moved window stepped alone toward where it went. The column it passed
> crossed the display in about 60ms, so the third the step opened showed the desktop.

## The vertical workspace switch

- The workspaces MUST scroll as one column, each sitting directly on the one above it. There MUST NOT
  be a band of empty desktop the size of the bar between one workspace and the next: the only space
  between two workspaces' windows is their own outer margins, the same gap two tiled windows have.
- The animation MUST NOT extend under the bar. Its surface starts at the bar's lower edge, and a
  window scrolling up off the top passes behind the bar rather than into a strip above it.

> **Reported 2026-09-29.** "remove the menu bar sized gap between workspaces during the animation so
> they'd sit directly on top of each other during the workspaces vertical switch animation" and "make
> the animation overlay a bit smaller at the top to start right under the new menu bar". The strips
> were stacked one FULL display height apart while each workspace's windows started below the bar
> inside its row, so a bar-sized strip of desktop showed between workspaces as they scrolled. The
> overlay covered the whole display, including the band the bar sits in.

## Arriving mid-flight

The layout does not wait for an animation to land, so a second pass can arrive with a different
destination for a window already moving.

- A pass repeating a destination MUST be treated as redundant, not as a new flight. Otherwise a held
  key restarts the animation on every repeat and it never lands.
- A pass with a new destination for a moving window MUST bend that window toward it rather than
  restarting: it continues from where it is drawn, at the speed it is moving. Its speed MUST NOT jump,
  so a burst of presses reads as one movement that settles after the last. That holds for a window a
  pass takes out of the piece it rode, as a second move of a narrower column does with the column it
  passes.

> **Reported 2026-09-29.** "as a general rule animation should not get interrupted mid-filight and
> restarted a new, the target should fluidly move to the next window and feel like one smooth extended
> flight". Each press restarted the motion curve from wherever the strip was drawn. The curve leaves
> at 6.25x its average speed, so a strip that had nearly stopped was thrown forward again: about
> 890pt/s to 15,375pt/s in one frame at a typical interval between presses.
- A window the flight has not seen MUST be able to join it.
- A merged press MUST NOT show a stale, transparent, stretched or zoomed tile. A picture taken for an
  end its tile no longer has, or of a window more than 20pt from where its tile ends, never reaches a
  tile: drawn, it is a stale tile, or one stretched or zoomed to the other size.
- A merged press MUST NOT lose a picture taken for an end its tile still has. Pictures are taken on
  their own thread and arrive while a press is merged, and none is taken twice.
- A merged press MUST NOT replace a tile's picture with the cache's: the tile keeps frame zero's, a
  focused look, or a picture the flight took.
- A press merged before the flight moves MUST compose with it as one movement from frame zero: the
  strip, and on a switch every row and floating window, travel together as one piece. Composed window
  by window from where each tile starts to where the last press sends it, the pieces slid over one
  another.
- A window a merged press brings, before the flight moves or while it does, MUST start beside the
  windows it sits beside in the layout, wherever they are drawn, and travel with them. Before the
  motion a column the second press brought started under the column the first had brought; once
  moving, one placed where the last press sends the strip started short of the strip by the travel
  the strip had left.

> **Reported 2026-10-03.** "rapid press navigation messes things up, i periodically see missing tiles
> (or they're just transparrent), occasionally i see tiles that are zoomed in." Each picture is now
> pinned to the end its tile had when it was taken and to the frame its tile ends at; which picture
> each of those tiles drew was not measured. A press merged before the flight moved also split the
> strip into pieces that slid over one another, one covering another; it now composes as one
> movement. Whether either is what was seen is not measured.

## A window that has just opened

- It travels from the frame the window server reports to its slot, if there is a usable picture of it
  and the capture budget allows.
- Otherwise the flight holds a place for it and waits for its first picture. Each reason for holding
  MUST be recorded distinctly: "the window appeared without animating" has four causes and the log is
  the only way to tell them apart afterwards.

## The pictures a flight starts with

- A flight MUST start from pictures taken at the press: every window wholly on screen where its tile
  starts, and the floating windows seen behind them, a pass merged before the overlay is up included.
  The overlay MUST wait for them no longer than 80ms, and then flies what the cache holds until they
  come: each then replaces the cache's on its tile, and the flight MUST NOT start again.
- A real window MUST NOT be written before the overlay is up, whatever the flight: written earlier, it
  jumps on screen before the overlay hides it. Every frame MUST be written as the overlay goes up.
- A window the overlay covers MUST keep painting: a point of the overlay is left clear over each
  window that ends wholly on screen, so the window server counts it visible. Covered wholly, a
  terminal never reached its focused look within 450ms in 24 tries; with the point clear it did in
  all 24.
- A floating window seen through a translucent one MUST show its live content while the flight runs,
  kept fresh behind the strip once the window gaining focus has its look.

> **Reported 2026-10-03.** "the background window images are served stale constantly, it's not a
> minor issue, because i can see changing content in the background windows during animation with
> semi-transparrent windows. this won't do." The windows behind the strip flew the cache's pictures,
> which no capture landing mid-flight replaced; the cache's pictures a flight drew were 1.6s old at
> the median, measured 2026-10-02.

## Correctness the user can see

- The frame written to a window is the layout's answer, never the animation's. A window parked off the
  strip is written its real park position; the tile is merely drawn heading off the edge.
- A window MUST NOT be left at an animation's intermediate frame if a pass is interrupted.
- The overlay MUST stop drawing a tile whose window is gone, and MUST drop a container once the last
  tile leaves it. An empty container is a layer the compositor keeps compositing.
- Every window MUST be asked for a picture the first time rini lays it out, on whatever workspace,
  before anything flies. After a restart nothing moves, and a pass that moves nothing asked for no
  pictures, so the first flights had none to draw. Per window, not per display: apps are found one
  by one at startup, and the first pass on the display knew one window of 21.
- A change of display MUST NOT drop the window pictures being captured. Only the desktop render belongs
  to a display; with a second display attached, every flight on the other one threw the window captures
  in flight away.

> **Reported 2026-09-29.** "i also occassionally run into issues where windows are missing during the
> animation, or maybe have holes in their screenshots... i just notice momentary gaps where only the
> window frame is moving during the animation. i've seen plenty of those during your automated testing
> too". Over four restarts no picture landed for the first 11s, and 49% of the flights in the first
> 30s were missing a window; after 2 minutes, 5%. The replays ran seconds after each restart.

## Windows with no picture yet

- A window with no picture MUST NOT leave a hole in the strip. A known window parked off this display —
  every window after a restart, with the picture cache empty — MUST fly with a dark stand-in tile in its
  place, moving exactly as its picture would. That holds for every flight: a layout pass, a strip
  scroll, a switch and a move, where the column a full-width window passes is parked at both ends.
- The stand-in MUST keep the window's rounded corners, and MUST be replaced by the window's real picture
  as soon as one lands, on the same tile, so nothing else in the flight is disturbed. A flight that does
  not wait for pictures, a strip movement, MUST ask for the picture of a window it brings onto this
  display once the window is placed there.
- The stand-in MUST NOT be cached. Cached, the switcher would read it as the window's picture and the
  next flight would take it for one.
- A genuinely new window, on this display with no picture, keeps its spawn capture and entrance.

> **Reported 2026-09-28.** "When the initial image is missing (after a restart or whatnot) the
> animations render a hole in the strip. I think having a default dark grey fallback image would be
> better." A parked window with no picture took the reservation meant for a newly opened window, which
> draws nothing in its slot until a picture lands.

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

## The window's hairline

- Every tile MUST wear the hairline macOS composites around each window, which no capture carries: a
  1pt ring of white on the window's outermost point, at 20% on the focused window and 18% on any
  other, and a 0.5pt stroke of black at 90% in the half point just outside it.
- Both MUST round to the window's own corner radius once `pictures` has measured it, and to 10pt
  until then, never past half the tile's shorter side.
- Both MUST follow the tile through a resize, on the tile's own timing, and onto its window's real
  frame when the tile snaps there. The outline MUST stay whole on a tile whose picture is clipped to
  its corners.
- When focus hands over during a flight, the ring MUST follow it: the window gaining focus brighter,
  the window losing it dimmer.

The ring's values are a fit measured 2026-10-02 in dark mode, on 27 window edges across 11
applications: within 0.3 levels rms on focused edges and 0.1 on unfocused ones. Light mode is not
measured. Before it, a tile wore a hairline cut out of a capture of its window, and none until that
capture had run: the 53-level p99 at the focused window's edge in "The handover, measured"
(`src/animation/docs/capture-overlay-research.md`).

## Focus changing during a flight

- When a flight moves focus from one window to another, both MUST land in their new focus rendering:
  the window gaining focus active, the window losing it inactive. The change MUST reach each tile while
  the flight still moves, so nothing changes at rest after the lift.
- The window gaining focus MUST be pictured where it ends from the moment the window server reports it
  focused until its look changes, and drawn focused from that picture on: its ring and its shadow with
  it. A look it already had when it landed counts as changed. If no picture shows the change by the
  last capture before the lift, it MUST be drawn focused from then on all the same, while the flight
  still moves. Until then the window losing focus MUST be drawn focused, as it was on screen when the
  flight began, and after it unfocused. Ending wholly on a display, it MUST be pictured again before
  the lift if its look has changed. Ending clipped by the display's edge, it keeps its picture to the
  lift, only its ring and its shadow turning unfocused, and its look turns at rest: SkyLight returns
  only its visible part, and the one ScreenCaptureKit capture below is the window gaining focus's.
- A focus report that arrives before the flight watches for it MUST still count, and so MUST one that
  arrives before the request it causes, as a click's or an app activation's does: the window focused
  before it is the one the flight begins drawn focused.
- A window gaining focus that starts off the display, where nothing can picture it, MUST fly the last
  picture taken of it focused, drawn focused from the first frame of the press giving it focus, a press
  merged into a flight included, unless that picture no longer fits it or a later picture shows its
  content changed. Its landing MUST NOT replace that picture before focus hands over: it is often taken
  before the window repaints focused, and would turn it unfocused and back.
- A picture MUST be kept as a window's focused look only if it shows the window focused: its change
  pictured as it gains focus, or frame zero's picture of the window focus leaves if that window was
  the key window as the press went out. A click's report has made its target key before the request it
  causes, and another application's panel taking the keys, Spotlight's or Raycast's, leaves the
  layout's window drawn unfocused: kept, that picture flies the window in as its focused look looking
  unfocused.
- A window gaining focus that ends clipped by the display's edge MUST still be recaptured. SkyLight
  returns only its visible part, so it takes ScreenCaptureKit's one capture.
- Every other ScreenCaptureKit capture, a warm or a background capture included, MUST wait from the
  press to the quiet after the lift. With four of its captures out, the capture the overlay waits for
  at the press took three times as long.
- A flight that moves focus nowhere MUST NOT recapture through ScreenCaptureKit, and MUST NOT watch for
  a change of focus: no report comes to arm the watch, and one left waiting holds the windows behind
  the strip back for the whole flight.
- Both windows a change of the key window turns, the one that becomes key and the one that stops being,
  MUST be pictured again for the next flight and the switcher, whoever moved focus: a click, rini's own
  move, or another application's panel taking the keys. With nothing flying both MUST be asked for at
  once; in flight, each one no picture of the flight showed after the change MUST be asked for at the
  quiet after the lift, the window losing focus that ends clipped or parked among them. A focus change
  alters a window's look and never its size, and a picture that still fits was never taken again for
  its own sake, so a window kept the look it had until it was resized, and its tile turned at the lift.
  A window whose picture carries its blur keeps it over this capture of the window alone, and its look
  turns when a flight next pictures it on screen.
- Once the overlay is up, a tile MUST change its picture only for one the flight took: frame zero's
  coming after the overlay went up without it, as its window lands, as the window gaining focus
  changes its look, as a floating window behind the strip is refreshed, or just before the lift. A
  capture made for anything else shows the window as it was, and cut in after the flight's own it
  would cut the tile back.
- A picture the cache refuses MUST NOT reach a tile either. The flight would draw what the next one
  will not.
- A picture of a window that was not wholly on one display while it was captured MUST NOT be drawn,
  cached as a picture, or cut onto a tile. The capture comes back full-size with the off-screen part
  transparent, and drawn it is a half or fully missing window.

> **Reported 2026-10-03.** "fcache doesn't bother me … windows arrive in focus every single time". The
> prototype flew a focus target parked off the display with its last focused picture, which can be
> older than the window's content; asked about that, Kai preferred the window arriving focused.

> **Reported 2026-10-02.** "mostly bugged by the flicker into the active window". The window gaining
> focus flew its picture from before the change and turned focused only at the lift.

> **Reported 2026-09-28.** "The old window stays in its old active state throughout the entire
> transition and then flickers in in its inactive state once the animation finished. Would be great if
> we could swap the image during the animation itself if it's available." The recapture already
> covered both ends, but at 0.5 through the background service. Its pictures landed at 0.82 to 1.16 of
> the flight, measured the same day, past the 0.6 cutoff it had then, so neither end was ever cut in.

> **Reported 2026-09-28**, against the fix above. "Windows half cut by the display cause all sorts of
> random issues, it displays half or fully missing frame during transitions constantly." The refresh
> judged a window wholly on screen, and the strip pan moved it past the edge before the capture ran.

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

## Watching transitions: the journal

- rini MUST record every step of a transition's lifecycle: each thing it hears and each thing it
  decides, with when, so a glitch seen while moving around can be read back afterwards.
- rini MUST keep the last 8192 steps of its transitions in memory at all times, so a record can be
  written of a glitch seen with the journal off.
- Unless the journal is turned off (`animations { journal { off; } }`), rini MUST write each burst of
  transitions once it has settled, and MUST NOT start a write while a transition is up, so writing
  adds nothing to the durations it records. The record MUST be at most two files: the current one,
  started afresh once it passes 50MB, and the one before it.
- Turning the journal on or off MUST NOT change what is drawn or when.
- Each step MUST carry its time on the outside probe's clock (mach absolute time), so a probe's trial
  and rini's steps line up with no wall clock between them.
- CPU and GPU use MAY be recorded only once what reading them costs is measured, and found to slow
  nothing down.

> **Reported 2026-10-03.** "can you add transitions logs? as in log timing for every transition step,
> make sure you don't flush until after the transition settles so the disk io didn't add to the
> durations. i want you to have a record overything as i move around so you could have a postmortem
> picture for debugging, you can log CPU/GPU utilisation in the moment too if it doesn't cost anything
> in terms of slowing down". The `scratch/handover` prototype had kept a text journal from about 135
> call sites, with a lock and an allocation for each mark. This one marks the lifecycle where it is
> driven, keeps its marks in a ring allocated once, and writes them by default.

## Where it lives

`src/animation/domain/motion/` is how things move: the rigid pieces and their merge, the curves, the
depth bands, the edge bounce and the nudge, and the hairline every tile wears. The looks
(`src/animation/domain/choreography.rs`, Slide the default in
`src/animation/domain/choreography/slide.rs`) compose a flight's score, fold a press before its motion
into it and bend it at a press after, and stage a press on the strip. `src/animation/platform/overlay.rs`
plays the score on Core Animation. When each step happens is `src/transitions/`:
`src/transitions/domain/flight_score.rs` keeps a flight's score, `src/transitions/domain/strip_move.rs`
finds where a moved pair starts and puts the look's staging of it on the strip,
`src/transitions/domain/lifecycle.rs` decides when every step of a flight happens,
`src/transitions/domain/admission.rs` how a pass arriving mid-flight is admitted, and
`src/transitions/platform/engine.rs` is the actor that carries them out. The journal is kept with the
lifecycle in `src/transitions/domain/journal.rs` and written by `src/transitions/platform/journal.rs`;
what each line says is `src/transitions/docs/journal.md`. The pictures a flight
draws are `src/pictures/`: whether one covers and fits its window, the stand-in, the blur, and the
schedule that holds background capture off while a flight runs in `src/pictures/domain/`; the
captures in `src/pictures/platform/` (`src/pictures/platform/skylight.rs` for the framed capture with
its blur, `src/pictures/platform/screencapturekit.rs` for the background captures), and the service
that makes them and keeps the cache, `src/pictures/platform/service.rs`. Measurements are in
`src/transitions/docs/coordination.md` (when a flight does what), `src/animation/docs/animation-smoothness.md`
(how it moves), `src/animation/docs/capture-overlay-research.md` (the overlay) and
`src/pictures/docs/capture-research.md` (the captures).
