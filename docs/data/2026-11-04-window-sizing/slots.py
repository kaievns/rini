import re, sys, collections, csv
s=open(sys.argv[1]).read()
i=s.find("launch:(apps:{")+len("launch:(apps:")
# walk the braces, tracking strings
def walk(i):
    depth=0; j=i; ins=False
    while True:
        c=s[j]
        if ins:
            if c=='\\': j+=2; continue
            if c=='"': ins=False
        else:
            if c=='"': ins=True
            elif c in '{[(': depth+=1
            elif c in '}])':
                depth-=1
                if depth==0: return j
        j+=1
end=walk(i); body=s[i:end+1]
rows=[]
# app level: "bundle":{ ... }
pos=1
app_re=re.compile(r'"([^"]+)":\{')
while True:
    m=app_re.match(body,pos)
    if not m: break
    app=m.group(1); a_end=None
    # find matching brace for this app
    k=m.end()-1
    depth=0; j=k; ins=False
    while True:
        c=body[j]
        if ins:
            if c=='\\': j+=2; continue
            if c=='"': ins=False
        else:
            if c=='"': ins=True
            elif c in '{[(': depth+=1
            elif c in '}])':
                depth-=1
                if depth==0: break
        j+=1
    abody=body[k+1:j]
    for am in re.finditer(r'"([0-9A-F\-\+]+)":\[', abody):
        arr=am.group(1)
        # slots until matching ]
        q=am.end()-1; depth=0; r=q; ins=False
        while True:
            c=abody[r]
            if ins:
                if c=='\\': r+=2; continue
                if c=='"': ins=False
            else:
                if c=='"': ins=True
                elif c in '{[(': depth+=1
                elif c in '}])':
                    depth-=1
                    if depth==0: break
            r+=1
        sbody=abody[q:r+1]
        for sm in re.finditer(r'\(title:(None|Some\("((?:[^"\\]|\\.)*)"\)),display_uuid:"([^"]+)",workspace_index:(\d+),width:(None|Some\(([^)]*\)?)\))(?:,width_dragged:(true|false))?', sbody):
            title=sm.group(2) or ""
            rows.append(dict(app=app, arrangement=arr, lab="yes" if title.startswith("lab-") else "no", display=sm.group(3), workspace=sm.group(4), width=sm.group(6) or "None", dragged="yes" if sm.group(7)=="true" else "no"))
    pos=j+1
    if pos<len(body) and body[pos]==',': pos+=1
w=csv.DictWriter(open(sys.argv[2],"w",newline=""), fieldnames=list(rows[0]))
w.writeheader(); w.writerows(rows)
print(len(rows))
c=collections.Counter((r["display"][:8]) for r in rows); print("display", c)
c=collections.Counter(r["arrangement"] for r in rows); print("arrangement", c)
def kind(wd): return "none" if wd=="None" else ("full" if wd.startswith("FullWidth") else "offset")
print(collections.Counter(kind(r["width"]) for r in rows))
print(collections.Counter((r["arrangement"].count("+")>0, r["display"][:8]) for r in rows))
