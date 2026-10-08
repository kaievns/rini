import Foundation
import CoreGraphics
import AppKit

let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
typealias ListImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
let cg = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW)
let listImage = unsafeBitCast(dlsym(cg, "CGWindowListCreateImage"), to: ListImage.self)
let OPT = UInt32((1 << 11) | (1 << 8))

func composite(_ ids: [UInt32]) -> CGImage? {
  guard let arr = ids.withUnsafeBufferPointer({ cap(cid, $0.baseAddress!, Int32(ids.count), OPT) })?.takeRetainedValue() as? [CGImage] else { return nil }
  return arr.first
}

func rect(_ d: [String: Any]) -> CGRect {
  guard let b = d[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: b) else { return .zero }
  return r
}

// probe <rini's pid> [dump dir]: every window wholly on the main display, alone against baked with the
// desktop windows behind it, by rini's material test; the 64x64 crops written to the dump dir if given.
let rini = Int32(CommandLine.arguments.count > 1 ? Int32(CommandLine.arguments[1])! : 0)
let dump: String? = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : nil
let display = CGDisplayBounds(CGMainDisplayID())
let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []
var desktop: [UInt32] = []
var desktopUnion = CGRect.null
var targets: [(UInt32, String, CGRect)] = []
for w in list {
  let layer = w[kCGWindowLayer as String] as? Int ?? 0
  let id = w[kCGWindowNumber as String] as? UInt32 ?? 0
  let pid = w[kCGWindowOwnerPID as String] as? Int32 ?? 0
  let owner = w[kCGWindowOwnerName as String] as? String ?? "?"
  let alpha = w[kCGWindowAlpha as String] as? Double ?? -1
  let r = rect(w)
  if pid == rini {
    print("rini window id=\(id) layer=\(layer) alpha=\(alpha) bounds=\(r)")
    continue
  }
  if layer <= -2147483600 && r.intersects(display) { desktop.append(id); desktopUnion = desktopUnion.union(r) }
  if layer == 0 && r.width >= 100 && r.height >= 100 && display.contains(r) { targets.append((id, owner, r)) }
}
print("desktop windows \(desktop.count) union \(desktopUnion)")

func small(_ img: CGImage, crop: CGRect?, side: Int) -> [UInt8] {
  var px = [UInt8](repeating: 0, count: side * side * 4)
  let ctx = CGContext(data: &px, width: side, height: side, bitsPerComponent: 8, bytesPerRow: side * 4,
                      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
  var source = img
  if let crop = crop, let c = img.cropping(to: crop) { source = c }
  ctx.draw(source, in: CGRect(x: 0, y: 0, width: side, height: side))
  return px
}

func diff(_ a: [UInt8], _ b: [UInt8], side: Int, border: Int) -> Double {
  var sum = 0.0, n = 0
  for y in border..<(side - border) { for x in border..<(side - border) {
    let at = (y * side + x) * 4
    sum += (0..<3).map { abs(Double(a[at + $0]) - Double(b[at + $0])) }.reduce(0, +) / 3; n += 1
  } }
  return sum / Double(n)
}

func changedShare(_ a: [UInt8], _ b: [UInt8], side: Int, border: Int, above: Int) -> Double {
  var changed = 0, n = 0
  for y in border..<(side - border) { for x in border..<(side - border) {
    let at = (y * side + x) * 4
    let d = (0..<3).map { abs(Int(a[at + $0]) - Int(b[at + $0])) }.reduce(0, +)
    n += 1; if d > above { changed += 1 }
  } }
  return Double(changed) / Double(n)
}

func opaqueShare(_ a: [UInt8], side: Int) -> Double {
  var clear = 0, n = 0
  for y in 4..<(side - 4) { for x in 4..<(side - 4) { n += 1; if a[(y * side + x) * 4 + 3] < 250 { clear += 1 } } }
  return Double(clear) / Double(n)
}

let front = NSWorkspace.shared.frontmostApplication?.localizedName ?? "?"
print("front app: \(front)")
for (id, owner, r) in targets {
  guard let alone = composite([id]), let baked = composite([id] + desktop) else { print("\(owner) \(id): no capture"); continue }
  let covers = r.union(desktopUnion).intersection(display)
  let scale = Double(baked.width) / covers.width
  let crop = CGRect(x: (r.minX - covers.minX) * scale, y: (r.minY - covers.minY) * scale, width: r.width * scale, height: r.height * scale).integral
  let a = small(alone, crop: nil, side: 64)
  let b = small(baked, crop: crop, side: 64)
  let t = small(alone, crop: nil, side: 48)
  if let dump = dump {
    let stamp = Int(Date().timeIntervalSince1970)
    try? Data(a).write(to: URL(fileURLWithPath: "\(dump)/\(owner.replacingOccurrences(of: " ", with: "_"))-\(id)-\(stamp)-alone.rgba"))
    try? Data(b).write(to: URL(fileURLWithPath: "\(dump)/\(owner.replacingOccurrences(of: " ", with: "_"))-\(id)-\(stamp)-baked.rgba"))
  }
  let screen = listImage(r, CGWindowListOption([.optionOnScreenBelowWindow, .optionIncludingWindow]).rawValue, UInt32(id), 0)?.takeRetainedValue()
  let s = screen.map { small($0, crop: nil, side: 64) }
  let belowDiff = s.map { diff(b, $0, side: 64, border: 6) } ?? -1
  let aloneBelow = s.map { diff(a, $0, side: 64, border: 6) } ?? -1
  print(String(format: "%@ id=%u %.0fx%.0f at %.0f,%.0f alone=%dx%d baked=%dx%d scale=%.2f clear=%.3f material_diff=%.2f baked_vs_below=%.2f alone_vs_below=%.2f changed6=%.3f changed12=%.3f",
               owner, id, r.width, r.height, r.minX, r.minY, alone.width, alone.height, baked.width, baked.height, scale, opaqueShare(t, side: 48), diff(a, b, side: 64, border: 6), belowDiff, aloneBelow, changedShare(a, b, side: 64, border: 6, above: 6), changedShare(a, b, side: 64, border: 6, above: 12)))
}
