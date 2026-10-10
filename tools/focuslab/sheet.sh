#!/bin/bash
# sheet.sh <run.mov> <press> [dur_ms=700] [crop x,y,w,h in points] [cols=4] [width=576]: press <press> (from 1) of a
# screenrec run, from 20ms before it, frame by frame: the frames in <run>-p<press>/ and a contact sheet of them,
# <run>-p<press>.png, beside the movie.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
[ $# -ge 2 ] || { echo "usage: sheet.sh <run.mov> <press> [dur_ms] [x,y,w,h] [cols] [width]" >&2; exit 2; }
MOV=$1 PRESS=$2 DUR=${3:-700} CROP=${4:-} COLS=${5:-4} WIDTH=${6:-576}
read -r SS T VF < <(python3 -B "$HERE/screenrec.py" cut "$MOV.json" "$PRESS" "$DUR" "$CROP" "$WIDTH")
DIR=${MOV%.mov}-p$PRESS
mkdir -p "$DIR"
ffmpeg -hide_banner -loglevel quiet -y -ss "$SS" -i "$MOV" -t "$T" -fps_mode passthrough -vf "$VF" "$DIR/%03d.png"
N=$(ls "$DIR" | wc -l | tr -d ' '); ROWS=$(( (N + COLS - 1) / COLS ))
ffmpeg -hide_banner -loglevel error -y -i "$DIR/%03d.png" -vf "tile=${COLS}x${ROWS}:padding=6:color=red" -frames:v 1 "$DIR.png"
echo "$DIR.png frames=$N"
