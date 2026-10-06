// Read-only: lists on-screen layer-0 windows and the corner radii SkyLight reports for them.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
typedef int cid_t;
extern cid_t SLSMainConnectionID(void);
extern CFTypeRef SLSWindowQueryWindows(cid_t, CFArrayRef, int);
extern CFTypeRef SLSWindowQueryResultCopyWindows(CFTypeRef);
extern bool SLSWindowIteratorAdvance(CFTypeRef);
extern uint32_t SLSWindowIteratorGetWindowID(CFTypeRef);
extern CGRect SLSWindowIteratorGetBounds(CFTypeRef);
int main(void) {
  @autoreleasepool {
    void *lib = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
    CFArrayRef (*radii_fn)(CFTypeRef) = dlsym(lib, "SLSWindowIteratorGetCornerRadii");
    printf("SLSWindowIteratorGetCornerRadii %s\n", radii_fn ? "present" : "absent");
    NSArray *infos = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    NSMutableArray *ids = [NSMutableArray array];
    NSMutableDictionary *owner = [NSMutableDictionary dictionary];
    for (NSDictionary *i in infos) {
      if ([i[(id)kCGWindowLayer] intValue] != 0) continue;
      [ids addObject:i[(id)kCGWindowNumber]];
      owner[i[(id)kCGWindowNumber]] = i[(id)kCGWindowOwnerName] ?: @"?";
    }
    cid_t cid = SLSMainConnectionID();
    CFTypeRef q = SLSWindowQueryWindows(cid, (__bridge CFArrayRef)ids, 0);
    if (!q) { printf("no query\n"); return 1; }
    CFTypeRef it = SLSWindowQueryResultCopyWindows(q);
    while (it && SLSWindowIteratorAdvance(it)) {
      uint32_t wid = SLSWindowIteratorGetWindowID(it);
      CGRect b = SLSWindowIteratorGetBounds(it);
      NSString *radii = @"-";
      if (radii_fn) { CFArrayRef r = radii_fn(it); if (r) { radii = [(__bridge NSArray *)r componentsJoinedByString:@","]; CFRelease(r); } else radii = @"null"; }
      printf("%u\t%s\t%.0fx%.0f\tradii=%s\n", wid, [owner[@(wid)] UTF8String], b.size.width, b.size.height, [radii UTF8String]);
    }
  }
  return 0;
}
