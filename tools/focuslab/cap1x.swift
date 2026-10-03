import Foundation
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
for (label, opt) in [("2x", UInt32((1 << 11) | (1 << 8))), ("1x", UInt32((1 << 11) | (1 << 8) | (1 << 9)))] {
  var ts: [Double] = []
  for _ in 0..<12 { let t = now(); _ = ids.withUnsafeBufferPointer { cap(cid, $0.baseAddress!, Int32(ids.count), opt) }?.takeRetainedValue(); ts.append(now() - t) }
  ts.sort(); print(label, "p50", String(format: "%.1f", ts[6]), "min", String(format: "%.1f", ts[0]), "max", String(format: "%.1f", ts[11]))
}
