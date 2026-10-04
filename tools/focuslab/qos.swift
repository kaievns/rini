import Foundation
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
func one() -> Double { let t = now(); _ = ids.withUnsafeBufferPointer { cap(cid, $0.baseAddress!, Int32(ids.count), (1 << 11) | (1 << 8)) }?.takeRetainedValue(); return now() - t }
let classes: [(String, qos_class_t)] = [("default", QOS_CLASS_DEFAULT), ("user-interactive", QOS_CLASS_USER_INTERACTIVE), ("utility", QOS_CLASS_UTILITY), ("user-initiated", QOS_CLASS_USER_INITIATED)]
var res: [String: [Double]] = [:]
for round in 0..<10 {
  for (name, q) in classes {
    usleep(400000)
    var d = 0.0
    let s = DispatchSemaphore(value: 0)
    var attr = pthread_attr_t(); pthread_attr_init(&attr); pthread_attr_set_qos_class_np(&attr, q, 0)
    let box = Unmanaged.passRetained({ d = one(); s.signal() } as AnyObject)
    _ = box
    let th = Thread { d = one(); s.signal() }
    th.qualityOfService = [QOS_CLASS_DEFAULT: QualityOfService.default, QOS_CLASS_USER_INTERACTIVE: .userInteractive, QOS_CLASS_UTILITY: .utility, QOS_CLASS_USER_INITIATED: .userInitiated][q]!
    th.start(); s.wait()
    res[name, default: []].append(d)
    _ = round
  }
}
for (name, _) in classes { let v = res[name]!.sorted(); print(name.padding(toLength: 18, withPad: " ", startingAt: 0), "cold capture p50", String(format: "%5.1f", v[5]), " p10", String(format: "%5.1f", v[1]), " p90", String(format: "%5.1f", v[8])) }
