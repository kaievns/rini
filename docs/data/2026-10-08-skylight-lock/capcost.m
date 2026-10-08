// capcost <wid> <rounds>: SLSHWCaptureWindowList of one window on SLSMainConnectionID and on an SLSNewConnection,
// alternating, at gaps of 0, 20, 120 and 300ms since the previous capture; p50 per connection and gap. Reads only.
#import <Foundation/Foundation.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
typedef CFArrayRef (*HWCap)(int, uint32_t*, int, uint32_t);
static double ms(void){static mach_timebase_info_data_t tb; if(!tb.denom) mach_timebase_info(&tb); return mach_absolute_time()*tb.numer/tb.denom/1e6;}
static int cmp(const void*a,const void*b){double x=*(double*)a,y=*(double*)b; return x<y?-1:x>y;}
int main(int argc,char**argv){
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  HWCap hw=dlsym(sl,"SLSHWCaptureWindowList");
  int cids[2]={((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(),0}; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&cids[1]);
  uint32_t wid=(uint32_t)atoi(argv[1]); int rounds=atoi(argv[2]); int gaps[4]={0,20,120,300};
  for(int g=0;g<4;g++){ double t[2][64];
    for(int r=0;r<rounds;r++) for(int k=0;k<2;k++){ usleep(gaps[g]*1000); double t0=ms(); CFArrayRef a=hw(cids[k],&wid,1,(1<<11)|(1<<8)); t[k][r]=ms()-t0; if(a)CFRelease(a); }
    for(int k=0;k<2;k++) qsort(t[k],rounds,sizeof(double),cmp);
    printf("gap %3dms  main p50 %.1fms  new p50 %.1fms\n",gaps[g],t[0][rounds/2],t[1][rounds/2]);
  }
}
