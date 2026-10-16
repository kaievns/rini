#include <ApplicationServices/ApplicationServices.h>
#include <stdio.h>
static CGEventRef cb(CGEventTapProxy p, CGEventType t, CGEventRef e, void *u) { return e; }
int main(void) {
  void *refcon = (void *)0x1234;
  CFMachPortRef port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap,
      kCGEventTapOptionListenOnly, CGEventMaskBit(kCGEventMouseMoved), cb, refcon);
  if (!port) { printf("tap null\n"); return 1; }
  CFMachPortContext ctx = {0};
  CFMachPortGetContext(port, &ctx);
  printf("refcon=%p info=%p retain=%p release=%p\n", refcon, ctx.info, ctx.retain, ctx.release);
  CFMachPortInvalidate(port);
  CFRelease(port);
  return 0;
}
