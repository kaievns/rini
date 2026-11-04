import json, sys, collections, datetime
WIDTH = {"cycle_preset_column_width","toggle_fullscreen_within_gaps","resize_window_by"}
BIRTH = {"window_created","windows_discovered","application_launched"}
day = None
by_day = collections.Counter(); by_day_what = collections.Counter()
births = collections.Counter(); whats = collections.Counter(); mouse = collections.Counter()
for line in open(sys.argv[1]):
    if '"burst"' in line:
        d = json.loads(line)
        if "unix_ms" in d:
            day = datetime.datetime.fromtimestamp(d["unix_ms"]/1000).strftime("%m-%d")
        continue
    if '"press"' not in line: continue
    d = json.loads(line)
    if d.get("mark") != "press": continue
    w = d.get("what")
    whats[w] += 1
    if w in WIDTH:
        by_day[day] += 1; by_day_what[(day,w)] += 1
    if w in BIRTH: births[w] += 1
    if w == "mouse_up": mouse[day] += 1
print("width by day", sorted(by_day.items()), sum(by_day.values()))
for k in sorted(by_day_what): print(k, by_day_what[k])
print("births", births)
print("mouse_up", sum(mouse.values()), sorted(mouse.items()))
print("top whats", whats.most_common(40))
