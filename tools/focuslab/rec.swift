// rec <rini-pid> <out.jsonl> <tail_s> <flagfile> <x0,y0,x1,y1> <code gap_ms>...
// Records rini's overlay window alone (the pid's window at level 18, SCStream, 60fps, point size, display P3 BGRA)
// while posting Option+<code> presses tagged with eventSourceUserData 0x52494e49. Writes one JSON line per press and
// per sample: a frame's displayTime on the uptime clock (ns, the journal's clock), its status, the share of a 64x40
// grid with alpha > 0, a signature of that grid's colour (equal signatures: an unchanged picture), and the pane box
// (overlay points x0..x1, y0..y1, every 2nd pixel) mean RGB, chroma and alpha. No picture is written anywhere.
// Before each press, a present <flagfile> (real input) stops the run: exit 3.
import Cocoa
import ScreenCaptureKit
let MAGIC: Int64 = 0x52494e49
var tb = mach_timebase_info_data_t(); mach_timebase_info(&tb)
func machNs(_ t: UInt64) -> UInt64 { t * UInt64(tb.numer) / UInt64(tb.denom) }
func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
var args = Array(CommandLine.arguments.dropFirst())
let pid = Int32(args[0])!, outPath = args[1], tail = Double(args[2])!, flag = args[3]
let pane = args[4].split(separator: ",").map { Int($0)! }
precondition(pane.count == 4, "pane is x0,y0,x1,y1")
args.removeFirst(5)
var presses: [(CGKeyCode, UInt32)] = []
var i = 0; while i + 1 < args.count { presses.append((CGKeyCode(args[i])!, UInt32(args[i+1])!)); i += 2 }
FileManager.default.createFile(atPath: outPath, contents: nil)
let out = FileHandle(forWritingAtPath: outPath)!
let outQ = DispatchQueue(label: "out")
func emit(_ s: String) { outQ.async { out.write((s + "\n").data(using: .utf8)!) } }
final class Sink: NSObject, SCStreamOutput {
    var n = 0
    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        guard let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = att.first, let st = info[.status] as? Int else { return }
        let arr = nowNs()
        let disp = (info[.displayTime] as? UInt64).map(machNs) ?? 0
        guard st == SCFrameStatus.complete.rawValue, let px = CMSampleBufferGetImageBuffer(sb) else {
            emit("{\"type\":\"sample\",\"status\":\(st),\"t_ns\":\(disp),\"arr_ns\":\(arr)}"); return
        }
        CVPixelBufferLockBaseAddress(px, .readOnly)
        let w = CVPixelBufferGetWidth(px), h = CVPixelBufferGetHeight(px), bpr = CVPixelBufferGetBytesPerRow(px)
        let base = CVPixelBufferGetBaseAddress(px)!.assumingMemoryBound(to: UInt8.self)
        var vis = 0, tot = 0, asum = 0.0, sig = 0.0
        for gy in 0..<40 { for gx in 0..<64 {
            let x = (gx * 2 + 1) * w / 128, y = (gy * 2 + 1) * h / 80, o = y * bpr + x * 4
            let a = base[o + 3]; tot += 1; asum += Double(a); if a > 0 { vis += 1 }
            sig += Double(base[o]) + Double(base[o + 1]) * 3 + Double(base[o + 2]) * 7 } }
        var r = 0.0, g = 0.0, b = 0.0, c = 0.0, pa = 0.0, pn = 0.0
        var y = pane[1]; while y < min(h, pane[3]) { var x = pane[0]; while x < min(w, pane[2]) {
            let o = y * bpr + x * 4; let bb = Double(base[o]), gg = Double(base[o+1]), rr = Double(base[o+2])
            r += rr; g += gg; b += bb; pa += Double(base[o+3]); c += max(rr, gg, bb) - min(rr, gg, bb); pn += 1; x += 2 }; y += 2 }
        CVPixelBufferUnlockBaseAddress(px, .readOnly)
        pn = max(pn, 1)
        emit(String(format: "{\"type\":\"frame\",\"status\":%d,\"t_ns\":%llu,\"arr_ns\":%llu,\"w\":%d,\"h\":%d,\"vis\":%.4f,\"alpha\":%.1f,\"sig\":%.0f,\"pane\":[%.2f,%.2f,%.2f,%.2f,%.1f]}",
                     st, disp, arr, w, h, Double(vis) / Double(tot), asum / Double(tot), sig, r / pn, g / pn, b / pn, c / pn, pa / pn))
        n += 1
    }
}
let sink = Sink()
let sem = DispatchSemaphore(value: 0)
var stream: SCStream?
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
    guard let content = content else { print("no content \(String(describing: err))"); exit(2) }
    let wins = content.windows.filter { $0.owningApplication?.processID == pid && $0.windowLayer == 18 && $0.frame.height > 500 }
    guard let w = wins.first else { print("no overlay window"); exit(2) }
    let f = SCContentFilter(desktopIndependentWindow: w)
    let c = SCStreamConfiguration()
    c.width = Int(w.frame.width); c.height = Int(w.frame.height)
    c.minimumFrameInterval = CMTime(value: 1, timescale: 60); c.queueDepth = 8; c.showsCursor = false
    c.pixelFormat = kCVPixelFormatType_32BGRA; c.colorSpaceName = CGColorSpace.displayP3
    emit(String(format: "{\"type\":\"window\",\"id\":%u,\"frame\":[%.0f,%.0f,%.0f,%.0f]}", w.windowID, w.frame.minX, w.frame.minY, w.frame.width, w.frame.height))
    let s = SCStream(filter: f, configuration: c, delegate: nil)
    try! s.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "f", qos: .userInteractive))
    s.startCapture { e in if let e = e { print("start failed \(e)"); exit(2) }; stream = s; sem.signal() }
}
sem.wait()
// The first event a process posts reaches rini 20-40ms late (measured 2026-10-08); a released Option, tagged, warms it.
if let w = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 58, keyDown: false) {
    w.type = .flagsChanged; w.flags = []; w.setIntegerValueField(.eventSourceUserData, value: MAGIC); w.post(tap: .cghidEventTap)
}
usleep(UInt32(ProcessInfo.processInfo.environment["REC_WARM_MS"].flatMap { UInt32($0) } ?? 400) * 1000)
var aborted = false
for (code, gap) in presses {
    if FileManager.default.fileExists(atPath: flag) { aborted = true; break }
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = .maskAlternate
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = .maskAlternate
    d.setIntegerValueField(.eventSourceUserData, value: MAGIC); u.setIntegerValueField(.eventSourceUserData, value: MAGIC)
    let t = nowNs()
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap)
    emit("{\"type\":\"press\",\"code\":\(code),\"t_ns\":\(t)}")
    usleep(gap * 1000)
}
if !aborted { usleep(UInt32(tail * 1e6)) }
let done = DispatchSemaphore(value: 0)
stream?.stopCapture { _ in done.signal() }; done.wait()
outQ.sync {}
print("frames \(sink.n)\(aborted ? " ABORTED" : "")")
exit(aborted ? 3 : 0)
