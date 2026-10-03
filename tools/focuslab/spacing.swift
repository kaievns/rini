import Foundation
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
for gap in [0, 5, 10, 20, 35, 60, 120, 300, 800] {
  var ts: [Double] = []
  for _ in 0..<10 { usleep(UInt32(gap * 1000)); let t = now(); _ = ids.withUnsafeBufferPointer { cap(cid, $0.baseAddress!, Int32(ids.count), (1 << 11) | (1 << 8)) }?.takeRetainedValue(); ts.append(now() - t) }
  ts.sort(); print("gap \(gap)ms".padding(toLength: 12, withPad: " ", startingAt: 0), "p50", String(format: "%5.1f", ts[5]), " p10", String(format: "%5.1f", ts[1]), " p90", String(format: "%5.1f", ts[8]))
}
