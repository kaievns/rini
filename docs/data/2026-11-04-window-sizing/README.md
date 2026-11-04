# Window sizing, 2026-11-04

The inputs and outputs behind `docs/window-sizing-research.md`. Run each script with `python3 -I`.

- `slots.py` parses `~/.rini/layout.ron`'s launch slots into `slots.csv`, one row per slot, with a
  window's title reduced to whether it is a focuslab window (`lab`). Run 2026-11-04 20:45; the file
  parsed to the same 133 rows it held at 18:17, when the research first read it.
- `slots.csv` is that output: app, arrangement, display, workspace, stored width, `width_dragged`.
- `recount.py slots.csv` gives `recount.txt`: the slot counts, the width kinds and the class table
  of the research's §1, with the two arrangements holding a 1920x1200 display left out, as Kai
  asked.
- `presses.py` and `births.py` read `~/Library/Logs/rini/transitions.jsonl` up to the research's
  baseline, `head -n 507423`, and give `presses.txt` and `births.txt`: width presses by day, birth
  events, and presses after a birth (§4).
- `presses-by-day.csv` is `presses.txt`'s width presses per day, split by `what`, with the
  focuslab days marked.
- `resets.csv` is §4's table of resets, each tied by hand to its line of
  `/tmp/rini_kaievns.err.log`, the user wakes of `pmset -g log`, `ps -o lstart` and
  `last reboot`. Those sources were read on 2026-11-04 and are not kept here.
- `fit.py` gives `fit.txt`: the displays, the point widths Kai's fractions imply, the snap test in
  points and in millimetres, Model B's cells, the best targets, the display-width rules and the
  round trips (§2 and "Models A, B and C"), with the 5K2K at 3360x1418 as Kai stated.
- Not kept: §4's count of presses within 3 minutes after a wake (7 of 14 width presses, 61 of 253
  others), which was taken from `pmset -g log` and the journal with no saved script.
