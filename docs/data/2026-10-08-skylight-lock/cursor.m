// cursor: whether SLSGetOnScreenWindowList on the main connection and on an SLSNewConnection lists the
// Window Server's cursor window (id 4), and where CGWindowListCopyWindowInfo lists it. Reads only.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
typedef int (*List)(int, int, int, uint32_t*, int*);
int main(){ void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW); List on=dlsym(sl,"SLSGetOnScreenWindowList");
  int m=((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(), n=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&n);
  for(int k=0;k<2;k++){ int c=k?n:m; uint32_t ids[4096]; int cnt=0; on(c,0,4096,ids,&cnt); int has=0; for(int i=0;i<cnt;i++) if(ids[i]==4) has=i+1; printf("%s %d: %d windows, window 4 at %d\n",k?"new":"main",c,cnt,has); }
  CFArrayRef l=CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly,0); int i=0; for(NSDictionary*w in (__bridge NSArray*)l){ i++; if([w[(id)kCGWindowNumber] intValue]==4) printf("cg: window 4 at %d of %ld\n",i,CFArrayGetCount(l)); } }
