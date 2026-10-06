import AppKit
import QuartzCore
func render(_ l: CALayer, _ w: CGFloat, _ h: CGFloat) -> [Double] {
    let pw = Int((w * 2).rounded(.up)), ph = Int((h * 2).rounded(.up))
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph)); ctx.scaleBy(x: 2, y: -2); l.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4)
    return (0..<(pw * ph)).map { Double(d[$0 * 4 + 3]) / 255 }
}
func ca(_ w: CGFloat, _ h: CGFloat, _ r: CGFloat) -> [Double] { let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.backgroundColor = .white; l.cornerRadius = r; l.cornerCurve = .continuous; l.masksToBounds = true; return render(l, w, h) }
func shape(_ w: CGFloat, _ h: CGFloat, _ p: CGPath) -> [Double] { let s = CAShapeLayer(); s.frame = CGRect(x: 0, y: 0, width: w, height: h); s.isGeometryFlipped = true; s.fillColor = .white; s.path = p; return render(s, w, h) }
func cmp(_ a: [Double], _ b: [Double]) -> (Double, Int) { var m = 0.0, big = 0; for i in 0..<a.count { let e = abs(a[i] - b[i]); m = max(m, e); if e > 0.5 { big += 1 } }; return (m, big) }
for (w, h, r) in [(CGFloat(860), CGFloat(1079), CGFloat(26)), (80, 400, 26), (60, 400, 26), (52, 400, 26), (40, 400, 16), (32, 400, 16), (26, 400, 13), (12, 400, 6), (4, 400, 2)] {
    let ref = ca(w, h, r)
    let a = cmp(ref, shape(w, h, continuousPath(CGRect(x: 0, y: 0, width: w, height: h), r)))
    let circ = { () -> [Double] in let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.backgroundColor = .white; l.cornerRadius = r; l.cornerCurve = .circular; l.masksToBounds = true; return render(l, w, h) }()
    let c = cmp(ref, circ)
    var bestK = 0.0, bestB = Int.max
    var k = 0.30
    while k <= 1.0 { let rr = r * CGFloat(k); let b = cmp(ref, shape(w, h, continuousPath(CGRect(x: 0, y: 0, width: w, height: h), rr))).1; if b < bestB { bestB = b; bestK = k }; k += 0.01 }
    print(String(format: "      vs CA circular same r: max %.3f px>0.5 %d; best uniformly-scaled hand path r*%.2f px>0.5 %d", c.0, c.1, bestK, bestB))
    // alternative: what CA would draw for a radius the hand path did not shrink
    print(String(format: "%4.0fx%4.0f r=%4.1f (reach %.1f vs half side %.1f): hand path vs CA continuous max %.3f px>0.5 %d", w, h, r, 1.52866483 * r, min(w, h) / 2, a.0, a.1))
}
