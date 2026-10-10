# The resize recordings, 2026-10-09

The journals and press lists behind the 2026-10-09 resize findings: the first two Reported lines of
`specs/animation.md` "Resizing", and the n1 timings in "A grow waits for its repaint"
(`src/transitions/docs/coordination.md`). The movies are not kept: they show Kai's screen, 330MB
for the three.

Each run is two files. `<run>.journal.jsonl` holds the lines rini's transitions journal wrote while
the run recorded, read with `tools/focuslab/journal.py`. `<run>.mov.json` is what `screenrec` wrote
beside the movie: `scale`, `presses` (the key code, the modifier mask and the milliseconds since
recording began, 524288 being Option) and `frames_ms`, each frame's time.

## The runs

| Run | Build | Lab windows on `work` | Presses |
|---|---|---|---|
| `s1` | 71217e6b, before the rework | stack [lab-C, lab-D] | 8 |
| `s2` | 71217e6b, before the rework | stack [lab-C, lab-D, lab-B] | 11 |
| `n1` | b4c187a2, grows flown at once, no hold | stack [lab-C, lab-D] | 8 |

The lab windows are Ghostty windows running `tools/focuslab/pattern.py`, each row numbered at both
ends. The key codes are Kai's binds: 43 `,` folds and unfolds to the left, 3 `F` toggles full
width, 34 `I` and 40 `K` move focus up and down the stack.

s1 and n1 press the same sequence:

| Press | Key | What it does |
|---|---|---|
| 1 | 43 | folds lab-D under lab-C |
| 2 | 3 | maximizes the bottom row, lab-D |
| 3 | 3 | restores it |
| 4 | 34 | focuses lab-C |
| 5 | 3 | maximizes the top row, lab-C |
| 6 | 3 | restores it |
| 7 | 40 | focuses lab-D |
| 8 | 43 | unfolds lab-D out of the stack |

## What the frames showed

Read from frame sheets cut from the movies with `tools/focuslab/sheet.sh`.

| Run, press | Seen |
|---|---|
| s1 1 | lab-C, shrinking, shows rows 29 down under its title bar; the rows above them are sliced out |
| s1 2 | lab-D, the bottom row, holds its content against the bottom with the reveal at the top; the siblings slide up off the screen |
| s1 3, s2 5 | the siblings come down from above; a tenth of the way in, lab-D's small picture is cut in and stretched, glyphs 1.5 to 2 times their size |
| s1 5 | lab-C, the top row, unrolls its content downward from under its title bar |
| s1 8 | lab-D, unfolded, flies a half-repainted picture: its old rows at the top of a blank surface |
| n1 1-8 | nothing stretched; each row holds its content against the edge that moves less; the siblings slide sideways |

## What to know before reading them

**`frames_ms` lists frames the movie dropped.** `screenrec` recorded a frame's time whether or not
the writer took the frame. s1 lists 260 frames against 256 in its movie, n1 238 against 234, s2 341
against 341 (`ffprobe -count_frames`). Pacing read from `frames_ms` is off by those frames.
`tools/focuslab/screenrec.swift` now lists only the frames written.
