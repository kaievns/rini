import QuartzCore
import AppKit
// The middle cubic's control c against Core Animation's continuous corner, on a 200x200 layer at 8x: per
// device column from the diagonal out, how far the path's edge sits from Core Animation's, in 8x px,
// beside how far Core Animation's circular clip sits from Core Graphics' circular fill.
func render(_ l: CALayer, _ w: CGFloat, _ h: CGFloat, _ scale: CGFloat) -> [Int] {
    let pw = Int((w * scale).rounded(.up)), ph = Int((h * scale).rounded(.up))
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph)); ctx.scaleBy(x: scale, y: -scale); l.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4)
    return (0..<(pw * ph)).map { Int(d[$0 * 4 + 3]) }
}
func ca(_ w: CGFloat, _ h: CGFloat, _ r: CGFloat, _ c: CALayerCornerCurve, _ s: CGFloat) -> [Int] { let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.backgroundColor = .white; l.cornerRadius = r; l.cornerCurve = c; l.masksToBounds = true; return render(l, w, h, s) }
func shape(_ w: CGFloat, _ h: CGFloat, _ p: CGPath, _ s: CGFloat) -> [Int] { let l = CAShapeLayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.fillColor = .white; l.path = p; return render(l, w, h, s) }
func path(_ rect: CGRect, _ r: CGFloat, _ c: CGFloat) -> CGPath {
    let pts: [(CGFloat, CGFloat)] = [(1.528665,0),(1.08849323,0),(0.86840689,0),(0.63149399,0.07491100),(0.37282392,c),(c,0.37282392),(0.07491100,0.63149399),(0,0.86840689),(0,1.08849323),(0,1.528665)]
    let p = CGMutablePath()
    let (x0, y0, x1, y1) = (rect.minX, rect.minY, rect.maxX, rect.maxY)
    func corner(_ cc: CGPoint, _ u: CGPoint, _ v: CGPoint) {
        func at(_ q: (CGFloat, CGFloat)) -> CGPoint { CGPoint(x: cc.x + (q.0 * u.x + q.1 * v.x) * r, y: cc.y + (q.0 * u.y + q.1 * v.y) * r) }
        p.addLine(to: at(pts[0]))
        for i in [1, 4, 7] { p.addCurve(to: at(pts[i+2]), control1: at(pts[i]), control2: at(pts[i+1])) }
    }
    p.move(to: CGPoint(x: x0 + pts[0].0 * r, y: y0))
    corner(CGPoint(x: x1, y: y0), CGPoint(x: -1, y: 0), CGPoint(x: 0, y: 1))
    corner(CGPoint(x: x1, y: y1), CGPoint(x: 0, y: -1), CGPoint(x: -1, y: 0))
    corner(CGPoint(x: x0, y: y1), CGPoint(x: 1, y: 0), CGPoint(x: 0, y: -1))
    corner(CGPoint(x: x0, y: y0), CGPoint(x: 0, y: 1), CGPoint(x: 1, y: 0))
    p.closeSubpath(); return p
}
// per column from the top-left corner, how many device px are clear in the top box (depth of the edge), at scale s
func depth(_ a: [Int], _ w: CGFloat, _ s: CGFloat, _ box: Int) -> [Double] {
  let pw = Int(w * s)
  return (0..<box).map { x in (0..<box).reduce(0.0) { $0 + Double(255 - a[$1 * pw + x]) / 255 } }
}
let w: CGFloat = 200, h: CGFloat = 200, s: CGFloat = 8
for r in [CGFloat(12), 16, 26, 40] {
  let rect = CGRect(x: 0, y: 0, width: w, height: h)
  let box = Int(r * s * 1.6)
  // only columns from the diagonal outwards (x from 0.2929r.. to 1.6r) where the edge is shallow enough for columns
  let from = Int(r * s * 0.30)
  let dc = depth(ca(w,h,r,.continuous,s), w, s, box), dci = depth(ca(w,h,r,.circular,s), w, s, box), dg = depth(shape(w,h,CGPath(roundedRect: rect, cornerWidth: r, cornerHeight: r, transform: nil),s), w, s, box)
  let bias = (from..<box).map { abs(dci[$0] - dg[$0]) }.max()!
  var line = String(format: "r=%2.0f  CA circ vs CG circ max %.3f px(8x) |", r, bias)
  for c in [0.16905899, 0.170, 0.172, 0.175, 0.18603501] {
    let dp = depth(shape(w, h, path(rect, r, CGFloat(c)), s), w, s, box)
    let e = (from..<box).map { abs(dc[$0] - dp[$0]) }.max()!
    line += String(format: " c=%.4f max %.3f", c, e)
  }
  print(line)
}
