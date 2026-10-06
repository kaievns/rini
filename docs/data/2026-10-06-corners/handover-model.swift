// Offscreen model of the handover metric (src/pictures/domain/handover.rs) at a window's corners:
// the "screen" is a real window capture (alpha corners, screencapture -o) over a flat wallpaper with
// a hairline drawn along the window's own continuous corner; the "overlay" is what rini's tile
// draws: the picture (baked and clipped, or alpha), the white line and the outline, at a radius and
// curve. No shadow on either side. Rendered by Core Animation into bitmaps, no window.
import AppKit
import QuartzCore

let SCALE: CGFloat = 2
let MARGIN: CGFloat = 40

struct Scene { let w: CGFloat; let h: CGFloat; let image: CGImage }

func load(_ path: String) -> Scene {
    let rep = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: path)))!
    return Scene(w: CGFloat(rep.pixelsWide) / SCALE, h: CGFloat(rep.pixelsHigh) / SCALE, image: rep.cgImage!)
}

func colour(_ g: (CGFloat, CGFloat, CGFloat), _ a: CGFloat = 1) -> CGColor { CGColor(red: g.0, green: g.1, blue: g.2, alpha: a) }

enum Picture { case alpha, baked(clip: CGFloat, curve: CALayerCornerCurve) }
enum Outline { case circularPath, continuousPath, borderLayer }

struct Dress {
    let picture: Picture
    let hairline: CGFloat
    let curve: CALayerCornerCurve
    let outline: Outline
}

/// A continuous rounded rect as Core Animation draws it, approximated by rendering: not available as
/// a CGPath on macOS, so the outline of the "continuous" dress is a border layer on a rect half a
/// point out, which renders exactly on a bitmap at 2x.
func outlineLayer(_ s: Scene, corner: CGFloat, curve: CALayerCornerCurve, kind: Outline) -> CALayer {
    switch kind {
    case .circularPath, .continuousPath:
        let l = CAShapeLayer()
        l.frame = CGRect(x: MARGIN, y: MARGIN, width: s.w, height: s.h)
        let r = CGRect(x: -0.25, y: -0.25, width: s.w + 0.5, height: s.h + 0.5)
        l.path = kind == .circularPath
            ? CGPath(roundedRect: r, cornerWidth: corner + 0.25, cornerHeight: corner + 0.25, transform: nil)
            : continuousPath(r, corner + 0.25)
        l.fillColor = nil
        l.strokeColor = colour((0, 0, 0), 0.9)
        l.lineWidth = 0.5
        return l
    case .borderLayer:
        let l = CALayer()
        l.frame = CGRect(x: MARGIN - 0.5, y: MARGIN - 0.5, width: s.w + 1, height: s.h + 1)
        l.borderWidth = 0.5
        l.borderColor = colour((0, 0, 0), 0.9)
        l.cornerRadius = corner + 0.5
        l.cornerCurve = curve
        return l
    }
}

func render(_ s: Scene, wallpaper: (CGFloat, CGFloat, CGFloat), real: Bool, dress: Dress?) -> [UInt8] {
    let cw = s.w + 2 * MARGIN, ch = s.h + 2 * MARGIN
    let root = CALayer()
    root.frame = CGRect(x: 0, y: 0, width: cw, height: ch)
    root.backgroundColor = colour(wallpaper)
    root.isGeometryFlipped = true
    let pic = CALayer()
    pic.frame = CGRect(x: MARGIN, y: MARGIN, width: s.w, height: s.h)
    pic.contents = s.image
    pic.contentsGravity = .resize
    if real {
        root.addSublayer(pic)
        let white = CALayer()
        white.frame = pic.frame
        white.borderWidth = 1
        white.borderColor = colour((1, 1, 1), 0.2)
        white.cornerRadius = realRadius
        white.cornerCurve = .continuous
        root.addSublayer(white)
        root.addSublayer(outlineLayer(s, corner: realRadius, curve: .continuous, kind: .borderLayer))
    } else if let d = dress {
        switch d.picture {
        case .alpha:
            root.addSublayer(pic)
        case let .baked(clip, curve):
            // The baked picture carries the wallpaper in its corners: the window over the desktop.
            let holder = CALayer()
            holder.frame = pic.frame
            holder.backgroundColor = colour(wallpaper)
            holder.masksToBounds = true
            holder.cornerRadius = clip
            holder.cornerCurve = curve
            pic.frame = holder.bounds
            holder.addSublayer(pic)
            root.addSublayer(holder)
        }
        let white = CALayer()
        white.frame = CGRect(x: MARGIN, y: MARGIN, width: s.w, height: s.h)
        white.borderWidth = 1
        white.borderColor = colour((1, 1, 1), 0.2)
        white.cornerRadius = d.hairline
        white.cornerCurve = d.curve
        root.addSublayer(white)
        root.addSublayer(outlineLayer(s, corner: d.hairline, curve: d.curve, kind: d.outline))
    }
    let pw = Int(cw * SCALE), ph = Int(ch * SCALE)
    let ctx = CGContext(data: nil, width: pw, height: ph, bitsPerComponent: 8, bytesPerRow: pw * 4,
                        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.translateBy(x: 0, y: CGFloat(ph))
    ctx.scaleBy(x: SCALE, y: -SCALE)
    root.render(in: ctx)
    // Bitmap memory is top row first for a CGBitmapContext.
    return Array(UnsafeBufferPointer(start: ctx.data!.bindMemory(to: UInt8.self, capacity: pw * ph * 4), count: pw * ph * 4))
}

var realRadius: CGFloat = 26

// --- the handover metric, ported from src/pictures/domain/handover.rs ---
struct Diff { var interior = 0.0, over = 0.0, edgeIn = 0.0, edgeOut = 0.0
    var cornerIn = 0.0, cornerOut = 0.0, straightIn = 0.0, straightOut = 0.0, nIn = 0, nCornerIn = 0 }

func compare(_ a: [UInt8], _ b: [UInt8], _ s: Scene, cornerZone: Double) -> Diff {
    let pw = Int((s.w + 2 * MARGIN) * SCALE)
    func px(_ img: [UInt8], _ x: Double, _ y: Double) -> [Double] {
        let c = Int((x * Double(SCALE)).rounded(.down)), r = Int((y * Double(SCALE)).rounded(.down))
        let i = (r * pw + c) * 4
        return [Double(img[i]), Double(img[i + 1]), Double(img[i + 2])]
    }
    func d(_ x: Double, _ y: Double) -> Double {
        let p = px(a, x, y), q = px(b, x, y)
        return (abs(p[0] - q[0]) + abs(p[1] - q[1]) + abs(p[2] - q[2])) / 3
    }
    let left = Double(MARGIN), top = Double(MARGIN), right = left + Double(s.w), bottom = top + Double(s.h)
    var r = Diff()
    var n = 0, over = 0, sum = 0.0
    var y = top + 4
    while y < bottom - 4 { var x = left + 4
        while x < right - 4 { let v = d(x, y); sum += v; n += 1; if v > 12 { over += 1 }; x += 1.5 }
        y += 1.5 }
    r.interior = sum / Double(n); r.over = Double(over) / Double(n)
    var si = 0.0, so = 0.0, ni = 0, no = 0, ci = 0.0, co = 0.0, nci = 0, nco = 0
    for across in [-0.75, -0.25, 0.25, 0.75] {
        func add(_ v: Double, _ along: Double, _ length: Double) {
            let inCorner = min(along, length - along) < cornerZone
            if across > 0 { si += v; ni += 1; if inCorner { ci += v; nci += 1 } }
            else { so += v; no += 1; if inCorner { co += v; nco += 1 } }
        }
        var x = left + 16
        while x < right - 16 { for yy in [top + across, bottom - across] { add(d(x, yy), x - left, Double(s.w)) }; x += 2 }
        var yy = top + 16
        while yy < bottom - 16 { for xx in [left + across, right - across] { add(d(xx, yy), yy - top, Double(s.h)) }; yy += 2 }
    }
    r.edgeIn = si / Double(ni); r.edgeOut = so / Double(no)
    r.cornerIn = ci; r.cornerOut = co; r.nIn = ni; r.nCornerIn = nci
    r.straightIn = (si - ci) / Double(ni - nci); r.straightOut = (so - co) / Double(no - nco)
    _ = nco
    return r
}

let args = CommandLine.arguments
let scene = load(args[1])
realRadius = CGFloat(Double(args[2])!)
let walls: [(String, (CGFloat, CGFloat, CGFloat))] = [("dark wallpaper", (0.10, 0.12, 0.18)), ("mid wallpaper", (0.45, 0.42, 0.40))]
let r = realRadius
let dresses: [(String, Dress)] = [
    ("baked, circular r+1 (pre-R2, measured)", Dress(picture: .baked(clip: r + 1, curve: .circular), hairline: r + 1, curve: .circular, outline: .circularPath)),
    ("baked, circular 10 (pre-R2, unmeasured)", Dress(picture: .baked(clip: 10, curve: .circular), hairline: 10, curve: .circular, outline: .circularPath)),
    ("baked, circular r (R2)", Dress(picture: .baked(clip: r, curve: .circular), hairline: r, curve: .circular, outline: .circularPath)),
    ("alpha, circular r (R2)", Dress(picture: .alpha, hairline: r, curve: .circular, outline: .circularPath)),
    ("baked, continuous r, path (proposed)", Dress(picture: .baked(clip: r, curve: .continuous), hairline: r, curve: .continuous, outline: .continuousPath)),
    ("alpha, continuous r, path (proposed)", Dress(picture: .alpha, hairline: r, curve: .continuous, outline: .continuousPath)),
    ("baked, continuous r, border model", Dress(picture: .baked(clip: r, curve: .continuous), hairline: r, curve: .continuous, outline: .borderLayer)),
]
print(String(format: "%@ %.0fx%.0f, window radius %.0f (continuous)", (args[1] as NSString).lastPathComponent, scene.w, scene.h, r))
for (wname, wall) in walls {
    let screen = render(scene, wallpaper: wall, real: true, dress: nil)
    print("  \(wname)")
    print("    dress                                       interior  over     edge_in  edge_out | in: corner-zone share  straight-only in/out")
    for (name, dr) in dresses {
        let overlay = render(scene, wallpaper: wall, real: false, dress: dr)
        let x = compare(overlay, screen, scene, cornerZone: 40)
        print(String(format: "    %-42@ %8.3f %7.5f %8.3f %8.3f | %5.1f%% of edge_in, %d/%d samples  %6.3f / %6.3f",
                     name as NSString, x.interior, x.over, x.edgeIn, x.edgeOut,
                     x.edgeIn > 0 ? 100 * x.cornerIn / (x.edgeIn * Double(x.nIn)) : 0, x.nCornerIn, x.nIn, x.straightIn, x.straightOut))
    }
}
