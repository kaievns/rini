import re,sys,collections
lines=open('/Users/kaievns/Library/Logs/rini/transitions.log').read().splitlines()
ev=[]  # (abs_ms, event, thread, detail)
base=None
for l in lines:
    m=re.match(r'(\d\d):(\d\d):(\d\d)\.(\d\d\d) #transition',l)
    if m:
        h,mi,s,ms=map(int,m.groups()); base=((h*60+mi)*60+s)*1000+ms; continue
    m=re.match(r'\s+\+\s*([\d.]+)ms (\S+)\s+\[([^\]]*)\] ?(.*)',l)
    if m and base is not None:
        ev.append((base+float(m[1]),m[2],m[3],m[4]))
ev.sort(key=lambda e:e[0])
def wid(s):
    m=re.search(r'pid: (\d+), idx: (\d+)',s); return (int(m[1]),int(m[2])) if m else None
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
apps={}
for t,e,th,d in ev:
    m=re.match(r'([\w.\-]+)\((\d+)\)',th)
    if m: apps[int(m[2])]=m[1].split('.')[-1]
# transitions: start at cmd, or at raise_dispatch / engine_start* with no start in the previous 250ms
starts=[]
for i,(t,e,th,d) in enumerate(ev):
    if e=='cmd' or (e in ('raise_dispatch','engine_start','engine_start_surface') and (not starts or t-ev[starts[-1]][0]>250)):
        starts.append(i)
T=[]
for n,i in enumerate(starts):
    end=starts[n+1] if n+1<len(starts) else len(ev)
    seg=ev[i:end]; t0=seg[0][0]
    g=lambda k,f=lambda d:True: next((t-t0 for t,e,th,d in seg if e==k and f(d)),None)
    gl=lambda k: next((t-t0 for t,e,th,d in reversed(seg) if e==k),None)
    focus=next((wid(d.split('raise=')[0]) for t,e,th,d in seg if e=='raise_dispatch'),None)
    what=seg[0][3].split(' focused_before')[0] if seg[0][1]=='cmd' else seg[0][1]
    before=wid(seg[0][3].split('focused_before')[1]) if seg[0][1]=='cmd' and 'focused_before=Some' in seg[0][3] else None
    sysv=[d for t,e,th,d in seg if e=='sys']
    def f(d,k):
        m=re.search(k+r'=([\d.]+)',d); return float(m[1]) if m else None
    cpu=gpu0=gpu1=None
    if len(sysv)>=2:
        a,b=sysv[0],sysv[-1]
        db=f(b,'busy')-f(a,'busy'); dt=f(b,'total')-f(a,'total'); cpu=100*db/dt if dt else None
        gpu0=f(a,'gpu'); gpu1=f(b,'gpu')
    T.append(dict(t0=t0,what=what,focus=focus,before=before,
        raise_=g('raise_dispatch'),raise_start=g('raise_start'),make_key=g('make_key_done'),axraise=g('axraise_done'),
        focused=g('ws_focus',lambda d: focus is not None and wid(d)==focus),
        engine=g('engine_start_surface') if g('engine_start_surface') is not None else g('engine_start'),
        f1=next((f(d,'ms') for t,e,th,d in seg if e=='proto_f1'),None),
        overlay=g('overlay_shown'),dur=next((f(d,'duration_ms') for t,e,th,d in seg if e=='flight_start'),None),
        frames=g('request_frames'),placed=gl('setframe_done'),mirror=g('proto_end_pictures'),passes=sum(1 for x in seg if x[1]=='proto_end_pictures'),
        snaps=sum(1 for x in seg if x[1]=='proto_snap'),lift=g('flight_finish'),settle=seg[-1][0]-t0,
        sck=sum(1 for x in seg if x[1]=='sck_capture_start'),framed=sum(1 for x in seg if x[1]=='harvest_start'),
        cpu=cpu,gpu0=gpu0,gpu1=gpu1,n=len(seg)))
open('/tmp/journal-T.repr','w').write(repr(T)); open('/tmp/journal-apps.repr','w').write(repr(apps))
lo=sys.argv[1] if len(sys.argv)>1 else '00:00:00'; hi=sys.argv[2] if len(sys.argv)>2 else '99'
o=lambda v:'-' if v is None else f"{v:.0f}"
print(f"{'time':12s} {'what':22s} {'to':10s} | {'raise':>5s} {'mkkey':>5s} {'axrs':>5s} {'focus':>5s} | {'eng':>4s} {'f0cap':>5s} {'ovl':>4s} {'dur':>4s} {'frm':>4s} {'placed':>6s} | {'mirror':>6s} {'pass':>4s} {'snap':>4s} {'lift':>5s} {'settle':>6s} | {'sck':>3s} {'frmd':>4s} {'cpu':>4s} {'gpu':>7s}")
for x in T:
    if not (lo<=fmt(x['t0'])<=hi): continue
    to=apps.get(x['focus'][0],'?')[:10] if x['focus'] else '-'
    print(f"{fmt(x['t0'])} {x['what'][:22]:22s} {to:10s} | {o(x['raise_']):>5s} {o(x['make_key']):>5s} {o(x['axraise']):>5s} {o(x['focused']):>5s} | {o(x['engine']):>4s} {o(x['f1']):>5s} {o(x['overlay']):>4s} {o(x['dur']):>4s} {o(x['frames']):>4s} {o(x['placed']):>6s} | {o(x['mirror']):>6s} {x['passes']:>4d} {x['snaps']:>4d} {o(x['lift']):>5s} {o(x['settle']):>6s} | {x['sck']:>3d} {x['framed']:>4d} {o(x['cpu']):>4s} {o(x['gpu0'])+'->'+o(x['gpu1']):>7s}")
