# The rapid Alt-R recording, 2026-10-10

The journal, press list and block counts behind the altr1 finding: the 2026-10-10 Reported line of
`specs/animation.md` "Resizing", and the held passes let go at once in "Mid-flight passes"
(`src/transitions/docs/coordination.md`). The movie is not kept: it shows Kai's screen.

The run is three files. `altr1.journal.jsonl` holds the 419 lines rini's transitions journal wrote
while the run recorded, g3 to g14, read with `tools/focuslab/journal.py`. `altr1.mov.json` is what
`screenrec` wrote beside the movie: `scale`, `t0_ns`, `presses` (the key code, the modifier mask and
the milliseconds since the first frame, 524288 being Option), `frames_ms` and `dropped_ms`.
`altr1.blank.txt` is `tools/focuslab/screenrec.py blank altr1.mov`: each frame's time and its count
of flat dark blocks, 8x8 pixels at 432 wide, mean under 60 and variance under 6, the 2 bar rows
skipped.

## The run

| Run | Build | Lab windows on `work` | Presses |
|---|---|---|---|
| `altr1` | e050e168, held merges of grows | lab-A, the window resized, and its neighbour | 12 |

Every press is key code 15 `R` with Option, Kai's bind cycling the column's preset widths. They
were posted after gaps of 150, 150, 150, 150, 120, 120, 120, 120, 400, 400, 400 and 600ms, and
recorded 166-170, 134-139 and 416-419ms apart. In the journal lab-A is window `[66124, 26553]`,
its neighbour `[66254, 26567]`. Times below are milliseconds from the first frame.

## The held passes

| `hold_merge` | g | grows | `retarget` | `waited_ms` | What let it go |
|---|---|---|---|---|---|
| 1199.7 | g5 | 1 | 1348.9 | 149 | lab-A's repaint at 860, where the pass ends it |
| 1378.4 | g6 | 1 | 1498.3 | 119 | lab-A's repaint at 1148, where the pass ends it |
| 1534.0 | g7 | 1 | 1654.5 | 120 | the next pass, shrinking lab-A 1722 to 572, growing nothing |
| 1654.5 | g7 | 0 | (the same) | | merges both at once |
| 1800.4 | g9 | 1 | 1800.4 | 0 | lab-A's picture at 572, the end before the press, taken 41.3ms before it was handled |
| 1929.5 | g10 | 1 | 1929.5 | 0 | lab-A's picture at 860, the end before the press, taken 47.3ms before it was handled |

2 of the 5 merges went at once on a picture of the end before. At g9 the tile was kept
(`Flight::keeps`), the 572 picture already on it, so no `swap` followed. At g10 the 860 picture
was swapped on and the motion set off toward 1148 with it; the repaint at 1148 came under g11 at
2066.1, 137ms later.

## The ground

Every frame with nothing moving reads 17 blocks. One stretch reads above it by more than a block,
1954.2-2083.3ms, 55 rising to 197: a dark band by lab-A's moving right edge, up to 180 blocks over the baseline, about
170pt wide down its whole height (inferred: 180 blocks over 34 block rows of 32pt). It ends after
the g11 swap at 2066.1: the two frames after it still read 197, and 2100.0 reads 17. One other
frame, 2858.3ms, reads 18.
