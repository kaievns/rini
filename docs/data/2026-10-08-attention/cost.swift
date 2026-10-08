// P14, the cost of a sweep from a thread without AppKit, a sweep each 250ms: swiftc -O cost.swift && ./cost
import Foundation
let h = dlopen("/System/Library/Frameworks/CoreServices.framework/CoreServices", RTLD_NOW)
typealias ArrT = @convention(c) (Int32, UInt32) -> Unmanaged<CFArray>?
typealias InfoT = @convention(c) (Int32, CFTypeRef, CFArray?) -> Unmanaged<CFDictionary>?
let info = unsafeBitCast(dlsym(h, "_LSCopyApplicationInformation"), to: InfoT.self)
typealias ItemT = @convention(c) (Int32, CFTypeRef, CFString) -> Unmanaged<CFTypeRef>?
let arr = unsafeBitCast(dlsym(h, "_LSCopyApplicationArrayInFrontToBackOrder"), to: ArrT.self)
let item = unsafeBitCast(dlsym(h, "_LSCopyApplicationInformationItem"), to: ItemT.self)
func measure(_ label: String, _ body: () -> Void) {
  var costs: [Double] = []
  for _ in 0..<41 { let t0 = DispatchTime.now().uptimeNanoseconds; body(); costs.append(Double(DispatchTime.now().uptimeNanoseconds - t0) / 1e6); usleep(250_000) }
  costs.sort(); print(label, "p50", costs[20], "p90", costs[36], "max", costs[40])
}
let a = arr(-2, 0)!.takeRetainedValue() as [AnyObject]
measure("array only") { _ = arr(-2, 0)?.takeRetainedValue() }
measure("item per app, cached array") { for asn in a { _ = item(-2, asn as CFTypeRef, "LSWantsAttention" as CFString)?.takeRetainedValue() } }
measure("array + item per app") { for asn in arr(-2, 0)!.takeRetainedValue() as [AnyObject] { _ = item(-2, asn as CFTypeRef, "LSWantsAttention" as CFString)?.takeRetainedValue() } }
let keys = ["pid", "LSWantsAttention"] as CFArray
measure("array + information, two keys, per app (what rini reads)") { for asn in arr(-2, 0)!.takeRetainedValue() as [AnyObject] { _ = info(-2, asn as CFTypeRef, keys)?.takeRetainedValue() } }
