// keys <code> <count> <gap_ms> [code2 count2 gap2 ...]: Ctrl+key presses; aborts on foreign input.
import Cocoa
func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
var lastPost = nowNs()
func hidIdle() -> Double {
    let s = Process(); s.launchPath = "/usr/sbin/ioreg"; s.arguments = ["-c", "IOHIDSystem", "-d", "4"]
    let p = Pipe(); s.standardOutput = p; s.launch()
    let out = String(data: p.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    s.waitUntilExit()
    for l in out.split(separator: "\n") where l.contains("HIDIdleTime") { return (Double(l.split(separator: " ").last!) ?? -1) / 1e9 }
    return -1
}
func press(_ code: CGKeyCode) {
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = .maskControl
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = .maskControl
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap); lastPost = nowNs()
}
var a = Array(CommandLine.arguments.dropFirst())
while a.count >= 3 {
    let code = CGKeyCode(a[0])!, n = Int(a[1])!, gap = UInt32(a[2])!
    a.removeFirst(3)
    let since = Double(nowNs() - lastPost) / 1e9
    if hidIdle() + 0.3 < min(since, 30) { print("ABORT user active"); exit(3) }
    for _ in 0..<n { press(code); usleep(gap * 1000) }
}
