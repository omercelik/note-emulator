import AVFoundation
import CoreGraphics
import CoreVideo

/// Records committed panel states to H.264 MP4 (CAP-01). Each state is held from its virtual
/// timestamp until the next one, so an e-paper image that stays for 40 s lasts 40 s in the file,
/// and time the emulator spent paused does not appear.
/// Not thread-safe: one owner calls it in order (the device window uses it from the main actor).
public final class FrameRecorder: @unchecked Sendable {
    public let url: URL
    private let writer: AVAssetWriter
    private let input: AVAssetWriterInput
    private let adaptor: AVAssetWriterInputPixelBufferAdaptor
    private let width: Int
    private let height: Int
    private var origin: Double?
    private var last: (CGImage, Double)?
    public private(set) var frames = 0

    public init(url: URL, width: Int, height: Int, scale: Int = 2) throws {
        self.url = url
        // H.264 wants even sizes; 2× keeps e-paper pixels sharp after encoding.
        self.width = (width * scale + 1) & ~1
        self.height = (height * scale + 1) & ~1
        try? FileManager.default.removeItem(at: url)
        writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: self.width,
            AVVideoHeightKey: self.height,
        ])
        input.expectsMediaDataInRealTime = false
        adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: self.width,
            kCVPixelBufferHeightKey as String: self.height,
        ])
        writer.add(input)
        guard writer.startWriting() else { throw writer.error ?? RecorderError.failed("startWriting") }
        writer.startSession(atSourceTime: .zero)
    }

    /// A new committed state at `virtualSeconds`. Earlier-or-equal times replace the pending state.
    public func append(_ image: CGImage, virtualSeconds: Double) throws {
        if origin == nil { origin = virtualSeconds }
        if let (previous, at) = last, virtualSeconds > at {
            try write(previous, at: at)
        }
        last = (image, virtualSeconds)
    }

    /// Hold the last state until `virtualSeconds` and close the file.
    public func finish(virtualSeconds: Double) async throws {
        var end = CMTime.zero
        if let (image, at) = last {
            try write(image, at: at)
            end = CMTime(seconds: max(virtualSeconds, at + 0.001) - (origin ?? at), preferredTimescale: 1000)
        }
        input.markAsFinished()
        writer.endSession(atSourceTime: end)
        await writer.finishWriting()
        if writer.status != .completed { throw writer.error ?? RecorderError.failed("finishWriting") }
    }

    private func write(_ image: CGImage, at seconds: Double) throws {
        let time = CMTime(seconds: seconds - (origin ?? seconds), preferredTimescale: 1000)
        while !input.isReadyForMoreMediaData { usleep(1000) }
        guard let pool = adaptor.pixelBufferPool else { throw RecorderError.failed("pixel buffer pool") }
        var buffer: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
        guard let buffer else { throw RecorderError.failed("pixel buffer") }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let context = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer), width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer), space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { throw RecorderError.failed("context") }
        context.interpolationQuality = .none
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        guard adaptor.append(buffer, withPresentationTime: time) else { throw writer.error ?? RecorderError.failed("append") }
        frames += 1
    }
}

public enum RecorderError: Error {
    case failed(String)
}
