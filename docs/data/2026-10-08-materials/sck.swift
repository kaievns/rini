import Foundation
import CoreGraphics
import ScreenCaptureKit
// sck <window id> <x0> <y0> <x1> <y1>, fractions of the window: mean RGB and chroma over that rect
// of the window alone through SkyLight (rini's options) and through ScreenCaptureKit, both at 2x.
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
let a = CommandLine.arguments
let wid = UInt32(a[1])!
let f = a[2...5].map { Double($0)! }
func stats(_ img: CGImage, _ name: String) {
  let w = img.width, h = img.height
  var px = [UInt8](repeating: 0, count: w * h * 4)
  let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
  ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
  var s = [0.0, 0.0, 0.0], chroma = 0.0, n = 0.0
  for y in Int(f[1] * Double(h))..<Int(f[3] * Double(h)) { for x in Int(f[0] * Double(w))..<Int(f[2] * Double(w)) {
    let i = (y * w + x) * 4; let r = Double(px[i]), g = Double(px[i+1]), b = Double(px[i+2])
    s[0] += r; s[1] += g; s[2] += b; chroma += max(r, g, b) - min(r, g, b); n += 1 } }
  print(name, "\(w)x\(h)", String(format: "%.1f/%.1f/%.1f chroma %.1f", s[0]/n, s[1]/n, s[2]/n, chroma/n))
}
var ids = [wid]
if let arr = cap(cid, &ids, 1, UInt32((1 << 11) | (1 << 8)))?.takeRetainedValue() as? [CGImage], let img = arr.first { stats(img, "skylight-alone") }
let sem = DispatchSemaphore(value: 0)
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
  guard let win = content?.windows.first(where: { $0.windowID == wid }) else { print("no window", err as Any); sem.signal(); return }
  let filter = SCContentFilter(desktopIndependentWindow: win)
  let cfg = SCStreamConfiguration(); cfg.width = Int(win.frame.width * 2); cfg.height = Int(win.frame.height * 2); cfg.showsCursor = false
  SCScreenshotManager.captureImage(contentFilter: filter, configuration: cfg) { img, err in
    if let img = img { stats(img, "screencapturekit") } else { print("sck failed", err as Any) }
    sem.signal()
  }
}
sem.wait()
