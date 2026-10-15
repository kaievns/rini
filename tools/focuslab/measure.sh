#!/bin/bash
# measure.sh [--out DIR] <binary> <prefix> [battery options]: deploys <binary> as the service's rini, then runs
# battery.sh with the ring off (<prefix>-off-warm, -1, -2, -3) and on (<prefix>-on-warm, -1, -2, -3), and puts rini's
# config back as it found it. The binary stays deployed; the one it replaced is kept, and the line to roll back printed.
# measure.sh [--out DIR] --deploy <binary>: the deploy alone, a rollback included.
#
# The deploy is .claude/rules/deploying-rini.md's, in order, and stops at the first check that fails: a binary
# signed as git.kaievns.rini with a certificate leaf, moved INTO target/release/rini (copy, then rename), the plist
# running target/release/rini, `target/release/rini service restart` (never --move: a refusal stops it), and the
# new process running from that same path. Nothing here builds. Any other path costs Kai a re-grant: stop and ask.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
MAIN=$(cd "$(git -C "$HERE" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
DEPLOYED=$MAIN/target/release/rini
PLIST=$HOME/Library/LaunchAgents/git.kaievns.rini.plist
CONFIG=$HOME/.config/rini/config.kdl
ROOT=/tmp/rini-focuslab
DEPLOY_ONLY=0
export PYTHONDONTWRITEBYTECODE=1
die() { echo "measure: $*" >&2; exit 2; }
usage() { sed -n '2,5p' "$0" | cut -c3- >&2; exit 2; }
while [ $# -gt 0 ]; do
  case $1 in
    --out) ROOT=${2:?}; shift 2 ;;
    --deploy) DEPLOY_ONLY=1; shift ;;
    *) break ;;
  esac
done
if [ $DEPLOY_ONLY = 1 ]; then [ $# = 1 ] || usage; else [ $# -ge 2 ] || usage; fi
BINARY=$1
PREFIX=${2:-}
[ $# -ge 2 ] && shift 2 || shift 1
BATTERY_ARGS=("$@")

signed() { codesign -d -r- "$1" 2>&1 | grep -F 'designated => identifier "git.kaievns.rini"' | grep -qF 'certificate leaf'; }
plist_runs_deployed() { [ "$(plutil -extract ProgramArguments.0 raw "$PLIST" 2>/dev/null)" = "$DEPLOYED" ]; }
running() { pgrep -f "^$DEPLOYED\$"; }

deploy() {
  local old new command sha keep
  signed "$BINARY" || die "$BINARY is not signed as git.kaievns.rini with a certificate leaf (codesign -f -s rini-dev --identifier git.kaievns.rini)"
  plist_runs_deployed || die "$PLIST does not run $DEPLOYED: stop and ask Kai (a path change costs a re-grant)"
  old=$(running)
  [ -n "$old" ] && [ "$(echo "$old" | wc -l | tr -d ' ')" = 1 ] || die "need exactly one $DEPLOYED running: '$old'"
  sha=$(shasum -a 256 "$DEPLOYED" | cut -c1-12)
  keep=$ROOT/rollback/rini-$sha
  mkdir -p "$ROOT/rollback"
  [ -e "$keep" ] || cp "$DEPLOYED" "$keep" || die "could not keep the deployed binary at $keep"
  cp "$BINARY" "$DEPLOYED.new" && mv -f "$DEPLOYED.new" "$DEPLOYED" || die "could not move $BINARY into $DEPLOYED"
  signed "$DEPLOYED" || die "$DEPLOYED lost its certificate leaf; roll back: $0 --deploy $keep"
  plist_runs_deployed || die "$PLIST does not run $DEPLOYED: stop and ask Kai"
  if ! "$DEPLOYED" service restart > "$ROOT/restart.out" 2>&1; then
    cat "$ROOT/restart.out" >&2
    die "service restart refused or failed: fix the path, never --move; roll back: $0 --deploy $keep"
  fi
  for _ in $(seq 1 20); do sleep 1; new=$(running); [ -n "$new" ] && [ "$new" != "$old" ] && break; done
  [ -n "$new" ] && [ "$new" != "$old" ] || die "no new rini at $DEPLOYED 20s after the restart"
  command=$(ps -o command= -p "$new")
  [ "$command" = "$DEPLOYED" ] || die "the service runs '$command', not $DEPLOYED: stop and ask Kai"
  echo "deployed $(shasum -a 256 "$BINARY" | cut -c1-12) pid $new; roll back: $0 --deploy $keep"
}

if [ $DEPLOY_ONLY = 1 ]; then
  mkdir -p "$ROOT"
  deploy
  exit 0
fi

[ -n "$PREFIX" ] || usage
for p in "$PREFIX"-off-warm "$PREFIX"-on-warm; do [ -e "$ROOT/$p" ] && die "$ROOT/$p exists; pick another prefix"; done
idle=$(ioreg -c IOHIDSystem | awk '/HIDIdleTime/ {printf "%d", $NF/1e9; exit}')
[ "$idle" -ge 300 ] || die "HID idle ${idle}s < 300s: Kai may be here"
mkdir -p "$ROOT"
python3 "$HERE/battery.py" ring state "$CONFIG" > /dev/null || die "cannot turn the ring on and off in $CONFIG"
KEPT=$ROOT/$PREFIX.config.kdl
cp "$CONFIG" "$KEPT" || die "could not keep $CONFIG"
put_back() { cmp -s "$KEPT" "$CONFIG" || cp "$KEPT" "$CONFIG" || echo "measure: put $KEPT back at $CONFIG by hand" >&2; }
trap put_back EXIT
trap 'exit 3' INT TERM
python3 "$HERE/battery.py" mark

ring() {
  python3 "$HERE/battery.py" ring "$1" "$CONFIG" || die "could not turn the ring $1"
  [ "$(python3 "$HERE/battery.py" ring state "$CONFIG")" = "$1" ] || die "the ring is not $1 in $CONFIG"
  sleep 3
}
runs() {
  local label rc
  for label in "$1-warm" "$1-1" "$1-2" "$1-3"; do
    "$HERE/battery.sh" --out "$ROOT" ${BATTERY_ARGS[@]+"${BATTERY_ARGS[@]}"} "$label" > "$ROOT/$label.out" 2>&1
    rc=$?
    echo "$label exit $rc"
    [ $rc = 0 ] || die "$label stopped (exit $rc, $ROOT/$label.out); the session ends here"
  done
}

ring off
deploy
for _ in $(seq 1 40); do "$HERE/battery.sh" --check ${BATTERY_ARGS[@]+"${BATTERY_ARGS[@]}"} > /dev/null 2>&1 && break; sleep 2; done
"$HERE/battery.sh" --check ${BATTERY_ARGS[@]+"${BATTERY_ARGS[@]}"} || die "the new rini does not list the battery's windows"
sleep 15
runs "$PREFIX-off"
ring on
runs "$PREFIX-on"
echo "done: python3 $HERE/compare.py $ROOT/$PREFIX-off-{1,2,3} -- $ROOT/$PREFIX-on-{1,2,3}"
