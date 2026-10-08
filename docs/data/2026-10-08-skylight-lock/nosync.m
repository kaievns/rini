// nosync: the reads that wait on the MAIN connection whatever connection they are given
// (SLSGetOnScreenWindowList, SLSCopyWindowsWithOptionsAndTags, SLSWindowIsOrderedIn), against calls that wait on
// nothing or on the connection they are given, asked on an SLSNewConnection: SLSGetWindowListWithTags with its
// on-screen flag, SLSWindowQueryRun over the same spaces and options, and each window's iterator tags and
// attributes against whether it is ordered in. Reads only.
#import <Foundation/Foundation.h>
#include <dlfcn.h>
typedef int (*List)(int, int, int, uint32_t*, int*);
typedef int (*ListTags)(int, int, int, uint32_t*, int*, int, uint64_t*, uint64_t*, int);
typedef CFArrayRef (*CopyWT)(int, uint32_t, CFArrayRef, uint32_t, uint64_t*, uint64_t*);
typedef CFTypeRef (*QCreate)(CFDictionaryRef); typedef void (*QSet)(CFTypeRef, CFStringRef, CFTypeRef); typedef CFTypeRef (*QRun)(int, CFTypeRef, int);
typedef CFTypeRef (*QueryW)(int, CFArrayRef, int); typedef CFTypeRef (*CopyW)(CFTypeRef); typedef bool (*Adv)(CFTypeRef);
typedef uint32_t (*GetU)(CFTypeRef); typedef uint64_t (*GetQ)(CFTypeRef); typedef int (*Ordered)(int, uint32_t, uint8_t*);
typedef CFArrayRef (*CopySp)(int, int);
static void*sl;
static NSArray*ids_of(CFTypeRef it, GetU wid, Adv adv){ NSMutableArray*a=[NSMutableArray array]; while(it&&adv(it)) [a addObject:@(wid(it))]; return a; }
int main(){ @autoreleasepool{
  sl=dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",RTLD_NOW);
  int mainc=((int(*)(void))dlsym(sl,"SLSMainConnectionID"))(), newc=0; ((int(*)(int,int*))dlsym(sl,"SLSNewConnection"))(0,&newc);
  if(newc==0||newc==mainc){ fprintf(stderr,"no connection of its own\n"); return 1; } printf("main %d, new %d\n",mainc,newc);
  List on=dlsym(sl,"SLSGetOnScreenWindowList"), all=dlsym(sl,"SLSGetWindowList"); ListTags tl=dlsym(sl,"SLSGetWindowListWithTags");
  CopyWT copywt=dlsym(sl,"SLSCopyWindowsWithOptionsAndTags"); QCreate qc=dlsym(sl,"SLSWindowQueryCreate"); QSet qs=dlsym(sl,"SLSWindowQuerySetValue"); QRun qr=dlsym(sl,"SLSWindowQueryRun");
  QueryW q=dlsym(sl,"SLSWindowQueryWindows"); CopyW cw=dlsym(sl,"SLSWindowQueryResultCopyWindows"); Adv adv=dlsym(sl,"SLSWindowIteratorAdvance");
  GetU wid=dlsym(sl,"SLSWindowIteratorGetWindowID"); GetQ tags=dlsym(sl,"SLSWindowIteratorGetTags"), attrs=dlsym(sl,"SLSWindowIteratorGetAttributes"); Ordered ord=dlsym(sl,"SLSWindowIsOrderedIn");
  CopySp sp=dlsym(sl,"SLSCopySpaces");
  #define KEY(n) (*(CFStringRef*)dlsym(sl,n))
  // 1. on screen
  for(int flag=0;flag<2;flag++){ uint32_t a[4096],b[4096]; int na=0,nb=0; if(flag) on(newc,0,4096,a,&na); else all(newc,0,4096,a,&na);
    uint64_t z1=0,z2=0; int e=tl(newc,0,4096,b,&nb,flag,&z1,&z2,64); int same=na==nb; for(int i=0;same&&i<na;i++) same=a[i]==b[i];
    printf("%s: %d windows; SLSGetWindowListWithTags flag %d (err %d): %d windows, same list in the same order: %s\n",flag?"SLSGetOnScreenWindowList":"SLSGetWindowList",na,flag,e,nb,same?"yes":"NO"); }
  // 2. space window lists
  int masks[2]={(1<<0)|(1<<2), (1<<0)|(1<<2)|(1<<3)}; uint32_t opts[2]={0x2,0x7};
  for(int m=0;m<2;m++) for(int o=0;o<2;o++){ CFArrayRef spaces=sp(mainc,masks[m]); NSArray*ss=(__bridge NSArray*)spaces;
    for(NSNumber*s in ss){ CFArrayRef one=(__bridge CFArrayRef)@[s]; uint64_t st=0,ct=0; CFArrayRef w=copywt(newc,0,one,opts[o],&st,&ct); NSArray*ref=w?[(__bridge NSArray*)w copy]:@[]; if(w)CFRelease(w);
      for(int fl=0;fl<3;fl+=2){ CFTypeRef qq=qc(NULL); qs(qq,KEY("SLSWindowQueryKeyOwner"),(__bridge CFTypeRef)@0); qs(qq,KEY("SLSWindowQueryKeySpaces"),one);
        qs(qq,KEY("SLSWindowQueryKeyWorkspaceWindowListOptions"),(__bridge CFTypeRef)@(opts[o])); qs(qq,KEY("SLSWindowQueryKeyIncludeTags"),(__bridge CFTypeRef)@0); qs(qq,KEY("SLSWindowQueryKeyExcludeTags"),(__bridge CFTypeRef)@0);
        CFTypeRef r=qr(newc,qq,fl); CFTypeRef it=r?cw(r):NULL; NSArray*got=ids_of(it,wid,adv); if(it)CFRelease(it); if(r)CFRelease(r); CFRelease(qq);
        printf("space %s options %x: SLSCopyWindowsWithOptionsAndTags %lu, SLSWindowQueryRun flags %d %lu, same: %s\n",s.stringValue.UTF8String,opts[o],(unsigned long)ref.count,fl,(unsigned long)got.count,[ref isEqualToArray:got]?"yes":"NO"); } }
    CFRelease(spaces); }
  // 3. ordered in, against each tag and attribute bit
  uint32_t a[4096]; int na=0; all(newc,0,4096,a,&na); NSMutableArray*ids=[NSMutableArray array]; for(int i=0;i<na;i++) [ids addObject:@(a[i])];
  CFTypeRef r=q(newc,(__bridge CFArrayRef)ids,0); CFTypeRef it=r?cw(r):NULL; int n=0, nin=0; uint64_t tg[4096], at[4096]; uint8_t oi[4096];
  while(it&&adv(it)){ uint32_t w=wid(it); tg[n]=tags(it); at[n]=attrs(it); uint8_t o=0; ord(newc,w,&o); oi[n]=o; nin+=o; n++; }
  printf("%d windows, %d ordered in\n",n,nin);
  for(int src=0;src<2;src++) for(int bit=0;bit<64;bit++){ int agree=0; for(int i=0;i<n;i++){ int set=((src?at[i]:tg[i])>>bit)&1; if(set==oi[i]) agree++; }
    if(agree>=n-0||agree<=0) printf("  %s bit %d %s ordered in for all %d\n",src?"attribute":"tag",bit,agree?"is":"is NOT",n); }
  if(it)CFRelease(it); if(r)CFRelease(r);
}}
