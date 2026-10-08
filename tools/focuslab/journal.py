#!/usr/bin/env python3
"""Reads rini's transitions journal: where each flight's time goes, each probe press joined to the
flight it began, each measured lift's handover, the picture each tile took, and each slow turn of the
main thread.

    journal.py summary [--kind KIND] [--csv OUT] [JOURNAL ...]
    journal.py probe PROBE [JOURNAL ...]
    journal.py handover [JOURNAL ...]
    journal.py pictures [--target] [JOURNAL ...]
    journal.py turns [JOURNAL ...]

JOURNAL is ~/Library/Logs/rini/transitions.1.jsonl, then transitions.jsonl, unless named. PROBE is
the JSON lines `focuslab.swift` or `overlay.swift` wrote. What every row measures is defined in
src/transitions/docs/journal.md ("Reading it"); this implements those definitions.
"""
import argparse
import bisect
import csv
import json
import math
import os
import sys
from collections import defaultdict

from e1 import look

LOGS = os.path.expanduser("~/Library/Logs/rini")
JOIN_NS = 250_000_000

SPLIT = [
    ("reactor", "reactor"),
    ("frame_zero_asked", "frame zero asked"),
    ("service_wait", "service wait"),
    ("skylight_round", "the SkyLight round"),
    ("engine_wait", "engine wait"),
    ("the_show", "the show"),
    ("to_renderer", "the hop to the renderer"),
    ("show_commit", "the show's commit"),
    ("press_to_up", "press to overlay up"),
]
ROWS = [
    ("compose", "compose"),
    ("frame_zero_capture_starts", "frame-zero capture starts"),
    ("frame_zero_pictures_ready", "frame-zero pictures ready"),
    ("show_given", "show given"),
    ("overlay_up", "overlay up"),
    ("rini_sees_it_landed", "rini sees it landed"),
    ("focused_picture_captured", "focused picture captured"),
    ("show_to_landed", "show to landed"),
    ("lift", "lift"),
    ("lift_to_settled", "lift to settled"),
]
COUNTS = [
    ("merges", "merges"),
    ("swaps", "swaps"),
    ("snaps", "snaps"),
    ("stale_deliveries", "stale deliveries"),
    ("lost", "lost"),
    ("overdue_lifts", "overdue lifts"),
    ("frame_zero_timeouts", "frame-zero timeouts"),
]
DRAINS = [
    ("queued", "waiting for the renderer"),
    ("applied", "played to its flush"),
    ("heard", "flushed to heard"),
]
PROBE = [
    ("post_to_press", "post to press"),
    ("post_to_show", "post to show given"),
    ("post_to_landed", "post to landed"),
    ("landed_after_still", "landed after the probe's still"),
    ("post_to_look", "post to focused picture"),
    ("look_after_look_full", "focused picture after look_full"),
    ("up_after_show", "overlay seen up after show"),
    ("post_to_lift", "post to lift"),
    ("gone_after_lift", "overlay seen gone after lift"),
]
HANDOVER = [
    ("interior", "interior"),
    ("edge_in", "edge in"),
    ("edge_out", "edge out"),
    ("band", "band outside the outline"),
]
HEADLINE_KIND = "pan"
RESEARCH = 'against "The handover, measured" in src/animation/docs/capture-overlay-research.md'


def read(paths):
    """The bursts in `paths`, in order: each its header and its marks as they were pushed."""
    bursts = []
    for path in paths:
        with open(path) as file:
            for text in file:
                line = json.loads(text)
                if line["mark"] == "burst":
                    bursts.append((line, []))
                elif bursts:
                    bursts[-1][1].append(line)
    return bursts


def flights(bursts):
    """Every flight in `bursts`, in order. A mark before a burst's first flight is in none."""
    return [Flight(header, marks[start:end]) for header, marks in bursts for start, end in spans(marks)]


def spans(marks):
    """Where each flight in a burst's `marks` starts and ends, in order."""
    starts = [opening(marks, at) for at, mark in enumerate(marks) if mark["mark"] == "hold"]
    return list(zip(starts, starts[1:] + [len(marks)]))


def opening(marks, hold):
    """Where the flight holding at `hold` starts: at its begin, or the press just before that."""
    start = hold
    for name in ("begin", "press"):
        if start > 0 and marks[start - 1]["mark"] == name and marks[start - 1]["g"] == marks[hold]["g"]:
            start -= 1
    return start


def deliveries(marks):
    """Each delivery in `marks`, where it is, and the pictures marked after it. A `delivered` with
    no `delivery` before it, one the ring overwrote, is dropped."""
    found = []
    current = None
    for at, mark in enumerate(marks):
        if mark["mark"] == "delivery":
            current = (at, mark, [])
            found.append(current)
        elif mark["mark"] == "delivered" and current is not None:
            current[2].append(mark)
        else:
            current = None
    return found


def capture_start(delivered):
    return delivered["t"] - delivered["taken_before_us"] * 1000


def round_end(delivery):
    return delivery["t"] - delivery["sent_before_us"] * 1000


def flushed(drawn):
    return drawn["t"] - drawn["flushed_before_us"] * 1000


def ms(later, earlier):
    return None if later is None or earlier is None else (later - earlier) / 1e6


def at(mark):
    return None if mark is None else mark["t"]


class Flight:
    """One flight's marks: from its press, or its begin, to the next flight's or its burst's end."""

    def __init__(self, header, marks):
        self.header = header
        self.marks = marks
        self.g = next(mark["g"] for mark in marks if mark["mark"] == "hold")
        self.press = marks[0] if marks[0]["mark"] == "press" else None
        self.begin = self.first("begin")
        self.target = tuple(self.begin["focus"]) if self.begin and self.begin["focus"] else None
        self.deliveries = deliveries(marks)
        self.frame_zero = self.frame_zero_delivery()

    def find(self, name, keep=lambda mark: True, start=0):
        """The first mark named `name` from `start` that `keep` keeps, and where it is."""
        for index in range(start, len(self.marks)):
            if self.marks[index]["mark"] == name and keep(self.marks[index]):
                return index, self.marks[index]
        return None, None

    def first(self, name, keep=lambda mark: True):
        return self.find(name, keep)[1]

    def count(self, name, keep=lambda mark: True):
        return sum(1 for mark in self.marks if mark["mark"] == name and keep(mark))

    def frame_zero_delivery(self):
        """The last `delivery {frame_zero}` of the flight's generation before its show."""
        shown, _ = self.find("show")
        if shown is None:
            return None
        found = [(delivery, pictures) for index, delivery, pictures in self.deliveries
                 if index < shown and delivery["why"] == "frame_zero" and delivery["of"] == self.g]
        return found[-1] if found else None

    def overlay_drawn(self):
        """When the drain that put the overlay up began and was flushed: the first `drawn {shown: true}`
        after the show. A flight with no `drawn` mark, written before the renderer's commits were
        journalled, is drawn at its show."""
        shown, show = self.find("show")
        if show is None:
            return None
        if not self.count("drawn"):
            return show["t"], show["t"]
        _, up = self.find("drawn", lambda mark: mark["shown"] is True, start=shown)
        if up is None:
            return None
        return flushed(up) - up["applied_us"] * 1000, flushed(up)

    def target_capture(self, why):
        """When the focus target's first picture under a `delivery {why}` started its capture."""
        for _, delivery, pictures in self.deliveries:
            for picture in pictures:
                if delivery["why"] == why and tuple(picture["window"]) == self.target:
                    return capture_start(picture)
        return None

    def show_to_landed(self):
        """From the start of the generation the flight lifted in to the last of its end windows'
        landed captures, once every end window its last watch named has one."""
        lift = self.first("lift")
        if lift is None:
            return None
        g = lift["g"]
        since = self.first("show") if g == self.g else self.first("merge", lambda mark: mark["g"] == g)
        watches = [mark for mark in self.marks if mark["mark"] == "watch" and mark["g"] == g]
        if since is None or not watches:
            return None
        ends = watches[-1]["landing"] + watches[-1]["awaiting"]
        landed = {}
        for _, delivery, pictures in self.deliveries:
            if delivery["why"] == "landed" and delivery["of"] == g:
                for picture in pictures:
                    landed.setdefault(tuple(picture["window"]), capture_start(picture))
        if ends == 0 or len(landed) < ends:
            return None
        return ms(max(landed.values()), since["t"])

    def times(self):
        """Every row and count of the flight, in ms, None where it has none."""
        press, begin = at(self.press), at(self.begin)
        ask, show = at(self.first("ask_frame_zero")), at(self.first("show"))
        lifted, lift = self.find("lift")
        settled = at(self.find("report_settled", start=lifted)[1]) if lifted is not None else None
        compose = self.first("merge", lambda mark: not mark["moved"] and mark["g"] == self.g)
        began, up = self.overlay_drawn() or (None, None)
        row = {key: None for key, _ in SPLIT + ROWS}
        if self.frame_zero:
            delivery, pictures = self.frame_zero
            ready = round_end(delivery)
            starts = min((capture_start(picture) for picture in pictures), default=ready)
            if press is not None:
                row.update(
                    reactor=ms(begin, press),
                    frame_zero_asked=ms(ask, begin),
                    service_wait=ms(starts, ask),
                    skylight_round=ms(ready, starts),
                    engine_wait=ms(delivery["t"], ready),
                    the_show=ms(show, delivery["t"]),
                    to_renderer=ms(began, show),
                    show_commit=ms(up, began),
                    press_to_up=ms(up, press),
                )
            row.update(frame_zero_capture_starts=ms(starts, press), frame_zero_pictures_ready=ms(ready, press))
        row.update(
            compose=ms(at(compose), begin),
            show_given=ms(show, press),
            overlay_up=ms(up, press),
            rini_sees_it_landed=ms(self.target_capture("landed"), press),
            focused_picture_captured=ms(self.target_capture("focus_changed"), press),
            show_to_landed=self.show_to_landed(),
            lift=ms(at(lift), press),
            lift_to_settled=ms(settled, at(lift)),
            merges=len({mark["g"] for mark in self.marks}) - 1,
            swaps=self.count("swap"),
            snaps=self.count("snap"),
            stale_deliveries=self.count("delivery", lambda mark: mark["of"] != mark["g"]),
            lost=self.count("lost"),
            overdue_lifts=int(lift is not None and not (lift["at_rest"] and lift["landed"])),
            frame_zero_timeouts=int(show is not None and self.frame_zero is None),
        )
        return row

    def unix_ms(self):
        """The wall clock at the flight's first mark."""
        return self.header["unix_ms"] - (self.header["t"] - self.marks[0]["t"]) / 1e6

    def kind(self):
        return self.begin["kind"] if self.begin else None


def percentile(values, p):
    """Nearest rank."""
    return values[max(0, math.ceil(p / 100 * len(values)) - 1)]


def spread(values, digits=1):
    values = sorted(value for value in values if value is not None)
    if not values:
        return f"{'-':>7s}  {'-':>7s}  {'-':>7s}  n=0"
    p50, p90, p99 = (percentile(values, p) for p in (50, 90, 99))
    return f"{p50:7.{digits}f}  {p90:7.{digits}f}  {p99:7.{digits}f}  n={len(values)}"


def table(title, rows, keys, digits=1):
    lines = [f"{title:34s}     p50      p90      p99"]
    lines += [f"  {label:32s} {spread([row[key] for row in rows], digits)}" for key, label in keys]
    return lines


def drains(found):
    """Every drain the renderer committed in `found`: how long its oldest batch waited for the
    renderer, how long playing it took to its flush, and the way back to the engine, in ms."""
    return [{"queued": mark["queued_us"] / 1000, "applied": mark["applied_us"] / 1000,
             "heard": mark["flushed_before_us"] / 1000}
            for flight in found for mark in flight.marks if mark["mark"] == "drawn"]


def summary(found, bursts):
    rows = [flight.times() for flight in found]
    kinds = defaultdict(int)
    for flight in found:
        kinds[flight.kind()] += 1
    lifted = sum(1 for flight in found if flight.first("lift"))
    abandoned = sum(1 for flight in found if flight.first("abandon"))
    lost = sum(header["lost"] for header, _ in bursts)
    by_kind = "".join(f", {kind} {n}" for kind, n in sorted(kinds.items(), key=lambda kv: -kv[1]))
    lines = [
        f"{len(found)} flights ({lifted} lifted, {abandoned} abandoned{by_kind}) in {len(bursts)} bursts,"
        f" {lost} marks lost",
        "",
    ]
    lines += table("press to overlay up, in order (ms)", rows, SPLIT)
    lines += [""]
    lines += table("ms after the press", rows, ROWS)
    lines += [""]
    lines += table("each drain drawn (ms)", drains(found), DRAINS, digits=2)
    lines += ["", f"{'counts':34s}  total  flights"]
    for key, label in COUNTS:
        total, flights_with = sum(row[key] for row in rows), sum(1 for row in rows if row[key])
        lines.append(f"  {label:32s} {total:6d}  {flights_with} of {len(rows)}")
    return "\n".join(lines)


def write_csv(path, found):
    keys = [key for key, _ in SPLIT + ROWS + COUNTS]
    with open(path, "w", newline="") as file:
        out = csv.writer(file)
        out.writerow(["unix_ms", "g", "kind", "what", "focus"] + keys)
        for flight in found:
            row = flight.times()
            focus = "" if flight.target is None else f"{flight.target[0]}:{flight.target[1]}"
            what = flight.press["what"] if flight.press else ""
            head = [f"{flight.unix_ms():.0f}", flight.g, flight.kind(), what, focus]
            out.writerow(head + [row[key] for key in keys])


def join(trials, found):
    """Each trial, its label, and the press it caused, found as the first press within 250ms after
    the probe posted: (label, how it joined, the trial's row)."""
    presses = [(mark["t"], mark is flight.press, flight)
               for flight in found for mark in flight.marks if mark["mark"] == "press"]
    presses.sort(key=lambda press: press[0])
    times = [press[0] for press in presses]
    joined = []
    for trial in trials:
        post = trial["t0_ns"]
        nearest = bisect.bisect_left(times, post)
        if nearest == len(times) or times[nearest] - post > JOIN_NS:
            joined.append((trial["label"], "unmatched", {}))
            continue
        pressed, began, flight = presses[nearest]
        row = {"post_to_press": ms(pressed, post)}
        if not began:
            joined.append((trial["label"], "joined", row))
            continue
        if "to_wid" in trial and (flight.target is None or flight.target[1] != trial["to_wid"]):
            joined.append((trial["label"], "another target", row))
            continue
        joined.append((trial["label"], "began", probe_row(trial, row["post_to_press"], flight.times())))
    return joined


def probe_row(trial, post_to_press, times):
    after_post = lambda key: None if times[key] is None else post_to_press + times[key]
    minus = lambda a, b: None if a is None or b is None else a - b
    row = {
        "post_to_press": post_to_press,
        "post_to_show": after_post("show_given"),
        "post_to_landed": after_post("rini_sees_it_landed"),
        "post_to_look": after_post("focused_picture_captured"),
        "post_to_lift": after_post("lift"),
    }
    full = look(trial)[1] if trial.get("frames") else None
    row.update(
        landed_after_still=minus(row["post_to_landed"], trial.get("still_ms")),
        look_after_look_full=minus(row["post_to_look"], full),
        up_after_show=minus(trial.get("up_ms"), row["post_to_show"]),
        gone_after_lift=minus(trial.get("lift_ms"), row["post_to_lift"]),
    )
    return row


def probe(trials, found):
    lines = []
    by_label = defaultdict(list)
    for label, how, row in join(trials, found):
        by_label[label].append((how, row))
    for label, trials in by_label.items():
        hows = defaultdict(int)
        for how, _ in trials:
            hows[how] += 1
        joins = ", ".join(f"{hows[how]} {how}" for how in ("began", "joined", "another target", "unmatched"))
        lines.append(f"{label}: {len(trials)} trials, {joins}")
        rows = [row for how, row in trials if how == "began"]
        lines += table("  ms after the probe posted", rows, PROBE)
        lines.append("")
    return "\n".join(lines)


def generations(bursts):
    """Each generation's kind and focus target, as the last begin or merge in it says."""
    named = {}
    for _, marks in bursts:
        for mark in marks:
            if mark["mark"] in ("begin", "merge"):
                named[mark["g"]] = (mark["kind"], tuple(mark["focus"]) if mark["focus"] else None)
    return named


def handovers(bursts):
    """Each window of each measured lift: the kind of the generation that lifted, whether the window
    is its focus target, and its differences."""
    named = generations(bursts)
    rows = []
    for _, marks in bursts:
        for mark in marks:
            if mark["mark"] != "handover":
                continue
            kind, focus = named.get(mark["of"], (None, None))
            rows.append({
                "of": mark["of"],
                "kind": kind,
                "focused": focus is not None and tuple(mark["window"]) == focus,
                "interior": mark["interior"],
                "edge_in": mark["edge_in"],
                "edge_out": mark["edge_out"],
                "band": mark.get("band"),
            })
    return rows


def holds(bursts):
    """Each lift held for the overlay's picture, as its `hide` says: whether it hid on that picture,
    and how long it held, in ms."""
    return [{"measured": mark["measured"], "waited": mark["waited_us"] / 1000}
            for _, marks in bursts for mark in marks if mark["mark"] == "hide"]


def handover(bursts):
    rows = handovers(bursts)
    held = holds(bursts)
    headline = sorted(row["edge_in"] for row in rows if row["kind"] == HEADLINE_KIND and row["focused"])
    p99 = f"{percentile(headline, 99):.2f}" if headline else "-"
    lines = [
        f"{len({row['of'] for row in rows})} of {len(held)} held lifts measured, {len(rows)} windows",
        f"edge in p99, focused window, {HEADLINE_KIND}: {p99} n={len(headline)} ({RESEARCH})",
        "",
    ]
    lines += table("lift held for the overlay's picture (ms)", held, [("waited", "waited")])
    for kind in sorted({row["kind"] for row in rows if row["kind"]}):
        for focused in (True, False):
            group = [row for row in rows if row["kind"] == kind and row["focused"] == focused]
            if group:
                title = f"{kind}, {'focused' if focused else 'other'} window (levels)"
                lines += [""] + table(title, group, HANDOVER, digits=2)
    return "\n".join(lines)


def wears(found):
    """Every picture a tile took, flight by flight, as its `wears` mark says: the flight's generation,
    kind and focus target, when the tile took it, its route, whether it carries its window's blur, and
    its age in ms."""
    return [{
        "g": mark["g"],
        "kind": flight.kind(),
        "window": tuple(mark["window"]),
        "target": tuple(mark["window"]) == flight.target,
        "at": mark["at"],
        "route": mark["route"],
        "blur": mark["blur"],
        "age": mark["taken_before_us"] / 1000,
    } for flight in found for mark in flight.marks if mark["mark"] == "wears"]


def pictures(found, target=False):
    rows = [row for row in wears(found) if row["target"] or not target]
    lines = [f"{'g':>6s}  {'kind':12s} {'window':14s} {'at':13s} {'route':17s} blur  {'age ms':>9s}"]
    for row in rows:
        window = f"{row['window'][0]},{row['window'][1]}" + ("*" if row["target"] else "")
        lines.append(f"{row['g']:>6d}  {row['kind'] or '-':12s} {window:14s} {row['at']:13s} {row['route']:17s} "
                     f"{'yes ' if row['blur'] else 'no  '}  {row['age']:9.1f}")
    counts = defaultdict(int)
    for row in rows:
        counts[(row["at"], row["route"], row["blur"])] += 1
    lines += ["", f"{len(rows)} pictures worn, * the flight's focus target"]
    lines += [f"  {at:13s} {route:17s} {'blur' if blur else 'no blur':8s} {n}" for (at, route, blur), n in sorted(counts.items())]
    return "\n".join(lines)


def main_turns(marks):
    """Each `main_turn` in `marks`, where it is, its actors' polls in ms by name, and its window changes,
    each with how long before the turn began in ms, from the marks after it."""
    found = []
    current = None
    for at, mark in enumerate(marks):
        if mark["mark"] == "main_turn":
            current = (at, mark, {}, [])
            found.append(current)
        elif mark["mark"] == "turn_actor" and current is not None:
            current[2][mark["actor"]] = mark["us"] / 1000
        elif mark["mark"] == "turn_change" and current is not None:
            current[3].append((mark["what"], mark["before_us"] / 1000))
        else:
            current = None
    return found


def turns(bursts):
    """Every slow turn of the main thread: the flight it fell in, its generation and kind and the ms
    from its first mark (its press, or its begin) to the turn, all None for a turn at rest; how long the
    turn ran and the time outside its actors' polls, in ms; its actors and its window changes."""
    rows = []
    for header, marks in bursts:
        within = spans(marks)
        for at, mark, actors, changes in main_turns(marks):
            span = next(((start, end) for start, end in within if start <= at < end), None)
            flight = Flight(header, marks[span[0]:span[1]]) if span else None
            rows.append({
                "g": flight.g if flight else None,
                "kind": flight.kind() if flight else None,
                "after": ms(mark["t"], flight.marks[0]["t"]) if flight else None,
                "busy": mark["busy_us"] / 1000,
                "outside": mark["outside_us"] / 1000,
                "actors": actors,
                "changes": changes,
            })
    return rows


def turn_listing(bursts):
    rows = turns(bursts)
    lines = [f"{'g':>6s}  {'kind':12s} {'after ms':>9s} {'busy ms':>8s} {'outside':>8s}  "
             "actors at 0.1ms or more; window changes, ms before the turn"]
    for row in rows:
        g = f"{row['g']:>6d}" if row["g"] is not None else f"{'rest':>6s}"
        after = f"{row['after']:9.1f}" if row["after"] is not None else f"{'-':>9s}"
        ranked = sorted(row["actors"].items(), key=lambda item: -item[1])
        actors = ", ".join(f"{name} {took:.1f}" for name, took in ranked if took >= 0.1) or "-"
        changes = ", ".join(f"{what} {before:.1f}" for what, before in row["changes"]) or "-"
        lines.append(f"{g}  {row['kind'] or '-':12s} {after} {row['busy']:8.1f} {row['outside']:8.1f}  {actors}; {changes}")
    in_flights = sum(1 for row in rows if row["g"] is not None)
    lines += ["", f"{len(rows)} main-thread turns over 8ms, {in_flights} in flights"]
    lines += table("turns (ms)", rows, [("busy", "busy"), ("outside", "outside the actors")])
    return "\n".join(lines)


def journals(named):
    if named:
        return named
    paths = (os.path.join(LOGS, "transitions.1.jsonl"), os.path.join(LOGS, "transitions.jsonl"))
    return [path for path in paths if os.path.exists(path)]


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    summarise = commands.add_parser("summary")
    summarise.add_argument("--kind")
    summarise.add_argument("--csv")
    summarise.add_argument("journal", nargs="*")
    joining = commands.add_parser("probe")
    joining.add_argument("probe")
    joining.add_argument("journal", nargs="*")
    measuring = commands.add_parser("handover")
    measuring.add_argument("journal", nargs="*")
    picturing = commands.add_parser("pictures")
    picturing.add_argument("--target", action="store_true")
    picturing.add_argument("journal", nargs="*")
    turning = commands.add_parser("turns")
    turning.add_argument("journal", nargs="*")
    args = parser.parse_args(argv)
    bursts = read(journals(args.journal))
    found = flights(bursts)
    if args.command == "handover":
        print(handover(bursts))
    elif args.command == "pictures":
        print(pictures(found, args.target))
    elif args.command == "turns":
        print(turn_listing(bursts))
    elif args.command == "summary":
        if args.kind:
            found = [flight for flight in found if flight.kind() == args.kind]
        print(summary(found, bursts))
        if args.csv:
            write_csv(args.csv, found)
    else:
        with open(args.probe) as file:
            print(probe([json.loads(text) for text in file], found))


if __name__ == "__main__":
    main(sys.argv[1:])
