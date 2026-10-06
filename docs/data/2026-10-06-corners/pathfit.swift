import AppKit
import QuartzCore
// How close the hand-built continuous path is to Core Animation's own continuous corner, and how far
// a circular corner is from it: alpha of a filled 2x render, over the top-left corner box.
func alpha(_ layer: CALayer, _ w: CGFloat, _ h: CGFloat) -> [Double] {
    let pw = Int(w * 2), ph = Int(h * 2)
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph)); ctx.scaleBy(x: 2, y: -2)
    layer.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4)
    return (0..<(pw * ph)).map { Double(d[$0 * 4 + 3]) / 255 }
}
func ca(_ w: CGFloat, _ h: CGFloat, _ r: CGFloat, _ curve: CALayerCornerCurve) -> [Double] {
    let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true
    l.backgroundColor = CGColor(red: 1, green: 1, blue: 1, alpha: 1); l.cornerRadius = r; l.cornerCurve = curve; l.masksToBounds = true
    return alpha(l, w, h)
}
func shape(_ w: CGFloat, _ h: CGFloat, _ path: CGPath) -> [Double] {
    let l = CAShapeLayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true
    l.path = path; l.fillColor = CGColor(red: 1, green: 1, blue: 1, alpha: 1)
    return alpha(l, w, h)
}
/// Per device column from the corner, how deep the shape's edge sits below the straight side, in px
/// (the clear part of the column's top rows, summed).
func inset(_ a: [Double], _ w: CGFloat, _ box: Int) -> [Double] {
    let pw = Int(w * 2)
    return (0..<box).map { x in (0..<box).reduce(0.0) { $0 + (1 - a[$1 * pw + x]) } }
}
let size = (w: CGFloat(860), h: CGFloat(1079))
for r in [CGFloat(12), 16, 20, 26] {
    let box = Int(r * 2 * 1.6) + 4
    let cont = ca(size.w, size.h, r, .continuous), circ = ca(size.w, size.h, r, .circular)
    let mine = shape(size.w, size.h, continuousPath(CGRect(x: 0, y: 0, width: size.w, height: size.h), r))
    let pw = Int(size.w * 2)
    func stats(_ a: [Double], _ b: [Double]) -> (Double, Double, Int) {
        var s = 0.0, m = 0.0, n = 0, big = 0
        for y in 0..<box { for x in 0..<box { let e = abs(a[y * pw + x] - b[y * pw + x]); s += e; m = max(m, e); n += 1; if e > 0.5 { big += 1 } } }
        return (s / Double(n), m, big)
    }
    let (m1, x1, b1) = stats(cont, mine), (m2, x2, b2) = stats(cont, circ)
    let ic = inset(cont, size.w, box), ir = inset(circ, size.w, box), im = inset(mine, size.w, box)
    let normal = zip(ic, ir).map { abs($0 - $1) }.max()!
    let normalMine = zip(ic, im).map { abs($0 - $1) }.max()!
    let areaPx = zip(ic, ir).reduce(0.0) { $0 + abs($1.0 - $1.1) }
    let net = zip(ic, ir).reduce(0.0) { $0 + ($1.0 - $1.1) }
    print(String(format: "r=%2.0f  CA continuous vs hand path: mean %.4f max %.3f (px >0.5: %d)  | vs CA circular: mean %.4f max %.3f (px >0.5: %d)", r, m1, x1, b1, m2, x2, b2))
    print(String(format: "      edge inset gap circular vs continuous, along the side: max %.2f px, %.1f px² per side-run (net %+.1f: + = continuous cuts more); hand path max gap %.2f px", normal, areaPx, net, normalMine))
    var row = "      inset px at d(pt)=   "
    for d in [2.0, 4, 8, 12, 16, 18, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38] { let i = Int(d * 2); if i < box { row += String(format: " %4.0f:%5.2f/%5.2f", d, ic[i], ir[i]) } }
    print(row + "   (continuous/circular)")
}
