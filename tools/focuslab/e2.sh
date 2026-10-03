#!/bin/bash
# E2: moving focus on the comms strip, VS Code 245 <-> Ghostty 56, flag sets interleaved in blocks.
cd /tmp/probe
OUT=$1; shift
idle() { ioreg -c IOHIDSystem | awk '/HIDIdleTime/ {printf "%.1f", $NF/1e9; exit}'; }
first=1
for blk in "$@"; do
  case $blk in
    A) echo 'label=A f1 f2 f3 f4 f6 zc bg=1' > ~/.config/rini/proto ;;
    B) echo 'label=B f1 f2 f3 f4 f6 zc bg=2 early bgvis pos1' > ~/.config/rini/proto ;;
    C) echo 'label=C f1 f2 f3 f4 f6 zc bg=2 early bgvis pos1 mkfirst land0' > ~/.config/rini/proto ;;
    D) echo 'label=D f1 f2 f3 f4 f6 zc bg=2 early bgvis pos1 mkfirst land0 fcache' > ~/.config/rini/proto ;;
    *) cp "$blk" ~/.config/rini/proto ;;
  esac
  if [ $first = 1 ]; then first=0; else FOCUSLAB_CONT=1; export FOCUSLAB_CONT; fi
  echo "$(date +%H:%M:%S) $blk" >> $OUT.blocks
  ./focuslab $OUT $blk key 6 650 0 35 40 A=12408,245,0,0,10,1700,37 B=3086,56,0,0,10,1140,38 > $OUT.log 2>&1 || { echo "stopped at block $blk: $(tail -1 $OUT.log)"; break; }
done
echo 'label=proto f1 f2 f3 f4 f6 zc bg=2 early bgvis pos1 mkfirst land0' > ~/.config/rini/proto
