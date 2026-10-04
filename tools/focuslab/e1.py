import json,collections,sys
def look(d):
    fr=[x for x in d['frames'] if x[0]>0]
    if not fr: return None,None
    final=fr[-1][1]
    first=next((x[0] for x in fr if x[1]>0.5),None)
    full=next((x[0] for x in fr if final>0.5 and x[1]>=0.8*final),None)
    return first,full
def P(xs):
    xs=sorted(x for x in xs if x is not None)
    if not xs: return '        -         '
    q=lambda p: xs[min(len(xs)-1,int(round(p*(len(xs)-1))))]
    return f"{q(.5):5.1f} /{q(.9):5.1f} /{xs[-1]:5.1f}"
def main():
    f=sys.argv[1] if len(sys.argv)>1 else '/tmp/probe/e1.jsonl'
    R=collections.defaultdict(list)
    for l in open(f):
        d=json.loads(l)
        a,b=look(d)
        d['look_first'],d['look_full']=a,b
        lf=[x for x in d['frames'] if x[0]>0]
        d['lose']=next((x[0] for x in lf if x[2]>0.5),None)
        R[(d['mode'],'Chrome' if d['to_wid']==102 else 'Ghostty')].append(d)
    print(f"{'mode':8s} {'to':8s} {'n':>2s} | {'WS front proc':^17s} | {'WS key proc':^17s} | {'window on top':^17s} | {'look starts':^17s} | {'look complete':^17s} | {'old loses look':^17s}")
    for k in sorted(R):
        v=R[k]
        print(f"{k[0]:8s} {k[1]:8s} {len(v):2d} | {P([x['front_ms'] for x in v])} | {P([x['key_ms'] for x in v])} | {P([x['top_ms'] for x in v])} | {P([x['look_first'] for x in v])} | {P([x['look_full'] for x in v])} | {P([x['lose'] for x in v])}")
    print("columns: p50 / p90 / max, ms after the input (key post, click post, or the probe's make-key call)")
if __name__=='__main__': main()
