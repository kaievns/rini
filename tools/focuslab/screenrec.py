#!/usr/bin/env python3
"""What lab.sh and sheet.sh read: the lab's workspace and windows in `rini-cli query workspaces`, and
where one press's frames sit in a screenrec movie; and how much of each frame is ground.

    rini-cli query workspaces | screenrec.py index WORKSPACE   the workspace's index, or exit 1
    rini-cli query workspaces | screenrec.py lab WORKSPACE     each lab window: id, title, frame
    screenrec.py cut MOV.json PRESS DUR_MS [x,y,w,h] [WIDTH]   ffmpeg's -ss and -t, then its -vf
    screenrec.py blank MOV                                     per frame: its ms, its flat dark blocks

PRESS counts from 1. The crop is in points, read at the movie's `scale`; the frames are scaled to
WIDTH pixels wide, 576 by default. `blank` reads MOV.json beside the movie and decodes it with ffmpeg
to grey at 432 pixels wide, where a block of 8x8 pixels is 1/54 of the frame's width whatever its
scale, 32pt on a 1728pt-wide display; a block is flat dark when its mean is under 60 and its
variance under 6, and the top 2 block rows, the bar, are skipped. A frame with nothing moving
counts the screen's own dark blocks, a baseline to read against.
"""
import json
import subprocess
import sys

LEAD_MS = 20
BLANK_WIDTH = 432
BLOCK = 8
BAR_ROWS = 2
DARK = 60
FLAT = 6


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


def blank_blocks(grey, width, height):
    """How many 8x8 blocks of a frame of `width` x `height` grey bytes are flat and dark, the bar rows
    and any part block at the right and bottom edges left out."""
    count = 0
    area = BLOCK * BLOCK
    for row in range(BAR_ROWS, height // BLOCK):
        for column in range(width // BLOCK):
            total = squares = 0
            for y in range(row * BLOCK, row * BLOCK + BLOCK):
                at = y * width + column * BLOCK
                for value in grey[at:at + BLOCK]:
                    total += value
                    squares += value * value
            mean = total / area
            if mean < DARK and squares / area - mean * mean < FLAT:
                count += 1
    return count


def blank(mov):
    """Each frame of the movie `mov`: (its ms in the movie's JSON, its flat dark blocks)."""
    with open(mov + ".json") as file:
        times = json.load(file)["frames_ms"]
    size = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                           "stream=width,height", "-of", "csv=p=0", mov],
                          capture_output=True, text=True, check=True).stdout
    source_width, source_height = (int(v) for v in size.strip().split(","))
    width, height = BLANK_WIDTH, source_height * BLANK_WIDTH // source_width
    decoding = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "quiet", "-i", mov,
                                 "-fps_mode", "passthrough", "-vf", f"scale={width}:{height},format=gray",
                                 "-f", "rawvideo", "-"], stdout=subprocess.PIPE)
    counts = []
    while True:
        grey = decoding.stdout.read(width * height)
        if len(grey) < width * height:
            break
        counts.append(blank_blocks(grey, width, height))
    if decoding.wait() != 0 or len(counts) != len(times):
        raise SystemExit(f"{mov}: decoded {len(counts)} frames, its JSON times {len(times)}")
    return list(zip(times, counts))


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
    if command == "blank" and len(argv) == 2:
        for at, count in blank(argv[1]):
            print("%8.1f %4d %s" % (at, count, "#" * (count // 4)))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
