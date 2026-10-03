import Cocoa
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let big = UInt32(CommandLine.arguments[1])!
func capture(_ w: UInt32) -> Double { var w = w; let t = now(); _ = cap(cid, &w, 1, (1 << 11) | (1 << 8))?.takeRetainedValue(); return now() - t }
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let win = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 2, height: 2), styleMask: [.borderless], backing: .buffered, defer: false)
win.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopWindow)) + 1)
win.backgroundColor = NSColor(white: 0.5, alpha: 1)
win.orderBack(nil)
let tiny = UInt32(win.windowNumber)
Thread.detachNewThread {
  usleep(300000)
  var tt: [Double] = []
  for _ in 0..<20 { tt.append(capture(tiny)) }
  tt.sort(); print("tiny window capture back-to-back p50", String(format: "%.1f", tt[10]))
  for period in [0, 4, 8, 16] {
    var on = period > 0
    let ka = Thread { while on { _ = capture(tiny); usleep(UInt32(period * 1000)) } }
    if period > 0 { ka.start() }
    var ts: [Double] = []
    for i in 0..<12 { usleep(UInt32(300000 + (i * 7919) % 50000)); ts.append(capture(big)) }
    on = false; usleep(100000)
    ts.sort(); print(period == 0 ? "no keep-alive          " : "keep-alive every \(period)ms   ", "big capture p50", String(format: "%5.1f", ts[6]), "p90", String(format: "%5.1f", ts[10]))
  }
  exit(0)
}
app.run()
