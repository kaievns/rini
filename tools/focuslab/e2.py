import re,sys,json,collections
exec(open('/tmp/journal-parse.py').read().split("def wid(s):")[0])
def wid(s):
    m=re.search(r'pid: (\d+), idx: (\d+)',s); return (int(m[1]),int(m[2])) if m else None
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
blocks=[l.split() for l in open(sys.argv[1]+'.blocks')]
def label_at(ts):
    lab=None
    for t,b in blocks:
        if ts>=t: lab=b
    return lab
lo=blocks[0][0]; hi=sys.argv[2] if len(sys.argv)>2 else '23:59:59'
E=[x for x in ev if lo<=fmt(x[0])[:8]<=hi]
cmds=[i for i,x in enumerate(E) if x[1]=='cmd' and 'MoveFocus' in x[3]]
rows=collections.defaultdict(list)
for n,i in enumerate(cmds):
    t0=E[i][0]; end=E[cmds[n+1]][0] if n+1<len(cmds) else t0+900
    seg=[x for x in E[i:] if x[0]<min(end,t0+900)]
    if not any(e=='flight_start' for t,e,th,d in seg): continue
    disp=next((d for t,e,th,d in seg if e=='raise_dispatch'),None); tgt=wid(disp) if disp else None
    g=lambda k,f=lambda th,d:True: next((t-t0 for t,e,th,d in seg if e==k and f(th,d)),None)
    crops=[(t-t0,d) for t,e,th,d in seg if e=='proto_crops' and th=='proto-f0']
    f0s=None; f0call=None
    if crops:
        tot=float(re.search(r'total_ms=([\d.]+)',crops[0][1])[1]); f0s=crops[0][0]-tot
        a=float(re.search(r'alpha_ms=([\d.]+)',crops[0][1])[1]); b=float(re.search(r'baked_ms=([\d.]+)',crops[0][1])[1]); f0call=a+b
    look=next((t-t0 for t,e,th,d in seg if e=='proto_capture' and 'focus_changed=true' in d),None)
    rows[label_at(fmt(t0)[:8])].append(dict(f0s=f0s,f0call=f0call,f0e=g('proto_f1'),ovl=g('overlay_shown'),
        sfd=g('setframe_done',lambda th,d: wid(d)==tgt),landed=g('proto_capture_landed'),look=look,
        mk=g('make_key_done',lambda th,d: wid(d)==tgt),wsf=g('ws_focus',lambda th,d: wid(d)==tgt),lift=g('flight_finish'),
        bgs=sum(1 for t,e,th,d in seg if e=='proto_capture' and 'why=background' in d)))
def P(xs):
    xs=sorted(x for x in xs if x is not None)
    if not xs: return '        -        '
    q=lambda p: xs[min(len(xs)-1,int(round(p*(len(xs)-1))))]
    return f"{q(.5):5.1f} /{q(.9):5.1f} /{xs[-1]:5.1f}"
keys=[('f0s','frame-zero capture starts'),('f0call','  its SkyLight calls (ms)'),('f0e','frame-zero pictures ready'),('ovl','overlay up, frames sent'),('sfd','target write returns'),('landed','worker sees it landed'),('look','focused picture captured'),('mk','focus issued'),('wsf','rini sees focus'),('lift','overlay lifted'),('bgs','background refreshes (count)')]
labs=sorted(rows)
print(f"{'ms after rini takes the press':34s} "+" ".join(f"{l+' (n='+str(len(rows[l]))+')':^19s}" for l in labs))
for k,lab in keys:
    print(f"  {lab:32s} "+" ".join(f"{P([r[k] for r in rows[l]]):^19s}" for l in labs))
# probe ground truth
R=collections.defaultdict(list)
for l in open(sys.argv[1]):
    d=json.loads(l); R[d['label']].append(d)
print("\nprobe, ms after the key is posted:")
for k,lab in [('front_ms','WS front process'),('still_ms','target WS frame final')]:
    print(f"  {lab:32s} "+" ".join(f"{P([x[k] for x in R[l]]):^19s}" for l in labs))
