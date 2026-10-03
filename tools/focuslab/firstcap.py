import re,sys,collections
exec(open('/tmp/journal-parse.py').read().split("def wid(s):")[0])
def fmt(t):
    t=int(t); return f"{t//3600000:02}:{t//60000%60:02}:{t//1000%60:02}.{t%1000:03}"
blocks=[l.split() for l in open(sys.argv[1]+'.blocks')]
def label_at(ts):
    lab=None
    for t,b in blocks:
        if ts>=t: lab=b
    return lab
E=[x for x in ev if blocks[0][0]<=fmt(x[0])[:8]]
cmds=[i for i,x in enumerate(E) if x[1]=='cmd' and 'MoveFocus' in x[3]]
c=collections.defaultdict(collections.Counter)
for n,i in enumerate(cmds):
    t0=E[i][0]; end=E[cmds[n+1]][0] if n+1<len(cmds) else t0+900
    seg=[x for x in E[i:] if x[0]<min(end,t0+900)]
    first=next((d for t,e,th,d in seg if e=='proto_capture' and 'why=placed' in d),None)
    if first: c[label_at(fmt(t0)[:8])]['first landing capture changed='+re.search(r'changed=(\d+)',first)[1]]+=1
for k,v in sorted(c.items()): print(k, dict(v))
