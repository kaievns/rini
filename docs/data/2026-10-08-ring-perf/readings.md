# The ring-perf timing batteries, 2026-10-08 night

**No pair the night measured passes the acceptance rule as it stands.** Every comparison has
metrics needing stage 2, and the night ran no stage 2. From N1 on, the show drain's wait and the
lift drain are back near the ring-off spread: 11.3ms and 4.03ms in `C-ringon`, 0.030ms and 1.61ms
in `F1-on`. Two things are left: the drains' applied p90 with the ring on, and one blur failure in
`final-on-1`.

Each `<label>/` holds one run's `metrics.json` and `summary.txt`. Each set of 3 came after a
warm-up run that is not kept, but `final-on`, which had none. Each `compare-<base>--<cand>.csv` is
`tools/focuslab/compare.py <base>-{1,2,3} -- <cand>-{1,2,3}` run in this directory. How to read
them: `tools/focuslab/README.md`, "The battery". No picture of the screen was kept.

## The runs

| Label | Binary (sha256) | What it is | When |
|---|---|---|---|
| `B-ringoff`, `C-ringon` | `de4dd880…` | 4cddd692, the borders as first shipped (pid 77575) | 10-08 20:52-20:57 |
| `RC-off`, `RC-on` | `ee0141c3…` | read-conn alone: waiting SkyLight reads on rini's own connection | 10-08 22:47-22:56 |
| `RC2-off`, `RC2-on` | `2d9e548b…` | read-conn after its review, 9ba2ccaa | 10-08 23:32-23:37 |
| `N1-off`, `N1-on` | `31a99234…` | 4cddd692 + read-conn + ring-perf | 10-09 01:22-01:27 |
| `F1-off`, `F1-on` | `84974a88…` | the code main e1f4657d holds, built on the night's branch | 10-09 02:33-02:38 |
| `final-on` | `9b3af5ca…` | main e1f4657d as deployed, ring on only, right after its restart (pid 80933) | 10-09 02:49-02:52 |

The binaries come from each run's `meta.binary_sha`. What each binary holds comes from the night's
session log, not from the binary itself. `meta.repo_head` is the main checkout's HEAD when the run
started, not the binary's commit. Load averages ran 4.6-7.1 (the night's `RC.log`, `RC2.log`).

## The verdicts

Ring on against ring off, same binary, same session (the acceptance rule):

| Base -- candidate | pass | need stage 2 | regression | pan seen up p50 | show drain queued p90 | lift drain applied p50 | drains applied p90 | blur fly-ins failed |
|---|---|---|---|---|---|---|---|---|
| `B-ringoff` -- `C-ringon` | 45 | 42 | 0 | 72.9..76.0 / **86.6** | 0.031..0.043 / **11.288** | 1.66..1.91 / **4.03** | 1.80..2.00 / **4.36** | 0 / 0 |
| `RC-off` -- `RC-on` | 50 | 37 | 0 | 65.9..77.1 / 72.0 | 0.016..0.024 / **6.513** | 1.66..1.74 / **3.98** | 1.96..2.11 / **4.39** | 0 / 0 |
| `RC2-off` -- `RC2-on` | 75 | 12 | 0 | 73.8..81.5 / 75.5 | 0.021..0.112 / **4.389** | 1.80..2.13 / **4.16** | 2.11..2.45 / **4.18** | 0 / 0 |
| `N1-off` -- `N1-on` | 63 | 24 | 0 | 67.0..79.9 / 74.9 | 0.024..0.183 / 0.028 | 1.53..1.72 / 1.48 | 1.80..2.11 / **4.38** | 0 / 0 |
| `F1-off` -- `F1-on` | 70 | 17 | 0 | 71.4..75.0 / **76.4** | 0.020..0.026 / **0.030** | 1.46..2.03 / 1.61 | 1.89..2.34 / **2.47** | 0 / 0 |
| `F1-off` -- `final-on` | 68 | 18 | **1** | 71.4..75.0 / **79.2** | 0.020..0.026 / 0.025 | 1.46..2.03 / 1.72 | 1.89..2.34 / **3.00** | 0 / **1** |

Ring off against the 4cddd692 ring off, across sessions (no erosion):

| Base -- candidate | pass | need stage 2 | regression | pan seen up p50 | pan overlay up p50 | drains applied p90 | frame interval p90, all |
|---|---|---|---|---|---|---|---|
| `B-ringoff` -- `RC-off` | 75 | 12 | 0 | 72.9..76.0 / 70.9 | 36.9..40.7 / 39.1 | 1.80..2.00 / **2.06** | 19.6..20.5 / **21.0** |
| `B-ringoff` -- `RC2-off` | 66 | 21 | 0 | 72.9..76.0 / 73.9 | 36.9..40.7 / 39.0 | 1.80..2.00 / **2.27** | 19.6..20.5 / **21.0** |
| `B-ringoff` -- `N1-off` | 71 | 16 | 0 | 72.9..76.0 / **76.8** | 36.9..40.7 / **41.2** | 1.80..2.00 / 1.99 | 19.6..20.5 / 19.9 |
| `B-ringoff` -- `F1-off` | 60 | 27 | 0 | 72.9..76.0 / 73.5 | 36.9..40.7 / **40.8** | 1.80..2.00 / 1.95 | 19.6..20.5 / 20.1 |

Each timing cell is the base runs' min..max / the candidate runs' median, in ms. The blur cell
counts failed fly-ins over the 3 runs, base / candidate. Bold marks a value that fails stage 1. The
87 rows of each comparison are in its CSV. The `B`/`C` comparison prints the same 87 rows, 42
failing, as the night's scratch comparison did over the same runs.

## Readings

- **Blur, `final-on-1`:** its first fly-in into Outlook read chroma 4.18, below the 5.0 floor
  (`final-on-1/summary.txt`). It was the first fly-in after the restart. The night's log calls it a
  cold-start limit: inferred, not verified. The rule allows no blur failure, so stage 2 cannot pass
  that comparison.
- **Drains applied p90, ring on:** 2.47ms in `F1-on` against 1.89-2.34ms with it off, and 3.00ms
  in `final-on`. The excess sits in the in-flight drains, where the place drain ahead of each lift
  falls: `phase.flight.applied` fails stage 1 at p50, p90 and max, and `phase.lift.applied` passes.
  What a place costs is in `src/decorations/docs/rest-windows.md` ("What a place costs").
- **Across sessions, ring off:** N1 and F1 fail stage 1 on pan overlay up p50 by 0.1-0.5ms against
  4cddd692. Those runs came 4.5-6h after `B-ringoff`, under a different load. Stage 2 would decide
  it; it was not run.

## Where these files came from

The night's own `metrics.json` came from a scratch `analyze.py` that keyed the windows by fixed ids.
The files here were made again by `tools/focuslab/analyze.py` from the night's journal slices and
recordings. Every number is the same as in the night's files. Two things were added:

- `drains_by_phase`, which `compare.py` had read from the journal slice;
- `meta.windows` and `meta.pane`, filled in with the ids, the Outlook pid and the pane box the night
  used.

Only `summary.txt`'s blur line differs, because it now gives the app as rini lists it,
"Microsoft Outlook". The journal slices and the recordings stay out of the repo.
