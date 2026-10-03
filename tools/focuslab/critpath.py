import re,sys,collections
exec(open('/tmp/journal-parse.py').read().split("def wid(s):")[0])
def wid(s):
    m=re.search(r'pid: (\d+), idx: (\d+)',s); return (int(m[1]),int(m[2])) if m else None
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
lo,hi=sys.argv[1],sys.argv[2]
E=[x for x in ev if lo<=fmt(x[0])[:8]<=hi]
cmds=[i for i,x in enumerate(E) if x[1]=='cmd' and 'MoveFocus' in x[3]]
rows=[]
for n,i in enumerate(cmds):
    t0=E[i][0]; end=E[cmds[n+1]][0] if n+1<len(cmds) else t0+900
    seg=[x for x in E[i:] if x[0]<min(end,t0+900)]
    if not any(e=='flight_start' for t,e,th,d in seg): continue
    disp=next((d for t,e,th,d in seg if e=='raise_dispatch'),None)
    tgt=wid(disp) if disp else None
    if not tgt: continue
    g=lambda k,f=lambda th,d:True: next((t-t0 for t,e,th,d in seg if e==k and f(th,d)),None)
    crops=[(t-t0,d) for t,e,th,d in seg if e=='proto_crops' and th=='proto-f0']
    f0s=None
    if crops:
        tot=float(re.search(r'total_ms=([\d.]+)',crops[0][1])[1]); f0s=crops[0][0]-tot
    r=dict(
      eng=g('engine_start_surface') if g('engine_start_surface') is not None else g('engine_start'),
      f0s=f0s, f0e=g('proto_f1'), ovl=g('overlay_shown'),
      sfs=g('setframe_start',lambda th,d: wid(d)==tgt), sfd=g('setframe_done',lambda th,d: wid(d)==tgt),
      mk=g('make_key_done',lambda th,d: wid(d)==tgt), wsf=g('ws_focus',lambda th,d: wid(d)==tgt),
      landed=g('proto_capture_landed'),
      look=next((t-t0 for t,e,th,d in seg if e=='proto_capture' and 'focus_changed=true' in d),None),
      lift=g('flight_finish'))
    rows.append(r)
def P(xs):
    xs=sorted(x for x in xs if x is not None)
    if not xs: return '        -        '
    q=lambda p: xs[min(len(xs)-1,int(round(p*(len(xs)-1))))]
    return f"{q(.5):5.1f} /{q(.9):5.1f} /{q(.99):5.1f}"
print(f"strip flights n={len(rows)}  (ms after rini receives the press; p50 / p90 / p99)")
for k,lab in [('eng','engine starts composing'),('f0s','frame-zero capture starts'),('f0e','frame-zero pictures ready'),('ovl','overlay up, frames sent'),('mk','focus issued (make-key)'),('wsf','rini sees focus change'),('sfs','target write starts (app thread)'),('sfd','target write returns'),('landed','window server shows it in place'),('look','focused picture captured'),('lift','overlay lifted')]:
    print(f"  {lab:34s} {P([r[k] for r in rows])}")
print("  durations:")
d=lambda a,b: [r[b]-r[a] for r in rows if r[a] is not None and r[b] is not None]
for a,b,lab in [('eng','f0s','compose before capture'),('f0s','f0e','frame-zero capture'),('ovl','sfs','hop to app thread'),('sfs','sfd','AX write'),('sfd','landed','write return -> server shows'),('landed','look','capture -> picture')]:
    print(f"  {lab:34s} {P(d(a,b))}")
