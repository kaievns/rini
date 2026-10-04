// overlay <rini-pid> <code> [code...]: per Ctrl+<code> press, rini's windows and their alpha in the window list, polled every 1ms.
import Cocoa
func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let pid = Int32(CommandLine.arguments[1])!
let codes = CommandLine.arguments.dropFirst(2).map { CGKeyCode($0)! }
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
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = .maskControl
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = .maskControl
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap)
}
let base = Set(riniWindows().map { "\($0.0)" })
for code in codes {
    let t0 = now(); press(code)
    var last = ""; var on: Double? = nil
    while now() - t0 < 1500 {
        let ws = riniWindows()
        let key = ws.map { "\($0.0)@\(Int($0.1.width))x\(Int($0.1.height)) a\($0.2) l\($0.3)" }.joined(separator: ",")
        if key != last { print(String(format: "  +%7.1fms %@", now() - t0, key.isEmpty ? "(none)" : key)); last = key; if !ws.isEmpty && on == nil { on = now() - t0 } }
        usleep(1000)
    }
    print("key \(code): first on at \(on.map { String(format: "%.1f", $0) } ?? "never")")
}
