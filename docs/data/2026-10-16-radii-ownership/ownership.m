// Built: clang -fobjc-arc -framework Foundation -framework CoreGraphics -F/System/Library/PrivateFrameworks -framework SkyLight ownership.m -o ownership
// Read-only: whether SLSWindowIteratorGetCornerRadii hands its caller a +1 array. For each on-screen
// layer-0 window, the array's retain count is read with one extra retain of the probe's own: inside
// an autorelease pool (a), after the pool drains (b), after the iterator is released (c) and after the
// query is released (d). A +1 the caller owns reads 2 at d; a +0 the iterator or a pool holds reads 1.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
typedef int cid_t;
extern cid_t SLSMainConnectionID(void);
extern CFTypeRef SLSWindowQueryWindows(cid_t, CFArrayRef, int);
extern CFTypeRef SLSWindowQueryResultCopyWindows(CFTypeRef);
extern bool SLSWindowIteratorAdvance(CFTypeRef);
extern uint32_t SLSWindowIteratorGetWindowID(CFTypeRef);
int main(void) {
  void *lib = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
  CFArrayRef (*radii_fn)(CFTypeRef) = dlsym(lib, "SLSWindowIteratorGetCornerRadii");
  if (!radii_fn) { printf("absent\n"); return 1; }
  NSMutableArray *ids = [NSMutableArray array];
  @autoreleasepool {
    NSArray *infos = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    for (NSDictionary *i in infos) if ([i[(id)kCGWindowLayer] intValue] == 0) [ids addObject:i[(id)kCGWindowNumber]];
  }
  printf("wid\tsame_on_second_call\ta_in_pool\tb_after_pool\tc_after_iterator\td_after_query\n");
  for (NSNumber *wid in ids) {
    CFTypeRef q = SLSWindowQueryWindows(SLSMainConnectionID(), (__bridge CFArrayRef)@[wid], 0);
    if (!q) continue;
    CFTypeRef it = SLSWindowQueryResultCopyWindows(q);
    if (!it || !SLSWindowIteratorAdvance(it)) { if (it) CFRelease(it); CFRelease(q); continue; }
    CFArrayRef r = NULL; CFIndex a = 0; int same = 0;
    @autoreleasepool {
      r = radii_fn(it);
      if (r) {
        CFRetain(r);
        a = CFGetRetainCount(r);
        CFArrayRef again = radii_fn(it);
        same = again == r;
        // Held by the probe only to compare; released as the contract under test would have it.
        if (again && !same) CFRelease(again);
        if (same) CFRelease(again);
      }
    }
    if (!r) { printf("%u\tnull\n", SLSWindowIteratorGetWindowID(it)); CFRelease(it); CFRelease(q); continue; }
    CFIndex b = CFGetRetainCount(r);
    uint32_t w = SLSWindowIteratorGetWindowID(it);
    CFRelease(it);
    CFIndex c = CFGetRetainCount(r);
    CFRelease(q);
    CFIndex d = CFGetRetainCount(r);
    printf("%u\t%d\t%ld\t%ld\t%ld\t%ld\n", w, same, a, b, c, d);
  }
  return 0;
}
