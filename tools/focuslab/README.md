# focuslab — timing a focus change from outside rini

Swift probes that time focus changes, window placement and SkyLight captures on one clock (mach
absolute time), without going through rini. They are what the numbers in
`src/animation/docs/capture-overlay-research.md` ("Where a moving focus change spends its time") and
`src/pictures/docs/capture-research.md` ("A SkyLight capture costs what the time since the previous
one says") were measured with, and the regression check for any change to how a transition is
sequenced. The timing battery (`battery.sh`, below) is the check for any change to what a flight
costs: it flies a fixed set of pans and switches, records rini's overlay and reads the journal.
`playback.swift` is the exception: it reads what Core Animation draws, for "The score" in
`src/animation/docs/animation-smoothness.md`. `journal.py` reads rini's own record of each
transition, on the same clock, and joins it to what a probe saw. `screenrec` is the other exception:
it films the whole display, for how a resize looks ("Watching a resize", below).

## The probes

| file | measures |
|---|---|
| `focuslab.swift` | one press or call per trial: the window server's front and key-focus process, watched windows' bounds (1ms poll), and the titlebar band through an `SCStream` at 120fps, timed by each frame's `displayTime`. Modes `key` (an Option+key, Kai's Mod, through rini's tap), `click`, `makekey`, `makekey_only`, `activate` |
| `spacing.swift`, `coldsize.swift`, `gpuwarm.swift`, `qos.swift`, `keepalive.swift` | a SkyLight capture's cost against the gap since the last one, picture size, a GPU or CPU warm-up, the caller's QoS, keep-alive captures |
| `par.swift` | SkyLight captures in parallel, on the main and on separate connections |
| `overlay.swift` | per Option+key press, when rini's overlay window turns opaque and back (its alpha in the window list, 1ms poll): press to overlay up, and the lift, one JSON line a press (`label`, `t0_ns`, `up_ms`, `lift_ms`) for `journal.py probe`. It does not watch for foreign input |
| `playback.swift` | what Core Animation draws for the overlay's animations at chosen media times, against the curves `Score` evaluates; touches nothing on screen and needs no grant |
| `rec.swift` | the battery's recorder: rini's overlay window alone (rini's window at level 18) through an `SCStream` at 60fps while posting tagged Option presses, one JSON line a press and a frame. It writes numbers only, never a picture |
| `screenrec.swift` | the main display, rini's overlay and the real windows beneath it, through an `SCStream` at 60fps into an H.264 movie while posting tagged presses; beside it a JSON of each press's time, each frame the movie holds and each one the writer dropped. `screenrec.sh` runs it |
| `inputwatch.swift` | the battery's guard: a listen-only tap over keys, clicks, moves and scrolls; the first event the battery did not post writes a flag file and exits |
| `poke.swift` | posts one untagged released Option, to check that `inputwatch` stops a battery |

`e1.py` reads the JSON lines `focuslab` writes for the strip's focus change; `journal.py probe` reads
the titlebar band through its `look`.

Build one with `swiftc -O <file>.swift -o <name>`. They need Accessibility and Screen Recording for
the terminal they run from.

## Running them

The probes press keys, click, focus and move windows. Start a run only after the machine has been
idle for at least five minutes (`ioreg -c IOHIDSystem`, `HIDIdleTime`), and put the workspace and
focused window back afterwards. Posted events and rini's own cursor warp both reset the idle time, so
`focuslab` compares it against the time since its own last post rather than a fixed floor. The
capture probes (`spacing`, `coldsize`, `par`, …) touch nothing on screen.

## The battery

`battery.sh [options] <label>` is one run of the timing battery against the rini the service runs
(`target/release/rini` of the main checkout, whichever worktree the script is run from). It flies
three segments, each press 1500ms after the last:

| segment | what it presses | recording |
|---|---|---|
| `comms_pan` | focuses the `Code` window on `comms`, then 8 pans, Alt+J and Alt+L alternating | `rec-comms_pan.jsonl` |
| `switch` | 4 workspace switches from there, Alt+; and Alt+P alternating | `rec-switch.jsonl` |
| `spare_pan` | focuses the `Messages` window on `spare`, then 6 pans, Alt+J and Alt+L alternating, 3 of them into `Microsoft Outlook` | `rec-spare_pan.jsonl` |

The windows are found by app and workspace name in `rini-cli query workspaces`, the first window of
the app the workspace lists (`battery.py window`). `--comms`, `--comms-app`, `--spare`,
`--spare-app` and `--blur-app` name others; the defaults are Kai's setup. `--pane x0,y0,x1,y1` is
the box of the overlay, in points, the blur check reads: by default Outlook's folder pane where its
window sits on `spare`. `battery.sh --check` resolves the windows, prints them and posts nothing.
The presses are Kai's binds `focus-column-left`, `-right`, `focus-workspace-down` and `-up`,
posted as the key codes rini reads them by (QWERTY J, L, ; and P).

**It refuses a run of another scene** (exit 2). Each pan segment, once its window is focused and
before its first press, reads its workspace (`battery.py scene`): the focused window must be the
workspace's leftmost column, so the first Alt+J bounces, and with `--scene FILE`, a run's `meta.json`
or a scene `battery.py scene` printed, every window must be where that scene had it, by app, within
1pt, a parked one by its size alone: macOS clamps a park's corner anywhere from y 1085 to 1116, and
where depends on the launch. The scene each segment flew is kept in the run. On 2026-10-10, `new-1`..`3` flew comms with its
columns mirrored and one of them stacked, their first press a pan, and were judged against runs
that bounced first.

**It is a screen test, run only while Kai is away.** It posts key presses and focus commands, and
the pans and switches move their windows. It refuses (exit 2) unless the HID idle time is at least
300s, or no input has come since the last battery, `measure.sh` session or `screenrec.sh` recording
marked the machine quiet (`<out>/.quiet_since`). `inputwatch` watches throughout: the first event
the battery did not post, a cursor warp aside, stops it before its next press (exit 3). Run or
stopped, it puts focus back on the window focused at its start, or on the `comms` window when none
was.

**No picture is written.** `rec` reduces each frame to numbers as it arrives: the share of a 64x40
grid with any alpha, a signature of its colours (an unchanged picture repeats it) and the pane
box's mean colour and chroma. Nothing of the screen is kept.

It reads the journal, so rini's config needs `animations { journal }`. Each run goes in
`<out>/<label>/` (`--out`, default `/tmp/rini-focuslab`). `rec` and `inputwatch` are built on first
use into `~/Library/Caches/rini-focuslab/`, and again when their source changes. A run holds:

- `meta.json`: the label, rini's pid, the deployed binary's sha256 (`binary_sha`), the main
  checkout's HEAD at the start (`repo_head`, which is not the binary's commit), the windows flown
  (`windows`), the pane box, the focus at the start, why it stopped, if it did (`aborted`), and the
  scene each pan segment flew (`scene`, by segment), which `--scene` takes as a reference;
- `scene-<segment>.json` and `.err`, what `battery.py scene` printed for it;
- `rec-<segment>.jsonl`, `rec-<segment>.out`, `inputwatch.out`;
- `journal.slice.jsonl`: the journal lines written during the run, with `journal-summary.txt` and
  `pictures-target.txt` from `journal.py`;
- `metrics.json` and `summary.txt`, from `analyze.py <rundir>`.

### Reading metrics.json

`analyze.py` joins each press it posted to the flight whose press mark comes first within 250ms
after it, as `journal.py probe` does, and to the frames recorded until the next press.

- `by_kind.<pan|switch>`: per flight kind, `overlay_up` (press to overlay up) and `lift` (press to
  lift) from the journal, `screen_up_after_post` and `screen_gone_after_post` from the recording,
  each `n`, `p50`, `p90`, `p99` and `max` (nearest rank, ms); `overdue_lifts`,
  `frame_zero_timeouts`.
- `drains_all`: every drain of the joined flights, `queued` (waiting for the renderer), `applied`
  (played to its flush), `heard` (flushed to heard) and `ground` (reading the grounds of the tiles it
  drew on the crop grid), defined in `src/transitions/docs/journal.md`.
- `crops`: the drains of the joined flights that drew any tile on the crop grid (`drains`), the
  times they filled a crop grid (`fills`, one tile counting once a fill), and the grounds read over
  those drains alone (`ground`, ms). A navigation flying only pictures that fit its frames has none.
- `drains_by_phase.<show|flight|lift>`: every drain in the slice by its `shown` (true, null, false),
  `queued` and `applied`. The place drain ahead of a lift falls in `flight`.
- `segments.<segment>` and `pacing_all`: the gaps between the frames the overlay showed, counted
  over 20, 25 and 33ms, and black frames (opaque, all black). A frame is shown while it has alpha
  and is not all black; the first frame after that is the overlay gone.
- `blur`: each fly-in to the blurred window (`--blur-app`), its pane box over its last 3 frames
  shown; it passes at a mean chroma of 5.0 or more. Of the 99 fly-ins in
  `docs/data/2026-10-08-ring-perf/`, 98 read 6.94-7.11 and one 4.18.
- `flights`: one row a press, with how it joined (`began`, `joined` a flight already flying, or
  `unmatched`).

### Reading the verdict

`compare.py BASE... -- CAND... [--stage2 BASE... -- CAND...]` judges runs of one configuration
against another, never warm-ups: the ring on (CAND) against the ring off (BASE) on one binary in one
session, or a new binary's ring off against an old one's for erosion. It prints one CSV row a
metric, 87 of them, and the verdict on stderr; exit 0 is `ACCEPTED`, 1 not, 2 a usage error and 3
runs of different scenes.

| group | metrics | n |
|---|---|---|
| `pan.*`, `switch.*` | `overlay_up`, `lift`, `screen_up_after_post`, `screen_gone_after_post`, each p50, p90, max; `overdue_lifts`; `frame_zero_timeouts` | 28 |
| `drains.*` | `queued`, `applied`, `heard`, each p50, p90, p99, max | 12 |
| `phase.*` | `show`, `flight`, `lift`, each `queued` and `applied` at p50, p90, max | 18 |
| `pacing.*` | `comms_pan`, `switch`, `spare_pan`, `all`, each frame interval p50, p90, p99, max, frames over 25ms, over 33ms, black frames | 28 |
| `blur.failed` | fly-ins that failed the blur check | 1 |

Every metric is lower-is-better. One passes when the median of its CAND runs is at most the max of
its BASE runs; below the spread passes too. Blur passes only at 0 failed fly-ins in every run
given, of both sides, and is missing when a run flew into no blurred window. A metric failing that
needs stage 2: 3 more runs of each, after a warm-up, given with `--stage2`, and the same rule over
all 6 against all 6. One failing stage 2 is a regression, to be found and fixed or reported to Kai.
With 3 against 3 and no real difference, the median lands above the max 20% of the time; the two
stages cut that to 3.8% a metric (200k simulated trials, the ring-perf work of 2026-10-08).

**It judges runs of one scene only.** Before any metric, every run given must fly what the first
does: each segment's flights of the same kinds in press order (`flights` in `metrics.json`) and the
same frames delivered (`delivered` in the journal slice), within 1pt. One that does not is named with
how it differs, and `compare.py` exits 3 judging nothing. A run whose tiles wore a picture taken over
5s before (`wears`, `taken_before_us`) is flagged on stderr with how many and the oldest, and judged
all the same. The 5s flags every run of 2026-10-11 (`docs/data/2026-10-11-resize-battery/`): 25-37
pictures a run on either build, the switch flying other workspaces' windows from the cache; `n1`, the
first after a launch, wore one 211s old, which the oldest tells apart.

**Two binaries are judged interleaved**, one run at a time: deploy one (`measure.sh --deploy`), run,
deploy the other, run, and so on, every run after the first given `--scene` of the first, so drift
in the machine through a session lands on both sides alike. Stage 1 is the first 3 of each, stage 2
all 6 against all 6.

Each row: `base_min`, `base_max`, `cand_median`, `cand_runs`, `stage1` (`pass`, `FAIL`, `missing`),
the 6-run `stage2_base_max` and `stage2_cand_median` when stage 2 ran, and `verdict`: `pass`,
`needs stage 2`, `regression` or `missing`. The verdict counts each and names every metric not
passing. The 2026-10-08 runs and their verdicts are in `docs/data/2026-10-08-ring-perf/`. The
2026-10-11 runs, the resize work against the build before it, are in
`docs/data/2026-10-11-resize-battery/`: 81 of 87 measures pass and 6 fail stage 2, none by more
than 3ms, within what the rule fails one build against itself.

### Measuring a binary

`measure.sh <binary> <prefix> [battery options]` is one comparison, start to end: it turns the ring
off in `~/.config/rini/config.kdl` (`battery.py ring`, which touches only `layout`'s `focus-ring`
block and refuses a config without exactly one), deploys `<binary>`, waits for rini to list the
windows and 15s more, runs `<prefix>-off-warm` and `-1`..`-3`, turns the ring on, runs
`<prefix>-on-warm` and `-1`..`-3`, and puts the config back as it found it. It stops at the first
battery that refuses or aborts. It needs 300s of idle to start.

The deploy is `.claude/rules/deploying-rini.md`'s, and `measure.sh --deploy <binary>` does it alone.
It refuses a binary not signed as `git.kaievns.rini` with a certificate leaf, keeps the deployed one
in `<out>/rollback/`, copies the new one in as `target/release/rini.new` and renames it over
`target/release/rini`, checks the signature and that the plist runs that path, runs
`target/release/rini service restart`, and checks that the new process runs from that path. Any
check failing stops it with the line to roll back. It builds nothing and never passes `--move`:
build and sign the binary in a worktree with its own `CARGO_TARGET_DIR` first.

## Watching a resize

How a resize looks, its content against the edges, its ground and the swap to the real window at
the lift, is seen only in a film of the whole screen: `rec` sees the overlay alone.
The lab is a few Ghostty windows full of a ruler, recorded while posting the resize keys.

| file | does |
|---|---|
| `pattern.py <letter>` | fills its terminal with a ruler redrawn at every size: row `n` reads `<letter><n>` at both ends, the first `TOP` and the last `BOT`, and every 5 rows change colour, so a frame shows which rows a tile draws, where it cuts and whether glyphs stretch |
| `lab.sh <workspace> [letter...]` | switches to the workspace and opens a Ghostty window `lab-<letter>` running `pattern.py` per letter, A to D by default, then prints the lab windows' ids and frames |
| `screenrec.sh [--out DIR] <name> <code:mods:gap_ms>...` | one recording, `<name>.mov`, `.mov.json` and `.journal.jsonl` in `DIR` (`/tmp/rini-focuslab/screenrec` by default), building `screenrec` and `inputwatch` into `~/Library/Caches/rini-focuslab/` as the battery does |
| `sheet.sh <run.mov> <press> [dur_ms] [x,y,w,h] [cols] [width]` | one press's frames from 20ms before it, beside the movie, and a contact sheet of them; the crop is in points |
| `screenrec.py` | what `lab.sh` and `sheet.sh` read, and `screenrec.py blank <run.mov>`: each frame's time and its flat dark blocks below the bar, each 1/54 of the frame's width (32pt on a 1728pt-wide display), the ground a resize shows, read against the count of a frame with nothing moving; tested by `test_screenrec.py` |

A press is `<key code>:<mods>:<gap after it, ms>`, mods any of `a` (Option), `s`, `c`, `m`, or `-`.
Kai's fold left is `43:a`, full width `3:a`, focus up and down the stack `34:a` and `40:a`.

**It is a screen test, run only while Kai is away**, with the battery's guard: `screenrec.sh`
refuses unless the HID idle time is at least 300s or no input came since the last battery or
recording marked the machine quiet, stops on the first input it did not post (exit 3), and puts
focus back, on an interrupt too. Like a battery, a recording no input stopped marks the machine quiet
as it ends, interrupted or not, so a battery can follow it at once. `lab.sh` opens windows and switches workspace, so it waits for the same. Ghostty 1.3
asks once whether it may run `pattern.py`. Close the lab windows with `kill -9` on their pids, which
saves no window state.

**The movie shows Kai's screen.** It stays out of the repo; what a run is read for, the journal
and the `.mov.json`, goes in `docs/data/`. The 2026-10-09 runs are in
`docs/data/2026-10-09-resize/`, the 2026-10-10 rapid Alt-R run in `docs/data/2026-10-10-rapid-altr/`,
and the 2026-10-10 fold and unfold runs, `fold1` on e050e168 and `fold2` on d9461781, in
`docs/data/2026-10-10-fold/`.

## Reading rini's journal

`journal.py` reads the journal rini writes once each flight settles,
`~/Library/Logs/rini/transitions.jsonl` and the `transitions.1.jsonl` before it, or the files it is
given:

- `journal.py summary [--kind pan] [--csv flights.csv]`: where the time from a press to the overlay up
  goes, the research doc's rows after the press, show to landed, each drain the renderer committed and
  the counts per flight; `--csv` writes one row a flight.
- `journal.py probe <probe.jsonl>`: each line `focuslab` or `overlay` wrote, joined to the press it
  caused on the one clock, grouped by its label.
- `journal.py handover`: with `animations { journal { measure-handover; } }`, each lift's overlay
  against the screen after it, by kind, focused window and other, the rows of "The handover, measured"
  in `src/animation/docs/capture-overlay-research.md`, and the band outside the outline where a
  window wore one at rest.
- `journal.py pictures [--target]`: every picture a tile took, flight by flight, as the flight
  composed it, from frame zero, as a focused look or swapped in: its route, whether it carries its
  window's blur, and its age; `--target` keeps the focus targets alone.
- `journal.py turns`: every turn of rini's main thread over 8ms, by flight or at rest: how long it
  ran, each actor's share, the time outside them, and the window changes made just before it.

What each row measures is defined once, in `src/transitions/docs/journal.md` ("Reading it"). The
tests read `testdata/journal-sample.jsonl`, which rini's own test pins to what the journal writes, and
`testdata/probe-sample.jsonl`; `test_battery.py` reads the same sample as a battery's journal slice,
with recordings it builds. `cargo test` runs them (`tests/focuslab.rs`, which skips with a word
when there is no `python3`); by hand, from the repo root,
`python3 -m unittest discover -s tools/focuslab`. `python3 -m unittest tools/focuslab/test_journal.py`
cannot import the `journal.py` beside it.
