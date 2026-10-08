"""Each timed call of a place drain, summarised: python3 summary.py place-before.jsonl.

One line a place drain, from a scratch build that timed each call on the main thread; each key is
[calls, microseconds]. s_place is the renderer's PlaceRest step whole, s_play the drain's own
transaction, committed and flushed after it; the rest are the calls inside s_place."""
import json
import statistics
import sys

drains = [json.loads(line) for line in open(sys.argv[1])]
print(len(drains), "place drains")
for key in ["s_place", "flush", "commit", "set_frame", "order", "alpha0", "lay", "s_play"]:
    took = sorted(drain[key][1] / 1000 for drain in drains if key in drain)
    if took:
        print(f"{key:10} in {len(took):3}  p50 {statistics.median(took):5.2f}  p90 {took[int(len(took) * 0.9)]:5.2f}  max {took[-1]:6.2f} ms")
