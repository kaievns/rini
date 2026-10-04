import Cocoa
import ApplicationServices
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConnT = @convention(c) () -> Int32
typealias BoundsT = @convention(c) (Int32, UInt32, UnsafeMutablePointer<CGRect>) -> Int32
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConnT.self)()
let getBounds = unsafeBitCast(dlsym(sky, "SLSGetWindowBounds"), to: BoundsT.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
typealias GetWidT = @convention(c) (AXUIElement, UnsafeMutablePointer<UInt32>) -> AXError
let getWid = unsafeBitCast(dlsym(dlopen(nil, RTLD_NOW), "_AXUIElementGetWindow"), to: GetWidT.self)
func axWindow(_ pid: pid_t, _ wid: UInt32) -> AXUIElement? {
  let app = AXUIElementCreateApplication(pid); var v: CFTypeRef?
  AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &v)
  for w in (v as? [AXUIElement]) ?? [] { var id: UInt32 = 0; if getWid(w, &id) == .success && id == wid { return w } }
  return nil
}
func bounds(_ w: UInt32) -> CGRect { var r = CGRect.zero; _ = getBounds(cid, w, &r); return r }
func cgBounds(_ w: UInt32) -> CGRect {
  guard let l = CGWindowListCopyWindowInfo([.optionIncludingWindow], w) as? [[String: Any]], let d = l.first?["kCGWindowBounds"] as? [String: Double] else { return .zero }
  return CGRect(x: d["X"]!, y: d["Y"]!, width: d["Width"]!, height: d["Height"]!)
}
for spec in CommandLine.arguments.dropFirst() {
  let p = spec.split(separator: ":"); let pid = pid_t(p[0])!, wid = UInt32(p[1])!, name = String(p[2])
  guard let w = axWindow(pid, wid) else { print(name, "no AX window"); continue }
  var calls: [Double] = [], lags: [Double] = [], sizeCalls: [Double] = [], cgLags: [Double] = []
  let b0 = bounds(wid)
  if b0.origin.x < 1700 { print(name, "not parked, skipping", b0); continue }
  for i in 0..<10 {
    let target = CGPoint(x: b0.origin.x, y: b0.origin.y + (i % 2 == 0 ? 1 : 0))
    var pt = target; let v = AXValueCreate(.cgPoint, &pt)!
    let t0 = now(); AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, v); let t1 = now()
    var seen = -1.0, cgSeen = -1.0
    while now() - t1 < 100 && (seen < 0 || cgSeen < 0) {
      if seen < 0 && abs(bounds(wid).origin.y - target.y) < 0.5 { seen = now() - t1 }
      if cgSeen < 0 && abs(cgBounds(wid).origin.y - target.y) < 0.5 { cgSeen = now() - t1 }
      usleep(250)
    }
    calls.append(t1 - t0); lags.append(seen); cgLags.append(cgSeen)
    // same size written again: what an unconditional size write costs
    var sz = b0.size; let sv = AXValueCreate(.cgSize, &sz)!
    let t2 = now(); AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, sv); sizeCalls.append(now() - t2)
    usleep(120000)
  }
  var pt = b0.origin; AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &pt)!)
  func f(_ a: [Double]) -> String { let s = a.sorted(); return String(format: "p50 %5.2f max %6.2f", s[s.count / 2], s.last!) }
  print(name.padding(toLength: 9, withPad: " ", startingAt: 0), "AX position write", f(calls), "| -> SLSGetWindowBounds", f(lags), "| -> CGWindowList", f(cgLags), "| same-size write", f(sizeCalls))
}
