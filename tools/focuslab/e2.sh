#!/bin/bash
# E2: moving focus on the comms strip, VS Code 245 <-> Ghostty 56, against the build rini runs.
# Each argument after the output name labels one block of presses, which e2.py groups by.
cd /tmp/probe
OUT=$1; shift
first=1
for blk in "$@"; do
  if [ $first = 1 ]; then first=0; else FOCUSLAB_CONT=1; export FOCUSLAB_CONT; fi
  echo "$(date +%H:%M:%S) $blk" >> $OUT.blocks
  ./focuslab $OUT $blk key 6 650 0 35 40 A=12408,245,0,0,10,1700,37 B=3086,56,0,0,10,1140,38 > $OUT.log 2>&1 || { echo "stopped at block $blk: $(tail -1 $OUT.log)"; break; }
done
