# The transitions journal

Every step of every transition, kept in memory and written once it settles, so a glitch seen while
moving around can be read back afterwards. Kai's ask, and what rini must do about it, are in
`specs/animation.md` ("Watching transitions: the journal"). The marks, the ring and the lines are
`src/transitions/domain/journal.rs`; the thread that writes them is
`src/transitions/platform/journal.rs`.

## Where a mark comes from

`Journalled` holds the coordinator and its journal as one pure unit, and the engine meets it at four
seams. Its one call into the lifecycle, `FlightEngine::drive`, goes through `Journalled::drive`: it
reads the event's own marks, the coordinator takes the event, and then those marks and one for each
command it gave are pushed, every one stamped with the drive's `now` and the generation the drive
left (`g`). Its inbox hands each request's press to `Journalled::pressed` before handling the
request and calls `Journalled::answered` after: the request's first drive pushes a `press` mark
ahead of that event's own, in the generation that drive left, and a request declined before any
drive has none. Its pictures inbox hands each delivery under its tag to `Journalled::delivery` as it
comes, before turning it into `FrameZero` and `Picture` events: a `delivery` mark, then a
`delivered` mark for each picture in it, stamped with when the engine heard it and the generation
the coordinator is in. The same inbox hands a measured lift's handover to `Journalled::handover`, a
`handover` mark for each window, and a lift the engine holds for the overlay's picture is told to
`Journalled::lifted`, with what it hears after to `Journalled::heard`, a `hide` mark when it comes
down (below). Its inbox hands each commit the renderer reports (`Rendered(Committed)`) to
`Journalled::drawn` before what that commit releases is carried out: a `drawn` mark, stamped with
when the engine heard it, so the real frames, the bar's picture and the quiet a commit releases go
out at its `t`. No mark call sits anywhere else, and the engine never reads the journal back: of
what a drive returns, it carries out the commands and sends the writer the burst, if one is due, and
of `heard` it reads whether the held lift hides now.

Both matches, over `Event` and over `Command`, have no `_` arm, so a new variant does not compile
until it has a mark.

| Source | Mark | Fields |
|---|---|---|
| a request's first drive, ahead of that event's own marks | `press`, stamped when the reactor took the event the request answers | `what` |
| `Event::Begin(stage)` | `begin` | `kind`, `coalesced` (the stage waits for the reactor's follow-up passes), `tiles`, `awaiting`, `repainting`, `focus`, `losing_focus`, `key`, `duration_ms` |
| `Event::Merge { stage, moved }` | `merge` | `kind`, `moved`, `tiles`, `awaiting`, `repainting`, `focus`, `focused_look`, `duration_ms` |
| `Event::Extend(d)` / `Abandon` / `Quiet(g)` | `extend` / `abandon` / `quiet` | `ms` / none / `of` |
| `Event::Tick { settled, landed }`, once the clock has run out | `at_rest`, `landed`: each once a generation, at its first `true` | |
| `Event::Lost { generation, window }` | `lost` | `of`, `window` |
| `Event::LooksTurned(windows)` | `looks_turned`, one for each window | `window` |
| `Event::Picture`, `Event::FrameZero` | none: the delivery's marks are the record | |
| a delivery of the pictures service under the engine's tag | `delivery` | `of`, `why`, `pictures` (a count), `sent_before_us` |
| each picture in it, after its `delivery` | `delivered` | `window`, `frame` (screen points, where the window was when it was taken), `taken_before_us` |
| `Hold` / `Settle` / `Release` | `hold` / `settle` / `release` | |
| `AskFrameZero` | `ask_frame_zero` | `strip`, `background`, `behind` (counts) |
| `Show` / `StartMoving` / `StopWatches` | `show` / `start_moving` / `stop_watches` | |
| `Write` / `Holes` / `Warm` / `Refresh` | `write` / `holes` / `warm` / `refresh` | `windows` (a count) |
| `Watch(EndWatch)` | `watch` | `landing`, `awaiting`, `background` (counts), `focus`, `losing_focus` |
| `AskFinal` | `ask_final` | `windows`, `behind` (counts) |
| `Swap` / `KeepFocusedLook` | `swap` / `keep_focused_look` | `of`, `window` |
| `Snap { window, frame }` | `snap` | `window`, `frame` (overlay points) |
| `Focus { window, focused }` | `focus` | `window`, `focused` |
| `Lift` | `lift` | `at_rest`, `landed`, as the tick that lifted it read them: either `false` is `LIFT_GRACE` run out |
| a lift held for the overlay's picture coming down | `hide` | `measured` (it hid on that picture), `waited_us` after the lift |
| the renderer's answer that a drain of render commands is committed | `drawn` | `seq` (the highest batch in it), `shown` (`true` it put the overlay up, `false` down, `null` neither), `queued_us`, `applied_us`, `flushed_before_us` |
| `ReportUp` / `ReportSettled` | `report_up` / `report_settled` | |
| a handover check's measure under the engine's tag, for a lift that hid on its overlay's picture | `handover`, one for each window | `of`, `window`, `interior`, `interior_over`, `edge_in`, `edge_out` (levels, and a share for `interior_over`) |

How to read them:

- **`g` is the generation after the event.** A merge that starts a generation is marked in it. `of`
  is the generation a delivery, a loss, a swap, a quiet or a handover was under; `of != g` is stale.
- **A delivery carries its times as ages,** in microseconds before its own `t`: `sent_before_us` is
  when the service sent it, and each `delivered`'s `taken_before_us` when that picture's capture
  started (below).
- **A `drawn` carries the renderer's times:** `queued_us` from the oldest batch in the drain being
  sent to the renderer beginning to play it, its wait on the main thread behind whatever ran there
  first; `applied_us` from there to its transaction's flush, the commands played, committed and
  flushed; and `flushed_before_us`, the flush's age at the mark, the way back to the engine. On the
  probe's clock the drain was flushed at `t - flushed_before_us` and began `applied_us` before that.
  The window's alpha goes to 1 just after the flush ("One transaction a drain, flushed" in
  `src/animation/docs/README.md`), and the window server shows it after that, which only a probe sees
  (`journal.py probe`, below).
- **`at_rest` and `landed` are the lift's condition, not when each window landed.** The engine reads
  where the tiles are drawn and where the windows are only once the clock has run out
  (`Coordinator::wants_landing`) and tells the coordinator `false` before, so a tick before then
  marks nothing.
- **A command's mark says the coordinator gave it,** stamped with its drive's `now`. Carrying it out
  is not timed, but for what the renderer draws: the `drawn` covering the batch a command's render
  commands went in says when they were committed, and what waited for them went out at its `t`.
- **Marks are in the order they were pushed, and `t` is not:** a delivery's as the engine hears it, an
  event's and its commands' as its drive runs. A `press` carries when the reactor took the event, so
  it can come before marks the engine pushed while the request waited for it. A drive nested in
  carrying out a command (`StartMoving`'s `Extend`) comes after every command of the drive it is
  nested in. Group by `g`, and never assume `t` is sorted.
- **`Picture` and `FrameZero` have no mark:** they restate a delivery of the pictures service, which is
  marked whole where the engine hears it. An empty frame-zero delivery is a `delivery` with no
  `delivered` after it.

## A line

One JSON object a line. Each burst opens with a header, then one line a mark, oldest first:

```
{"t":81235020000000,"mark":"burst","journal":1,"unix_ms":1791043200123,"lost":0}
{"t":81234561290123,"g":42,"mark":"press","what":"move_focus"}
{"t":81234567890123,"g":42,"mark":"begin","kind":"pan","coalesced":false,"tiles":3,"awaiting":0,"repainting":0,"focus":[812,3104],"losing_focus":[812,3090],"key":[812,3090],"duration_ms":350}
{"t":81234567890123,"g":42,"mark":"hold"}
{"t":81234598190123,"g":42,"mark":"delivery","of":42,"why":"frame_zero","pictures":1,"sent_before_us":4100}
{"t":81234598190123,"g":42,"mark":"delivered","window":[812,3104],"frame":[0.0,32.0,864.0,1085.0],"taken_before_us":28900}
{"t":81234970012345,"g":42,"mark":"snap","window":[812,3104],"frame":[3.0,35.0,860.0,1079.0]}
```

The header's `t` is when the burst was written, `journal` the format's version (1), `unix_ms` the wall
clock then, and `lost` how many marks the ring overwrote before they were written. A press's `what` is the event's name (below). A
window is `[pid, idx]`, its `idx` the window server's id; a rect is `[x, y, w, h]` as `f32`; a
generation is a number; a `why` is `frame_zero`, `landed`, `focus_changed`, `background` or `final`;
`null` is none. `a_line_is_one_object_with_t_g_and_mark` pins the format, and
`the_sample_the_tools_read_is_what_the_journal_writes` two whole bursts of it (below).

## The press

Every request the reactor sends the engine (a layout pass, a strip movement, a bounce, the debug
slide) carries its `Cause`: when the reactor took the event it answers and what that event was.
`Reactor::handle_event` stamps it as it takes each event, before anything else, and puts the outer
one back as it returns: a frame report that releases a drag handles the mouse up inside it, and the
report's passes after it are the report's again. `what` is the event's variant, `window_frame_changed`
or `mouse_up`, and for a layout or reactor command the command as the IPC names it, `move_focus`,
`switch_space` or `focus_window`. Every pass a command asks for carries the command.

So `press` to `begin` is the reactor's work on the event and the hop to the engine, and `press` is
where the time from a press to the overlay up starts, as the research measured it ("ms after rini
takes the press"). It is not the key-down: the tap and the reactor's queue come before it.

## A delivery's times

The pictures service stamps each delivery with when it sent it (`Delivered.sent`): for a round of
SkyLight captures, once its calls and band prints are done, as the cache and the schedule are handed
them; for the one ScreenCaptureKit capture of a clipped focus end, once that capture is back on the
service's thread. Each picture keeps when its capture started (`Picture::taken`), stamped before the
call that took it. On the probe's clock a `delivery`'s `t - sent_before_us` is the round's end and a
`delivered`'s `t - taken_before_us` its capture's start, so a delivery splits into two gaps:

- **Capture start to sent is the round.** For a frame-zero delivery, measured from the earliest
  picture's start, that is every SkyLight call of the round, the server frames of each later set read
  between them, and then the band prints.
- **Sent to the `delivery` mark is the wait for the engine.** The service putting the pictures in the
  cache and deciding, the hop to the `transitions` thread, and whatever the engine was doing when the
  delivery came. Composing the flight frame zero was asked for is one such thing; it ends at that
  generation's `merge {moved: false}`.

A delivery under a generation the flight has moved on from is marked all the same, with `of != g`.

## The clock

`t` is nanoseconds of mach absolute time (`CLOCK_UPTIME_RAW`), the clock focuslab's probes read as
`DispatchTime.uptimeNanoseconds`, so a probe's trial and rini's marks join on one clock. `Instant`
reads that clock on macOS: its `Debug` is the timespec (`instant_reads_the_probe_clock` pins it), and
over 20ms the two deltas differed by 41ns (a scratch probe, 2026-10-04, rustc 1.98.1, Darwin 25.6).
The writer takes an `Anchor` for each burst, `Instant::now()` and the uptime clock read together, and
converts every instant in it through that. The clock stops while the machine sleeps; the header's
`unix_ms` places a burst in the day.

## The ring

8192 marks in a `VecDeque` allocated once, each a `Stamped` of 72 bytes, its `Mark` 48 (measured
2026-10-05, rustc 1.98.1; at most 80 is checked at compile time), so about 0.6MB. A push is a copy into
it on the thread that already holds the event: no lock, no allocation, no syscall. It costs 2.1 to
3.8ns, the mean of 1M pushes over five runs of `a_push_costs_a_few_nanoseconds` (a release build,
2026-10-05). It is kept whether the journal is
written or not, so a glitch seen with it off is still in memory when it is turned on; at 40 to
100 marks a flight that is the last 80 to 200 flights (inferred, not counted). A burst longer than the
ring loses its oldest marks, and its header says how many.

## When it writes

While the journal is written, `Journalled` gives a burst, every mark since the last one, at each
`ReportSettled`: the quiet after a lift, or an abandon. It gives none between a `ReportUp` and its
`ReportSettled`, so nothing touches the disk while a transition is up. Marks pushed at rest, a look
turned with nothing flying, go with the next burst. On the `transitions` thread a write is one `Vec`
copied out of the ring and one channel send; the `journal` thread formats and appends.

The file is `~/Library/Logs/rini/transitions.jsonl` (`rini_core::paths::journal_file()`). Once it is
past 50MB the next burst renames it `transitions.1.jsonl`, over the one before, and starts it afresh,
so the record is at most two files.

The journal is written only once asked: `animations { journal; }` starts the writing, and
`journal false` or no `journal` node leaves it off. The config is read live, and the reactor tells
the engine at start and at each reload; the engine acts only on a change. Off drops the writer, whose
thread ends once it has written what it was sent. On starts it, and the ring is written at once, or at
the settle of the burst up; what the ring overwrote while off is not counted lost. Whether a lift was
held to be measured is its own `hide`'s to say, so a burst written as the journal turns on, of lifts
taken while it was off, says it truly.

A crash loses the marks not yet written: a panic hook cannot reach the engine's state.

## The handover, measured

`animations { journal { measure-handover; } }` measures every lift as "The handover, measured" in
`src/animation/docs/capture-overlay-research.md` did: the overlay's own picture just before it comes
down against the screen just after, over every window that ends wholly on screen, the ones the last
watch of the flight named (`landing` and `awaiting`), which are the windows the overlay is holed over.
It is off unless asked for, and `journal false` turns it off with the writing.

- **The engine.** A lift is held when the last watch named a window ending wholly on screen
  (`journal::measures`). At `Lift` the engine asks the pictures service for the check
  (`Pictures::check_handover`), tells `Journalled::lifted`, and holds back the hide and the tiles'
  release, and with them the bar's picture (`Settle`) and the quiet, which wait for the overlay to be
  drawn down: the bar is read off the screen and must not find the overlay there; a request that
  hides it holds its own transition next, which drops both (`Drawn::begun`). The rest goes out at the
  lift as ever. The hide comes when
  `Journalled::heard` says so: on the service's word for that lift's picture (`OverlayTaken`), on any
  request, since a press never waits for the overlay's picture, on the overlay about to move to
  another display or be laid out for another band or scale (a `SetDisplay`: ahead of a request onto
  another display, at a config reload and at the debug warm), so it never moves with the finished
  flight's tiles on it, or on `OVERLAY_TAKE_PATIENCE`. That
  is 60ms, past a cold SkyLight call: p50 43.3ms at an 800ms gap, 39-47ms cold ("A SkyLight capture
  costs what the time since the previous one says" in `src/pictures/docs/capture-research.md`). Its
  `hide` mark says whether it hid on its picture and how long it held. Only a lift that hid on its own
  picture, taken, is measured: on word that the picture was not taken, on a request, on the overlay
  moving or on the patience, the overlay may be gone from a picture taken later, and its measure, if
  one comes, is dropped.
- **The pictures service.** The overlay window alone is its next SkyLight call, ahead of any other
  (the number the renderer's `Committed` gives names it, and `window_origin` of
  the display the engine last laid the overlay out for says where it starts on screen), and
  it tells the engine `OverlayTaken` at once. `AFTER_LIFT` (45ms) after that picture, as the research
  waited after its hide, it takes the screen through `CGWindowListCreateImage` on its own thread. The
  research hid as its picture returned; here the hide waits for the hop to the `transitions` thread and
  its hop on to the main thread, so the screen comes that much less after it. A thread of the check's
  own, `handover`, draws the two whole
  and compares them over each window
  (`src/pictures/domain/handover.rs`: the research's sampling and thresholds), and the service tells
  the engine `Handover`, which the engine marks as it comes, usually before the quiet and so in the
  lift's own burst. The schedule holds the overlay's picture with the check, and a transition
  beginning drops both: the screen would show the next overlay. Neither picture is taken once a
  transition is asked and the pictures thread has not yet applied its hold: that press has hidden the
  overlay, and asks its frame zero of the same thread.
- **What it costs.** Each held lift's hide comes up to 60ms later, and the bar's picture and the quiet
  with it: its `hide`'s `waited_us` says how much. A press that lands while the pictures thread takes the overlay's picture or the screen waits
  for that one call, a SkyLight call or a `CGWindowListCreateImage` of the display, before its frame
  zero is taken. The screen capture goes through ScreenCaptureKit's delivery queue while the transition
  settles, the one capture besides a clipped focus end's that the hold lets through. The two pictures,
  a display's worth each, are drawn and compared on `handover`, off the pictures thread, and dropped
  once compared.

## Reading it

`tools/focuslab/journal.py` reads the file and the one before it (`tools/focuslab/README.md`). These
are the definitions it implements. Its tests check each one on
`tools/focuslab/testdata/journal-sample.jsonl`, two scripted flights, which
`the_sample_the_tools_read_is_what_the_journal_writes` pins to what `Journalled` gives and the writer
writes.

- **A flight starts at its `hold`.** The coordinator holds only at a fresh begin, and the `begin` and
  the `press` pushed just before it in its generation are its own. It runs to the next flight's start
  or its burst's end, so its lift, its quiet and its settle are its own too. A mark before a burst's
  first flight is in none, and a `delivered` whose `delivery` the ring overwrote is dropped.
- **Capture start** is a `delivered`'s `t - taken_before_us`; **round end** is a `delivery`'s
  `t - sent_before_us`.
- **The frame-zero delivery** is the last `delivery {frame_zero}` of the flight's generation before
  its `show`. Its earliest capture start is the earliest of its pictures', or its round end when it
  brought none. A `show` with none before it is a **frame-zero timeout**: the patience ran out.
- **The overlay drawn** is the first `drawn {shown: true}` after the flight's `show`: up at its flush,
  `t - flushed_before_us`, from a drain begun `applied_us` before that. The `show` is the coordinator's
  decision, a hop and a transaction before the overlay is up. A flight with no `drawn` mark at all,
  written before the renderer's commits were journalled, is drawn at its `show`; one with `drawn`
  marks and none showing it after its `show` never had the overlay up.
- **The focus target** is the `begin`'s `focus`.
- Percentiles are nearest-rank, printed as p50, p90 and p99 with their n.

`summary` splits press to overlay up in order, so the parts of each flight sum to it. It does so for
every flight with a press, a frame-zero delivery and the overlay drawn:

| Part | From → to |
|---|---|
| reactor | `press` → `begin`: the reactor's work on the event and the hop to the engine |
| frame zero asked | `begin` → `ask_frame_zero`: one drive, so 0 |
| service wait | `ask_frame_zero` → the earliest capture start |
| the SkyLight round | the earliest capture start → round end |
| engine wait | round end → the frame-zero `delivery`: the hop back and the engine busy. `compose` says how much of it was composing |
| the show | the frame-zero `delivery` → `show`: the engine putting the pictures on their tiles, and the wait for a second frame-zero ask when a merge made one |
| the hop to the renderer | `show` → the overlay drawn's drain begun: the rest of the event that gave the show, and its batch's wait for the renderer |
| the show's commit | that drain begun → its flush: its `applied_us` |

Then the rows of "Where a moving focus change spends its time"
(`src/animation/docs/capture-overlay-research.md`), in its order, ms after the press, and the rest:

| Row | From → to |
|---|---|
| compose | `begin` → the first `merge {moved: false}` of its generation, which is not after the press |
| frame-zero capture starts | `press` → the earliest capture start |
| frame-zero pictures ready | `press` → round end |
| show given | `press` → `show` |
| overlay up | `press` → the overlay drawn's flush |
| rini sees it landed | `press` → the capture start of the focus target's first picture under a `delivery {landed}` |
| focused picture captured | `press` → the capture start of its first under a `delivery {focus_changed}` |
| show to landed | the start of the generation the flight lifted in (its `show`, or the `merge` that began it) → the last end window's first capture start under a `delivery {landed}` of that generation. None unless as many windows landed as that generation's last `watch` named (`landing` and `awaiting`) |
| lift; lift to settled | `press` → `lift`; `lift` → the `report_settled` after it, none if another flight began first |

Then each drain drawn, every `drawn` mark of the flights: `queued_us`, `applied_us` and
`flushed_before_us`, in ms.

The counts, per flight: merges (the generations after its first), swaps, snaps, stale deliveries
(`of != g`), `lost` marks, overdue lifts (a `lift` with `at_rest` or `landed` false) and frame-zero
timeouts. `--csv` writes the split, the rows and the counts, one row a flight; the drains are only
printed.

`probe` joins each line a probe wrote to the first `press` within 250ms after the probe's `t0_ns`,
its key post, and groups the trials by the probe's `label`. A trial whose press joined a flight
already up gives only post to press. One whose flight has a focus target other than the probe's
`to_wid` is set aside: a window's `idx` is its window server id, which is what the probe names. For
the rest it gives:

- post to press: the tap and the reactor's queue;
- post to show given and post to landed;
- rini's landed against the probe's `still_ms`, when the probe saw the target's server frame final;
- the focused picture against `look_full`, when the probe saw the titlebar band reach 80% of its
  final change (`e1.py`'s `look`);
- for an `overlay.swift` line, its `up_ms` against post to show given, which is the hop to the
  renderer, the show's commit and the window server showing the overlay after `show`, and its
  `lift_ms` against post to lift.

`handover` reads every `handover` mark. Each window takes the kind and focus target of the generation
that lifted, as the last `begin` or `merge` in it says, and is the focused window when it is that
target. It prints the lifts measured against every lift held for the overlay's picture (each
`hide`), how long each held, and interior, edge in and edge out by kind, focused window and other.
The headline is edge in p99 of the focused window on `pan` flights, the row to read against "The
handover, measured" in `src/animation/docs/capture-overlay-research.md`.

## What it does not record yet

- The key-down's own time (its `CGEvent` timestamp), which would split the tap and the reactor's
  queue from the reactor's work.
- How long the engine takes to carry out a drive's commands. What the renderer draws of them is
  timed by `drawn`.
- CPU and GPU use. Kai allowed it if it costs nothing; what the IOKit read costs is not measured, so
  it waits for that measurement.
