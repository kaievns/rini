#include <ApplicationServices/ApplicationServices.h>
#include <stdio.h>
static CGEventRef cb(CGEventTapProxy p, CGEventType t, CGEventRef e, void *u) { return e; }
static void invalidated(CFMachPortRef port, void *info) { printf("invalidation callback info=%p\n", info); }
int main(void) {
  void *refcon = (void *)0x1234;
  CFMachPortRef port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap,
      kCGEventTapOptionListenOnly, CGEventMaskBit(kCGEventMouseMoved), cb, refcon);
  if (!port) { printf("tap null\n"); return 1; }
  CFMachPortSetInvalidationCallBack(port, invalidated);
  printf("refcon=%p\n", refcon);
  CFMachPortInvalidate(port);
  CFRelease(port);
  return 0;
}
