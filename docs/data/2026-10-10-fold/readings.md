# The fold and unfold recordings, 2026-10-10

The journals, press lists and block counts behind two findings. fold1's: the last 2026-10-10
Reported line of `specs/animation.md` "Resizing", and a shrink starting off the display keeping its
cached picture and a fold flying in two steps in "A grow waits for its repaint"
(`src/transitions/docs/coordination.md`). fold2's: the 2026-10-11 Reported line in the same spec,
and `ELSEWHERE_HOLD_CAP`, the 450ms cap on a hold waiting for a grow that ends cut by the display's
edge, in the same section. The movies are not kept: they show Kai's screen.

Each run is three files. `fold1.journal.jsonl` holds the 365 lines rini's transitions journal wrote
while the run recorded, g15 to g19, read with `tools/focuslab/journal.py`; g19, at 8559.9ms, is
after the movie's last frame (8258.3ms). `fold2.journal.jsonl` holds 235 lines, g15 to g18; g18,
at 7091.6ms, is after the movie's last frame (6758.3ms). `fold1.mov.json` and `fold2.mov.json` are
what `screenrec` wrote beside each movie: `scale`, `t0_ns`, `presses` (the key code, the modifier
mask and the milliseconds since the first frame, 524288 being Option), `frames_ms` and
`dropped_ms`. `fold1.blank.txt` and `fold2.blank.txt` are `tools/focuslab/screenrec.py blank` of
each movie: each frame's time and its count of flat dark blocks, 8x8 pixels at 432 wide, mean under
60 and variance under 6, the 2 bar rows skipped.

## The runs

| Run | Build | Lab windows on `work` | Presses |
|---|---|---|---|
| `fold1` | e050e168 | lab-A `[66124, 26553]`, lab-C `[66254, 26567]`, lab-D `[66306, 26574]` | 4 |
| `fold2` | d9461781 | lab-A `[66124, 26553]`, lab-B `[66173, 26560]` | 3 |

Times below are milliseconds from the run's first frame. fold1's presses:

| Press | Key | g | What it did |
|---|---|---|---|
| 598.5 | Option-R (15) | g15 | lab-A 860 to 1148 wide at x=3, lab-C pushed to 1154 |
| 2117.7 | Option-L (37) | g16 | pan: lab-C to 866,35 860x1079, lab-A to about -285,35 1148x1079, part off the left edge |
| 3337.1 | Option-, (43) | g17 | fold lab-C under lab-A |
| 5154.5 | Option-, (43) | g18 | unfold |

lab-C's 1154 and lab-A's -285 are inferred from the landings and the 3pt gap.

## The fold, g17 of fold1

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
repaint swapped on before the motion, its ground above that 538-tall picture on the 1079-tall start
until the height caught up. lab-A only shrinks, 1148x1079 to 1148x538, moving right 288pt.
Frame zero never asked for it, so its tile flew the cache's picture, 1148x1079 (inferred from the
quiet's warm after g16), and its 538-tall landing was swapped on at 3491.4.

## The unfold, g18 of fold1

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

## The ground on fold1

Every frame outside three stretches reads 17 blocks, but 800.0, which reads 16. The frames are
8.3-608.3ms apart, most 16.7 or 25.0; the first after lab-A's swap at 3491.4 is 3508.3, and the
one before it, 3483.3, reads 17.

| Frames (ms) | Blocks | Read as (inferred) |
|---|---|---|
| 3508.3-3625.0 | 422 falling to 53 | lab-A's ground under its 538-tall landing, on the 1079-tall start, from before the motion; from the motion's start at 3510.2 lab-C's as well, above its 538-tall repaint on its 1079-tall start, seen by eye on the movie's frames at 3520 and 3540; both shrinking away |
| 3725.0-3783.3 | 35-46 | not attributed |
| 5400.0-5725.0 | 213 rising to 422 from 5550.0 to the lift | lab-A's ground in its growth |

A block is 1/54 of the width of the 1728x1117pt built-in display, 32pt. lab-A's visible part at
-285 is 863pt wide, and its ground below a 538-tall picture on a 1079-tall frame is 541pt tall:
27 by 16 blocks, 432 (inferred), against the 422 read. 422 blocks is about a quarter of the screen.

## The unfold watched, g17 of fold2

fold2's presses, its journal's `t` read against the movie's `t0_ns`, 634203079718250:

| Press | Key | g | What it did |
|---|---|---|---|
| 614.3 | Option-L (37) | g15 | pan, `move_focus` to lab-B |
| 1833.0 | Option-, (43) | g16 | fold lab-B under lab-A: lab-A to 3,35 1148x538, lab-B to 3,576 1148x538 |
| 3651.8 | Option-, (43) | g17 | unfold: lab-B to 865,35 860x1079, lab-A to -286,35 1148x1079, part off the left edge |

| ms | Journal line | Mark |
|---|---|---|
| 3652.3 | 133 | `press toggle_fold` |
| 3657.7 | 134-136 | `begin`; `ask_frame_zero strip 2` |
| 3689.4 | 141 | `merge awaiting 2 repainting 2`: lab-B and lab-A both grow |
| 3708.0 | 145-152 | frame zero of lab-A and lab-B delivered at their starts; `show`; `watch landing 0 awaiting 2` |
| 3922.1 | 155-156 | lab-B delivered at 865,35 860x1079 through SkyLight, swapped, 214.1ms after the show |
| 4008.4 | 160-161 | `start_moving`, 300.4ms after the show: the 300ms hold ran out |
| 4072.5 | 164-166 | lab-A delivered at -286,35 1148x1079, 364.5ms after the show, swapped, `wears at swap route screencapturekit` |
| 4260.8 | 168 | `ask_final windows 1` |
| 4362.8 | 174 | `lift` |

lab-A grows from 3,35 1148x538 to -286,35 1148x1079 and ends part off the display, so it was
watched through ScreenCaptureKit, the way bc10a06b watches such an end. Its repaint came 64.1ms
after the hold let the motion go, and its tile flew its frame-zero picture, 538 tall, until then.

| Frames (ms) | Blocks | Read as (inferred) |
|---|---|---|
| 4033.3, 4050.0, 4066.7 | 67, 156, 266 | lab-A's ground in its growth, between the motion's start at 4008.4 and its repaint's swap at 4072.6 |
| 2062.5, 2070.8 | 1, 2 | in g16's motion, not attributed |

Every other frame reads 0 blocks, the frame before the motion, 3991.7, and the first after the
swap, 4083.3, among them. The frames are 8.3-608.3ms apart: 41.6 from 3991.7 to the first ground
frame, and 16.7 from there to 4083.3.
