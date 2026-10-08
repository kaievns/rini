// diffscreen: CGWindowListCreateImage of the main display against SLSCaptureWindowsContentsToRectWithOptions
// of every window SLSGetOnScreenWindowList lists, on an SLSNewConnection, options 0x900: how many pixels
// differ and where. Reads only.
#import <AppKit/AppKit.h>
#include <dlfcn.h>
typedef int (*RectCap)(int, uint32_t*, int, CGRect, uint32_t, CGImageRef*);
typedef int (*List)(int, int, int, uint32_t*, int*);
static NSData*px(CGImageRef img){ size_t w=CGImageGetWidth(img),h=CGImageGetHeight(img); NSMutableData*d=[NSMutableData dataWithLength:w*h*4]; CGColorSpaceRef cs=CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef c=CGBitmapContextCreate(d.mutableBytes,w,h,8,w*4,cs,(CGBitmapInfo)kCGImageAlphaPremultipliedLast); CGContextDrawImage(c,CGRectMake(0,0,w,h),img); CGContextRelease(c); CGColorSpaceRelease(cs); return d; }
int main(){
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW); RectCap rc=dlsym(sl,"SLSCaptureWindowsContentsToRectWithOptions"); List on=dlsym(sl,"SLSGetOnScreenWindowList");
  int newc=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  uint32_t ids[4096]; int n=0; on(newc,0,4096,ids,&n); CGRect r=CGDisplayBounds(CGMainDisplayID());
  CGImageRef a=CGWindowListCreateImage(r,kCGWindowListOptionOnScreenOnly,0,kCGWindowImageDefault), x=NULL; rc(newc,ids,n,r,0x900,&x);
  NSData*pa=px(a),*pb=px(x); const uint8_t*p=pa.bytes,*q=pb.bytes; size_t w=CGImageGetWidth(a),cnt=0,mx=0,minx=1e9,maxx=0,miny=1e9,maxy=0;
  for(size_t i=0;i<pa.length;i+=4){ int d=0; for(int k=0;k<4;k++){int e=abs(p[i+k]-q[i+k]); if(e>d)d=e;} if(d){cnt++; if(d>mx)mx=d; size_t px_=i/4, x_=px_%w, y_=px_/w; if(x_<minx)minx=x_; if(x_>maxx)maxx=x_; if(y_<miny)miny=y_; if(y_>maxy)maxy=y_;}}
  printf("screen, %d windows: differing pixels %zu of %zu, max channel diff %zu, box x %zu-%zu y %zu-%zu (pixels)\n",n,cnt,pa.length/4,mx,minx,maxx,miny,maxy);
}
