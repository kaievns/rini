import json, sys, datetime, bisect
WIDTH = {"cycle_preset_column_width","toggle_fullscreen_within_gaps","resize_window_by"}
BIRTH = {"window_created","windows_discovered","application_launched"}
LAB = {"10-09","10-10","10-11"}
anchor=None; ev=[]
for line in open(sys.argv[1]):
    if '"burst"' in line:
        d=json.loads(line)
        if "unix_ms" in d: anchor=(d["unix_ms"], d["t"])
        continue
    if '"mark":"press"' not in line: continue
    d=json.loads(line); w=d.get("what")
    if w not in WIDTH and w not in BIRTH: continue
    ms = anchor[0] + (d["t"]-anchor[1])/1e6
    ev.append((ms,w))
ev.sort()
def day(ms): return datetime.datetime.fromtimestamp(ms/1000).strftime("%m-%d")
for label, keep in (("all", lambda m: True), ("non-lab", lambda m: day(m) not in LAB)):
    b=[m for m,w in ev if w in BIRTH and keep(m)]
    p=[m for m,w in ev if w in WIDTH and keep(m)]
    bk=[(m,w) for m,w in ev if w in BIRTH and keep(m)]
    import collections
    kinds=collections.Counter(w for m,w in bk)
    followed=sum(1 for m in b if any(0<=x-m<=60000 for x in p[bisect.bisect_left(p,m):bisect.bisect_left(p,m)+5]))
    late=0
    for x in p:
        i=bisect.bisect_right(b,x)
        if i==0 or x-b[i-1]>=600000: late+=1
    print(label, "births",len(b),dict(kinds),"followed<=60s",followed,"presses",len(p),"presses>=10min after birth",late)
