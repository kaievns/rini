// P14, the read half (no window, no event, no bounce): does _LSCopyApplicationInformationItem read what lsappinfo reads, and what does a sweep cost? swiftc -O read.swift && ./read
import Cocoa
let h = dlopen(nil, RTLD_NOW)
typealias ASNT = @convention(c) (CFAllocator?, Int32) -> Unmanaged<CFTypeRef>?
typealias ItemT = @convention(c) (Int32, CFTypeRef, CFString) -> Unmanaged<CFTypeRef>?
typealias InfoT = @convention(c) (Int32, CFTypeRef, CFArray?) -> Unmanaged<CFDictionary>?
for s in ["_LSASNCreateWithPid", "_LSCopyApplicationInformationItem", "_LSCopyApplicationInformation", "_kLSWantsAttentionKey", "_LSScheduleNotificationOnQueueWithBlock", "_LSModifyNotification", "_LSUnscheduleNotificationFunction", "_LSCopyApplicationArrayInFrontToBackOrder", "_LSGetCurrentApplicationASN"] {
  print(s, dlsym(h, s) != nil)
}
if let k = dlsym(h, "_kLSWantsAttentionKey") { let v = k.assumingMemoryBound(to: CFString?.self).pointee; print("key constant =", v as Any) }
let asnOf = unsafeBitCast(dlsym(h, "_LSASNCreateWithPid"), to: ASNT.self)
let item = unsafeBitCast(dlsym(h, "_LSCopyApplicationInformationItem"), to: ItemT.self)
let info = unsafeBitCast(dlsym(h, "_LSCopyApplicationInformation"), to: InfoT.self)
let apps = NSWorkspace.shared.runningApplications.filter { $0.activationPolicy == .regular }
print("regular apps", apps.count)
var keyName = "LSWantsAttention" as CFString
var t0 = DispatchTime.now().uptimeNanoseconds
var values: [(Int32, String)] = []
for a in apps {
  guard let asn = asnOf(nil, a.processIdentifier)?.takeRetainedValue() else { values.append((a.processIdentifier, "noasn")); continue }
  let v = item(-2, asn, keyName)?.takeRetainedValue()
  values.append((a.processIdentifier, v.map { "\($0)" } ?? "nil"))
}
var t1 = DispatchTime.now().uptimeNanoseconds
print("one sweep ms", Double(t1 - t0) / 1e6)
// 20 more sweeps for cost
var costs: [Double] = []
for _ in 0..<20 {
  t0 = DispatchTime.now().uptimeNanoseconds
  for a in apps { if let asn = asnOf(nil, a.processIdentifier)?.takeRetainedValue() { _ = item(-2, asn, keyName)?.takeRetainedValue() } }
  t1 = DispatchTime.now().uptimeNanoseconds
  costs.append(Double(t1 - t0) / 1e6)
}
costs.sort(); print("sweep ms p50", costs[10], "max", costs[19], "per app us", costs[10] * 1000 / Double(apps.count))
// what rini reads: _LSCopyApplicationInformation over every ASN, two keys at once
typealias ArrT = @convention(c) (Int32, UInt32) -> Unmanaged<CFArray>?
let arr = unsafeBitCast(dlsym(h, "_LSCopyApplicationArrayInFrontToBackOrder"), to: ArrT.self)
var both: [Int32: String] = [:]
for asn in arr(-2, 0)!.takeRetainedValue() as [AnyObject] {
  let d = info(-2, asn as CFTypeRef, ["pid", "LSWantsAttention"] as CFArray)?.takeRetainedValue() as? [String: Any]
  if let pid = d?["pid"] as? Int32 { both[pid] = d?["LSWantsAttention"].map { "\($0)" } ?? "nil" }
}
print("pid item information")
for (pid, v) in values { print(pid, v, both[pid] ?? "unlisted") }
// what the full info dictionary names, for one app
if let a = apps.first, let asn = asnOf(nil, a.processIdentifier)?.takeRetainedValue(), let d = info(-2, asn, nil)?.takeRetainedValue() as? [String: Any] {
  print("keys of", a.localizedName ?? "", d.keys.sorted().joined(separator: ","))
}
// the array call's second argument
for n: UInt32 in [0, 1, 2, 3, 4, 8, 0x8000_0000, 0xFFFF_FFFF] {
  print("second argument", n, "lists", (arr(-2, n)?.takeRetainedValue() as? [AnyObject])?.count ?? -1)
}
