// What Core Animation draws for the animations the overlay installs, read off a layer frozen at
// chosen media times (speed 0, timeOffset), against the exact curves the score evaluates.
import AppKit
import QuartzCore

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let window = NSWindow(contentRect: NSRect(x: -5000, y: -5000, width: 200, height: 200),
                      styleMask: [.borderless], backing: .buffered, defer: false)
window.alphaValue = 0
let view = NSView(frame: NSRect(x: 0, y: 0, width: 200, height: 200))
view.wantsLayer = true
window.contentView = view
let layer = CALayer()
view.layer!.addSublayer(layer)
layer.speed = 0
CATransaction.flush()

func drawn(_ t: Double) -> CGPoint {
  CATransaction.begin(); CATransaction.setDisableActions(true)
  layer.timeOffset = t
  CATransaction.commit(); CATransaction.flush()
  return layer.presentation()!.position
}
func install(_ model: CGPoint, _ animations: [(String, CAAnimation)]) {
  CATransaction.begin(); CATransaction.setDisableActions(true)
  layer.removeAllAnimations()
  layer.position = model
  for (key, a) in animations { layer.add(a, forKey: key) }
  CATransaction.commit(); CATransaction.flush()
}
func pt(_ x: Double, _ y: Double = 0) -> NSValue { NSValue(point: NSPoint(x: x, y: y)) }
func basic(_ from: NSValue, _ to: NSValue, _ begin: Double, _ seconds: Double,
           _ f: CAMediaTimingFunction, additive: Bool = false) -> CABasicAnimation {
  let a = CABasicAnimation(keyPath: "position")
  a.fromValue = from; a.toValue = to; a.beginTime = begin; a.duration = seconds
  a.timingFunction = f; a.isAdditive = additive
  return a
}

// The exact cubic Bezier timing curve, solved by bisection to machine precision.
func ease(_ t: Double, _ x1: Double, _ y1: Double, _ x2: Double, _ y2: Double) -> Double {
  if t <= 0 { return 0 }
  if t >= 1 { return 1 }
  func c(_ s: Double, _ p1: Double, _ p2: Double) -> Double {
    let i = 1 - s; return 3 * i * i * s * p1 + 3 * i * s * s * p2 + s * s * s
  }
  var (lo, hi) = (0.0, 1.0)
  for _ in 0..<200 { let m = (lo + hi) / 2; if c(m, x1, x2) < t { lo = m } else { hi = m } }
  return c((lo + hi) / 2, y1, y2)
}
func motion(_ t: Double) -> Double { ease(t, 0.16, 1, 0.3, 1) }
func easeInOut(_ t: Double) -> Double { ease(t, 0.42, 0, 0.58, 1) }
let motionCurve = CAMediaTimingFunction(controlPoints: 0.16, 1.0, 0.3, 1.0)
let linear = CAMediaTimingFunction(name: .linear)

var points: [Float] = []
let named = CAMediaTimingFunction(name: .easeInEaseOut)
for i in 0..<4 { var v: [Float] = [0, 0]; named.getControlPoint(at: i, values: &v); points += v }
print("easeInEaseOut control points", points)

// 1722pt over 0.35s on the motion curve, and the 72pt bounce riding it.
let bounce = CAKeyframeAnimation(keyPath: "position")
bounce.values = [pt(0), pt(0, -72), pt(0)]
bounce.keyTimes = [0, 0.35, 1]
bounce.timingFunctions = [motionCurve, CAMediaTimingFunction(name: .easeInEaseOut)]
bounce.isAdditive = true; bounce.beginTime = 1000; bounce.duration = 0.35
install(CGPoint(x: 1722, y: 0), [("move", basic(pt(0), pt(1722), 1000, 0.35, motionCurve)), ("bounce", bounce)])
var (curveGap, bounceGap) = (0.0, 0.0)
for i in 0..<3500 {
  let t = Double(i) / 10000, u = t / 0.35, p = drawn(1000 + t)
  let home = u <= 0.35 ? -72 * motion(u / 0.35) : -72 * (1 - easeInOut((u - 0.35) / 0.65))
  curveGap = max(curveGap, abs(p.x - 1722 * motion(u)))
  bounceGap = max(bounceGap, abs(p.y - home))
}
print(String(format: "1722pt on the motion curve over 0.35s: within %.3fpt of the exact curve", curveGap))
print(String(format: "72pt bounce over 0.35s: within %.3fpt of the exact curves", bounceGap))

// The window: model before begin and from the end on.
install(CGPoint(x: 100, y: 0), [("move", basic(pt(0), pt(100), 10, 1, linear))])
print("before begin", drawn(9.5).x, "at begin", drawn(10).x, "at end", drawn(11).x)

// Keyframes with no keyTimes or timingFunctions.
let even = CAKeyframeAnimation(keyPath: "position")
even.values = [pt(0), pt(100), pt(50), pt(300)]; even.beginTime = 10; even.duration = 1
install(CGPoint(x: 300, y: 0), [("move", even)])
print("evenly spaced:", [1.0 / 12, 5.0 / 12, 0.75].map { drawn(10 + $0).x }, "expect [25, 87.5, 112.5]")

// A zero duration.
install(CGPoint(x: 100, y: 0), [("move", basic(pt(0), pt(100), 10, 0, linear))])
print("duration 0, at +0.1s and +0.25s:", drawn(10.1).x, drawn(10.25).x)

// Order: a movement replaced after an additive animation.
let bump = basic(pt(0, 0), pt(0, 50), 10, 1, linear, additive: true)
install(CGPoint(x: 1000, y: 0), [("move", basic(pt(0), pt(1000), 10, 1, linear)), ("bump", bump)])
print("move then bump, keys", layer.animationKeys() ?? [], "at 10.5:", drawn(10.5))
CATransaction.begin(); CATransaction.setDisableActions(true)
layer.position = CGPoint(x: 2000, y: 0)
layer.add(basic(pt(500), pt(2000), 10.5, 1, linear), forKey: "move")
CATransaction.commit(); CATransaction.flush()
print("move replaced, keys", layer.animationKeys() ?? [], "at 10.75:", drawn(10.75), "bump alone would add 37.5")
install(CGPoint(x: 1000, y: 0), [("bump", bump), ("move", basic(pt(0), pt(1000), 10, 1, linear))])
print("bump then move, at 10.5:", drawn(10.5))
