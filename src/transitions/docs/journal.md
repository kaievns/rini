# The transitions journal

Every step of every transition, kept in memory and written once it settles, so a glitch seen while
moving around can be read back afterwards. Kai's ask, and what rini must do about it, are in
`specs/animation.md` ("Watching transitions: the journal"). The marks, the ring and the lines are
`src/transitions/domain/journal.rs`; the thread that writes them is
`src/transitions/platform/journal.rs`.

## Where a mark comes from

`Journalled` holds the coordinator and its journal as one pure unit. The engine's one call into the
lifecycle, `FlightEngine::drive`, goes through `Journalled::drive`: it reads the event's own marks,
the coordinator takes the event, and then those marks and one for each command it gave are pushed,
every one stamped with the drive's `now` and the generation the drive left (`g`). No mark call sits
anywhere else, and the engine never reads the journal back: of what a drive returns, it carries out
the commands and sends the writer the burst, if one is due.

Both matches, over `Event` and over `Command`, have no `_` arm, so a new variant does not compile
until it has a mark.

| Source | Mark | Fields |
|---|---|---|
| `Event::Begin(stage)` | `begin` | `kind`, `coalesced` (the stage waits for the reactor's follow-up passes), `tiles`, `awaiting`, `repainting`, `focus`, `losing_focus`, `key`, `duration_ms` |
| `Event::Merge { stage, moved }` | `merge` | `kind`, `moved`, `tiles`, `awaiting`, `repainting`, `focus`, `focused_look`, `duration_ms` |
| `Event::Extend(d)` / `Abandon` / `Quiet(g)` | `extend` / `abandon` / `quiet` | `ms` / none / `of` |
| `Event::Tick { settled, landed }`, once the clock has run out | `at_rest`, `landed`: each once a generation, at its first `true` | |
| `Event::Lost { generation, window }` | `lost` | `of`, `window` |
| `Event::LooksTurned(windows)` | `looks_turned`, one for each window | `window` |
| `Event::Picture`, `Event::FrameZero` | none | |
| `Hold` / `Settle` / `Release` | `hold` / `settle` / `release` | |
| `AskFrameZero` | `ask_frame_zero` | `strip`, `background`, `behind` (counts) |
| `Show` / `StartMoving` / `StopWatches` | `show` / `start_moving` / `stop_watches` | |
| `Write` / `Holes` / `Warm` / `Refresh` | `write` / `holes` / `warm` / `refresh` | `windows` (a count) |
| `Watch(EndWatch)` | `watch` | `landing`, `awaiting`, `background` (counts), `focus` |
| `AskFinal` | `ask_final` | `windows`, `behind` (counts) |
| `Swap` / `KeepFocusedLook` | `swap` / `keep_focused_look` | `of`, `window` |
| `Snap { window, frame }` | `snap` | `window`, `frame` (overlay points) |
| `HandOverFocus { to, from }` | `hand_over_focus` | `to`, `from` |
| `Lift` | `lift` | `at_rest`, `landed`, as the tick that lifted it read them: either `false` is `LIFT_GRACE` run out |
| `ReportUp` / `ReportSettled` | `report_up` / `report_settled` | |

How to read them:

- **`g` is the generation after the event.** A merge that starts a generation is marked in it. `of`
  is the generation a delivery, a loss, a swap or a quiet was under; `of != g` is stale.
- **`at_rest` and `landed` are the lift's condition, not when each window landed.** The engine reads
  where the tiles are drawn and where the windows are only once the clock has run out
  (`Coordinator::wants_landing`) and tells the coordinator `false` before, so a tick before then
  marks nothing.
- **A command's mark says the coordinator gave it,** stamped with its drive's `now`. Carrying it out
  is not timed.
- **Marks are in the order their drives ran.** A drive nested in carrying out a command
  (`StartMoving`'s `Extend`) comes after every command of the drive it is nested in. Group by `g`.
- **`Picture` and `FrameZero` have no mark:** they restate a delivery of the pictures service, and the
  delivery is to be the record. Deliveries are not journalled yet (below).

## A line

One JSON object a line. Each burst opens with a header, then one line a mark, oldest first:

```
{"t":81234599990000,"mark":"burst","journal":1,"unix_ms":1791043200123,"lost":0}
{"t":81234567890123,"g":42,"mark":"begin","kind":"pan","coalesced":false,"tiles":3,"awaiting":0,"repainting":0,"focus":[812,3104],"losing_focus":[812,3090],"key":[812,3090],"duration_ms":350}
{"t":81234567890123,"g":42,"mark":"hold"}
{"t":81234570012345,"g":42,"mark":"snap","window":[812,3104],"frame":[3.0,35.0,860.0,1079.0]}
```

The header's `t` is when the burst was written, `journal` the format's version (1), `unix_ms` the wall
clock then, and `lost` how many marks the ring overwrote before they were written. A window is
`[pid, idx]`, its `idx` the window server's id; a rect is `[x, y, w, h]` as `f32`; a generation is a
number; `null` is none. `a_line_is_one_object_with_t_g_and_mark` pins the format.

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
written or not, so a glitch seen with it off is still in memory when it is turned back on; at 40 to
100 marks a flight that is the last 80 to 200 flights (inferred, not counted). A burst longer than the
ring loses its oldest marks, and its header says how many.

## When it writes

While the journal is written, `Journalled` gives a burst, every mark since the last one, at each
`ReportSettled`: the quiet after a lift, or an abandon. It gives none between a `ReportUp` and its
`ReportSettled`, so nothing touches the disk while a transition is up. Marks pushed at rest, a look
turned with nothing flying, go with the next burst. On the main thread a write is one `Vec` copied out
of the ring and one channel send; the `journal` thread formats and appends.

The file is `~/Library/Logs/rini/transitions.jsonl` (`rini_core::paths::journal_file()`). Once it is
past 50MB the next burst renames it `transitions.1.jsonl`, over the one before, and starts it afresh,
so the record is at most two files.

`animations { journal { off; } }` stops the writing. The config is read live, and the reactor tells
the engine at start and at each reload; the engine acts only on a change. Off drops the writer, whose
thread ends once it has written what it was sent. On starts it, and the ring is written at once, or at
the settle of the burst up; what the ring overwrote while off is not counted lost.

A crash loses the marks not yet written: a panic hook cannot reach the engine's state.

## What it does not record yet

- The press: when the reactor took the event a request answers.
- Deliveries: when the pictures service sent each, and when each picture in it started its capture.
- How long carrying out a drive's commands takes.
- CPU and GPU use. Kai allowed it if it costs nothing; what the IOKit read costs is not measured, so
  it waits for that measurement.
