// Read-only: corner radii SkyLight reports for every window (on and off screen), and the cost of one query.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
typedef int cid_t;
extern cid_t SLSMainConnectionID(void);
extern CFTypeRef SLSWindowQueryWindows(cid_t, CFArrayRef, int);
extern CFTypeRef SLSWindowQueryResultCopyWindows(CFTypeRef);
extern bool SLSWindowIteratorAdvance(CFTypeRef);
extern int SLSWindowIteratorGetCount(CFTypeRef);
extern uint32_t SLSWindowIteratorGetWindowID(CFTypeRef);
extern int SLSWindowIteratorGetLevel(CFTypeRef);
extern CGRect SLSWindowIteratorGetBounds(CFTypeRef);
static double ms(uint64_t t){ static mach_timebase_info_data_t tb; if(!tb.denom) mach_timebase_info(&tb); return (double)t*tb.numer/tb.denom/1e6; }
int main(void) {
  @autoreleasepool {
    void *lib = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
    CFArrayRef (*radii_fn)(CFTypeRef) = dlsym(lib, "SLSWindowIteratorGetCornerRadii");
    if (!radii_fn) { printf("absent\n"); return 1; }
    NSArray *infos = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID));
    NSMutableArray *ids = [NSMutableArray array]; NSMutableDictionary *owner = [NSMutableDictionary dictionary]; NSMutableDictionary *onscreen=[NSMutableDictionary dictionary];
    for (NSDictionary *i in infos) { [ids addObject:i[(id)kCGWindowNumber]]; owner[i[(id)kCGWindowNumber]] = i[(id)kCGWindowOwnerName] ?: @"?"; onscreen[i[(id)kCGWindowNumber]] = i[(id)kCGWindowIsOnscreen] ?: @0; }
    cid_t cid = SLSMainConnectionID();
    CFTypeRef q = SLSWindowQueryWindows(cid, (__bridge CFArrayRef)ids, 0);
    CFTypeRef it = SLSWindowQueryResultCopyWindows(q);
    NSMutableDictionary *hist = [NSMutableDictionary dictionary];
    int unequal = 0, nulls = 0, n = 0;
    while (it && SLSWindowIteratorAdvance(it)) {
      uint32_t wid = SLSWindowIteratorGetWindowID(it); int level = SLSWindowIteratorGetLevel(it); CGRect b = SLSWindowIteratorGetBounds(it);
      CFArrayRef r = radii_fn(it); n++;
      NSString *s = @"null"; NSString *type = @"";
      if (r) { NSArray *a = (__bridge NSArray*)r; s = [a componentsJoinedByString:@","];
        if (a.count) type = [NSString stringWithFormat:@"%s", [(NSNumber*)a[0] objCType]];
        for (NSNumber *v in a) if (![v isEqual:a[0]]) { unequal++; break; }
        CFRelease(r); } else nulls++;
      NSString *key = [NSString stringWithFormat:@"L%d %@", level, s];
      hist[key] = @([hist[key] intValue] + 1);
      if (level == 0) printf("%u\tL%d\ton=%d\t%s\t%.0fx%.0f\tradii=%s %s\n", wid, level, [onscreen[@(wid)] intValue], [owner[@(wid)] UTF8String], b.size.width, b.size.height, s.UTF8String, type.UTF8String);
    }
    printf("windows=%d unequal=%d null=%d\n", n, unequal, nulls);
    for (NSString *k in [hist.allKeys sortedArrayUsingSelector:@selector(compare:)]) printf("  %s: %d\n", k.UTF8String, [hist[k] intValue]);
    CFRelease(it); CFRelease(q);
    // Cost: query of the first 12 layer-0 ids, radii read for each, 300 times.
    NSMutableArray *some = [NSMutableArray array];
    for (NSDictionary *i in infos) if ([i[(id)kCGWindowLayer] intValue]==0 && some.count<12) [some addObject:i[(id)kCGWindowNumber]];
    NSMutableArray *t = [NSMutableArray array];
    for (int k = 0; k < 300; k++) {
      uint64_t a = mach_absolute_time();
      CFTypeRef q2 = SLSWindowQueryWindows(cid, (__bridge CFArrayRef)some, 0);
      CFTypeRef it2 = SLSWindowQueryResultCopyWindows(q2); CFRelease(q2);
      SLSWindowIteratorGetCount(it2);
      while (SLSWindowIteratorAdvance(it2)) { CFArrayRef r = radii_fn(it2); if (r) CFRelease(r); }
      CFRelease(it2);
      [t addObject:@(ms(mach_absolute_time()-a))];
    }
    NSArray *s = [t sortedArrayUsingSelector:@selector(compare:)];
    printf("query+radii for %lu windows, n=300: p50 %.3fms p90 %.3fms max %.3fms\n", (unsigned long)some.count, [s[150] doubleValue], [s[270] doubleValue], [s[299] doubleValue]);
  }
  return 0;
}
