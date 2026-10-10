#!/usr/bin/env python3
"""compare.py BASE... -- CAND... [--stage2 BASE... -- CAND...]: judges battery runs of one
configuration (CAND, as the ring on) against runs of another (BASE, as the ring off), metric by
metric, and prints one CSV row a metric, then the verdict on stderr. Each run is a directory
analyze.py wrote metrics.json into; warm-ups are not given.

The rule (README.md, "Reading the verdict"): every metric is lower-is-better, and passes when the
median of its CAND runs is at most the max of its BASE runs. Blur passes only with 0 failed fly-ins
in every run given, of both. A metric failing that, blur aside, is judged again with --stage2's runs
added to each side, the same rule: failing then, it is a regression.

Runs of different scenes are never judged: every run given must fly what the first does, each
segment's flights of the same kinds in the same order and the same frames delivered, within
FRAME_SLACK points. And every run whose tiles wore a picture taken over STALE_US before it is
flagged on stderr, its count and its oldest, which judges nothing.

Exit 0 when every metric passes, 1 when any does not, 2 on a usage error, 3 when the runs flew
different scenes.
"""
import json
import os
import statistics
import sys

BLUR = "blur.failed"
FRAME_SLACK = 1.0
STALE_US = 5_000_000
COLUMNS = ("metric", "base_min", "base_max", "cand_median", "cand_runs", "stage1",
           "stage2_base_max", "stage2_cand_median", "verdict")


def metrics(run):
    """The 87 metrics of one run, by name: None for one the run has no number for."""
    with open(os.path.join(run, "metrics.json")) as file:
        m = json.load(file)
    out = {}
    for kind in ("pan", "switch"):
        k = m["by_kind"].get(kind, {})
        for key in ("overlay_up", "lift", "screen_up_after_post", "screen_gone_after_post"):
            for s in ("p50", "p90", "max"):
                out[f"{kind}.{key}.{s}"] = (k.get(key) or {}).get(s)
        out[f"{kind}.overdue_lifts"] = k.get("overdue_lifts")
        out[f"{kind}.frame_zero_timeouts"] = k.get("frame_zero_timeouts")
    for key in ("queued", "applied", "heard"):
        for s in ("p50", "p90", "p99", "max"):
            out[f"drains.{key}.{s}"] = m["drains_all"][key].get(s)
    for phase in ("show", "flight", "lift"):
        for key in ("queued", "applied"):
            for s in ("p50", "p90", "max"):
                out[f"phase.{phase}.{key}.{s}"] = m["drains_by_phase"][phase][key].get(s)
    for seg in ("comms_pan", "switch", "spare_pan", "all"):
        s = m["pacing_all"] if seg == "all" else m["segments"].get(seg)
        for q in ("p50", "p90", "p99", "max"):
            out[f"pacing.{seg}.interval.{q}"] = None if s is None else s["intervals"].get(q)
        for c in ("over_25ms", "over_33ms", "black_frames"):
            out[f"pacing.{seg}.{c}"] = None if s is None else s.get(c, 0)
    b = m["blur"]
    out[BLUR] = b["flights"] - b["passed"] if b["flights"] else None
    return out


def journal_marks(run):
    with open(os.path.join(run, "journal.slice.jsonl")) as file:
        return [json.loads(line) for line in file if line.strip()]


def scene(run):
    """What a run flew: each segment's flights' kinds in press order, from metrics.json, and every
    frame a picture was delivered at, from its journal slice, once each, sorted."""
    with open(os.path.join(run, "metrics.json")) as file:
        flights = json.load(file)["flights"]
    kinds = {}
    for flight in flights:
        kinds.setdefault(flight["segment"], []).append(flight.get("kind") or "-")
    frames = sorted({tuple(mark["frame"]) for mark in journal_marks(run) if mark.get("mark") == "delivered"})
    return {"kinds": kinds, "frames": [list(frame) for frame in frames]}


def scene_differences(a, b):
    """How scene `b` differs from `a`: a segment's kinds, and a frame either delivered that the other
    has nothing within FRAME_SLACK of. Empty for one scene."""
    out = []
    for segment in sorted(set(a["kinds"]) | set(b["kinds"])):
        if a["kinds"].get(segment) != b["kinds"].get(segment):
            out.append(f"{segment} flew {b['kinds'].get(segment)}, not {a['kinds'].get(segment)}")
    near = lambda frame, frames: any(all(abs(x - y) <= FRAME_SLACK for x, y in zip(frame, other))
                                     for other in frames)
    out += [f"delivered at {frame}, which it never was" for frame in b["frames"] if not near(frame, a["frames"])]
    out += [f"never delivered at {frame}" for frame in a["frames"] if not near(frame, b["frames"])]
    return out


def stale_wears(run, over_us=STALE_US):
    """The pictures a run's tiles wore taken over `over_us` before: how many, and the oldest, as
    (taken_before_us, window), None with none."""
    stale = [(mark["taken_before_us"], mark["window"]) for mark in journal_marks(run)
             if mark.get("mark") == "wears" and mark.get("taken_before_us", 0) > over_us]
    return len(stale), max(stale, default=None)


def refusal(runs):
    """Why `runs` are not to be judged together: every run whose scene differs from the first's,
    with how. None when they all flew one."""
    first = scene(runs[0])
    lines = [f"{run} flew another scene than {runs[0]}: " + "; ".join(found)
             for run in runs[1:] for found in [scene_differences(first, scene(run))] if found]
    return "\n".join(lines) or None


def flags(runs):
    """One line a run whose tiles wore a picture over STALE_US old."""
    lines = []
    for run in runs:
        count, oldest = stale_wears(run)
        if count:
            lines.append(f"flag: {run}: {count} pictures worn over {STALE_US // 1_000_000}s old, the oldest "
                         f"{oldest[0] / 1e6:.1f}s on {oldest[1]}")
    return lines


def passes(base, cand):
    """The rule: the median of `cand` at most the max of `base`."""
    return statistics.median(cand) <= max(base)


def judge(name, base, cand, base2=None, cand2=None):
    """One metric's row: its values over the runs on each side, and its verdict."""
    row = dict.fromkeys(COLUMNS, "")
    row["metric"] = name
    if None in base or None in cand:
        row["stage1"] = row["verdict"] = "missing"
        return row
    row.update(base_min=min(base), base_max=max(base), cand_median=statistics.median(cand),
               cand_runs=" ".join(map(str, cand)))
    if name == BLUR:
        every = base + cand + (base2 or []) + (cand2 or [])
        if None in every:
            row["stage1"] = row["verdict"] = "missing"
            return row
        row["stage1"] = "pass" if not any(base + cand) else "FAIL"
        row["verdict"] = "pass" if not any(every) else "regression"
        return row
    if passes(base, cand):
        row["stage1"] = row["verdict"] = "pass"
        return row
    row["stage1"] = "FAIL"
    if base2 is None:
        row["verdict"] = "needs stage 2"
        return row
    base6, cand6 = base + base2, cand + cand2
    if None in base6 or None in cand6:
        row["verdict"] = "missing"
        return row
    row.update(stage2_base_max=max(base6), stage2_cand_median=statistics.median(cand6))
    row["verdict"] = "pass" if passes(base6, cand6) else "regression"
    return row


def compare(base, cand, base2=None, cand2=None):
    """Every metric's row, judging the runs' metrics (dicts by name) on each side."""
    names = list(dict.fromkeys(name for run in base + cand + (base2 or []) + (cand2 or []) for name in run))
    side = lambda runs, name: None if runs is None else [run.get(name) for run in runs]
    return [judge(name, side(base, name), side(cand, name), side(base2, name), side(cand2, name))
            for name in names]


def verdict(rows):
    counts = {v: sum(1 for row in rows if row["verdict"] == v)
              for v in ("pass", "needs stage 2", "regression", "missing")}
    accepted = counts["pass"] == len(rows)
    named = lambda v: ", ".join(row["metric"] for row in rows if row["verdict"] == v)
    lines = [f"{len(rows)} metrics: {counts['pass']} pass, {counts['needs stage 2']} need stage 2, "
             f"{counts['regression']} regressions, {counts['missing']} missing: "
             + ("ACCEPTED" if accepted else "NOT ACCEPTED")]
    for v in ("regression", "needs stage 2", "missing"):
        if counts[v]:
            lines.append(f"  {v}: {named(v)}")
    return accepted, "\n".join(lines)


def split(args):
    """BASE... -- CAND...: the two lists of runs, each at least one."""
    if args.count("--") != 1:
        raise ValueError("give the base runs, --, then the candidate runs")
    cut = args.index("--")
    base, cand = args[:cut], args[cut + 1:]
    if not base or not cand:
        raise ValueError("each side needs at least one run")
    return base, cand


def main(argv):
    try:
        if "--stage2" in argv:
            at = argv.index("--stage2")
            (base, cand), (base2, cand2) = split(argv[:at]), split(argv[at + 1:])
        else:
            (base, cand), base2, cand2 = split(argv), None, None
    except ValueError as error:
        print(f"compare.py: {error}\n\n{__doc__}", file=sys.stderr)
        return 2
    runs = base + cand + (base2 or []) + (cand2 or [])
    refused = refusal(runs)
    if refused:
        print(f"compare.py: refused, the runs flew different scenes:\n{refused}", file=sys.stderr)
        return 3
    for line in flags(runs):
        print(line, file=sys.stderr)
    load = lambda runs: None if runs is None else [metrics(run) for run in runs]
    rows = compare(load(base), load(cand), load(base2), load(cand2))
    print(",".join(COLUMNS))
    for row in rows:
        print(",".join(str(row[c]) for c in COLUMNS))
    accepted, text = verdict(rows)
    print(text, file=sys.stderr)
    return 0 if accepted else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
