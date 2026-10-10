// screenrec <out.mov> <tail-seconds> <flagfile> <code:mods:gap_ms>...: records the main display (everything on it,
// rini's overlay and the real windows) through an SCStream at 60fps into an H.264 movie while posting the presses,
// tagged with eventSourceUserData 0x52494e49 as rec.swift's are. mods is any of a (Option), s (Shift), c (Control),
// m (Command), or - for none. Writes <out.mov>.json: each press's time, each frame the movie holds and each frame the
// writer was not ready for, in ms since the first frame.
// Stops early, exit 3, once <flagfile> exists (inputwatch writes it on real input). SCALE=1 records at points, 2
// (default) at pixels.
import AVFoundation
import Cocoa
import ScreenCaptureKit

let MAGIC: Int64 = 0x52494e49
let args = CommandLine.arguments
guard args.count >= 4 else { print("usage: screenrec <out.mov> <tail-s> <flagfile> <code:mods:gap_ms>..."); exit(2) }
let out = args[1], tail = Double(args[2])!, flag = args[3]
let scale = Int(ProcessInfo.processInfo.environment["SCALE"] ?? "2") ?? 2
var presses: [(CGKeyCode, CGEventFlags, UInt32)] = []
for spec in args.dropFirst(4) {
    let p = spec.split(separator: ":", omittingEmptySubsequences: false)
    guard p.count == 3, let code = UInt16(p[0]), let gap = UInt32(p[2]) else { print("bad press \(spec)"); exit(2) }
    var f: CGEventFlags = []
    for ch in p[1] {
        switch ch {
        case "a": f.insert(.maskAlternate)
        case "s": f.insert(.maskShift)
        case "c": f.insert(.maskControl)
        case "m": f.insert(.maskCommand)
        default: break
        }
    }
    presses.append((code, f, gap))
}
var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func ns(_ ticks: UInt64) -> UInt64 { ticks * UInt64(timebase.numer) / UInt64(timebase.denom) }
func nowNs() -> UInt64 { ns(mach_absolute_time()) }

final class Sink: NSObject, SCStreamOutput {
    var writer: AVAssetWriter?
    var input: AVAssetWriterInput?
    var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    var first: CMTime?
    var firstNs: UInt64 = 0
    var frames: [Double] = []
    var dropped: [Double] = []
    let lock = NSLock()
    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let px = CMSampleBufferGetImageBuffer(sb) else { return }
        guard let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let st = att.first?[.status] as? Int, st == SCFrameStatus.complete.rawValue,
              let disp = att.first?[.displayTime] as? UInt64 else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sb)
        lock.lock(); defer { lock.unlock() }
        if writer == nil {
            let w = try! AVAssetWriter(outputURL: URL(fileURLWithPath: out), fileType: .mov)
            let settings: [String: Any] = [AVVideoCodecKey: AVVideoCodecType.h264,
                                           AVVideoWidthKey: CVPixelBufferGetWidth(px), AVVideoHeightKey: CVPixelBufferGetHeight(px),
                                           AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 60_000_000]]
            let i = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
            i.expectsMediaDataInRealTime = true
            w.add(i)
            adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: i, sourcePixelBufferAttributes: nil)
            w.startWriting(); w.startSession(atSourceTime: pts)
            writer = w; input = i; first = pts; firstNs = ns(disp)
        }
        let ms = Double(ns(disp) - firstNs) / 1e6
        if input!.isReadyForMoreMediaData, adaptor!.append(px, withPresentationTime: pts) {
            frames.append(ms)
        } else {
            dropped.append(ms)
        }
    }
}
let sink = Sink()
let sem = DispatchSemaphore(value: 0)
var stream: SCStream?
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, err in
    guard let content = content, let d = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) else {
        print("no display \(String(describing: err))"); exit(2)
    }
    let f = SCContentFilter(display: d, excludingWindows: [])
    let c = SCStreamConfiguration()
    c.width = d.width * scale; c.height = d.height * scale
    c.minimumFrameInterval = CMTime(value: 1, timescale: 60); c.queueDepth = 8; c.showsCursor = false
    c.pixelFormat = kCVPixelFormatType_32BGRA
    let s = SCStream(filter: f, configuration: c, delegate: nil)
    try! s.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "f", qos: .userInteractive))
    s.startCapture { e in if let e = e { print("start failed \(e)"); exit(2) }; stream = s; sem.signal() }
}
sem.wait()
while sink.lock.withLock({ sink.writer == nil }) { usleep(2000) }
if let w = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 58, keyDown: false) {
    w.type = .flagsChanged; w.flags = []; w.setIntegerValueField(.eventSourceUserData, value: MAGIC); w.post(tap: .cghidEventTap)
}
usleep(400_000)
var aborted = false
var pressLog: [String] = []
for (code, mods, gap) in presses {
    if FileManager.default.fileExists(atPath: flag) { aborted = true; break }
    let src = CGEventSource(stateID: .hidSystemState)
    let d = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!; d.flags = mods
    let u = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!; u.flags = mods
    d.setIntegerValueField(.eventSourceUserData, value: MAGIC); u.setIntegerValueField(.eventSourceUserData, value: MAGIC)
    let t = nowNs()
    d.post(tap: .cghidEventTap); usleep(12000); u.post(tap: .cghidEventTap)
    pressLog.append(String(format: "{\"code\":%d,\"mods\":%llu,\"ms\":%.1f}", code, mods.rawValue,
                           Double(Int64(t) - Int64(sink.firstNs)) / 1e6))
    usleep(gap * 1000)
}
if !aborted { usleep(UInt32(tail * 1e6)) }
if FileManager.default.fileExists(atPath: flag) { aborted = true }
let done = DispatchSemaphore(value: 0)
stream?.stopCapture { _ in done.signal() }; done.wait()
sink.lock.lock()
sink.input?.markAsFinished()
let fin = DispatchSemaphore(value: 0)
sink.writer?.finishWriting { fin.signal() }
sink.lock.unlock()
fin.wait()
func list(_ ms: [Double]) -> String { ms.map { String(format: "%.1f", $0) }.joined(separator: ",") }
try! ("{\"scale\":\(scale),\"t0_ns\":\(sink.firstNs),\"presses\":[\(pressLog.joined(separator: ","))],"
      + "\"frames_ms\":[\(list(sink.frames))],\"dropped_ms\":[\(list(sink.dropped))]}\n")
    .write(toFile: out + ".json", atomically: true, encoding: .utf8)
print("frames \(sink.frames.count) dropped \(sink.dropped.count)\(aborted ? " ABORTED" : "")")
exit(aborted ? 3 : 0)
