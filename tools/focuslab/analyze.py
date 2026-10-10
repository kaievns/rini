#!/usr/bin/env python3
"""analyze.py RUNDIR: one battery run's metrics.json and summary.txt, the summary printed too.

Reads RUNDIR/journal.slice.jsonl (rini's transitions journal written during the run),
RUNDIR/rec-SEGMENT.jsonl (each press rec posted and each frame it recorded of the overlay) and
RUNDIR/meta.json (what battery.sh found and ran against). Each press is joined to the flight whose
press mark is the first within 250ms after the post (`journal.press_after`), and to the frames
recorded between it and the next press. What each number is, is in README.md ("The battery").
"""
import json
import os
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import journal  # noqa: E402

SEGMENTS = ("comms_pan", "switch", "spare_pan")
DROP_MS, MISS_MS, DROP2_MS = 20.0, 25.0, 33.0
BLUR_FRAMES = 3
BLUR_MIN_CHROMA = 5.0
CODES = {37: "alt+l", 38: "alt+j", 35: "alt+p", 41: "alt+;"}
PHASES = (("show", True), ("flight", None), ("lift", False))


def stats(values, digits=2):
    """p50, p90, p99 (nearest rank) and max of `values`, Nones left out, rounded to `digits`."""
    values = sorted(v for v in values if v is not None)
    if not values:
        return {"n": 0}
    out = {"n": len(values)}
    for name, p in (("p50", 50), ("p90", 90), ("p99", 99)):
        out[name] = round(journal.percentile(values, p), digits)
    out["max"] = round(values[-1], digits)
    return out


def load(path):
    with open(path) as file:
        return [json.loads(line) for line in file if line.strip()]


def showing(frame):
    """The overlay carries a picture: some of it opaque, and not all black. An opaque all-black frame
    comes at some lifts, the window's content released while (inferred) its alpha already hides it."""
    return frame["vis"] > 0 and frame["sig"] > 0


def frame_flight(frames, start_ns, end_ns):
    """The frames from the first showing one at or after `start_ns` to the first after it that does
    not show (the overlay coming down), all before `end_ns`: (those shown, the one gone or None, how
    many opaque all-black frames came in the window)."""
    window = [f for f in frames if start_ns <= f["t_ns"] < end_ns]
    first = next((i for i, f in enumerate(window) if showing(f)), None)
    if first is None:
        return [], None, 0
    shown, gone = [], None
    for f in window[first:]:
        if showing(f):
            shown.append(f)
        else:
            gone = f
            break
    black = sum(1 for f in window if f["vis"] > 0 and f["sig"] == 0)
    return shown, gone, black


def pacing(shown):
    """The gaps between the frames shown, ms, and their counts past 20, 25 and 33ms."""
    gaps = [(b["t_ns"] - a["t_ns"]) / 1e6 for a, b in zip(shown, shown[1:])]
    changed = [gap for gap, (a, b) in zip(gaps, zip(shown, shown[1:])) if a["sig"] != b["sig"]]
    return {
        "frames": len(shown),
        "intervals": stats(gaps),
        "over_20ms": sum(1 for g in gaps if g > DROP_MS),
        "over_25ms": sum(1 for g in gaps if g > MISS_MS),
        "over_33ms": sum(1 for g in gaps if g > DROP2_MS),
        "over_20ms_changed": sum(1 for g in changed if g > DROP_MS),
        "repeats": sum(1 for a, b in zip(shown, shown[1:]) if a["sig"] == b["sig"]),
    }, gaps


def blur(shown):
    """The pane box's mean colour and chroma over the last frames a fly-in showed, and whether the
    chroma says the window flew with its blur."""
    last = shown[-BLUR_FRAMES:]
    rgb = [round(sum(f["pane"][k] for f in last) / len(last), 1) for k in range(3)]
    chroma = round(sum(f["pane"][3] for f in last) / len(last), 2)
    return {"frames": len(last), "rgb": rgb, "chroma": chroma,
            "per_frame_chroma": [round(f["pane"][3], 2) for f in last],
            "pass": chroma >= BLUR_MIN_CHROMA}


def blur_pid(meta):
    return ((meta.get("windows") or {}).get("blur") or {}).get("pid")


def analyse(rundir, meta):
    found = journal.flights(journal.read([os.path.join(rundir, "journal.slice.jsonl")]))
    pressed = journal.presses(found)
    checked = blur_pid(meta)
    flights_out, all_gaps, by_seg_gaps = [], [], {}
    for seg in SEGMENTS:
        path = os.path.join(rundir, f"rec-{seg}.jsonl")
        if not os.path.exists(path):
            continue
        lines = load(path)
        posts = [line for line in lines if line["type"] == "press"]
        frames = [line for line in lines if line["type"] == "frame"]
        by_seg_gaps[seg] = []
        for n, post in enumerate(posts):
            end = posts[n + 1]["t_ns"] if n + 1 < len(posts) else float("inf")
            press = journal.press_after(post["t_ns"], pressed)
            how = "unmatched" if press is None else "began" if press[1] else "joined"
            row = {"segment": seg, "n": n, "key": CODES.get(post["code"], post["code"]), "join": how}
            if how == "began":
                flight = press[2]
                t = flight.times()
                row.update(kind=flight.kind(), g=flight.g,
                           target=list(flight.target) if flight.target else None,
                           overlay_up=t["overlay_up"], press_to_up=t["press_to_up"], lift=t["lift"],
                           show_to_landed=t["show_to_landed"], merges=t["merges"], swaps=t["swaps"],
                           overdue_lift=t["overdue_lifts"], frame_zero_timeout=t["frame_zero_timeouts"],
                           post_to_press=journal.ms(flight.press["t"], post["t_ns"]),
                           drains=journal.drains([flight]))
            shown, gone, black = frame_flight(frames, post["t_ns"], end)
            counts, gaps = pacing(shown)
            counts["black_frames"] = black
            all_gaps += gaps
            by_seg_gaps[seg] += gaps
            row.update(counts)
            row["screen_up"] = journal.ms(shown[0]["t_ns"], post["t_ns"]) if shown else None
            row["screen_gone"] = journal.ms(gone["t_ns"], post["t_ns"]) if gone else None
            if checked is not None and row.get("target") and row["target"][0] == checked and shown:
                row["blur"] = blur(shown)
            flights_out.append(row)
    return flights_out, all_gaps, by_seg_gaps


def phases(rundir):
    """Every drain the slice journalled, by where it fell: at the show (`shown` true), in flight
    (null) and at the lift (false), each its wait for the renderer and its play to its flush, ms."""
    drawn = [line for line in load(os.path.join(rundir, "journal.slice.jsonl")) if line.get("mark") == "drawn"]
    return {phase: {key: stats([d[f"{key}_us"] / 1000 for d in drawn if d["shown"] is shown], digits=3)
                    for key in ("queued", "applied")}
            for phase, shown in PHASES}


DRAIN_KEYS = tuple(key for key, _ in journal.DRAINS)


def crops(drains):
    """What the drains of the joined flights spent on crop-drawn tiles: how many drew any, the tiles
    they drew, and their ground reads (ms) over those drains alone."""
    cropped = [d for d in drains if d["crops"]]
    return {"drains": len(cropped), "tiles": sum(d["crops"] for d in cropped),
            "ground": stats([d["ground"] for d in cropped], digits=3)}


def totals(flights_out, all_gaps, by_seg_gaps):
    kinds = sorted({f.get("kind") for f in flights_out if f.get("kind")})
    by_kind = {}
    for kind in kinds:
        rows = [f for f in flights_out if f.get("kind") == kind]
        drains = [d for f in rows for d in f["drains"]]
        by_kind[kind] = {
            "flights": len(rows),
            "overlay_up": stats([f["overlay_up"] for f in rows]),
            "lift": stats([f["lift"] for f in rows]),
            "screen_up_after_post": stats([f["screen_up"] for f in rows]),
            "screen_gone_after_post": stats([f["screen_gone"] for f in rows]),
            "drains": {key: stats([d[key] for d in drains]) for key in DRAIN_KEYS},
            "overdue_lifts": sum(f["overdue_lift"] for f in rows),
            "frame_zero_timeouts": sum(f["frame_zero_timeout"] for f in rows),
        }
    drains = [d for f in flights_out for d in f.get("drains", [])]
    segments = {}
    for seg, gaps in by_seg_gaps.items():
        rows = [f for f in flights_out if f["segment"] == seg]
        segments[seg] = {
            "presses": len(rows),
            "joined": sum(1 for f in rows if f["join"] == "began"),
            "kinds": sorted({f.get("kind") or "-" for f in rows}),
            "frames": sum(f["frames"] for f in rows),
            "intervals": stats(gaps),
            "over_20ms": sum(f["over_20ms"] for f in rows),
            "over_25ms": sum(f["over_25ms"] for f in rows),
            "over_33ms": sum(f["over_33ms"] for f in rows),
            "flights_without_frames": sum(1 for f in rows if f["frames"] == 0),
            "black_frames": sum(f["black_frames"] for f in rows),
            "by_key": {key: {"flights": len(group), "over_25ms": sum(f["over_25ms"] for f in group),
                             "max": max((f["intervals"].get("max", 0) for f in group), default=0)}
                       for key in sorted({f["key"] for f in rows})
                       for group in [[f for f in rows if f["key"] == key]]},
        }
    checked = [f["blur"] for f in flights_out if "blur" in f]
    return {
        "by_kind": by_kind,
        "drains_all": {key: stats([d[key] for d in drains]) for key in DRAIN_KEYS},
        "crops": crops(drains),
        "segments": segments,
        "pacing_all": {"intervals": stats(all_gaps), "over_20ms": sum(1 for g in all_gaps if g > DROP_MS),
                       "over_25ms": sum(1 for g in all_gaps if g > MISS_MS),
                       "over_33ms": sum(1 for g in all_gaps if g > DROP2_MS), "intervals_n": len(all_gaps)},
        "blur": {"flights": len(checked), "passed": sum(1 for b in checked if b["pass"]),
                 "chroma": [b["chroma"] for b in checked], "rgb": [b["rgb"] for b in checked]},
    }


def fmt(s, keys=("p50", "p90", "max")):
    if not s or not s.get("n"):
        return "-"
    return "/".join(f"{s[k]:.1f}" for k in keys) + f" n={s['n']}"


def names(meta):
    windows = meta.get("windows") or {}
    spare = (windows.get("spare") or {}).get("app", "?")
    checked = (windows.get("blur") or {}).get("app", "?")
    return spare, checked


def summary(meta, agg, flights_out):
    lines = [f"battery {meta['label']}  {meta.get('started', '')}  run {meta.get('run_s', '?')}s  "
             f"rini pid {meta.get('rini_pid')}  binary {meta.get('binary_sha', '')[:12]}"]
    if meta.get("aborted"):
        lines.append(f"ABORTED: {meta['aborted']}")
    lines.append("")
    lines.append(f"{'journal, by kind (ms)':24s} {'p50/p90/max':>22s}")
    for kind, k in agg["by_kind"].items():
        lines.append(f"  {kind:8s} flights {k['flights']:2d}   overdue lifts {k['overdue_lifts']}  "
                     f"frame-zero timeouts {k['frame_zero_timeouts']}")
        lines.append(f"    press->overlay up      {fmt(k['overlay_up'])}")
        lines.append(f"    lift                   {fmt(k['lift'])}")
        lines.append(f"    post->seen up / gone   {fmt(k['screen_up_after_post'])}  |  {fmt(k['screen_gone_after_post'])}")
    d = agg["drains_all"]
    lines.append("")
    lines.append("renderer drains, all joined flights (ms, p50/p90/p99/max)")
    for key, label in journal.DRAINS:
        lines.append(f"  {label:26s} {fmt(d[key], ('p50', 'p90', 'p99', 'max'))}")
    c = agg["crops"]
    lines.append(f"  crop-drawn tiles {c['tiles']} in {c['drains']} drains, their grounds read "
                 f"{fmt(c['ground'], ('p50', 'p90', 'max'))}")
    lines.append("")
    lines.append("frame pacing (recorded overlay, 60fps cap; intervals p50/p90/max ms)")
    for seg, s in agg["segments"].items():
        lines.append(f"  {seg:10s} {s['joined']}/{s['presses']} joined {','.join(s['kinds']):12s} frames {s['frames']:4d}  "
                     f"{fmt(s['intervals'])}  >20 {s['over_20ms']} >25 {s['over_25ms']} >33 {s['over_33ms']}"
                     + (f"  black {s['black_frames']}" if s['black_frames'] else ""))
        lines.append(f"  {'':10s} " + "   ".join(f"{key}: >25 {k['over_25ms']} max {k['max']:.1f}" for key, k in s["by_key"].items()))
    p = agg["pacing_all"]
    lines.append(f"  {'all':10s} {'':29s} frames {sum(x['frames'] for x in agg['segments'].values()):4d}  {fmt(p['intervals'])}  >20 {p['over_20ms']} >25 {p['over_25ms']} >33 {p['over_33ms']}")
    worst = sorted(flights_out, key=lambda f: -(f["intervals"].get("max") or 0))[:3]
    lines.append("  worst flights: " + "; ".join(
        f"{f['segment']}#{f['n']} {f.get('kind', '-')} max {f['intervals'].get('max', 0):.1f} >20ms {f['over_20ms']}" for f in worst))
    b = agg["blur"]
    spare, checked = names(meta)
    lines.append("")
    lines.append(f"blur, {spare}->{checked} (pane last {BLUR_FRAMES} frames, pass chroma >= {BLUR_MIN_CHROMA}): "
                 f"{b['passed']}/{b['flights']} pass")
    for rgb, chroma in zip(b["rgb"], b["chroma"]):
        lines.append(f"  rgb {rgb[0]:5.1f} {rgb[1]:5.1f} {rgb[2]:5.1f}  chroma {chroma:5.2f}  {'PASS' if chroma >= BLUR_MIN_CHROMA else 'FAIL'}")
    unjoined = [f for f in flights_out if f["join"] != "began"]
    if unjoined:
        lines.append("")
        lines.append("presses not beginning a flight: " + ", ".join(f"{f['segment']}#{f['n']} {f['key']} ({f['join']})" for f in unjoined))
    return "\n".join(lines)


def main(rundir):
    meta_path = os.path.join(rundir, "meta.json")
    meta = {"label": os.path.basename(rundir)}
    if os.path.exists(meta_path):
        with open(meta_path) as file:
            meta = json.load(file)
    flights_out, all_gaps, by_seg_gaps = analyse(rundir, meta)
    agg = totals(flights_out, all_gaps, by_seg_gaps)
    metrics = {"meta": meta, **agg, "drains_by_phase": phases(rundir), "flights": flights_out}
    for f in metrics["flights"]:
        f.pop("drains", None)
    with open(os.path.join(rundir, "metrics.json"), "w") as file:
        json.dump(metrics, file, indent=1)
    text = summary(meta, agg, flights_out)
    with open(os.path.join(rundir, "summary.txt"), "w") as file:
        file.write(text + "\n")
    print(text)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
