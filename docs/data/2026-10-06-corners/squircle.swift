import CoreGraphics

/// A rounded rect with continuous corners: per corner a cubic off the straight side, a cubic through
/// the diagonal, and a cubic back onto the next side, starting 1.52866483 × r from the corner.
/// Coefficients: UIKit's continuous rounded rect, as reverse-engineered for iOS 7.
let K: [CGFloat] = [1.52866483, 1.08849323, 0.86840689, 0.63149399, 0.07491100, 0.37282392, 0.18603501]

func continuousPath(_ rect: CGRect, _ radius: CGFloat) -> CGPath {
    let p = CGMutablePath()
    // Core Animation limits a continuous corner's reach to half the side.
    let r = min(radius, min(rect.width, rect.height) / 2 / K[0])
    let (x0, y0, x1, y1) = (rect.minX, rect.minY, rect.maxX, rect.maxY)
    // Each corner in a frame: origin at the corner, u along the incoming side towards it, v along the outgoing.
    func corner(_ c: CGPoint, _ u: CGPoint, _ v: CGPoint) {
        func at(_ a: CGFloat, _ b: CGFloat) -> CGPoint { CGPoint(x: c.x + (a * u.x + b * v.x) * r, y: c.y + (a * u.y + b * v.y) * r) }
        p.addLine(to: at(K[0], 0))
        p.addCurve(to: at(K[3], K[4]), control1: at(K[1], 0), control2: at(K[2], 0))
        p.addCurve(to: at(K[4], K[3]), control1: at(K[5], K[6]), control2: at(K[6], K[5]))
        p.addCurve(to: at(0, K[0]), control1: at(0, K[2]), control2: at(0, K[1]))
    }
    p.move(to: CGPoint(x: x0 + K[0] * r, y: y0))
    corner(CGPoint(x: x1, y: y0), CGPoint(x: -1, y: 0), CGPoint(x: 0, y: 1))
    corner(CGPoint(x: x1, y: y1), CGPoint(x: 0, y: -1), CGPoint(x: -1, y: 0))
    corner(CGPoint(x: x0, y: y1), CGPoint(x: 1, y: 0), CGPoint(x: 0, y: -1))
    corner(CGPoint(x: x0, y: y0), CGPoint(x: 0, y: 1), CGPoint(x: 1, y: 0))
    p.closeSubpath()
    return p
}
