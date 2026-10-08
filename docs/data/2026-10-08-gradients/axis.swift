import QuartzCore
import CoreGraphics
func render(_ layer: CALayer, _ w: Int, _ h: Int) -> [UInt8] {
    let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w*4, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(h)); ctx.scaleBy(x: 1, y: -1)
    layer.isGeometryFlipped = true
    layer.render(in: ctx)
    let p = ctx.data!.bindMemory(to: UInt8.self, capacity: w*h*4)
    return Array(UnsafeBufferPointer(start: p, count: w*h*4))
}
func px(_ d: [UInt8], _ w: Int, _ x: Int, _ y: Int) -> UInt8 { d[(y*w+x)*4] }
let g = CAGradientLayer(); g.frame = CGRect(x: 0, y: 0, width: 200, height: 100)
g.colors = [CGColor(gray: 0, alpha: 1), CGColor(gray: 1, alpha: 1)]
g.startPoint = .zero; g.endPoint = CGPoint(x: 1, y: 1)
let d = render(g, 200, 100)
print("top-right", px(d,200,199,0), "bottom-left", px(d,200,0,99), "centre", px(d,200,100,50), "tl", px(d,200,0,0), "br", px(d,200,199,99))
// also ramp along the first row
print("row0", stride(from: 0, to: 200, by: 25).map { px(d,200,$0,0) })
