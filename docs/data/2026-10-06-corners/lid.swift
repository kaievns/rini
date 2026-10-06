import AppKit
import QuartzCore
// The outline's stroke on a tile narrower than its corners' reach: the stroke follows the sides where
// they run back over each other, where the edge it outlines is the deeper of the two corners. Against
// the ring it should cover, the continuous shape grown by the stroke's half minus the one shrunk by it,
// filled even-odd: how many 2x pixels differ by more than a quarter, and how far the edge sits below
// the side at the tile's middle.
func render(_ l: CALayer, _ w: CGFloat, _ h: CGFloat) -> [Int] {
    let pw = Int((w * 2).rounded(.up)), ph = Int((h * 2).rounded(.up))
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph)); ctx.scaleBy(x: 2, y: -2); l.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4)
    return (0..<(pw * ph)).map { Int(d[$0 * 4 + 3]) }
}
func layer(_ w: CGFloat, _ h: CGFloat, _ p: CGPath, stroke: Bool) -> CAShapeLayer {
    let l = CAShapeLayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.path = p
    if stroke { l.fillColor = nil; l.strokeColor = .black; l.lineWidth = 0.5 } else { l.fillColor = .black; l.fillRule = .evenOdd }
    return l
}
let pad: CGFloat = 2
for (w, h, reported) in [(CGFloat(860), CGFloat(1079), CGFloat(26)), (80, 400, 26), (70, 400, 26), (65, 400, 26), (60, 400, 26), (52, 400, 26), (40, 400, 16), (32, 400, 16), (12, 400, 16), (4, 400, 16)] {
    let r = min(reported, min(w, h) / 2)
    let (W, H) = (w + 2 * pad, h + 2 * pad)
    let outline = continuousPath(CGRect(x: pad - 0.25, y: pad - 0.25, width: w + 0.5, height: h + 0.5), r + 0.25)
    let ring = CGMutablePath()
    ring.addPath(continuousPath(CGRect(x: pad - 0.5, y: pad - 0.5, width: w + 1, height: h + 1), r + 0.5))
    ring.addPath(continuousPath(CGRect(x: pad, y: pad, width: w, height: h), r))
    let a = render(layer(W, H, outline, stroke: true), W, H), b = render(layer(W, H, ring, stroke: false), W, H)
    let off = zip(a, b).filter { abs($0 - $1) > 64 }.count
    // depth of the deeper corner at the middle: its first cubic at (w/2)/r radii from the corner
    let along = (w / 2) / r
    let depth = along >= 1.52866483 ? 0 : { () -> CGFloat in
        var lo: CGFloat = 0, hi: CGFloat = 1
        func x(_ t: CGFloat) -> CGFloat { let u = 1 - t; return u*u*u*K[0] + 3*u*u*t*K[1] + 3*u*t*t*K[2] + t*t*t*K[3] }
        for _ in 0..<60 { let m = (lo + hi) / 2; if x(m) > along { lo = m } else { hi = m } }
        return lo * lo * lo * K[4] * r
    }()
    print(String(format: "%4.0fx%4.0f r=%5.2f  stroke vs ring: %4d px off by >64; edge below the side at the middle %.3fpt", w, h, r, off, depth))
}
