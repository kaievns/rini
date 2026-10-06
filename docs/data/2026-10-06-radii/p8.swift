import AppKit
import QuartzCore

func alphaCorner(_ rep: NSBitmapImageRep, n: Int, corner: Int) -> [Double] {
    // corner 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right; returned oriented as top-left
    let w = rep.pixelsWide, h = rep.pixelsHigh
    var out = [Double](repeating: 0, count: n*n)
    var p = [Int](repeating: 0, count: 4)
    for y in 0..<n { for x in 0..<n {
        let sx = (corner == 1 || corner == 3) ? w-1-x : x
        let sy = (corner >= 2) ? h-1-y : y
        rep.getPixel(&p, atX: sx, y: sy)
        out[y*n+x] = Double(p[3])/255.0
    }}
    return out
}

func renderCorner(wpt: CGFloat, hpt: CGFloat, r: CGFloat, curve: CALayerCornerCurve, n: Int) -> [Double] {
    let layer = CALayer()
    layer.bounds = CGRect(x: 0, y: 0, width: wpt, height: hpt)
    layer.position = CGPoint(x: wpt/2, y: hpt/2)
    layer.backgroundColor = CGColor(red: 1, green: 1, blue: 1, alpha: 1)
    layer.cornerRadius = r
    layer.cornerCurve = curve
    layer.masksToBounds = true
    let cs = CGColorSpaceCreateDeviceRGB()
    let ctx = CGContext(data: nil, width: n, height: n, bitsPerComponent: 8, bytesPerRow: n*4, space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: 2, y: 2)
    ctx.translateBy(x: 0, y: -(hpt - CGFloat(n)/2))
    layer.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: n*n*4)
    var out = [Double](repeating: 0, count: n*n)
    for i in 0..<(n*n) { out[i] = Double(d[i*4+3])/255.0 }
    return out
}

func err(_ a: [Double], _ b: [Double], _ n: Int, _ rpx: Int) -> (Double, Double) {
    // mean abs and max abs diff over the corner box rpx*2 x rpx*2 (curve region incl. continuous extension)
    var s = 0.0, m = 0.0, c = 0.0
    let lim = min(n, rpx)
    for y in 0..<lim { for x in 0..<lim {
        let e = abs(a[y*n+x]-b[y*n+x]); s += e; m = max(m, e); c += 1 }}
    return (s/c, m)
}

let args = CommandLine.arguments.dropFirst()
let probeSelf = renderCorner(wpt: 400, hpt: 400, r: 16, curve: .circular, n: 96)
let probeCont = renderCorner(wpt: 400, hpt: 400, r: 16, curve: .continuous, n: 96)
print("renderInContext distinguishes curves: diff", err(probeSelf, probeCont, 96, 96))
for path in args {
    let parts = path.split(separator: ":")
    let file = String(parts[0]); let srvR = Double(parts[1])!
    let rep = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: file)))!
    let wpt = CGFloat(rep.pixelsWide)/2, hpt = CGFloat(rep.pixelsHigh)/2
    let n = 128
    for corner in [0, 1, 2, 3] {
        let a = alphaCorner(rep, n: n, corner: corner)
        var best: [String: (Double, Double, Double)] = [:]
        var atSrv: [String: (Double, Double)] = [:]
        for curve in [CALayerCornerCurve.circular, .continuous] {
            var r = 6.0
            while r <= 40.0 {
                let b = renderCorner(wpt: wpt, hpt: hpt, r: CGFloat(r), curve: curve, n: n)
                let e = err(a, b, n, Int(srvR*2*1.6)+4)
                if best[curve.rawValue] == nil || e.0 < best[curve.rawValue]!.1 { best[curve.rawValue] = (r, e.0, e.1) }
                if abs(r - srvR) < 0.01 { atSrv[curve.rawValue] = e }
                r += 0.25
            }
        }
        let c = best["circular"]!, k = best["continuous"]!
        let cs = atSrv["circular"]!, ks = atSrv["continuous"]!
        print(String(format: "%@ srv=%.0f corner=%d | circular best r=%.2f mean=%.4f max=%.3f at srv mean=%.4f max=%.3f | continuous best r=%.2f mean=%.4f max=%.3f at srv mean=%.4f max=%.3f",
            (file as NSString).lastPathComponent, srvR, corner, c.0, c.1, c.2, cs.0, cs.1, k.0, k.1, k.2, ks.0, ks.1))
    }
}
