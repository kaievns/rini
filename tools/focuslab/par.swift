import Foundation
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias NewConn = @convention(c) (Int32, UnsafeMutablePointer<Int32>) -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let mainConn = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)
let newConn = unsafeBitCast(dlsym(sky, "SLSNewConnection"), to: NewConn.self)
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
func one(_ c: Int32, _ w: UInt32) -> Double { let t = now(); var w = w; _ = cap(c, &w, 1, (1 << 11) | (1 << 8))?.takeRetainedValue(); return now() - t }
let main = mainConn()
var conns: [Int32] = []
for _ in ids { var c: Int32 = 0; _ = newConn(0, &c); conns.append(c) }
for (i, w) in ids.enumerated() { _ = one(main, w); _ = one(conns[i], w) }
for round in 0..<3 {
  var serial: [Double] = []
  let t0 = now(); for w in ids { serial.append(one(main, w)) }; let st = now() - t0
  // parallel on the main connection
  var par1 = [Double](repeating: 0, count: ids.count); let g1 = DispatchGroup(); let t1 = now()
  for (i, w) in ids.enumerated() { DispatchQueue.global(qos: .userInteractive).async(group: g1) { par1[i] = one(main, w) } }
  g1.wait(); let p1 = now() - t1
  // parallel, own connection each
  var par2 = [Double](repeating: 0, count: ids.count); let g2 = DispatchGroup(); let t2 = now()
  for (i, w) in ids.enumerated() { DispatchQueue.global(qos: .userInteractive).async(group: g2) { par2[i] = one(conns[i], w) } }
  g2.wait(); let p2 = now() - t2
  print(String(format: "round %d  serial total %.1f %@ | parallel main-conn total %.1f %@ | parallel own-conn total %.1f %@", round, st, serial.map { String(format: "%.1f", $0) }.joined(separator: ","), p1, par1.map { String(format: "%.1f", $0) }.joined(separator: ","), p2, par2.map { String(format: "%.1f", $0) }.joined(separator: ",")))
}
