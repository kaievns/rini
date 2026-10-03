import Foundation
import CoreGraphics
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
for spec in CommandLine.arguments.dropFirst() {
  let ids = spec.split(separator: ",").map { UInt32($0)! }
  for (label, opt) in [("2x", UInt32((1 << 11) | (1 << 8))), ("1x", UInt32((1 << 11) | (1 << 8) | (1 << 9)))] {
    var cold: [Double] = [], warm: [Double] = []
    var px = 0
    for _ in 0..<8 {
      usleep(400000)
      for k in 0..<3 {
        let t = now(); let a = ids.withUnsafeBufferPointer { cap(cid, $0.baseAddress!, Int32(ids.count), opt) }?.takeRetainedValue(); let d = now() - t
        if let i = (a as? [CGImage])?.first { px = i.width * i.height }
        if k == 0 { cold.append(d) } else { warm.append(d) }
      }
    }
    cold.sort(); warm.sort()
    print(spec.padding(toLength: 10, withPad: " ", startingAt: 0), label, String(format: "%5.1f Mpx  cold p50 %5.1f   warm p50 %5.1f", Double(px) / 1e6, cold[4], warm[8]))
  }
}
