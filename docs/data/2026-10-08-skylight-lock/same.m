// same <ids...>: SLSWindowQueryWindows on SLSMainConnectionID and on an SLSNewConnection; prints each window's
// id, pid, level, alpha and bounds from both, and whether they agree.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
typedef CFTypeRef (*QueryW)(int, CFArrayRef, int); typedef CFTypeRef (*CopyW)(CFTypeRef);
typedef bool (*Adv)(CFTypeRef); typedef uint32_t (*GetU)(CFTypeRef); typedef int (*GetI)(CFTypeRef);
typedef CGRect (*GetR)(CFTypeRef); typedef float (*GetF)(CFTypeRef);
int main(int argc,char**argv){
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  QueryW q=dlsym(sl,"SLSWindowQueryWindows"); CopyW c=dlsym(sl,"SLSWindowQueryResultCopyWindows"); Adv adv=dlsym(sl,"SLSWindowIteratorAdvance");
  GetU wid=dlsym(sl,"SLSWindowIteratorGetWindowID"); GetI pid=dlsym(sl,"SLSWindowIteratorGetPID"), lvl=dlsym(sl,"SLSWindowIteratorGetLevel");
  GetR b=dlsym(sl,"SLSWindowIteratorGetBounds"); GetF al=dlsym(sl,"SLSWindowIteratorGetAlpha");
  int mainc=((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(), newc=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  CFMutableArrayRef a=CFArrayCreateMutable(NULL,0,&kCFTypeArrayCallBacks);
  for(int i=1;i<argc;i++){int v=atoi(argv[i]); CFNumberRef n=CFNumberCreate(NULL,kCFNumberIntType,&v); CFArrayAppendValue(a,n); CFRelease(n);}
  char out[2][8192]; int cids[2]={mainc,newc};
  for(int k=0;k<2;k++){ out[k][0]=0; CFTypeRef r=q(cids[k],a,0); CFTypeRef it=r?c(r):NULL;
    while(it&&adv(it)){CGRect f=b(it); char l[256]; snprintf(l,256,"%u pid=%d lvl=%d a=%.2f %.0f,%.0f %.0fx%.0f\n",wid(it),pid(it),lvl(it),al(it),f.origin.x,f.origin.y,f.size.width,f.size.height); strcat(out[k],l);}
    if(it)CFRelease(it); if(r)CFRelease(r);}
  printf("main %d:\n%snew %d:\n%sagree=%s\n",mainc,out[0],newc,out[1],strcmp(out[0],out[1])?"NO":"yes");
}
