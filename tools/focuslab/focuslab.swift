// focuslab: times focus changes and window placement from outside rini, on one clock.
// Clock: mach absolute time in ns (DispatchTime.uptimeNanoseconds); SCStream displayTime converted.
import Cocoa
import ScreenCaptureKit
import CoreMedia
import ApplicationServices

var tb = mach_timebase_info_data_t()
mach_timebase_info(&tb)
func ticksToNs(_ t: UInt64) -> UInt64 { t * UInt64(tb.numer) / UInt64(tb.denom) }
func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW)
typealias MainConnT = @convention(c) () -> Int32
typealias BoundsT = @convention(c) (Int32, UInt32, UnsafeMutablePointer<CGRect>) -> Int32
typealias FrontT = @convention(c) (UnsafeMutablePointer<ProcessSerialNumber>) -> Int32
typealias SetFrontT = @convention(c) (UnsafePointer<ProcessSerialNumber>, UInt32, UInt32) -> Int32
typealias PostRecT = @convention(c) (UnsafePointer<ProcessSerialNumber>, UnsafePointer<UInt8>) -> Int32
typealias CapT = @convention(c) (Int32, UnsafePointer<UInt32>, Int32, UInt32) -> Unmanaged<CFArray>?
let mainConn = unsafeBitCast(dlsym(sky, "SLSMainConnectionID"), to: MainConnT.self)
let getBounds = unsafeBitCast(dlsym(sky, "SLSGetWindowBounds"), to: BoundsT.self)
let getFront = unsafeBitCast(dlsym(sky, "_SLPSGetFrontProcess"), to: FrontT.self)
let setFront = unsafeBitCast(dlsym(sky, "_SLPSSetFrontProcessWithOptions"), to: SetFrontT.self)
let postRec = unsafeBitCast(dlsym(sky, "SLPSPostEventRecordTo"), to: PostRecT.self)
let capList = unsafeBitCast(dlsym(sky, "SLSHWCaptureWindowList"), to: CapT.self)
typealias KeyFocusT = @convention(c) (UnsafeMutablePointer<ProcessSerialNumber>, UnsafeMutablePointer<UInt8>) -> Int32
let getKeyFocus = unsafeBitCast(dlsym(sky, "SLPSGetKeyFocusProcess"), to: KeyFocusT.self)
let cid = mainConn()

typealias PsnForPidT = @convention(c) (pid_t, UnsafeMutablePointer<ProcessSerialNumber>) -> Int32
typealias PidForPsnT = @convention(c) (UnsafePointer<ProcessSerialNumber>, UnsafeMutablePointer<pid_t>) -> Int32
let hi = dlopen("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices", RTLD_NOW)
let psnForPid = unsafeBitCast(dlsym(hi, "GetProcessForPID"), to: PsnForPidT.self)
let pidForPsn = unsafeBitCast(dlsym(hi, "GetProcessPID"), to: PidForPsnT.self)
func psn(_ pid: pid_t) -> ProcessSerialNumber {
    var p = ProcessSerialNumber()
    _ = psnForPid(pid, &p)
    return p
}
func frontPid() -> pid_t {
    var p = ProcessSerialNumber()
    _ = getFront(&p)
    var pid: pid_t = 0
    _ = pidForPsn(&p, &pid)
    return pid
}
func bounds(_ wid: UInt32) -> CGRect {
    var r = CGRect.zero
    _ = getBounds(cid, wid, &r)
    return r
}
func makeKey(_ pid: pid_t, _ wid: UInt32) {
    var e1 = [UInt8](repeating: 0, count: 0x100)
    e1[0x04] = 0xf8; e1[0x08] = 0x01; e1[0x3a] = 0x10
    withUnsafeBytes(of: wid.littleEndian) { for i in 0..<4 { e1[0x3c + i] = $0[i] } }
    for i in 0x20..<0x30 { e1[i] = 0xff }
    var e2 = e1; e2[0x08] = 0x02
    var p = psn(pid)
    _ = setFront(&p, wid, 0x200)
    _ = postRec(&p, e1)
    _ = postRec(&p, e2)
}
func axWindow(_ pid: pid_t, _ wid: UInt32) -> AXUIElement? {
    typealias GetWidT = @convention(c) (AXUIElement, UnsafeMutablePointer<UInt32>) -> AXError
    let f = unsafeBitCast(dlsym(dlopen(nil, RTLD_NOW), "_AXUIElementGetWindow"), to: GetWidT.self)
    let app = AXUIElementCreateApplication(pid)
    var v: CFTypeRef?
    AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &v)
    for w in (v as? [AXUIElement]) ?? [] {
        var id: UInt32 = 0
        if f(w, &id) == .success && id == wid { return w }
    }
    return nil
}

// ---- idle guard: the posted events reset HIDIdleTime, so compare against our own last post.
var lastPostNs: UInt64 = 0
func hidIdle() -> Double {
    let s = Process(); s.launchPath = "/usr/sbin/ioreg"; s.arguments = ["-c", "IOHIDSystem", "-d", "4"]
    let p = Pipe(); s.standardOutput = p; s.launch()
    let out = String(data: p.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    s.waitUntilExit()
    for l in out.split(separator: "\n") where l.contains("HIDIdleTime") { return Double(l.split(separator: " ").last!) ?? -1 }
    return -1
}
func userActive() -> Bool {
    let idle = hidIdle() / 1e9
    let sincePost = Double(nowNs() - lastPostNs) / 1e9
    return idle + 0.3 < min(sincePost, 30)
}

// ---- actions
func postKey(_ code: CGKeyCode, ctrl: Bool) {
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!
    if ctrl { d.flags = .maskControl; u.flags = .maskControl }
    lastPostNs = nowNs()
    d.post(tap: .cghidEventTap)
    usleep(15000)
    u.post(tap: .cghidEventTap)
    lastPostNs = nowNs()
}
func postClick(_ at: CGPoint) {
    let d = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: at, mouseButton: .left)!
    let u = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: at, mouseButton: .left)!
    lastPostNs = nowNs()
    d.post(tap: .cghidEventTap)
    usleep(20000)
    u.post(tap: .cghidEventTap)
    lastPostNs = nowNs()
}

// ---- screen observer: a band of the display, every changed frame with its display time.
struct Frame { var t: UInt64; var px: [UInt8]; var w: Int; var h: Int }
final class Observer: NSObject, SCStreamOutput {
    let lock = NSLock()
    var frames: [Frame] = []
    var stream: SCStream?
    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let a = atts.first,
              let raw = a[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let dt = a[.displayTime] as? UInt64,
              let pb = CMSampleBufferGetImageBuffer(sb) else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb), bpr = CVPixelBufferGetBytesPerRow(pb)
        let base = CVPixelBufferGetBaseAddress(pb)!.assumingMemoryBound(to: UInt8.self)
        var px = [UInt8](repeating: 0, count: w * h * 4)
        for y in 0..<h { memcpy(&px[y * w * 4], base + y * bpr, w * 4) }
        CVPixelBufferUnlockBaseAddress(pb, .readOnly)
        lock.lock(); frames.append(Frame(t: ticksToNs(dt), px: px, w: w, h: h)); if frames.count > 400 { frames.removeFirst(200) }; lock.unlock()
    }
    func start(band: CGRect) {
        let sem = DispatchSemaphore(value: 0)
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { c, _ in
            let d = c!.displays.first!
            let f = SCContentFilter(display: d, excludingWindows: [])
            let cfg = SCStreamConfiguration()
            cfg.sourceRect = band
            cfg.width = Int(band.width); cfg.height = Int(band.height)
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: 120)
            cfg.queueDepth = 6
            cfg.showsCursor = false
            cfg.pixelFormat = kCVPixelFormatType_32BGRA
            let s = SCStream(filter: f, configuration: cfg, delegate: nil)
            try! s.addStreamOutput(self, type: .screen, sampleHandlerQueue: DispatchQueue(label: "obs", qos: .userInteractive))
            s.startCapture { e in if let e { print("stream error", e) }; sem.signal() }
            self.stream = s
        }
        sem.wait()
    }
    func snapshot() -> [Frame] { lock.lock(); defer { lock.unlock() }; return frames }
}

// Mean abs difference of region [x0,x1) of two frames, every 2nd pixel.
func diff(_ a: Frame, _ b: Frame, _ x0: Int, _ x1: Int) -> Double {
    var s = 0, n = 0
    for y in stride(from: 0, to: min(a.h, b.h), by: 2) {
        for x in stride(from: max(0, x0), to: min(x1, a.w), by: 2) {
            let i = (y * a.w + x) * 4
            s += abs(Int(a.px[i]) - Int(b.px[i])) + abs(Int(a.px[i + 1]) - Int(b.px[i + 1])) + abs(Int(a.px[i + 2]) - Int(b.px[i + 2]))
            n += 3
        }
    }
    return n > 0 ? Double(s) / Double(n) : 0
}

// ---- background SkyLight load, to test contention
var loadIds: [UInt32] = []
var loadOn = false
var loadCalls = 0
func loadLoop() {
    Thread.detachNewThread {
        while true {
            if loadOn && !loadIds.isEmpty {
                _ = loadIds.withUnsafeBufferPointer { capList(cid, $0.baseAddress!, Int32(loadIds.count), (1 << 11) | (1 << 8)) }?.takeRetainedValue()
                loadCalls += 1
            } else { usleep(1000) }
        }
    }
}

// ---- poller: the window server's front process and watched windows' bounds, every 1ms.
final class Poller {
    let lock = NSLock()
    var front: [(UInt64, pid_t)] = []
    var keyp: [(UInt64, pid_t)] = []
    var top: [(UInt64, UInt32)] = []
    var bounds: [UInt32: [(UInt64, CGRect)]] = [:]
    var watch: [UInt32] = []
    func start() {
        let t = Thread {
            var lastFront: pid_t = -1
            var lastKey: pid_t = -1
            var lastTop: UInt32 = 0
            var tick = 0
            var lastB: [UInt32: CGRect] = [:]
            while true {
                let n = nowNs()
                let f = frontPid()
                self.lock.lock()
                if f != lastFront { self.front.append((n, f)); lastFront = f; if self.front.count > 2000 { self.front.removeFirst(1000) } }
                var kp = ProcessSerialNumber(); var fb: UInt8 = 0; _ = getKeyFocus(&kp, &fb)
                var kpid: pid_t = 0; _ = pidForPsn(&kp, &kpid)
                if kpid != lastKey { self.keyp.append((n, kpid)); lastKey = kpid }
                tick += 1
                if tick % 2 == 0, let l = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], 0) as? [[String: Any]],
                   let first = l.first(where: { ($0["kCGWindowLayer"] as? Int) == 0 }), let num = first["kCGWindowNumber"] as? UInt32 {
                    if num != lastTop { self.top.append((nowNs(), num)); lastTop = num }
                }
                for w in self.watch {
                    let b = focuslab.bounds(w)
                    if lastB[w] != b { self.bounds[w, default: []].append((n, b)); lastB[w] = b }
                }
                self.lock.unlock()
                usleep(1000)
            }
        }
        t.qualityOfService = .userInteractive
        t.start()
    }
    func frontAfter(_ t0: UInt64, _ pid: pid_t) -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        return front.first { $0.0 > t0 && $0.1 == pid }?.0 ?? 0
    }
    func keyAfter(_ t0: UInt64, _ pid: pid_t) -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        return keyp.first { $0.0 > t0 && $0.1 == pid }?.0 ?? 0
    }
    func topAfter(_ t0: UInt64, _ w: UInt32) -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        return top.first { $0.0 > t0 && $0.1 == w }?.0 ?? 0
    }
    func boundsAfter(_ t0: UInt64, _ w: UInt32) -> [(UInt64, CGRect)] {
        lock.lock(); defer { lock.unlock() }
        return (bounds[w] ?? []).filter { $0.0 > t0 }
    }
}
let poller = Poller()

// ---- one trial
struct Target { let pid: pid_t; let wid: UInt32; let click: CGPoint; let region: (Int, Int) }
func trial(mode: String, from: Target, to: Target, key: CGKeyCode, obs: Observer, band: CGRect, out: FileHandle, label: String, watchMs: UInt64) {
    // Settled: no changed frame for 300ms.
    var waited = 0
    while waited < 3000 {
        let fr = obs.snapshot()
        if let last = fr.last, Int64(nowNs()) - Int64(last.t) > 300_000_000 { break }
        usleep(20000); waited += 20
    }
    let base = obs.snapshot().last
    let b0 = bounds(to.wid)
    let front0 = frontPid()
    let t0 = nowNs()
    var tAct: UInt64 = 0
    switch mode {
    case "click": postClick(to.click)
    case "key": postKey(key, ctrl: true)
    case "makekey": makeKey(to.pid, to.wid); tAct = nowNs(); if let w = axWindow(to.pid, to.wid) { AXUIElementPerformAction(w, kAXRaiseAction as CFString) }
    case "makekey_only": makeKey(to.pid, to.wid)
    case "activate": NSRunningApplication(processIdentifier: to.pid)?.activate(options: []); tAct = nowNs(); if let w = axWindow(to.pid, to.wid) { AXUIElementPerformAction(w, kAXRaiseAction as CFString) }
    default: break
    }
    let tCall = nowNs()
    while nowNs() - t0 < watchMs * 1_000_000 { usleep(5000) }
    // rini warps the cursor to the focused window, which counts as input.
    lastPostNs = nowNs()
    let tFront = poller.frontAfter(t0, to.pid)
    let tKey = poller.keyAfter(t0, to.pid)
    let tTop = poller.topAfter(t0, to.wid)
    let moves = poller.boundsAfter(t0, to.wid)
    let tMoved = moves.first?.0 ?? 0
    let tStill = moves.last?.0 ?? 0
    let last = moves.last?.1 ?? b0
    let frames = obs.snapshot().filter { $0.t > t0 - 50_000_000 }
    var series: [String] = []
    if let base {
        for f in frames {
            let dTo = diff(f, base, to.region.0, to.region.1)
            let dFrom = diff(f, base, from.region.0, from.region.1)
            series.append(String(format: "[%.1f,%.2f,%.2f]", Double(Int64(f.t) - Int64(t0)) / 1e6, dTo, dFrom))
        }
    }
    func ms(_ t: UInt64) -> String { t == 0 ? "null" : String(format: "%.2f", Double(t - t0) / 1e6) }
    let line = "{\"label\":\"\(label)\",\"mode\":\"\(mode)\",\"to_pid\":\(to.pid),\"to_wid\":\(to.wid),\"front_before\":\(front0),\"t0_ns\":\(t0),\"wall\":\(Date().timeIntervalSince1970),\"call_ms\":\(ms(tCall)),\"act_ms\":\(ms(tAct)),\"front_ms\":\(ms(tFront)),\"key_ms\":\(ms(tKey)),\"top_ms\":\(ms(tTop)),\"moved_ms\":\(ms(tMoved)),\"still_ms\":\(ms(tStill)),\"b0\":[\(b0.origin.x),\(b0.origin.y),\(b0.width),\(b0.height)],\"b1\":[\(last.origin.x),\(last.origin.y),\(last.width),\(last.height)],\"load_calls\":\(loadCalls),\"frames\":[\(series.joined(separator: ","))]}\n"
    out.write(line.data(using: .utf8)!)
}

// ---- main
// focuslab <out.jsonl> <label> <mode> <n> <watch_ms> <load:0|1> <bandY> <bandH> A=pid,wid,clickx,clicky,x0,x1[,key] B=...
let args = CommandLine.arguments
let outPath = args[1], label = args[2], mode = args[3], n = Int(args[4])!, watch = UInt64(args[5])!, load = args[6] == "1"
let bandY = Double(args[7])!, bandH = Double(args[8])!
func parse(_ s: String) -> (Target, CGKeyCode) {
    let p = s.split(separator: "=")[1].split(separator: ",").map { Double($0)! }
    return (Target(pid: pid_t(p[0]), wid: UInt32(p[1]), click: CGPoint(x: p[2], y: p[3]), region: (Int(p[4]), Int(p[5]))), p.count > 6 ? CGKeyCode(p[6]) : 0)
}
let (A, keyToA) = parse(args[9]), (B, keyToB) = parse(args[10])
if args.count > 11 { loadIds = args[11].split(separator: ",").map { UInt32($0)! } }
loadOn = load
loadLoop()
poller.watch = [A.wid, B.wid] + (args.count > 12 ? args[12].split(separator: ",").map { UInt32($0)! } : [])
poller.start()
if !FileManager.default.fileExists(atPath: outPath) { FileManager.default.createFile(atPath: outPath, contents: nil) }
let out = FileHandle(forWritingAtPath: outPath)!
out.seekToEndOfFile()
let band = CGRect(x: 0, y: bandY, width: 1728, height: bandH)
FileHandle.standardError.write("starting stream\n".data(using: .utf8)!)
let obs = Observer()
obs.start(band: band)
FileHandle.standardError.write("stream up\n".data(using: .utf8)!)
usleep(500_000)
lastPostNs = nowNs()
if ProcessInfo.processInfo.environment["FOCUSLAB_CONT"] == nil && hidIdle() / 1e9 < 30 { print("user active at start"); exit(2) }
for i in 0..<n {
    if userActive() { print("ABORT user active at trial \(i)"); exit(3) }
    let toB = i % 2 == 0
    let modes = mode.split(separator: ",").map(String.init)
    let m = modes[(i / 2) % modes.count]
    trial(mode: m, from: toB ? A : B, to: toB ? B : A, key: toB ? keyToB : keyToA, obs: obs, band: band, out: out, label: label, watchMs: watch)
    usleep(UInt32(900_000 + (i % 3) * 137_000))
}
print("done \(n)")
