import re,sys,statistics as st
exec(open('/tmp/journal-parse.py').read().split("def wid(s):")[0])
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
lo,hi=sys.argv[1],sys.argv[2]
E=[x for x in ev if lo<=fmt(x[0])[:8]<=hi]
cmds=[i for i,x in enumerate(E) if x[1]=='cmd']
rows=[]
for n,i in enumerate(cmds):
    t0=E[i][0]; end=E[cmds[n+1]][0] if n+1<len(cmds) else t0+800
    seg=[x for x in E[i:] if x[0]<min(end,t0+800)]
    crops=[(t-t0,d,th) for t,e,th,d in seg if e=='proto_crops' and th=='proto-f0']
    if not crops: continue
    t_end,d,_=crops[0]
    a=float(re.search(r'alpha_ms=([\d.]+)',d)[1]); b=float(re.search(r'baked_ms=([\d.]+)',d)[1]); tot=float(re.search(r'total_ms=([\d.]+)',d)[1])
    call=a if a>0 else b
    start=t_end-tot
    mk=next((t-t0 for t,e,th,d in seg if e=='make_key_done'),None)
    rs=next((t-t0 for t,e,th,d in seg if e=='raise_start'),None)
    act=next((t-t0 for t,e,th,d in seg if e=='activation_done'),None)
    inst=next((t-t0 for t,e,th,d in seg if e=='proto_install'),None)
    rows.append(dict(call=call,start=start,end=t_end,mk=mk,act=act,inst=inst,overlap=(rs is not None and rs< t_end and (act or rs+30)>start)))
def P(xs):
    xs=sorted(xs)
    if not xs: return '-'
    q=lambda p: xs[min(len(xs)-1,int(round(p*(len(xs)-1))))]
    return f"{q(.5):5.1f} /{q(.9):5.1f}  n={len(xs)}"
print("f0 SkyLight call ms, focus change overlapping the call:", P([r['call'] for r in rows if r['overlap']]))
print("f0 SkyLight call ms, no overlap                       :", P([r['call'] for r in rows if not r['overlap']]))
print("f0 call start ms after cmd:", P([r['start'] for r in rows]), " make_key done:", P([r['mk'] for r in rows if r['mk'] is not None]))
