// reads: every SkyLight read rini moves to its own connection, asked on SLSMainConnectionID and on an
// SLSNewConnection for every on-screen layer-0 window, and whether the two answers agree. Captures compare
// size and pixels; each window is asked on the main connection a second time, after the new one, so a window
// whose content changes between asks shows as live rather than as a disagreement. Reads only: no window, no event.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
typedef CFTypeRef (*QueryW)(int, CFArrayRef, int); typedef CFTypeRef (*CopyW)(CFTypeRef);
typedef bool (*Adv)(CFTypeRef); typedef uint32_t (*GetU)(CFTypeRef); typedef CFArrayRef (*GetA)(CFTypeRef);
typedef int (*Bounds)(int, uint32_t, CGRect*); typedef int (*Ordered)(int, uint32_t, uint8_t*);
typedef int (*Constr)(int, uint32_t, CGSize*, CGSize*, CGSize*);
typedef CFArrayRef (*CopyWT)(int, uint32_t, CFArrayRef, uint32_t, uint64_t*, uint64_t*);
typedef CFArrayRef (*HWCap)(int, uint32_t*, int, uint32_t);
typedef int (*RectCap)(int, uint32_t*, int, CGRect, uint32_t, CGImageRef*);
typedef uint64_t (*Active)(int);
static uint64_t fnv(NSData *d){ uint64_t h=1469598103934665603ULL; const uint8_t*b=d.bytes; for(NSUInteger i=0;i<d.length;i++){h^=b[i]; h*=1099511628211ULL;} return h; }
static NSData *pixels(CGImageRef img){
  if(!img) return nil; size_t w=CGImageGetWidth(img), h=CGImageGetHeight(img);
  NSMutableData *d=[NSMutableData dataWithLength:w*h*4];
  CGColorSpaceRef cs=CGColorSpaceCreateDeviceRGB();
  CGContextRef c=CGBitmapContextCreate(d.mutableBytes,w,h,8,w*4,cs,(CGBitmapInfo)kCGImageAlphaPremultipliedLast);
  CGContextDrawImage(c,CGRectMake(0,0,w,h),img); CGContextRelease(c); CGColorSpaceRelease(cs); return d;
}
int main(){
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  QueryW q=dlsym(sl,"SLSWindowQueryWindows"); CopyW cw=dlsym(sl,"SLSWindowQueryResultCopyWindows"); Adv adv=dlsym(sl,"SLSWindowIteratorAdvance");
  GetU wid=dlsym(sl,"SLSWindowIteratorGetWindowID"); GetA radii=dlsym(sl,"SLSWindowIteratorGetCornerRadii");
  Bounds bounds=dlsym(sl,"SLSGetWindowBounds"); Ordered ordered=dlsym(sl,"SLSWindowIsOrderedIn"); Constr constr=dlsym(sl,"SLSPackagesGetWindowConstraints");
  CopyWT copywt=dlsym(sl,"SLSCopyWindowsWithOptionsAndTags"); HWCap hw=dlsym(sl,"SLSHWCaptureWindowList"); RectCap rc=dlsym(sl,"SLSCaptureWindowsContentsToRectWithOptions");
  Active active=dlsym(sl,"SLSGetActiveSpace");
  int mainc=((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(), newc=0; int err=((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  printf("main %d new %d (err %d)\n",mainc,newc,err);
  CFArrayRef list=CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly|kCGWindowListExcludeDesktopElements,kCGNullWindowID);
  NSMutableArray<NSNumber*>*ids=[NSMutableArray array];
  for(NSDictionary*w in (__bridge NSArray*)list) if([w[(id)kCGWindowLayer] intValue]==0) [ids addObject:w[(id)kCGWindowNumber]];
  CFRelease(list);
  int cids[3]={mainc,newc,mainc}; int agree=0, differ=0, live=0;
  for(NSNumber*n in ids){
    uint32_t id=n.unsignedIntValue; NSString*a[3];
    for(int k=0;k<3;k++){
      CGRect b={0}; int be=bounds(cids[k],id,&b); uint8_t o=0; int oe=ordered(cids[k],id,&o);
      CGSize mn={0},mx={0},cu={0}; int ce=constr(cids[k],id,&mn,&mx,&cu);
      NSString*r=@"-"; CFArrayRef one=(__bridge CFArrayRef)@[n]; CFTypeRef res=q(cids[k],one,0); CFTypeRef it=res?cw(res):NULL;
      if(it&&adv(it)&&radii){CFArrayRef ra=radii(it); if(ra){r=[(__bridge NSArray*)ra componentsJoinedByString:@","]; CFRelease(ra);}}
      if(it)CFRelease(it); if(res)CFRelease(res);
      CFArrayRef imgs=hw(cids[k],&id,1,(1<<11)|(1<<8)); CGImageRef img=imgs&&CFArrayGetCount(imgs)?(CGImageRef)CFArrayGetValueAtIndex(imgs,0):NULL;
      NSData*px=pixels(img); CGImageRef rimg=NULL; int re=rc(cids[k],&id,1,CGRectNull,1<<8,&rimg); NSData*rpx=pixels(rimg);
      a[k]=[NSString stringWithFormat:@"bounds=%d:%.0f,%.0f,%.0fx%.0f ordered=%d:%d constraints=%d:%.0fx%.0f-%.0fx%.0f radii=%@ hw=%zux%zu:%lx rect=%d:%zux%zu:%lx",
        be,b.origin.x,b.origin.y,b.size.width,b.size.height,oe,o,ce,mn.width,mn.height,mx.width,mx.height,r,
        img?CGImageGetWidth(img):0,img?CGImageGetHeight(img):0,(unsigned long)fnv(px),re,rimg?CGImageGetWidth(rimg):0,rimg?CGImageGetHeight(rimg):0,(unsigned long)fnv(rpx)];
      if(imgs)CFRelease(imgs); if(rimg)CGImageRelease(rimg);
    }
    BOOL same=[a[0] isEqualToString:a[1]], steady=[a[0] isEqualToString:a[2]];
    const char*verdict=same?"agree":steady?"DIFFER":"live"; same?agree++:steady?differ++:live++;
    printf("%u %s\n  main  %s\n%s",id,verdict,a[0].UTF8String,same?"":[NSString stringWithFormat:@"  new   %@\n  main2 %@\n",a[1],a[2]].UTF8String);
  }
  uint64_t space=active(mainc); CFArrayRef spaces=(__bridge CFArrayRef)@[@(space)]; NSString*l[2];
  for(int k=0;k<2;k++){uint64_t s=0,c=0; CFArrayRef w=copywt(cids[k],0,spaces,0x2,&s,&c); l[k]=w?[(__bridge NSArray*)w componentsJoinedByString:@","]:@"null"; if(w)CFRelease(w);}
  printf("space %llu windows main [%s] new [%s] %s\n",space,l[0].UTF8String,l[1].UTF8String,[l[0] isEqualToString:l[1]]?"agree":"DIFFER");
  printf("windows=%lu agree=%d differ=%d live=%d\n",(unsigned long)ids.count,agree,differ,live);
}
