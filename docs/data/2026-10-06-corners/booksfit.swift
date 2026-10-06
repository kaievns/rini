import AppKit
import QuartzCore
func render(_ l: CALayer, _ w: CGFloat, _ h: CGFloat) -> [Double] {
    let pw = Int(w * 2), ph = Int(h * 2)
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph)); ctx.scaleBy(x: 2, y: -2); l.render(in: ctx)
    let d = ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4)
    return (0..<(pw * ph)).map { Double(d[$0 * 4 + 3]) / 255 }
}
for path in CommandLine.arguments.dropFirst() {
    let rep = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: path)))!
    let pw = rep.pixelsWide, ph = rep.pixelsHigh, w = CGFloat(pw) / 2, h = CGFloat(ph) / 2
    var real = [Double](repeating: 0, count: pw * ph); var p = [Int](repeating: 0, count: 4)
    for y in 0..<ph { for x in 0..<pw { rep.getPixel(&p, atX: x, y: y); real[y * pw + x] = Double(p[3]) / 255 } }
    for r in [CGFloat(25.5), 26, 26.25, 26.5, 27] {
        func layer(_ c: CALayerCornerCurve) -> CALayer { let l = CALayer(); l.frame = CGRect(x: 0, y: 0, width: w, height: h); l.isGeometryFlipped = true; l.backgroundColor = .white; l.cornerRadius = r; l.cornerCurve = c; l.masksToBounds = true; return l }
        let s = CAShapeLayer(); s.frame = CGRect(x: 0, y: 0, width: w, height: h); s.isGeometryFlipped = true; s.fillColor = .white; s.path = continuousPath(CGRect(x: 0, y: 0, width: w, height: h), r)
        var line = String(format: "%@ r=%5.2f", (path as NSString).lastPathComponent, r)
        for (name, a) in [("CA continuous", render(layer(.continuous), w, h)), ("CA circular", render(layer(.circular), w, h)), ("hand path", render(s, w, h))] {
            let box = 90; var sum = 0.0, mx = 0.0, n = 0, big = 0
            for (cx, cy) in [(0, 0), (pw - box, 0), (0, ph - box), (pw - box, ph - box)] { for y in cy..<(cy + box) { for x in cx..<(cx + box) { let e = abs(a[y * pw + x] - real[y * pw + x]); sum += e; mx = max(mx, e); n += 1; if e > 0.5 { big += 1 } } } }
            line += String(format: " | %@ mean %.4f max %.3f px>0.5 %d", name, sum / Double(n), mx, big)
        }
        print(line)
    }
}
