// diffpx <wid> <x> <y> <w> <h> <options hex>: a framed CGWindowListCreateImage of one window against
// SLSCaptureWindowsContentsToRectWithOptions on an SLSNewConnection: how many pixels differ, by how much,
// and where. Reads only.
#import <AppKit/AppKit.h>
#include <dlfcn.h>
typedef int (*RectCap)(int, uint32_t*, int, CGRect, uint32_t, CGImageRef*);
static NSData*px(CGImageRef img){ size_t w=CGImageGetWidth(img),h=CGImageGetHeight(img); NSMutableData*d=[NSMutableData dataWithLength:w*h*4]; CGColorSpaceRef cs=CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef c=CGBitmapContextCreate(d.mutableBytes,w,h,8,w*4,cs,(CGBitmapInfo)kCGImageAlphaPremultipliedLast); CGContextDrawImage(c,CGRectMake(0,0,w,h),img); CGContextRelease(c); CGColorSpaceRelease(cs); return d; }
int main(int argc,char**argv){
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW); RectCap rc=dlsym(sl,"SLSCaptureWindowsContentsToRectWithOptions");
  int newc=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  uint32_t id=atoi(argv[1]); CGRect r=CGRectMake(atof(argv[2]),atof(argv[3]),atof(argv[4]),atof(argv[5])); uint32_t opt=strtoul(argv[6],0,16);
  CGImageRef a=CGWindowListCreateImage(r,kCGWindowListOptionIncludingWindow,id,kCGWindowImageDefault), x=NULL; rc(newc,&id,1,r,opt,&x);
  NSData*pa=px(a),*pb=px(x); const uint8_t*p=pa.bytes,*q=pb.bytes; size_t w=CGImageGetWidth(a),n=0,mx=0,minx=1e9,maxx=0,miny=1e9,maxy=0, alpha=0;
  for(size_t i=0;i<pa.length;i+=4){ int d=0; for(int k=0;k<4;k++){int e=abs(p[i+k]-q[i+k]); if(e>d)d=e;} if(d){n++; if(d>mx)mx=d; size_t px_=i/4, x_=px_%w, y_=px_/w; if(x_<minx)minx=x_; if(x_>maxx)maxx=x_; if(y_<miny)miny=y_; if(y_>maxy)maxy=y_; if(p[i+3]!=q[i+3]) alpha++;}}
  printf("differing pixels %zu of %zu, max channel diff %zu, alpha differs in %zu, box x %zu-%zu y %zu-%zu\n",n,pa.length/4,mx,alpha,minx,maxx,miny,maxy);
}
