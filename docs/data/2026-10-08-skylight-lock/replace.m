// replace: each CGWindowList read rini made, against the SkyLight reads on an SLSNewConnection that
// replace it. Lists: the same windows, in the same order front to back, with the same level, owner,
// bounds, owner name and title. Captures: the same size and pixels, against the CG call made twice to
// show how much a live window changes between asks. Reads only: no window, no event.
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
#include <libproc.h>
typedef CFTypeRef (*QueryW)(int, CFArrayRef, int); typedef CFTypeRef (*CopyW)(CFTypeRef);
typedef bool (*Adv)(CFTypeRef); typedef uint32_t (*GetU)(CFTypeRef); typedef int (*GetI)(CFTypeRef);
typedef CGRect (*GetR)(CFTypeRef); typedef CFStringRef (*GetS)(CFTypeRef);
typedef int (*List)(int, int, int, uint32_t*, int*);
typedef int (*RectCap)(int, uint32_t*, int, CGRect, uint32_t, CGImageRef*);
static QueryW q; static CopyW cw; static Adv adv; static GetU wid; static GetI pid, lvl; static GetR bnd; static GetS title;
static int newc;
typedef struct { uint32_t id; int level, pid; CGRect b; char owner[128], name[256]; } W;
static NSString *ownerName(int p){
  NSRunningApplication*a=[NSRunningApplication runningApplicationWithProcessIdentifier:p];
  if(a.localizedName) return a.localizedName;
  char buf[256]={0}; if(proc_name(p,buf,sizeof buf)>0) return @(buf); return @"";
}
static int cgList(CGWindowListOption o, W *out){
  CFArrayRef l=CGWindowListCopyWindowInfo(o,kCGNullWindowID); int n=0;
  for(NSDictionary*w in (__bridge NSArray*)l){ W x={0}; x.id=[w[(id)kCGWindowNumber] unsignedIntValue]; x.level=[w[(id)kCGWindowLayer] intValue]; x.pid=[w[(id)kCGWindowOwnerPID] intValue];
    CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)w[(id)kCGWindowBounds],&x.b);
    snprintf(x.owner,128,"%s",[w[(id)kCGWindowOwnerName] UTF8String]?:""); snprintf(x.name,256,"%s",[w[(id)kCGWindowName] UTF8String]?:""); out[n++]=x; }
  CFRelease(l); return n;
}
static int slsList(List list, W *out){
  uint32_t ids[4096]; int n=0; list(newc,0,4096,ids,&n);
  NSMutableArray*a=[NSMutableArray array]; for(int i=0;i<n;i++) [a addObject:@(ids[i])];
  CFTypeRef r=q(newc,(__bridge CFArrayRef)a,1); CFTypeRef it=r?cw(r):NULL; int m=0;
  while(it&&adv(it)){ W x={0}; x.id=wid(it); x.level=lvl(it); x.pid=pid(it); x.b=bnd(it);
    snprintf(x.owner,128,"%s",ownerName(x.pid).UTF8String); CFStringRef t=title?title(it):NULL; snprintf(x.name,256,"%s",t?[(__bridge NSString*)t UTF8String]:""); if(t)CFRelease(t); out[m++]=x; }
  if(it)CFRelease(it); if(r)CFRelease(r);
  // the query's order against the list's
  int ordered=1; for(int i=0;i<m&&i<n;i++) if(out[i].id!=ids[i]) ordered=0;
  printf("  sls list %d ids, query %d, query keeps the list's order: %s\n",n,m,ordered?"yes":"NO");
  return m;
}
static int same(W a, W b, int names){ return a.id==b.id&&a.level==b.level&&CGRectEqualToRect(a.b,b.b)&&(!names||(!strcmp(a.owner,b.owner)&&!strcmp(a.name,b.name))); }
static void compare(const char*label, W*c, int nc, W*s, int ns, int (*keep)(W), int names){
  W cc[4096], ss[4096]; int a=0,b=0; for(int i=0;i<nc;i++) if(keep(c[i])) cc[a++]=c[i]; for(int i=0;i<ns;i++) if(keep(s[i])) ss[b++]=s[i];
  int differ=0, order=1; int j=0;
  for(int i=0;i<a;i++){ int k=-1; for(int t=0;t<b;t++) if(ss[t].id==cc[i].id) k=t; if(k<0){printf("    cg only: %u L%d p%d '%s' '%s'\n",cc[i].id,cc[i].level,cc[i].pid,cc[i].owner,cc[i].name); differ++; continue;}
    if(k<j) order=0; j=k; if(!same(cc[i],ss[k],names)){ differ++; printf("    differ: %u L%d p%d %.0f,%.0f %.0fx%.0f '%s' '%s' | L%d p%d %.0f,%.0f %.0fx%.0f '%s' '%s'\n",cc[i].id,cc[i].level,cc[i].pid,cc[i].b.origin.x,cc[i].b.origin.y,cc[i].b.size.width,cc[i].b.size.height,cc[i].owner,cc[i].name,ss[k].level,ss[k].pid,ss[k].b.origin.x,ss[k].b.origin.y,ss[k].b.size.width,ss[k].b.size.height,ss[k].owner,ss[k].name);} }
  for(int t=0;t<b;t++){ int k=-1; for(int i=0;i<a;i++) if(cc[i].id==ss[t].id) k=i; if(k<0){ printf("    sls only: %u L%d p%d '%s' '%s'\n",ss[t].id,ss[t].level,ss[t].pid,ss[t].owner,ss[t].name); differ++; } }
  printf("%s: cg %d, sls %d, differing %d, same order front to back: %s\n",label,a,b,differ,order?"yes":"NO");
}
static int any(W w){return 1;}
static int above_desktop(W w){return w.level>-2147483603;}
static int app(W w){return w.level==0;}
static int desk(W w){return w.level<=-2147483600;}
static int bar(W w){return w.level>-2147483600&&w.level<0;}
static int status(W w){return w.level==25;}
static NSData*px(CGImageRef img, size_t *w, size_t *h){ if(!img){*w=*h=0; return nil;} *w=CGImageGetWidth(img); *h=CGImageGetHeight(img);
  NSMutableData*d=[NSMutableData dataWithLength:*w**h*4]; CGColorSpaceRef cs=CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef c=CGBitmapContextCreate(d.mutableBytes,*w,*h,8,*w*4,cs,(CGBitmapInfo)kCGImageAlphaPremultipliedLast); CGContextDrawImage(c,CGRectMake(0,0,*w,*h),img); CGContextRelease(c); CGColorSpaceRelease(cs); return d; }
static double diff(NSData*a,NSData*b){ if(!a||!b||a.length!=b.length) return -1; const uint8_t*x=a.bytes,*y=b.bytes; double s=0; for(NSUInteger i=0;i<a.length;i++) s+=abs(x[i]-y[i]); return s/a.length; }
static void capcmp(const char*label, CGImageRef cg1, CGImageRef cg2, CGImageRef sls){
  size_t w1,h1,w2,h2,w3,h3; NSData*a=px(cg1,&w1,&h1),*b=px(cg2,&w2,&h2),*c=px(sls,&w3,&h3);
  printf("%s: cg %zux%zu, cg again %zux%zu, sls %zux%zu; mean |diff| per channel cg-cg %.3f, cg-sls %.3f\n",label,w1,h1,w2,h2,w3,h3,diff(a,b),diff(a,c));
}
int main(int argc,char**argv){
  @autoreleasepool{
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  q=dlsym(sl,"SLSWindowQueryWindows"); cw=dlsym(sl,"SLSWindowQueryResultCopyWindows"); adv=dlsym(sl,"SLSWindowIteratorAdvance");
  wid=dlsym(sl,"SLSWindowIteratorGetWindowID"); pid=dlsym(sl,"SLSWindowIteratorGetPID"); lvl=dlsym(sl,"SLSWindowIteratorGetLevel");
  bnd=dlsym(sl,"SLSWindowIteratorGetBounds"); title=dlsym(sl,"SLSWindowIteratorCopyTitle");
  List onscreen=dlsym(sl,"SLSGetOnScreenWindowList"), all=dlsym(sl,"SLSGetWindowList"); RectCap rc=dlsym(sl,"SLSCaptureWindowsContentsToRectWithOptions");
  int mainc=((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(); // first: the first SLSNewConnection would become the main one
  newc=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  if(newc==0||newc==mainc){ fprintf(stderr,"no connection of its own: new %d main %d\n",newc,mainc); return 1; }
  printf("main %d, new %d\n",mainc,newc);
  static W c[4096], s[4096]; int nc, ns;
  printf("on screen, desktop elements excluded (front_to_back_depths, visible_windows_on_display):\n");
  nc=cgList(kCGWindowListOptionOnScreenOnly|kCGWindowListExcludeDesktopElements,c); ns=slsList(onscreen,s);
  compare(" every window",c,nc,s,ns,any,0); compare(" every window, sls without level <= kCGDesktopIconWindowLevel",c,nc,s,ns,above_desktop,0); compare(" layer 0",c,nc,s,ns,app,1);
  printf("on screen (desktop_backdrop_windows, bar_strip):\n");
  nc=cgList(kCGWindowListOptionOnScreenOnly,c); ns=slsList(onscreen,s);
  compare(" every window",c,nc,s,ns,any,0); compare(" desktop levels",c,nc,s,ns,desk,1); compare(" bar levels",c,nc,s,ns,bar,1);
  printf("every window (menu extras):\n");
  nc=cgList(kCGWindowListOptionAll,c); ns=slsList(all,s);
  compare(" status level",c,nc,s,ns,status,1);
  printf("desktop icon level %d, desktop level %d\n",CGWindowLevelForKey(kCGDesktopIconWindowLevelKey),CGWindowLevelForKey(kCGDesktopWindowLevelKey));
  // captures: every layer-0 window of another process wholly on the main display, at its frame
  nc=cgList(kCGWindowListOptionOnScreenOnly,c); CGRect disp=CGDisplayBounds(CGMainDisplayID());
  uint32_t opts[]={0,1<<8,(1<<8)|(1<<11)}; int tried=0, wholes=0, framed[3]={0}, belowok[3]={0}, live=0;
  for(int i=0;i<nc;i++){ W t=c[i]; if(t.level!=0||!strcmp(t.owner,"rini")||t.b.size.width<100||t.b.size.height<100) continue; int whole=CGRectContainsRect(disp,t.b); tried++; wholes+=whole;
    uint32_t below[4096]; int nb=0; for(int k=i;k<nc;k++) below[nb++]=c[k].id;
    for(int o=0;o<3;o++){
      size_t w1,h1,w2,h2,w3,h3;
      CGImageRef a=CGWindowListCreateImage(t.b,kCGWindowListOptionIncludingWindow,t.id,kCGWindowImageDefault), b=CGWindowListCreateImage(t.b,kCGWindowListOptionIncludingWindow,t.id,kCGWindowImageDefault), x=NULL;
      rc(newc,&t.id,1,t.b,opts[o],&x); NSData*pa=px(a,&w1,&h1),*pb=px(b,&w2,&h2),*pc=px(x,&w3,&h3); double cc=diff(pa,pb), cs=diff(pa,pc);
      if(cc!=0&&o==0) live++; if(cs>=0&&cs<=cc+0.01) framed[o]++; else printf("  framed %u '%s' options %x: cg %zux%zu sls %zux%zu cg-cg %.3f cg-sls %.3f\n",t.id,t.owner,opts[o],w1,h1,w3,h3,cc,cs);
      CGImageRelease(a); CGImageRelease(b); if(x)CGImageRelease(x);
      if(!whole) continue;
      a=CGWindowListCreateImage(t.b,kCGWindowListOptionOnScreenBelowWindow|kCGWindowListOptionIncludingWindow,t.id,kCGWindowImageDefault); b=CGWindowListCreateImage(t.b,kCGWindowListOptionOnScreenBelowWindow|kCGWindowListOptionIncludingWindow,t.id,kCGWindowImageDefault); x=NULL;
      rc(newc,below,nb,t.b,opts[o],&x); pa=px(a,&w1,&h1); pb=px(b,&w2,&h2); pc=px(x,&w3,&h3); cc=diff(pa,pb); cs=diff(pa,pc);
      if(cs>=0&&cs<=cc+0.01) belowok[o]++; else printf("  below %u '%s' options %x: cg %zux%zu sls %zux%zu cg-cg %.3f cg-sls %.3f\n",t.id,t.owner,opts[o],w1,h1,w3,h3,cc,cs);
      CGImageRelease(a); CGImageRelease(b); if(x)CGImageRelease(x); } }
  for(int o=0;o<3;o++) printf("captures, options %x: framed same as cg for %d of %d windows (%d live between the two cg asks), below and including for %d of the %d wholly on the display\n",opts[o],framed[o],tried,live,belowok[o],wholes);
  // the screen: every on-screen window over the main display
  CGRect screen=CGDisplayBounds(CGMainDisplayID()); uint32_t every[4096]; for(int i=0;i<nc;i++) every[i]=c[i].id;
  for(int o=0;o<3;o++){ CGImageRef a=CGWindowListCreateImage(screen,kCGWindowListOptionOnScreenOnly,0,kCGWindowImageDefault), b=CGWindowListCreateImage(screen,kCGWindowListOptionOnScreenOnly,0,kCGWindowImageDefault), x=NULL;
    rc(newc,every,nc,screen,opts[o],&x); char l[64]; snprintf(l,64," screen (%d windows), options %x",nc,opts[o]); capcmp(l,a,b,x); CGImageRelease(a); CGImageRelease(b); if(x)CGImageRelease(x); }
  }
}
