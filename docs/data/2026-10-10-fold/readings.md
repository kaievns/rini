# The fold and unfold recording, 2026-10-10

The journal, press list and block counts behind the fold1 finding: the last 2026-10-10 Reported
line of `specs/animation.md` "Resizing", and a shrink starting off the display keeping its cached
picture in "A grow waits for its repaint" (`src/transitions/docs/coordination.md`). The movie is not
kept: it shows Kai's screen.

The run is three files. `fold1.journal.jsonl` holds the 365 lines rini's transitions journal wrote
while the run recorded, g15 to g19, read with `tools/focuslab/journal.py`; g19, at 8559.9ms, is
after the movie's last frame (8258.3ms). `fold1.mov.json` is what `screenrec` wrote beside the
movie: `scale`, `t0_ns`, `presses` (the key code, the modifier mask and the milliseconds since the
first frame, 524288 being Option), `frames_ms` and `dropped_ms`. `fold1.blank.txt` is
`tools/focuslab/screenrec.py blank fold1.mov`: each frame's time and its count of flat dark blocks,
8x8 pixels at 432 wide, mean under 60 and variance under 6, the 2 bar rows skipped.

## The run

| Run | Build | Lab windows on `work` | Presses |
|---|---|---|---|
| `fold1` | e050e168 | lab-A `[66124, 26553]`, lab-C `[66254, 26567]`, lab-D `[66306, 26574]` | 4 |

Times below are milliseconds from the first frame.

| Press | Key | g | What it did |
|---|---|---|---|
| 598.5 | Option-R (15) | g15 | lab-A 860 to 1148 wide at x=3, lab-C pushed to 1154 |
| 2117.7 | Option-L (37) | g16 | pan: lab-C to 866,35 860x1079, lab-A to about -285,35 1148x1079, part off the left edge |
| 3337.1 | Option-, (43) | g17 | fold lab-C under lab-A |
| 5154.5 | Option-, (43) | g18 | unfold |

lab-C's 1154 and lab-A's -285 are inferred from the landings and the 3pt gap.

## The fold, g17

| ms | Journal line | Mark |
|---|---|---|
| 3339.4 | 149-151 | `begin`, `awaiting 0`; `ask_frame_zero strip 1`: lab-C only, lab-A starting part off and lab-D off |
| 3349.4 | 153-156 | `wears` at compose: lab-A's picture by ScreenCaptureKit, taken 196ms before; `merge awaiting 1 repainting 2`, the same pass composed |
| 3354.2 | 159-164 | lab-C's frame zero delivered at 866,35 860x1079; `show`; `watch landing 1 awaiting 2` |
| 3424.3 | 182-183 | lab-D delivered at 1154,35 572x1079, swapped |
| 3491.4 | 187-188 | lab-A delivered at 3,35 1148x538, swapped on before the motion |
| 3510.2 | 192-195 | lab-C delivered at 3,576 1148x538, swapped; `start_moving` |
| 3874.7 | 225 | `lift` |

lab-C goes from 860x1079 to 1148x538, growing on one axis and shrinking on the other: held, its
repaint swapped on before the motion. lab-A only shrinks, 1148x1079 to 1148x538, moving right 288pt.
Frame zero never asked for it, so its tile flew the cache's picture, 1148x1079 (inferred from the
quiet's warm after g16), and its 538-tall landing was swapped on at 3491.4.

## The unfold, g18

| ms | Journal line | Mark |
|---|---|---|
| 5156.5 | 234-236 | `begin`; `ask_frame_zero strip 3` |
| 5161.1 | 241 | `merge awaiting 2 repainting 2`: lab-C and lab-A both grow |
| 5185.9 | 256-266 | frame zero of lab-C, lab-D and lab-A delivered; `show`; `watch landing 0 awaiting 1`: lab-A, ending part off the display, not watched |
| 5366.2 | 283-286 | lab-C delivered at 866,35 860x1079, swapped; `start_moving` |
| 5608.5 | 302 | `ask_final windows 1` |
| 5725.2 | 308 | `lift` |

lab-A grows from 3,35 1148x538 to about -285,35 1148x1079 and ends part off the display. It was
never held for and flew its frame-zero picture, 538 tall, its ground in the growth from the motion's
start to the lift, 359ms.

## The ground

Every frame outside three stretches reads 17 blocks, but 800.0, which reads 16. The frames are
8.3-608.3ms apart, most 16.7 or 25.0; the first after lab-A's swap at 3491.4 is 3508.3, and the
one before it, 3483.3, reads 17.

| Frames (ms) | Blocks | Read as (inferred) |
|---|---|---|
| 3508.3-3625.0 | 422 falling to 53 | lab-A's ground under its 538-tall landing, on the 1079-tall start, shrinking away |
| 3725.0-3783.3 | 35-46 | not attributed |
| 5400.0-5725.0 | 213 rising to 422 from 5550.0 to the lift | lab-A's ground in its growth |

A block is 1/54 of the width of the 1728x1117pt built-in display, 32pt. lab-A's visible part at
-285 is 863pt wide, and its ground below a 538-tall picture on a 1079-tall frame is 541pt tall:
27 by 16 blocks, 432 (inferred), against the 422 read. 422 blocks is about a quarter of the screen.
