#!/bin/bash
# battery.sh [options] <label>: one timing battery against the rini the service runs; the run goes in <out>/<label>/.
#   comms_pan: focus <comms-app> on <comms>, 8 pans Alt+J/Alt+L alternating, 1500ms apart  -> rec-comms_pan.jsonl
#   switch:    from there, 4 workspace switches Alt+;/Alt+P alternating, 1500ms apart      -> rec-switch.jsonl
#   spare_pan: focus <spare-app> on <spare>, 6 pans Alt+J/Alt+L alternating, 1500ms apart  -> rec-spare_pan.jsonl
# A screen test: it posts keys and records rini's overlay. It refuses unless the machine has been idle 300s, or idle
# since the last screen test marked it quiet; it aborts on any input it did not post, and puts focus back.
# Before each pan segment's first press it checks the scene (battery.py scene): the focused window is its workspace's
# leftmost column, and with --scene every window is where the reference had it; else it refuses (exit 2).
# battery.sh --check [options]: resolves the windows it would fly and prints them; posts nothing.
# README.md ("The battery") says what it records and how to read it.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
MAIN=$(cd "$(git -C "$HERE" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
RINI=$MAIN/target/release/rini
CLI=$MAIN/target/release/rini-cli
ROOT=/tmp/rini-focuslab
COMMS=comms; COMMS_APP=Code
SPARE=spare; SPARE_APP=Messages
BLUR_APP="Microsoft Outlook"
PANE=20,140,280,900
CHECK=0
SCENE=""
usage() {
  cat >&2 <<EOF
usage: battery.sh [--check] [--out DIR] [--comms WS] [--comms-app APP] [--spare WS] [--spare-app APP]
                  [--blur-app APP] [--pane x0,y0,x1,y1] [--scene FILE] <label>
  --out DIR        where runs go, DIR/<label> (default $ROOT)
  --comms WS       workspace of the first pans and the switches (default $COMMS)
  --comms-app APP  window focused on it before the pans (default $COMMS_APP)
  --spare WS       workspace of the second pans (default $SPARE)
  --spare-app APP  window focused on it before the pans (default $SPARE_APP)
  --blur-app APP   the window on WS spare whose fly-ins are checked for blur (default $BLUR_APP)
  --pane BOX       the box of the overlay, in points, whose chroma the blur check reads (default $PANE)
  --scene FILE     a run's meta.json, or a scene battery.py wrote: every window must be where it had it
EOF
  exit 2
}
while [ $# -gt 0 ]; do
  case $1 in
    --check) CHECK=1; shift ;;
    --out) ROOT=${2:?}; shift 2 ;;
    --comms) COMMS=${2:?}; shift 2 ;;
    --comms-app) COMMS_APP=${2:?}; shift 2 ;;
    --spare) SPARE=${2:?}; shift 2 ;;
    --spare-app) SPARE_APP=${2:?}; shift 2 ;;
    --blur-app) BLUR_APP=${2:?}; shift 2 ;;
    --pane) PANE=${2:?}; shift 2 ;;
    --scene) SCENE=${2:?}; shift 2 ;;
    -*) usage ;;
    *) break ;;
  esac
done
export PYTHONDONTWRITEBYTECODE=1
die() { echo "battery: $*" >&2; exit 2; }

ws() { "$CLI" query workspaces; }
lookup() { ws | python3 "$HERE/battery.py" "$@"; }
ws > /dev/null || die "$CLI query workspaces failed: is rini running?"
COMMS_ID=$(lookup window "$COMMS" "$COMMS_APP") || die "no $COMMS_APP window on workspace $COMMS"
SPARE_ID=$(lookup window "$SPARE" "$SPARE_APP") || die "no $SPARE_APP window on workspace $SPARE"
BLUR_PID=$(lookup pid "$SPARE" "$BLUR_APP") || die "no $BLUR_APP window on workspace $SPARE"
if [ $CHECK = 1 ]; then
  echo "$COMMS $COMMS_APP $COMMS_ID; $SPARE $SPARE_APP $SPARE_ID; $SPARE $BLUR_APP pid $BLUR_PID"
  exit 0
fi
[ $# = 1 ] || usage
LABEL=$1
OUT=$ROOT/$LABEL
FLAG=$ROOT/.input.flag
JDIR=$HOME/Library/Logs/rini
J=$JDIR/transitions.jsonl
GAP=1500

[ -e "$OUT" ] && die "$OUT exists; pick another label"
[ -z "$SCENE" ] || [ -f "$SCENE" ] || die "no scene at $SCENE"
[ -f "$J" ] || die "no journal at $J: turn on animations { journal } in rini's config"
idle=$(ioreg -c IOHIDSystem | awk '/HIDIdleTime/ {printf "%d", $NF/1e9; exit}')
python3 "$HERE/battery.py" quiet "$idle" || die "HID idle ${idle}s < 300s and input came since the last battery: Kai may be here"
PID=$(pgrep -f "^$RINI\$")
[ -n "$PID" ] && [ "$(echo "$PID" | wc -l | tr -d ' ')" = 1 ] || die "need exactly one $RINI running: '$PID'"

BIN=${XDG_CACHE_HOME:-$HOME/Library/Caches}/rini-focuslab
mkdir -p "$BIN"
for tool in rec inputwatch; do
  if [ ! "$BIN/$tool" -nt "$HERE/$tool.swift" ]; then
    swiftc -O "$HERE/$tool.swift" -o "$BIN/$tool" || die "swiftc $tool.swift failed"
  fi
done
mkdir -p "$OUT"
START=$(date +%s)

FOCUSED=$(lookup focused)
if [ -z "$FOCUSED" ]; then
  # Nothing managed is focused (another app is frontmost): start, and end, on the comms window.
  "$CLI" execute window focus --window-id "$COMMS_ID" > /dev/null 2>&1; sleep 2
  FOCUSED=$(printf '%s\t%s\t%s' "$COMMS_ID" "$COMMS" "$COMMS_APP")
fi
FOCUS_ID=$(echo "$FOCUSED" | cut -f1)

rm -f "$FLAG"
"$BIN/inputwatch" "$FLAG" > "$OUT/inputwatch.out" 2>&1 &
WATCH=$!
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do grep -q listening "$OUT/inputwatch.out" && break; sleep 0.25; done
grep -q listening "$OUT/inputwatch.out" || { kill $WATCH 2>/dev/null; die "inputwatch did not start: $(cat "$OUT/inputwatch.out")"; }

INODE=$(stat -f %i "$J"); L0=$(wc -l < "$J" | tr -d ' ')
ABORT=""
REFUSED=""
RESTORED=0
restore() {
  [ $RESTORED = 1 ] && return; RESTORED=1
  "$CLI" execute window focus --window-id "$FOCUS_ID" > /dev/null 2>&1 || echo "battery: restore focus failed" >&2
  sleep 2
  kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null
  [ -e "$FLAG" ] && [ -z "$ABORT" ] && ABORT=$(cat "$FLAG")
  [ -e "$FLAG" ] || python3 "$HERE/battery.py" mark
}
trap 'ABORT=${ABORT:-signal}; restore; exit 3' INT TERM
check() { if [ -e "$FLAG" ]; then ABORT=$(cat "$FLAG"); return 1; fi; kill -0 $WATCH 2>/dev/null || { ABORT="inputwatch died"; return 1; }; }
scene_check() { # scene_check <segment> <workspace> <app>: the scene the segment flies, kept as scene-<segment>.json
  local name=$1
  if ! ws | python3 "$HERE/battery.py" scene "$2" "$3" ${SCENE:+"$SCENE"} > "$OUT/scene-$name.json" 2> "$OUT/scene-$name.err"; then
    REFUSED="$name: $(tr '\n' ' ' < "$OUT/scene-$name.err")"; ABORT="refused, $REFUSED"; return 1
  fi
}
seq_keys() { local a=$1 b=$2 n=$3 out="" i; for ((i = 0; i < n; i++)); do if ((i % 2)); then out+="$b $GAP "; else out+="$a $GAP "; fi; done; echo $out; }
segment() { # segment <name> <keys...>
  local name=$1; shift
  check || return 1
  "$BIN/rec" "$PID" "$OUT/rec-$name.jsonl" 1.5 "$FLAG" "$PANE" "$@" > "$OUT/rec-$name.out" 2>&1
  local rc=$?
  [ $rc = 0 ] || { [ -e "$FLAG" ] && ABORT=$(cat "$FLAG") || ABORT="rec $name exited $rc: $(cat "$OUT/rec-$name.out")"; return 1; }
}
run() {
  "$CLI" execute window focus --window-id "$COMMS_ID" > /dev/null; sleep 1.5
  scene_check comms_pan "$COMMS" "$COMMS_APP" || return
  segment comms_pan $(seq_keys 38 37 8) || return
  sleep 0.5
  segment switch $(seq_keys 41 35 4) || return
  check || return
  "$CLI" execute window focus --window-id "$SPARE_ID" > /dev/null; sleep 1.5
  scene_check spare_pan "$SPARE" "$SPARE_APP" || return
  segment spare_pan $(seq_keys 38 37 6) || return
}
run
restore
trap - INT TERM

if [ "$(stat -f %i "$J")" = "$INODE" ]; then
  tail -n +$((L0 + 1)) "$J" > "$OUT/journal.slice.jsonl"
else
  { tail -n +$((L0 + 1)) "$JDIR/transitions.1.jsonl"; cat "$J"; } > "$OUT/journal.slice.jsonl"
fi
END=$(date +%s)
python3 - "$OUT/meta.json" "$OUT" "$LABEL" "$PID" "$START" "$END" "$FOCUSED" "$ABORT" \
  "$(shasum -a 256 "$RINI" | cut -c1-64)" "$(git -C "$MAIN" rev-parse --short HEAD 2>/dev/null)" "$L0" \
  "$COMMS" "$COMMS_APP" "$COMMS_ID" "$SPARE" "$SPARE_APP" "$SPARE_ID" "$BLUR_APP" "$BLUR_PID" "$PANE" <<'EOF'
import json, os, sys, time
(p, out, label, pid, start, end, focused, abort, sha, head, l0,
 comms, comms_app, comms_id, spare, spare_app, spare_id, blur_app, blur_pid, pane) = sys.argv[1:]
scene = {}
for segment in ("comms_pan", "spare_pan"):
    kept = os.path.join(out, f"scene-{segment}.json")
    if os.path.exists(kept) and os.path.getsize(kept):
        scene[segment] = json.load(open(kept))
json.dump({"label": label, "rini_pid": int(pid), "started": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(int(start))),
           "run_s": int(end) - int(start), "focused_at_start": focused.split("\t"), "aborted": abort or None,
           "binary_sha": sha, "repo_head": head, "journal_from_line": int(l0) + 1,
           "windows": {"comms": {"workspace": comms, "app": comms_app, "id": json.loads(comms_id)},
                       "spare": {"workspace": spare, "app": spare_app, "id": json.loads(spare_id)},
                       "blur": {"workspace": spare, "app": blur_app, "pid": int(blur_pid)}},
           "pane": [int(v) for v in pane.split(",")], "scene": scene}, open(p, "w"), indent=1)
EOF
python3 "$HERE/journal.py" summary "$OUT/journal.slice.jsonl" > "$OUT/journal-summary.txt" 2>&1
python3 "$HERE/journal.py" pictures --target "$OUT/journal.slice.jsonl" > "$OUT/pictures-target.txt" 2>&1
python3 "$HERE/analyze.py" "$OUT"
NOW=$(lookup focused | tr '\t' ' ')
echo "restored: now $NOW (start: $(echo "$FOCUSED" | tr '\t' ' '))"
[ -z "$REFUSED" ] || { echo "battery: $REFUSED" >&2; exit 2; }
[ -z "$ABORT" ] || exit 3
