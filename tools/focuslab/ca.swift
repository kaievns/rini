import Cocoa
import QuartzCore
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let mainConn = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)
let capf = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func cap(_ ids: [UInt32]) -> CGImage? {
  let arr = ids.withUnsafeBufferPointer { capf(mainConn(), $0.baseAddress!, Int32(ids.count), (1 << 11) | (1 << 8)) }?.takeRetainedValue()
  return (arr as? [CGImage])?.first
}
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 600), styleMask: [.borderless], backing: .buffered, defer: false)
w.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopWindow)) + 1)
let root = CALayer(); w.contentView!.layer = root; w.contentView!.wantsLayer = true
w.orderBack(nil)
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
_ = cap(ids)
let img0 = cap(ids)!
print("colorspace:", img0.colorSpace.map { String(describing: $0) } ?? "nil")
func commit(_ label: String, _ make: () -> Any) {
  var ts: [Double] = []
  for _ in 0..<6 {
    let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: 400, height: 300)
    let t0 = now()
    let c = make()
    let t1 = now()
    CATransaction.begin(); root.addSublayer(l); l.contents = c; CATransaction.commit(); CATransaction.flush()
    let t2 = now()
    ts.append(t2 - t1)
    _ = t0
    CATransaction.begin(); l.removeFromSuperlayer(); CATransaction.commit(); CATransaction.flush()
  }
  print(label, "commit ms:", ts.map { String(format: "%.1f", $0) }.joined(separator: " "))
}
let full = cap(ids)!
commit("full image       ") { cap(ids)! }
commit("crop half        ") { let i = cap(ids)!; return i.cropping(to: CGRect(x: 0, y: 0, width: i.width/2, height: i.height/2))! }
commit("full, contentsRect") { cap(ids)! }
// row-copy crop: same format, new buffer
func rowCopy(_ i: CGImage, _ r: CGRect) -> CGImage? {
  guard let d = i.dataProvider?.data, let p = CFDataGetBytePtr(d) else { return nil }
  let x = Int(r.origin.x), y = Int(r.origin.y), cw = Int(r.width), ch = Int(r.height)
  let bpr = cw * 4
  let out = UnsafeMutablePointer<UInt8>.allocate(capacity: bpr * ch)
  for row in 0..<ch { memcpy(out + row * bpr, p + (y + row) * i.bytesPerRow + x * 4, bpr) }
  let data = CFDataCreateWithBytesNoCopy(nil, out, bpr * ch, kCFAllocatorMalloc)!
  return CGImage(width: cw, height: ch, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: bpr, space: i.colorSpace ?? CGColorSpaceCreateDeviceRGB(), bitmapInfo: i.bitmapInfo, provider: CGDataProvider(data: data)!, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
}
var rc: [Double] = []
commit("row-copy half    ") { let i = cap(ids)!; let t = now(); let o = rowCopy(i, CGRect(x: 0, y: 0, width: i.width/2, height: i.height/2))!; rc.append(now()-t); return o }
print("   row-copy itself ms:", rc.map { String(format: "%.1f", $0) }.joined(separator: " "))
var sc: [Double] = []
commit("draw-copy half   ") { let i = cap(ids)!; let t = now(); let c = i.cropping(to: CGRect(x: 0, y: 0, width: i.width/2, height: i.height/2))!
  let ctx = CGContext(data: nil, width: c.width, height: c.height, bitsPerComponent: 8, bytesPerRow: 0, space: c.colorSpace ?? CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)!
  ctx.draw(c, in: CGRect(x: 0, y: 0, width: c.width, height: c.height)); let o = ctx.makeImage()!; sc.append(now()-t); return o }
print("   draw-copy itself ms:", sc.map { String(format: "%.1f", $0) }.joined(separator: " "))
_ = full
