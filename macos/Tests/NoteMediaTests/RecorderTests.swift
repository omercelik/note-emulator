import AVFoundation
import CoreGraphics
import Foundation
import Testing
@testable import NoteMedia

private func solid(_ gray: UInt8, width: Int = 400, height: Int = 300) -> CGImage {
    let bytes = [UInt8](repeating: gray, count: width * height)
    let provider = CGDataProvider(data: Data(bytes) as CFData)!
    return CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: width,
                   space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: 0),
                   provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
}

private func brightness(_ image: CGImage) -> Double {
    let w = 8, h = 8
    var px = [UInt8](repeating: 0, count: w * h)
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                        space: CGColorSpaceCreateDeviceGray(), bitmapInfo: 0)!
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    return Double(px.reduce(0) { $0 + Int($1) }) / Double(px.count)
}

@Suite struct RecorderTests {
    /// CAP-01: two known states keep their order and their virtual-time durations.
    @Test func mp4HoldsEachStateForItsVirtualDuration() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("cap01-\(UUID()).mp4")
        defer { try? FileManager.default.removeItem(at: url) }
        let recorder = try FrameRecorder(url: url, width: 400, height: 300)
        try recorder.append(solid(0), virtualSeconds: 10.0)     // black from 0 s
        try recorder.append(solid(255), virtualSeconds: 11.5)   // white from 1.5 s
        try await recorder.finish(virtualSeconds: 13.0)         // until 3 s

        let asset = AVURLAsset(url: url)
        let duration = try await asset.load(.duration).seconds
        #expect(abs(duration - 3.0) < 0.05, "duration \(duration)")
        let generator = AVAssetImageGenerator(asset: asset)
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        let early = try await generator.image(at: CMTime(seconds: 1.0, preferredTimescale: 600)).image
        let late = try await generator.image(at: CMTime(seconds: 2.5, preferredTimescale: 600)).image
        #expect(brightness(early) < 30)
        #expect(brightness(late) > 225)
        #expect(early.width == 800 && early.height == 600)
    }
}
