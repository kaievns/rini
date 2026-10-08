#!/usr/bin/env python3
"""compare.py BASE... -- CAND... [--stage2 BASE... -- CAND...]: judges battery runs of one
configuration (CAND, as the ring on) against runs of another (BASE, as the ring off), metric by
metric, and prints one CSV row a metric, then the verdict on stderr. Each run is a directory
analyze.py wrote metrics.json into; warm-ups are not given.

The rule (README.md, "Reading the verdict"): every metric is lower-is-better, and passes when the
median of its CAND runs is at most the max of its BASE runs. Blur passes only with 0 failed fly-ins
in every run given, of both. A metric failing that, blur aside, is judged again with --stage2's runs
added to each side, the same rule: failing then, it is a regression.

Exit 0 when every metric passes, 1 when any does not, 2 on a usage error.
"""
import json
import os
import statistics
import sys

BLUR = "blur.failed"
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
