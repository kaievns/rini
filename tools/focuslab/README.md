# focuslab — timing a focus change from outside rini

Swift probes that time focus changes, window placement and SkyLight captures on one clock (mach
absolute time), without going through rini. They are what the numbers in
`src/animation/docs/capture-overlay-research.md` ("Where a moving focus change spends its time") and
`src/pictures/docs/capture-research.md` ("A SkyLight capture costs what the time since the previous
one says") were measured with, and the regression check for any change to how a transition is
sequenced.
`playback.swift` is the exception: it reads what Core Animation draws, for "The score" in
`src/animation/docs/animation-smoothness.md`. `journal.py` reads rini's own record of each
transition, on the same clock, and joins it to what a probe saw.

## The probes

| file | measures |
|---|---|
| `focuslab.swift` | one press or call per trial: the window server's front and key-focus process, watched windows' bounds (1ms poll), and the titlebar band through an `SCStream` at 120fps, timed by each frame's `displayTime`. Modes `key` (a Ctrl+key through rini's tap), `click`, `makekey`, `makekey_only`, `activate` |
| `keys.swift` | bursts of Ctrl+key presses, for rapid-press runs |
| `axlag.swift` | an AX position write on a parked window, and when `SLSGetWindowBounds` and `CGWindowList` show it |
| `spacing.swift`, `coldsize.swift`, `gpuwarm.swift`, `qos.swift`, `keepalive.swift` | a SkyLight capture's cost against the gap since the last one, picture size, a GPU or CPU warm-up, the caller's QoS, keep-alive captures |
| `par.swift` | SkyLight captures in parallel, on the main and on separate connections |
| `cap1x.swift`, `ca.swift` | capture cost at 2x and with bit 9; Core Animation's commit cost for a whole composite, a crop, and copies |
| `overlay.swift` | per Ctrl+key press, when rini's overlay window turns opaque and back (its alpha in the window list, 1ms poll): press to overlay up, and the lift, one JSON line a press (`label`, `t0_ns`, `up_ms`, `lift_ms`) for `journal.py probe`. Neither overlay probe watches for foreign input |
| `overlayrec.swift` | rini's overlay window alone through an `SCStream` while posting presses, one PNG per frame named by its milliseconds since the first press: what the tiles show mid-flight, whatever the screen shows beneath |
| `playback.swift` | what Core Animation draws for the overlay's animations at chosen media times, against the curves `Score` evaluates; touches nothing on screen and needs no grant |

`e2.sh` runs `focuslab` on the strip's focus change in labelled blocks, against whichever build rini
runs, and `e1.py` reads its JSON lines.

Build one with `swiftc -O <file>.swift -o <name>`. They need Accessibility and Screen Recording for
the terminal they run from.

## Running them

The probes press keys, click, focus and move windows. Start a run only after the machine has been
idle for at least five minutes (`ioreg -c IOHIDSystem`, `HIDIdleTime`), and put the workspace and
focused window back afterwards. Posted events and rini's own cursor warp both reset the idle time, so
`focuslab` and `keys` compare it against the time since their own last post rather than a fixed
floor. The capture probes (`spacing`, `coldsize`, `par`, …) touch nothing on screen; `axlag` moves a
parked, off-screen window by a point and back.

## Reading rini's journal

`journal.py` reads the journal rini writes once each flight settles,
`~/Library/Logs/rini/transitions.jsonl` and the `transitions.1.jsonl` before it, or the files it is
given:

- `journal.py summary [--kind pan] [--csv flights.csv]`: where the time from a press to the overlay up
  goes, the research doc's rows after the press, show to landed and the counts per flight; `--csv`
  writes one row a flight.
- `journal.py probe <probe.jsonl>`: each line `focuslab` or `overlay` wrote, joined to the press it
  caused on the one clock, grouped by its label.

What each row measures is defined once, in `src/transitions/docs/journal.md` ("Reading it"). The
tests read `testdata/journal-sample.jsonl`, which rini's own test pins to what the journal writes, and
`testdata/probe-sample.jsonl`. Run them from the repo root with
`python3 -m unittest discover -s tools/focuslab`; `python3 -m unittest tools/focuslab/test_journal.py`
cannot import the `journal.py` beside it.
