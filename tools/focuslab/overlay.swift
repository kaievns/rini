// overlay <out.jsonl> <label> <rini-pid> <code> [code...]: per Option+<code> press, rini's windows and their alpha in the
// window list, polled every 1ms, and one JSON line appended to <out.jsonl>: when a rini window not showing at the press
// came up (up_ms) and went back (lift_ms), ms after the press was posted.
import Cocoa
func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
func ms(since t0: UInt64) -> Double { Double(nowNs() - t0) / 1e6 }
let args = CommandLine.arguments
let outPath = args[1], label = args[2]
let pid = Int32(args[3])!
let codes = args.dropFirst(4).map { CGKeyCode($0)! }
func riniWindows() -> [(Int, CGRect, Double, Int)] {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] else { return [] }
    return list.compactMap { w in
        guard (w[kCGWindowOwnerPID as String] as? Int32) == pid else { return nil }
        let b = w[kCGWindowBounds as String] as! [String: Double]
        let r = CGRect(x: b["X"]!, y: b["Y"]!, width: b["Width"]!, height: b["Height"]!)
        return (w[kCGWindowNumber as String] as! Int, r, w[kCGWindowAlpha as String] as? Double ?? -1, w[kCGWindowLayer as String] as? Int ?? 0)
    }
}
func press(_ code: CGKeyCode) {
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = .maskAlternate
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = .maskAlternate
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap)
}
if !FileManager.default.fileExists(atPath: outPath) { FileManager.default.createFile(atPath: outPath, contents: nil) }
let out = FileHandle(forWritingAtPath: outPath)!
out.seekToEndOfFile()
func json(_ v: Double?) -> String { v.map { String(format: "%.2f", $0) } ?? "null" }
for code in codes {
    let showing = Set(riniWindows().filter { $0.2 > 0 }.map { $0.0 })
    let t0 = nowNs(); press(code)
    var last = ""; var overlay: Int? = nil; var up: Double? = nil; var lift: Double? = nil
    while ms(since: t0) < 1500 {
        let ws = riniWindows()
        let key = ws.map { "\($0.0)@\(Int($0.1.width))x\(Int($0.1.height)) a\($0.2) l\($0.3)" }.joined(separator: ",")
        if key != last { print(String(format: "  +%7.1fms %@", ms(since: t0), key.isEmpty ? "(none)" : key)); last = key }
        if overlay == nil, let w = ws.first(where: { $0.2 > 0 && !showing.contains($0.0) }) { overlay = w.0; up = ms(since: t0) }
        else if let o = overlay, lift == nil, !ws.contains(where: { $0.0 == o && $0.2 > 0 }) { lift = ms(since: t0) }
        usleep(1000)
    }
    print("key \(code): up at \(json(up)), lifted at \(json(lift))")
    out.write("{\"label\":\"\(label)\",\"code\":\(code),\"t0_ns\":\(t0),\"up_ms\":\(json(up)),\"lift_ms\":\(json(lift))}\n".data(using: .utf8)!)
}
