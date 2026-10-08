import QuartzCore
import CoreGraphics
import Foundation
func render(_ layer: CALayer, _ w: Int, _ h: Int, _ space: CGColorSpace) -> [UInt8] {
    let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w*4, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(h)); ctx.scaleBy(x: 1, y: -1)
    layer.isGeometryFlipped = true
    layer.render(in: ctx)
    let p = ctx.data!.bindMemory(to: UInt8.self, capacity: w*h*4)
    return Array(UnsafeBufferPointer(start: p, count: w*h*4))
}
func px(_ d: [UInt8], _ w: Int, _ x: Int, _ y: Int) -> [UInt8] { let i = (y*w+x)*4; return Array(d[i..<i+4]) }
let spaces: [(String, CGColorSpace)] = [("srgb", CGColorSpace(name: CGColorSpace.sRGB)!), ("device", CGColorSpaceCreateDeviceRGB()), ("p3", CGColorSpace(name: CGColorSpace.displayP3)!), ("linear", CGColorSpace(name: CGColorSpace.linearSRGB)!)]
for (name, space) in spaces {
  let g = CAGradientLayer(); g.frame = CGRect(x: 0, y: 0, width: 4, height: 256)
  g.colors = [CGColor(srgbRed: 1, green: 0, blue: 0, alpha: 1), CGColor(srgbRed: 0, green: 0, blue: 1, alpha: 1)]
  g.startPoint = CGPoint(x: 0.5, y: 0); g.endPoint = CGPoint(x: 0.5, y: 1)
  let d = render(g, 4, 256, space)
  print(name, "rows 0 64 128 192 255:", [0,64,128,192,255].map { px(d,4,1,$0) })
  // alpha: red opaque -> blue transparent
  g.colors = [CGColor(srgbRed: 1, green: 0, blue: 0, alpha: 1), CGColor(srgbRed: 0, green: 0, blue: 1, alpha: 0)]
  let a = render(g, 4, 256, space)
  print(name, " alpha rows 0 128 255:", [0,128,255].map { px(a,4,1,$0) })
  // black -> white
  g.colors = [CGColor(srgbRed: 0, green: 0, blue: 0, alpha: 1), CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1)]
  let b = render(g, 4, 256, space)
  print(name, " bw rows 0 64 128 192 255:", [0,64,128,192,255].map { px(b,4,1,$0) })
}
