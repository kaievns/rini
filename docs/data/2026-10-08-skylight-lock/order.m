// order [main-first|app-first]: which connection SLSNewConnection opens when it is asked before
// SLSMainConnectionID, after it, and after NSApplication. Reads only.
#import <AppKit/AppKit.h>
#include <dlfcn.h>
int main(int argc,char**argv){ @autoreleasepool{
  void*sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  int(*nc)(int,int*)=dlsym(sl,"SLSNewConnection"); int(*mc)(void)=dlsym(sl,"SLSMainConnectionID");
  int a=0,b=0,e;
  if(argc>1 && !strcmp(argv[1],"main-first")){ int m=mc(); e=nc(0,&a); printf("main first: main %d, new %d (err %d), main again %d\n",m,a,e,mc()); }
  else if(argc>1 && !strcmp(argv[1],"app-first")){ [NSApplication sharedApplication]; e=nc(0,&a); printf("NSApp first: new %d (err %d), main %d\n",a,e,mc()); }
  else { e=nc(0,&a); int e2=nc(0,&b); printf("new first: new %d (err %d), second new %d (err %d), main %d\n",a,e,b,e2,mc()); }
}}
