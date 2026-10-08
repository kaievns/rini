// repro <main|new|cg> <seconds> [ops]: a borderless layer-backed panel whose alpha and frame change every ~4ms on the
// main thread (implicit CA flush per run-loop turn), while a background thread runs SLSWindowQueryWindows in a
// loop on either SLSMainConnectionID or its own SLSNewConnection, or (cg) CGWindowListCopyWindowInfo, which takes
// no connection. Prints the worst main-thread turn and query.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#include <dlfcn.h>
#include <pthread.h>
#include <mach/mach_time.h>
typedef int (*MainConn)(void); typedef int (*NewConn)(int, int*);
typedef CFTypeRef (*QueryW)(int, CFArrayRef, int); typedef CFTypeRef (*CopyW)(CFTypeRef);
static QueryW query; static CopyW copyw; static int cid; static int cg; static volatile int wid; static volatile int stop;
static double ms(void){static mach_timebase_info_data_t tb; if(!tb.denom) mach_timebase_info(&tb); return mach_absolute_time()*tb.numer/tb.denom/1e6;}
static double worstQ, worstT; static int ops; static long nq, slowQ, slowT, nt;
static void *reader(void *_){
  while(!stop){
    int ids[3]={wid, 1, 2}; CFNumberRef n[3]; for(int i=0;i<3;i++) n[i]=CFNumberCreate(NULL,kCFNumberIntType,&ids[i]);
    CFArrayRef a=CFArrayCreate(NULL,(const void**)n,3,&kCFTypeArrayCallBacks);
    double t0=ms();
    if(cg){CFArrayRef l=CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly|kCGWindowListExcludeDesktopElements,kCGNullWindowID); if(l)CFRelease(l);}
    else{CFTypeRef q=query(cid,a,0); if(q){CFTypeRef it=copyw(q); if(it)CFRelease(it); CFRelease(q);}}
    double d=ms()-t0;
    nq++; if(d>worstQ)worstQ=d; if(d>50){slowQ++; fprintf(stderr,"query %.1fms at %.0f\n",d,t0);}
    CFRelease(a); for(int i=0;i<3;i++)CFRelease(n[i]);
    usleep(500);
  }
  return NULL;
}
int main(int argc,char**argv){
  @autoreleasepool{
    void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
    query=(QueryW)dlsym(sl,"SLSWindowQueryWindows"); copyw=(CopyW)dlsym(sl,"SLSWindowQueryResultCopyWindows");
    int main_cid=((MainConn)dlsym(sl,"SLSMainConnectionID"))();
    [NSApplication sharedApplication]; [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    if(!strcmp(argv[1],"new")){ ((NewConn)dlsym(sl,"SLSNewConnection"))(0,&cid); } else cid=main_cid;
    cg=!strcmp(argv[1],"cg");
    double secs=atof(argv[2]); ops=argc>3?atoi(argv[3]):7;
    fprintf(stderr,"mode %s cid %d main %d\n",argv[1],cid,main_cid);
    NSPanel*p=[[NSPanel alloc] initWithContentRect:NSMakeRect(40,40,120,80) styleMask:NSWindowStyleMaskBorderless|NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
    p.opaque=NO; p.backgroundColor=NSColor.clearColor; p.ignoresMouseEvents=YES; p.level=NSStatusWindowLevel;
    NSView*v=[[NSView alloc] initWithFrame:NSMakeRect(0,0,120,80)]; v.wantsLayer=YES; p.contentView=v;
    v.layer.backgroundColor=NSColor.systemPinkColor.CGColor; [p orderFrontRegardless];
    wid=(int)p.windowNumber;
    pthread_t t; pthread_create(&t,NULL,reader,NULL);
    __block double last=ms(); __block int k=0; double start=ms();
    [NSTimer scheduledTimerWithTimeInterval:0.004 repeats:YES block:^(NSTimer*tm){
      double now=ms(), gap=now-last; last=now; nt++;
      if(gap>worstT)worstT=gap; if(gap>50){slowT++; fprintf(stderr,"main turn gap %.1fms at %.0f\n",gap,now);}
      k++; if(ops&1)p.alphaValue=(k&1)?0.6:1.0; if(ops&2)[p setFrame:NSMakeRect(40+(k%7),40,120,80) display:NO];
      if((ops&4)&&k%3==0)[p orderWindow:NSWindowAbove relativeTo:0];
      [CATransaction begin]; [CATransaction setDisableActions:YES]; v.layer.opacity=(k&2)?0.9:1.0; [CATransaction commit];
      if(now-start>secs*1000){stop=1; [NSApp stop:nil]; [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0] atStart:YES];}
    }];
    [NSApp run];
    pthread_join(t,NULL);
    printf("mode=%s queries=%ld worst_query=%.1fms slow_queries=%ld turns=%ld worst_turn_gap=%.1fms slow_turns=%ld\n",argv[1],nq,worstQ,slowQ,nt,worstT,slowT);
  }
  return 0;
}
