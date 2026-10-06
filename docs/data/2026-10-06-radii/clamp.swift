import AppKit
import QuartzCore
func render(_ w: CGFloat, _ h: CGFloat, _ r: CGFloat, _ curve: CALayerCornerCurve) -> [UInt8] {
    let n = Int(w*2), m = Int(h*2)
    let layer = CALayer(); layer.bounds = CGRect(x: 0, y: 0, width: w, height: h); layer.position = CGPoint(x: w/2, y: h/2)
    layer.backgroundColor = CGColor(red: 1, green: 1, blue: 1, alpha: 1); layer.cornerRadius = r; layer.cornerCurve = curve; layer.masksToBounds = true
    let ctx = CGContext(data: nil, width: n, height: m, bitsPerComponent: 8, bytesPerRow: n*4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: 2, y: 2); layer.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: n*m*4); return (0..<(n*m)).map { d[$0*4+3] }
}
for curve in [CALayerCornerCurve.circular, .continuous] {
  for (w, r) in [(20.0, 16.0), (12.0, 26.0), (0.5, 16.0)] as [(CGFloat, CGFloat)] {
    let a = render(w, 100, r, curve), b = render(w, 100, w/2, curve)
    let d = zip(a, b).map { abs(Int($0) - Int($1)) }.max()!
    print(curve.rawValue, "w", w, "r", r, "vs r=w/2: max diff", d)
  }
}
for (w, r) in [(20.0, 16.0), (20.0, 10.0), (20.0, 0.0)] as [(CGFloat, CGFloat)] {
    let a = render(w, 100, r, .circular)
    let n = Int(w*2)
    let rows = [0, 4, 10, 20, 40, 100].map { y in (0..<n).map { a[y*n+$0] > 128 ? "#" : "." }.joined() }
    print("w", w, "r", r, "opaque px", a.filter { $0 > 128 }.count); rows.forEach { print("  ", $0) }
}
