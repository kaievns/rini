// inputwatch <flagfile> [logfile]: listen-only tap over keyboard, mouse and scroll events at the HID tap. Any event not
// tagged with the battery's userData (0x52494e49) and not a zero-delta mouse move (a cursor warp) is real input: write a
// line naming it to <flagfile> and exit 3. With [logfile], every event seen is logged (tagged or not), for calibration.
import Cocoa
let MAGIC: Int64 = 0x52494e49
let args = CommandLine.arguments
let flag = args[1]
let logPath = args.count > 2 ? args[2] : nil
var logH: FileHandle? = nil
if let p = logPath { FileManager.default.createFile(atPath: p, contents: nil); logH = FileHandle(forWritingAtPath: p) }
func nowMs() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let types: [CGEventType] = [.keyDown, .keyUp, .flagsChanged, .mouseMoved, .leftMouseDown, .leftMouseUp, .rightMouseDown,
    .rightMouseUp, .otherMouseDown, .leftMouseDragged, .rightMouseDragged, .scrollWheel]
var mask: CGEventMask = 0
for t in types { mask |= CGEventMask(1) << CGEventMask(t.rawValue) }
let cb: CGEventTapCallBack = { _, type, ev, _ in
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput { return Unmanaged.passUnretained(ev) }
    let tag = ev.getIntegerValueField(.eventSourceUserData)
    let dx = ev.getIntegerValueField(.mouseEventDeltaX), dy = ev.getIntegerValueField(.mouseEventDeltaY)
    let pid = ev.getIntegerValueField(.eventSourceUnixProcessID)
    let line = String(format: "%.1f type=%d tag=%lld dx=%lld dy=%lld srcpid=%lld key=%lld\n", nowMs(), type.rawValue, tag, dx, dy, pid,
                      ev.getIntegerValueField(.keyboardEventKeycode))
    logH?.write(line.data(using: .utf8)!)
    let ours = tag == MAGIC
    let warp = type == .mouseMoved && dx == 0 && dy == 0
    if !ours && !warp {
        FileManager.default.createFile(atPath: flag, contents: String(format: "real input at %.1f: type=%d tag=%lld dx=%lld dy=%lld srcpid=%lld\n", nowMs(), type.rawValue, tag, dx, dy, pid).data(using: .utf8))
        exit(3)
    }
    return Unmanaged.passUnretained(ev)
}
guard let tap = CGEvent.tapCreate(tap: .cghidEventTap, place: .headInsertEventTap, options: .listenOnly,
                                  eventsOfInterest: mask, callback: cb, userInfo: nil) else {
    print("inputwatch: no tap (Input Monitoring / Accessibility?)"); exit(2)
}
let src = CFMachPortCreateRunLoopSource(nil, tap, 0)
CFRunLoopAddSource(CFRunLoopGetCurrent(), src, .commonModes)
CGEvent.tapEnable(tap: tap, enable: true)
print("inputwatch: listening"); fflush(stdout)
CFRunLoopRun()
