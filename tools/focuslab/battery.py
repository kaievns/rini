#!/usr/bin/env python3
"""What battery.sh and measure.sh set up: the windows the battery flies, found by app and workspace
name in `rini-cli query workspaces`, and the ring turned on or off in rini's config.

    rini-cli query workspaces | battery.py window WORKSPACE APP   {"pid": .., "idx": ..}, or exit 1
    rini-cli query workspaces | battery.py pid WORKSPACE APP      the app's pid, or exit 1
    rini-cli query workspaces | battery.py focused                id, workspace and app, tab-separated
    rini-cli query workspaces | battery.py scene WORKSPACE APP [REFERENCE]
                                                                  the scene as JSON, or exit 1 naming why
                                                                  a segment pressed from APP would fly
                                                                  another
    battery.py ring on|off|state CONFIG

`scene` reads the workspace named as its first press would fly it: APP's window must be its leftmost
column, so the first Alt+J bounces, and with REFERENCE, a scene file or a run's meta.json, every
window must be where that scene had it, by app, within FRAME_SLACK points.

`ring` edits only `layout { focus-ring { ... } }` written over several lines, adding or removing
its `off` line, and exits 2, changing nothing, when there is not exactly one such block.
"""
import json
import re
import sys


class NoRing(Exception):
    pass


def window(workspaces, name, app):
    """The first window of `app` the workspace called `name` lists, as {"pid", "idx"}, or None."""
    for workspace in workspaces:
        if workspace["name"] != name:
            continue
        for found in workspace["windows"]:
            if found.get("app_name") == app:
                return {"pid": found["id"]["pid"], "idx": found["id"]["idx"]}
    return None


def focused(workspaces):
    """The focused window on the active workspace: (its id, the workspace, the app), or None."""
    for workspace in workspaces:
        if not workspace["is_active"]:
            continue
        for found in workspace["windows"]:
            if found["is_focused"]:
                return ({"pid": found["id"]["pid"], "idx": found["id"]["idx"]}, workspace["name"],
                        found.get("app_name") or "")
    return None


FRAME_SLACK = 1.0


def frame_of(found):
    """A window's frame as `query workspaces` gives it: [x, y, width, height]."""
    frame = found["frame"]
    return [frame["origin"]["x"], frame["origin"]["y"], frame["size"]["width"], frame["size"]["height"]]


def scene(workspaces, name, app):
    """The workspace called `name` as a segment pressed from `app`'s window flies it: the frame of that
    window, the first of `app` it lists as `window` finds it (`focus`, None with none), and its
    windows, by app and frame, floating or not, left to right and top to bottom. None with no such
    workspace."""
    for workspace in workspaces:
        if workspace["name"] != name:
            continue
        windows = [{"app": found.get("app_name") or "", "frame": frame_of(found),
                    "floating": bool(found.get("is_floating"))} for found in workspace["windows"]]
        windows.sort(key=lambda w: (w["floating"], w["frame"][0], w["frame"][1], w["app"]))
        first = next((found for found in workspace["windows"] if found.get("app_name") == app), None)
        return {"workspace": name, "app": app, "focus": first and frame_of(first), "windows": windows}
    return None


def same_frame(a, b):
    return all(abs(x - y) <= FRAME_SLACK for x, y in zip(a, b))


# Below every row a tiled window is laid at on Kai's 1117pt display, above every park: macOS clamps a
# parked window's corner to y 1085-1116, and where depends on the launch, so a park is matched by size.
PARK_TOP = 1000


def same_place(a, b):
    """Whether two frames put a window in the same place: equal within a point, or both parked with
    the same size."""
    if a[1] >= PARK_TOP and b[1] >= PARK_TOP:
        return same_frame(a[2:], b[2:])
    return same_frame(a, b)


def scene_problems(found, reference=None):
    """Why a segment would not fly the scene it should: `app`'s window not the leftmost column, so its
    first press, Alt+J, pans rather than bounces; and, given `reference`, any window missing, added or
    elsewhere. Empty when it flies it."""
    if found is None:
        return ["no such workspace"]
    if found["focus"] is None:
        return [f"no {found['app']} window on {found['workspace']}"]
    problems = []
    tiled = [w for w in found["windows"] if not w["floating"]]
    left = min((w["frame"][0] for w in tiled), default=found["focus"][0])
    if found["focus"][0] - left > FRAME_SLACK:
        problems.append(f"{found['app']} is not the leftmost column of {found['workspace']} "
                        f"(x {found['focus'][0]:g} against {left:g}): its first press would pan, not bounce")
    if reference is not None:
        unmatched = list(reference["windows"])
        for w in found["windows"]:
            match = next((r for r in unmatched if r["app"] == w["app"] and r["floating"] == w["floating"]
                          and same_place(r["frame"], w["frame"])), None)
            if match is None:
                problems.append(f"{w['app'] or '?'} at {w['frame']} is not in the reference")
            else:
                unmatched.remove(match)
        problems += [f"{r['app'] or '?'} at {r['frame']} is missing" for r in unmatched]
    return problems


def reference_scene(data, name):
    """The scene of workspace `name` in `data`: a scene itself, or a run's meta.json, which keeps one
    a segment under `scene`. None when it holds none of that workspace."""
    if data.get("workspace") == name:
        return data
    for kept in (data.get("scene") or {}).values():
        if kept.get("workspace") == name:
            return kept
    return None


OPEN = re.compile(r"^\s*([A-Za-z0-9_-]+)\b[^{}]*\{\s*(//.*)?$")
CLOSE = re.compile(r"^\s*\}\s*(//.*)?$")
OFF = re.compile(r"^\s*off;?\s*(//.*)?$")


def ring_block(lines):
    """The lines of the one `focus-ring` block directly inside `layout`: (its opening line, its
    closing line, the indent of its children)."""
    stack, found = [], []
    for at, line in enumerate(lines):
        if line.strip().startswith("//"):
            continue
        opened = OPEN.match(line)
        if opened:
            stack.append((opened.group(1), at))
            continue
        if CLOSE.match(line) and stack:
            name, start = stack.pop()
            if name == "focus-ring" and [n for n, _ in stack] == ["layout"]:
                found.append((start, at))
    if len(found) != 1:
        raise NoRing(f"{len(found)} multi-line focus-ring blocks inside layout, not 1")
    start, end = found[0]
    indent = re.match(r"\s*", lines[start]).group(0) + "    "
    return start, end, indent


def ring_state(text):
    lines = text.split("\n")
    start, end, _ = ring_block(lines)
    return "off" if any(OFF.match(line) for line in lines[start + 1:end]) else "on"


def set_ring(text, state):
    """`text` with the ring `state` ("on" or "off"), all else as it was."""
    lines = text.split("\n")
    start, end, indent = ring_block(lines)
    body = [line for line in lines[start + 1:end] if not OFF.match(line)]
    if state == "off":
        body.insert(0, indent + "off")
    return "\n".join(lines[:start + 1] + body + lines[end:])


def main(argv):
    command = argv[0] if argv else ""
    if command == "ring" and len(argv) == 3 and argv[1] in ("on", "off", "state"):
        path = argv[2]
        with open(path) as file:
            text = file.read()
        try:
            if argv[1] == "state":
                print(ring_state(text))
                return 0
            changed = set_ring(text, argv[1])
        except NoRing as error:
            print(f"battery.py: {path}: {error}", file=sys.stderr)
            return 2
        if changed != text:
            with open(path, "w") as file:
                file.write(changed)
        return 0
    if command in ("window", "pid") and len(argv) == 3:
        found = window(json.load(sys.stdin), argv[1], argv[2])
        if found is None:
            return 1
        print(json.dumps(found) if command == "window" else found["pid"])
        return 0
    if command == "scene" and len(argv) in (3, 4):
        found = scene(json.load(sys.stdin), argv[1], argv[2])
        reference = None
        if len(argv) == 4:
            with open(argv[3]) as file:
                reference = reference_scene(json.load(file), argv[1])
            if reference is None:
                print(f"battery.py: {argv[3]} holds no scene of {argv[1]}", file=sys.stderr)
                return 1
        problems = scene_problems(found, reference)
        if found is not None:
            print(json.dumps(found))
        for problem in problems:
            print(f"scene of {argv[1]}: {problem}", file=sys.stderr)
        return 1 if problems else 0
    if command == "focused" and len(argv) == 1:
        found = focused(json.load(sys.stdin))
        if found is not None:
            print(json.dumps(found[0]) + "\t" + found[1] + "\t" + found[2])
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
