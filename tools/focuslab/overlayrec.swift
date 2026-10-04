// overlayrec <rini-pid> <outdir> <seconds> <code gap_ms>...: records rini's overlay window alone (SCStream, 60fps,
// half size) while posting Ctrl+<code> presses; each frame saved as <outdir>/<ms since first press>.png.
import Cocoa
import ScreenCaptureKit
import CoreImage
func nowMs() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let args = CommandLine.arguments
let pid = Int32(args[1])!, out = args[2], secs = Double(args[3])!
var presses: [(CGKeyCode, UInt32)] = []
var i = 4; while i + 1 < args.count { presses.append((CGKeyCode(args[i])!, UInt32(args[i+1])!)); i += 2 }
try? FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)
var t0 = 0.0
final class Sink: NSObject, SCStreamOutput {
    let ctx = CIContext()
    var n = 0
    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, t0 > 0, let px = CMSampleBufferGetImageBuffer(sb) else { return }
        guard let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let st = att.first?[.status] as? Int, st == SCFrameStatus.complete.rawValue else { return }
        let t = nowMs() - t0
        let img = CIImage(cvPixelBuffer: px)
        let url = URL(fileURLWithPath: String(format: "%@/%07.1f.png", out, t))
        try? ctx.writePNGRepresentation(of: img, to: url, format: .RGBA8, colorSpace: CGColorSpaceCreateDeviceRGB())
        n += 1
    }
}
let sink = Sink()
let sem = DispatchSemaphore(value: 0)
var stream: SCStream?
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
    guard let content = content else { print("no content \(String(describing: err))"); exit(2) }
    let wins = content.windows.filter { $0.owningApplication?.processID == pid && $0.frame.height > 500 }
    guard let w = wins.first else { print("no overlay window"); exit(2) }
    let f = SCContentFilter(desktopIndependentWindow: w)
    let c = SCStreamConfiguration()
    c.width = Int(w.frame.width); c.height = Int(w.frame.height)
    c.minimumFrameInterval = CMTime(value: 1, timescale: 60); c.queueDepth = 8; c.showsCursor = false
    let s = SCStream(filter: f, configuration: c, delegate: nil)
    try! s.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "f"))
    s.startCapture { e in if let e = e { print("start failed \(e)"); exit(2) }; stream = s; sem.signal() }
}
sem.wait()
usleep(300_000)
t0 = nowMs()
for (code, gap) in presses {
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = .maskControl
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = .maskControl
    print(String(format: "press %d at %.1f", code, nowMs() - t0))
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap)
    usleep(gap * 1000)
}
usleep(UInt32(secs * 1e6))
let done = DispatchSemaphore(value: 0)
stream?.stopCapture { _ in done.signal() }; done.wait()
print("frames \(sink.n)")
