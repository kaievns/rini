#!/bin/bash
# screenrec.sh [--out DIR] <name> <code:mods:gap_ms>...: records the main display, rini's overlay and the real windows,
# while posting the presses, into <out>/<name>.mov, with the press and frame times in <name>.mov.json and the journal
# lines written meanwhile in <name>.journal.jsonl.
# A screen test: it refuses unless the machine has been idle 300s, or idle since the last battery or recording ended;
# it stops on any input it did not post, and puts focus back. README.md ("Watching a resize") says how to read a run.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
MAIN=$(cd "$(git -C "$HERE" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
CLI=$MAIN/target/release/rini-cli
ROOT=/tmp/rini-focuslab
OUT=$ROOT/screenrec
TAIL=1.5
usage() {
  echo "usage: screenrec.sh [--out DIR] <name> <code:mods:gap_ms>...  (default DIR $OUT)" >&2
  exit 2
}
[ "${1:-}" = --out ] && { OUT=${2:?}; shift 2; }
[ $# -ge 2 ] || usage
NAME=$1; shift
export PYTHONDONTWRITEBYTECODE=1
die() { echo "screenrec: $*" >&2; exit 2; }

FLAG=$ROOT/.input.flag
JDIR=$HOME/Library/Logs/rini
J=$JDIR/transitions.jsonl
MOV=$OUT/$NAME.mov
[ -e "$MOV" ] && die "$MOV exists; pick another name"
[ -f "$J" ] || die "no journal at $J: turn on animations { journal } in rini's config"
idle=$(ioreg -c IOHIDSystem | awk '/HIDIdleTime/ {printf "%d", $NF/1e9; exit}')
python3 "$HERE/battery.py" quiet "$idle" || die "HID idle ${idle}s < 300s and input came since the last session: Kai may be here"

BIN=${XDG_CACHE_HOME:-$HOME/Library/Caches}/rini-focuslab
mkdir -p "$BIN" "$ROOT" "$OUT"
for tool in screenrec inputwatch; do
  if [ ! "$BIN/$tool" -nt "$HERE/$tool.swift" ]; then
    swiftc -O "$HERE/$tool.swift" -o "$BIN/$tool" || die "swiftc $tool.swift failed"
  fi
done

FOCUS_ID=$("$CLI" query workspaces | python3 "$HERE/battery.py" focused | cut -f1)
rm -f "$FLAG"
"$BIN/inputwatch" "$FLAG" > "$OUT/$NAME.inputwatch.out" 2>&1 &
WATCH=$!
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do grep -q listening "$OUT/$NAME.inputwatch.out" && break; sleep 0.25; done
grep -q listening "$OUT/$NAME.inputwatch.out" || { kill $WATCH 2>/dev/null; die "inputwatch did not start"; }

INODE=$(stat -f %i "$J"); L0=$(wc -l < "$J" | tr -d ' ')
RESTORED=0
restore() {
  [ $RESTORED = 1 ] && return; RESTORED=1
  [ -n "$FOCUS_ID" ] && { "$CLI" execute window focus --window-id "$FOCUS_ID" > /dev/null 2>&1 || echo "screenrec: restore focus failed" >&2; }
  sleep 1
  kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null
  [ -e "$FLAG" ] || python3 "$HERE/battery.py" mark
}
trap 'restore; exit 3' INT TERM
"$BIN/screenrec" "$MOV" $TAIL "$FLAG" "$@"; rc=$?
restore
if [ "$(stat -f %i "$J")" = "$INODE" ]; then
  tail -n +$((L0 + 1)) "$J" > "$OUT/$NAME.journal.jsonl"
else
  { tail -n +$((L0 + 1)) "$JDIR/transitions.1.jsonl"; cat "$J"; } > "$OUT/$NAME.journal.jsonl"
fi
if [ -e "$FLAG" ]; then
  echo "screenrec: stopped by input: $(cat "$FLAG")" >&2
  exit 3
fi
exit $rc
