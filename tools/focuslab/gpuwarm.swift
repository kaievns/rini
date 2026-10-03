import Foundation
import Metal
let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConn = @convention(c) () -> Int32
typealias Cap = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let cid = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConn.self)()
let cap = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: Cap.self)
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let ids = CommandLine.arguments.dropFirst().map { UInt32($0)! }
let dev = MTLCreateSystemDefaultDevice()!
let q = dev.makeCommandQueue()!
let src = "kernel void spin(device float *a [[buffer(0)]], uint i [[thread_position_in_grid]]) { float x = a[i]; for (int k = 0; k < 4000; k++) { x = x * 1.0001f + 0.5f; } a[i] = x; }"
let lib = try! dev.makeLibrary(source: src, options: nil)
let pso = try! dev.makeComputePipelineState(function: lib.makeFunction(name: "spin")!)
let buf = dev.makeBuffer(length: 4 * 65536)!
func gpuBurst(_ ms: Double) { let t = now(); while now() - t < ms { let cb = q.makeCommandBuffer()!; let e = cb.makeComputeCommandEncoder()!; e.setComputePipelineState(pso); e.setBuffer(buf, offset: 0, index: 0); e.dispatchThreads(MTLSize(width: 65536, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 256, height: 1, depth: 1)); e.endEncoding(); cb.commit(); cb.waitUntilCompleted() } }
func one() -> Double { let t = now(); _ = ids.withUnsafeBufferPointer { cap(cid, $0.baseAddress!, Int32(ids.count), (1 << 11) | (1 << 8)) }?.takeRetainedValue(); return now() - t }
func run(_ label: String, _ pre: () -> Void) {
  var ts: [Double] = []
  for _ in 0..<10 { usleep(400000); pre(); ts.append(one()) }
  ts.sort(); print(label.padding(toLength: 34, withPad: " ", startingAt: 0), "p50", String(format: "%5.1f", ts[5]), " p10", String(format: "%5.1f", ts[1]), " p90", String(format: "%5.1f", ts[8]))
}
run("cold (400ms gap)") {}
run("after 20ms GPU compute burst") { gpuBurst(20) }
run("after 5ms GPU compute burst") { gpuBurst(5) }
run("after 20ms CPU spin") { let t = now(); var x = 0.0; while now() - t < 20 { x += 1 }; _ = x }
run("cold again") {}
