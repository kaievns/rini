#!/usr/bin/env python3
"""What lab.sh and sheet.sh read: the lab's workspace and windows in `rini-cli query workspaces`, and
where one press's frames sit in a screenrec movie.

    rini-cli query workspaces | screenrec.py index WORKSPACE   the workspace's index, or exit 1
    rini-cli query workspaces | screenrec.py lab WORKSPACE     each lab window: id, title, frame
    screenrec.py cut MOV.json PRESS DUR_MS [x,y,w,h] [WIDTH]   ffmpeg's -ss and -t, then its -vf

PRESS counts from 1. The crop is in points, read at the movie's `scale`; the frames are scaled to
WIDTH pixels wide, 576 by default.
"""
import json
import sys

LEAD_MS = 20


def index(workspaces, name):
    """The index of the workspace called `name`, as `rini-cli execute workspace switch` takes it."""
    for workspace in workspaces:
        if workspace["name"] == name:
            return workspace["index"]
    return None


def lab(workspaces, name):
    """The windows titled `lab-*` the workspace called `name` lists: (id, title, frame)."""
    return [(found["id"], found.get("title"), found["frame"])
            for workspace in workspaces if workspace["name"] == name
            for found in workspace["windows"] if (found.get("title") or "").startswith("lab-")]


def cut(meta, press, dur_ms, crop=None, width=576):
    """Where press `press` of the run `meta` describes is in its movie, from `LEAD_MS` before it for
    `dur_ms`: (start in seconds, length in seconds, the frames' filter)."""
    at = meta["presses"][press - 1]["ms"] - LEAD_MS
    scaled = f"scale={width}:-1"
    if crop:
        x, y, w, h = (int(v) * meta["scale"] for v in crop.split(","))
        scaled = f"crop={w}:{h}:{x}:{y},{scaled}"
    return max(at, 0.0) / 1000, dur_ms / 1000, scaled


def main(argv):
    command = argv[0] if argv else ""
    if command == "index" and len(argv) == 2:
        found = index(json.load(sys.stdin), argv[1])
        if found is None:
            return 1
        print(found)
        return 0
    if command == "lab" and len(argv) == 2:
        for window, title, frame in lab(json.load(sys.stdin), argv[1]):
            print(json.dumps(window), title, json.dumps(frame))
        return 0
    if command == "cut" and 4 <= len(argv) <= 6:
        with open(argv[1]) as file:
            meta = json.load(file)
        crop = argv[4] if len(argv) > 4 and argv[4] else None
        width = int(argv[5]) if len(argv) > 5 else 576
        start, length, frames = cut(meta, int(argv[2]), float(argv[3]), crop, width)
        print(f"{start:.3f} {length:.3f} {frames}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
