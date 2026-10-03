import re,statistics as st,collections,sys
exec(open('/tmp/journal-parse.py').read().split("def wid(s):")[0])
def wid(s):
    m=re.search(r'pid: (\d+), idx: (\d+)',s); return (int(m[1]),int(m[2])) if m else None
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
lo,hi=sys.argv[1],sys.argv[2]
apps={}
for t,e,th,d in ev:
    m=re.match(r'([\w.\-]+)\((\d+)\)',th)
    if m: apps[int(m[2])]=m[1].split('.')[-1]
E=[x for x in ev if lo<=fmt(x[0])[:8]<=hi]
cmds=[i for i,x in enumerate(E) if x[1]=='cmd' and ('MoveFocus' in x[3] or 'Workspace' in x[3])]
rows=collections.defaultdict(list)
for n,i in enumerate(cmds):
    t0=E[i][0]; end=E[cmds[n+1]][0] if n+1<len(cmds) else t0+800
    seg=[x for x in E[i:] if x[0]<min(end,t0+800)]
    disp=next(((t,d) for t,e,th,d in seg if e=='raise_dispatch'),None)
    if not disp: continue
    target=wid(disp[1].split('raise=')[1]) if 'raise=[' in disp[1] and 'WindowId' in disp[1].split('raise=')[1] else wid(disp[1])
    if not target: continue
    g=lambda k,f=lambda th,d:True: next((t-t0 for t,e,th,d in seg if e==k and f(th,d)),None)
    rs=g('raise_start',lambda th,d: wid(d)==target)
    mk=g('make_key_done',lambda th,d: wid(d)==target)
    ax=g('axraise_done',lambda th,d: wid(d)==target)
    fo=g('ws_focus',lambda th,d: wid(d)==target)
    sf=[(t-t0,e) for t,e,th,d in seg if e in('setframe_start','setframe_done') and wid(d)==target]
    busy=[ (t-t0) for t,e,th,d in seg if e=='setframe_start' and wid(d) and wid(d)[0]==target[0] and (t-t0)<(rs or 0)]
    rows[apps.get(target[0],'?')].append(dict(disp=disp[0]-t0,rs=rs,mk=mk,ax=ax,fo=fo,busy=len(busy)))
def P(xs):
    xs=sorted(x for x in xs if x is not None)
    if not xs: return '      -      '
    q=lambda p: xs[min(len(xs)-1,int(round(p*(len(xs)-1))))]
    return f"{q(.5):5.1f} /{q(.9):5.1f} /{q(.99):5.1f}"
print(f"{'app':10s} {'n':>3s} | {'dispatch':^19s} | {'actor picks up':^19s} | {'make_key done':^19s} | {'AX raise done':^19s} | {'ws reports focus':^19s} | setframes to that app queued first")
for a,r in sorted(rows.items(), key=lambda kv:-len(kv[1])):
    print(f"{a[:10]:10s} {len(r):3d} | {P([x['disp'] for x in r])} | {P([x['rs'] for x in r])} | {P([x['mk'] for x in r])} | {P([x['ax'] for x in r])} | {P([x['fo'] for x in r])} | {sum(1 for x in r if x['busy'])}/{len(r)}")
allr=[x for r in rows.values() for x in r]
print(f"{'ALL':10s} {len(allr):3d} | {P([x['disp'] for x in allr])} | {P([x['rs'] for x in allr])} | {P([x['mk'] for x in allr])} | {P([x['ax'] for x in allr])} | {P([x['fo'] for x in allr])}")
print("ws focus - make_key:",P([x['fo']-x['mk'] for x in allr if x['fo'] is not None and x['mk'] is not None]))
