import json,sys,collections
def pct(xs,p):
    if not xs: return float('nan')
    xs=sorted(xs); return xs[min(len(xs)-1,int(p/100*len(xs)))]
for path in sys.argv[1:]:
    marks=[json.loads(l) for l in open(path)]
    gens=collections.defaultdict(dict)
    for m in marks:
        g=m.get('g'); 
        if g is None: continue
        k=m['mark']
        if k in('begin','show','lift','at_rest','start_moving') and k not in gens[g]: gens[g][k]=m['t']
        if k=='begin': gens[g]['kind']=m['kind']
    ph=collections.defaultdict(lambda: collections.defaultdict(list))
    for m in marks:
        if m['mark']!='drawn': continue
        G=gens.get(m['g'],{})
        kind=G.get('kind','?')
        if m['shown'] is True: p='show'
        elif m['shown'] is False: p='lift/down'
        elif 'show' in G and 'lift' in G and G['show']<m['t']<G['lift']: p='in flight'
        elif 'lift' in G and m['t']>=G['lift']: p='after lift'
        else: p='other'
        for q,v in (('queued',m['queued_us']),('applied',m['applied_us']),('back',m['flushed_before_us'])):
            ph[p][q].append(v/1000)
    print(path)
    for p,d in ph.items():
        print(f"  {p:12s} n={len(d['queued']):4d}  queued p50 {pct(d['queued'],50):6.2f} p90 {pct(d['queued'],90):6.2f} p99 {pct(d['queued'],99):6.2f} max {max(d['queued']):7.2f} | applied p50 {pct(d['applied'],50):5.2f} p90 {pct(d['applied'],90):5.2f} p99 {pct(d['applied'],99):6.2f} | back p90 {pct(d['back'],90):6.2f} p99 {pct(d['back'],99):6.2f}")
