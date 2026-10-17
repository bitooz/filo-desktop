// Records one window of a process at up to 60 frames a second and reports, for each new frame, how much of the
// area below the window's top bar is near-white (each channel 240 or more) and pure white (253 or more: WebKit's
// blank page, which a light theme's off-white background isn't). Used by artifact-check.py to catch a white frame before a page draws.
//
//   swiftc -O scripts/flash-recorder.swift -o target/flash-recorder
//   target/flash-recorder <pid> <window width in points> <output dir> <seconds>
//
// Writes frames.tsv (time, near-white and pure-white fractions, mean RGB) and the frame with the most pure white as
// whitest.png. Needs Screen Recording
// permission for the app running it.
import AppKit
import CoreImage
import Foundation
import ScreenCaptureKit

let arguments = CommandLine.arguments
guard arguments.count == 5, let pid = pid_t(arguments[1]), let width = Double(arguments[2]), let seconds = Double(arguments[4]) else {
    FileHandle.standardError.write("usage: flash-recorder <pid> <width> <output dir> <seconds>\n".data(using: .utf8)!)
    exit(2)
}
// Only where Screen Recording is already allowed: asking would put macOS's prompt on the user's screen, and this runs
// from checks (AGENTS.md).
guard CGPreflightScreenCaptureAccess() else {
    FileHandle.standardError.write("flash-recorder: Screen Recording isn't allowed for the app running this; not asking (AGENTS.md)\n".data(using: .utf8)!)
    exit(3)
}
let output = URL(fileURLWithPath: arguments[3])
// Below the top bar (title and page header), where the page's content draws.
let topInset = 96.0
let windowWidth = width

final class Recorder: NSObject, SCStreamOutput {
    let start = Date()
    var rows: [String] = []
    var whitest: (fraction: Double, image: CGImage?) = (0, nil)
    let context = CIContext()

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let pixels = CMSampleBufferGetImageBuffer(buffer) else { return }
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        let (w, h, stride) = (CVPixelBufferGetWidth(pixels), CVPixelBufferGetHeight(pixels), CVPixelBufferGetBytesPerRow(pixels))
        guard let base = CVPixelBufferGetBaseAddress(pixels)?.assumingMemoryBound(to: UInt8.self) else { return }
        let scale = Double(w) / windowWidth
        let top = Int(topInset * scale)
        var white = 0, pure = 0, total = 0, sum = (0, 0, 0)
        // Every other pixel each way is plenty for a fraction of the area.
        for y in Swift.stride(from: top, to: h, by: 2) {
            let row = base + y * stride
            for x in Swift.stride(from: 0, to: w, by: 2) {
                let pixel = row + x * 4 // BGRA
                let (b, g, r) = (Int(pixel[0]), Int(pixel[1]), Int(pixel[2]))
                if r >= 240 && g >= 240 && b >= 240 { white += 1 }
                if r >= 253 && g >= 253 && b >= 253 { pure += 1 }
                sum = (sum.0 + r, sum.1 + g, sum.2 + b)
                total += 1
            }
        }
        guard total > 0 else { return }
        let fraction = Double(pure) / Double(total)
        let time = Date().timeIntervalSince(start)
        rows.append(String(format: "%.3f\t%.4f\t%.4f\t%d,%d,%d", time, Double(white) / Double(total), fraction, sum.0 / total, sum.1 / total, sum.2 / total))
        if fraction > whitest.fraction || whitest.image == nil {
            let image = CIImage(cvPixelBuffer: pixels)
            whitest = (fraction, context.createCGImage(image, from: image.extent))
        }
    }
}

func save(_ image: CGImage, to url: URL) {
    let rep = NSBitmapImageRep(cgImage: image)
    try? rep.representation(using: .png, properties: [:])?.write(to: url)
}

let recorder = Recorder()
Task {
    do {
        // The window can take a moment to appear.
        var window: SCWindow?
        for _ in 0..<200 {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            window = content.windows.first { $0.owningApplication?.processID == pid && abs($0.frame.width - width) < 2 }
            if window != nil { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        guard let window else { print("no window"); exit(1) }
        let configuration = SCStreamConfiguration()
        configuration.width = Int(window.frame.width * 2)
        configuration.height = Int(window.frame.height * 2)
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.showsCursor = false
        configuration.queueDepth = 8
        let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: window), configuration: configuration, delegate: nil)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: DispatchQueue(label: "frames"))
        try await stream.startCapture()
        print("recording")
        fflush(stdout)
        // Until the time is up or the app quits.
        let end = Date().addingTimeInterval(seconds)
        while Date() < end && kill(pid, 0) == 0 {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        try? await stream.stopCapture()
        try? FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        try ("time\twhite\tpure\tmean\n" + recorder.rows.joined(separator: "\n") + "\n").write(to: output.appendingPathComponent("frames.tsv"), atomically: true, encoding: .utf8)
        if let image = recorder.whitest.image { save(image, to: output.appendingPathComponent("whitest.png")) }
        print(String(format: "frames %d, most pure white %.4f", recorder.rows.count, recorder.whitest.fraction))
        exit(0)
    } catch {
        print("error: \(error)")
        exit(1)
    }
}
RunLoop.main.run()
