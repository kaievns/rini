// What a frame of the overview's zoom costs with the desktop blurred, rendered off screen.
//
// Builds the overlay's shape at the built-in display's size, 1728x1117pt at 2x: a desktop picture,
// the tint over it and 32 thumbs, 4 rows of 8, each rounded and edged as the card. Renders 240
// frames through CARenderer into a Metal texture, the tint and the blur changing every frame as
// they do along the zoom, and times each frame to its command buffer's completion. Four ways:
// no blur; CAFilter's gaussianBlur on the desktop; a CIGaussianBlur on it; and a picture blurred
// once, faded in over the sharp one.
//
// swiftc -O blur.swift -o blur && ./blur
import CoreGraphics
import CoreImage
import Foundation
import Metal
import QuartzCore

let W = 1728.0, H = 1117.0, S = 2.0
let device = MTLCreateSystemDefaultDevice()!
let queue = device.makeCommandQueue()!
let descriptor = MTLTextureDescriptor.texture2DDescriptor(
  pixelFormat: .bgra8Unorm, width: Int(W * S), height: Int(H * S), mipmapped: false)
descriptor.usage = [.renderTarget, .shaderRead]
descriptor.storageMode = .private
let texture = device.makeTexture(descriptor: descriptor)!

func desktop() -> CGImage {
  let (w, h) = (Int(W * S), Int(H * S))
  let ctx = CGContext(
    data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue)!
  ctx.setFillColor(CGColor(gray: 0.5, alpha: 1))
  ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
  for i in 0..<4000 {
    ctx.setFillColor(
      CGColor(red: CGFloat(i % 7) / 7, green: CGFloat(i % 11) / 11, blue: CGFloat(i % 5) / 5, alpha: 1))
    ctx.fill(CGRect(x: (i * 97) % w, y: (i * 61) % h, width: 4 + (i * 13) % 60, height: 3 + (i * 17) % 40))
  }
  return ctx.makeImage()!
}
let image = desktop()

func filter(_ kind: String) -> AnyObject? {
  switch kind {
  case "CAFilter":
    let type = NSClassFromString("CAFilter") as! NSObject.Type
    let filter = type.perform(NSSelectorFromString("filterWithType:"), with: "gaussianBlur")!
      .takeUnretainedValue() as! NSObject
    filter.setValue(true, forKey: "inputNormalizeEdges")
    return filter
  case "CIFilter":
    let filter = CIFilter(name: "CIGaussianBlur")!
    filter.name = "blur"
    return filter
  default: return nil
  }
}

func run(_ kind: String, radius: Double) -> (Double, Double) {
  CATransaction.begin()
  CATransaction.setDisableActions(true)
  let root = CALayer()
  root.anchorPoint = .zero
  root.frame = CGRect(x: 0, y: 0, width: W, height: H)
  root.transform = CATransform3DMakeScale(S, S, 1)
  let back = CALayer()
  back.anchorPoint = .zero
  back.frame = root.bounds
  back.contents = image
  back.contentsScale = S
  root.addSublayer(back)
  if let filter = filter(kind) { back.filters = [filter] }
  var blurred: CALayer? = nil
  if kind == "pre-blurred" {
    let ci = CIImage(cgImage: image).clampedToExtent().applyingGaussianBlur(sigma: radius * S)
      .cropped(to: CGRect(x: 0, y: 0, width: W * S, height: H * S))
    let layer = CALayer()
    layer.anchorPoint = .zero
    layer.frame = root.bounds
    let began = Date()
    layer.contents = CIContext().createCGImage(ci, from: ci.extent)
    print("  blurring the picture once: \(String(format: "%.1f", Date().timeIntervalSince(began) * 1000))ms")
    root.addSublayer(layer)
    blurred = layer
  }
  let tint = CALayer()
  tint.frame = root.bounds
  tint.backgroundColor = CGColor(gray: 0.06, alpha: 1)
  tint.opacity = 0
  root.addSublayer(tint)
  for r in 0..<4 {
    for c in 0..<8 {
      let thumb = CALayer()
      thumb.frame = CGRect(x: 120 + Double(c) * 190, y: 60 + Double(r) * 260, width: 160, height: 200)
      thumb.contents = image
      thumb.cornerRadius = 11
      thumb.masksToBounds = true
      thumb.borderWidth = 1
      thumb.borderColor = CGColor(gray: 0.9, alpha: 0.16)
      root.addSublayer(thumb)
    }
  }
  let top = CALayer()
  top.frame = CGRect(x: 0, y: 0, width: W * S, height: H * S)
  top.addSublayer(root)
  CATransaction.commit()
  let renderer = CARenderer(mtlTexture: texture, options: [kCARendererMetalCommandQueue as String: queue])
  renderer.layer = top
  renderer.bounds = top.bounds
  var times: [Double] = []
  for frame in 0...240 {
    let share = Double(frame % 60) / 60.0
    CATransaction.begin()
    CATransaction.setDisableActions(true)
    tint.opacity = Float(0.6 * share)
    switch kind {
    case "CAFilter": back.setValue(radius * share, forKeyPath: "filters.gaussianBlur.inputRadius")
    case "CIFilter": back.setValue(radius * share, forKeyPath: "filters.blur.inputRadius")
    default: blurred?.opacity = Float(share)
    }
    CATransaction.commit()
    CATransaction.flush()
    let began = CACurrentMediaTime()
    renderer.beginFrame(atTime: began, timeStamp: nil)
    _ = renderer.updateBounds()
    renderer.addUpdate(renderer.bounds)
    renderer.render()
    renderer.endFrame()
    let done = queue.makeCommandBuffer()!
    done.commit()
    done.waitUntilCompleted()
    if frame > 0 { times.append((CACurrentMediaTime() - began) * 1000) }
  }
  times.sort()
  return (times[times.count / 2], times[Int(Double(times.count) * 0.95)])
}

for (kind, radii) in [("none", [0.0]), ("CAFilter", [4.0, 6.0, 8.0]), ("CIFilter", [6.0]), ("pre-blurred", [6.0])] {
  for radius in radii {
    let (p50, p95) = run(kind, radius: radius)
    print("\(kind) radius \(radius)pt: frame p50 \(String(format: "%.2f", p50))ms p95 \(String(format: "%.2f", p95))ms")
  }
}
