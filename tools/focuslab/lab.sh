#!/bin/bash
# lab.sh <workspace> [letter...]: switches to the workspace named and opens, after its focused window, one Ghostty
# window titled lab-<letter> running pattern.py for each letter (A B C D by default), then prints each lab window the
# workspace lists: its id, title and frame. Ghostty restores none of the user's windows with --window-save-state=never;
# `kill -9` the lab windows' pids to close them without a save. A screen test: run it only while Kai is away.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
MAIN=$(cd "$(git -C "$HERE" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
CLI=$MAIN/target/release/rini-cli
[ $# -ge 1 ] || { echo "usage: lab.sh <workspace> [letter...]" >&2; exit 2; }
WS=$1; shift
LETTERS=("$@"); [ ${#LETTERS[@]} -gt 0 ] || LETTERS=(A B C D)
INDEX=$("$CLI" query workspaces | python3 -B "$HERE/screenrec.py" index "$WS") || { echo "lab: no workspace $WS" >&2; exit 2; }
"$CLI" execute workspace switch "$INDEX" > /dev/null; sleep 1.2
for L in "${LETTERS[@]}"; do
  open -na /Applications/Ghostty.app --args --window-save-state=never --quit-after-last-window-closed=true \
    --title="lab-$L" --font-size=13 -e /usr/bin/python3 "$HERE/pattern.py" "$L"
  sleep 2.5
done
sleep 2
"$CLI" query workspaces | python3 -B "$HERE/screenrec.py" lab "$WS"
