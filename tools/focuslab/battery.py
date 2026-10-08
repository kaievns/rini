#!/usr/bin/env python3
"""What battery.sh and measure.sh set up: the windows the battery flies, found by app and workspace
name in `rini-cli query workspaces`, and the ring turned on or off in rini's config.

    rini-cli query workspaces | battery.py window WORKSPACE APP   {"pid": .., "idx": ..}, or exit 1
    rini-cli query workspaces | battery.py pid WORKSPACE APP      the app's pid, or exit 1
    rini-cli query workspaces | battery.py focused                id, workspace and app, tab-separated
    battery.py ring on|off|state CONFIG

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
    if command == "focused" and len(argv) == 1:
        found = focused(json.load(sys.stdin))
        if found is not None:
            print(json.dumps(found[0]) + "\t" + found[1] + "\t" + found[2])
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
