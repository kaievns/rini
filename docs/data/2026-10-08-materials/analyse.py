import sys,glob,os
S=64;B=6
for alone in sorted(glob.glob(sys.argv[1] + '/*-alone.rgba')):
    baked=alone.replace('-alone','-baked'); a=open(alone,'rb').read(); b=open(baked,'rb').read()
    diffs=[];cols=[0]*S
    for y in range(B,S-B):
        for x in range(B,S-B):
            i=(y*S+x)*4; d=sum(abs(a[i+k]-b[i+k]) for k in range(3)); diffs.append(d)
            if d>6: cols[x]+=1
    n=len(diffs); mean=sum(diffs)/3/n
    print(os.path.basename(alone)[:-11], 'mean %.2f'%mean, 'share>6 %.3f'%(sum(d>6 for d in diffs)/n), 'share>12 %.3f'%(sum(d>12 for d in diffs)/n), 'max %d'%max(diffs), 'cols', ''.join('%x'%min(15,c*15//52) for c in cols))
