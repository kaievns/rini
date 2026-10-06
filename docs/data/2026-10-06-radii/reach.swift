import AppKit
import QuartzCore
func render(_ w: CGFloat, _ h: CGFloat, _ r: CGFloat, _ curve: CALayerCornerCurve) -> ([UInt8], Int) {
    let n = Int(w*2), m = Int(h*2)
    let layer = CALayer(); layer.bounds = CGRect(x: 0, y: 0, width: w, height: h); layer.position = CGPoint(x: w/2, y: h/2)
    layer.backgroundColor = CGColor(red: 1, green: 1, blue: 1, alpha: 1); layer.cornerRadius = r; layer.cornerCurve = curve; layer.masksToBounds = true
    let ctx = CGContext(data: nil, width: n, height: m, bitsPerComponent: 8, bytesPerRow: n*4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: 2, y: 2); layer.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: n*m*4); return ((0..<(n*m)).map { d[$0*4+3] }, n)
}
for r in [12.0, 16.0, 20.0, 26.0] as [CGFloat] {
  for curve in [CALayerCornerCurve.circular, .continuous] {
    let (a, n) = render(200, 200, r, curve)
    let first = (0..<n).first { a[$0] >= 254 }!
    print(curve.rawValue, "r", r, "row 0 reaches full alpha at", Double(first)/2, "pt =", String(format: "%.2f r", Double(first)/2/Double(r)))
  }
}
